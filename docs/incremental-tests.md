# Tests and lints that follow the graph

Status: design for an exploratory branch. What was built and measured is in
[reports/INCREMENTAL-TESTS.md](reports/INCREMENTAL-TESTS.md); this file is the design, the
alternatives that were dropped, and the hazards with what is done about each.

## The goal

An agent runs `cargo test` and `cargo clippy` after nearly every edit. Both redo everything:
cargo rebuilds the crates the edit is in, every test runs, clippy checks every crate again.
The daemon already knows, per function, whether its machine code is what it was at the last
build. A test whose code is the same, and all the code it can reach, has the same result.
A function body nothing touched has the same lints.

The rule for tests is one-sided: **a test whose outcome would have differed is never
skipped.** Running too much is acceptable, skipping wrongly is not. Where the graph cannot
answer, the test runs.

## What the graph is

Compiling a function is a database query (`compiled_root`, `compiled_instance`,
`compiled_static` in `crates/hir-ty-codegen`). Its result is machine code plus `Requests`:
every function the code calls or takes the address of, and every data object it points to
(constants, vtables, statics, panic locations). `root_serial`, `instance_serial` and
`static_serial` are 128-bit hashes of those results. So the daemon holds a graph whose
nodes are monomorphised functions and statics, whose edges are "refers to", and each node
carries a hash of its code.

This graph is after everything the front end does: name resolution, trait selection, type
inference, const evaluation, monomorphisation, layout, drop elaboration, the inlining rustd
does. The selection below never reasons about source. It asks one thing: is any machine
code this test can execute different from last time?

## 1. Which tests can have another result

### Definition

For a test function `t` of a test executable, let `R(t)` be the set of nodes reachable from
`t` over the request edges. The **fingerprint** of `t` is a hash of

- for every node in `R(t)`: its serial, and the source locations its code reports
  (`Mark::locations`: file, line, column of each panic site, which stitching fills in and
  which a test can observe through a panic message or `Location::caller`);
- the edges among them (the hash is a Merkle hash over the condensation of the graph, so
  it is computed for every test in one pass over the program, not once per test);
- the ambient parts of the program, below;
- the inputs the graph cannot see, below.

A test is run when its fingerprint is not the one its last passing result was recorded
under. Otherwise the recorded result is reported.

Why this is sound for code: a test's outcome is a function of the instructions it executes
and the data they read. Every instruction of the workspace's crates that it can execute is
in a function it reaches by a call or a function pointer; every function pointer was made
by code that referred to the function, which is a request edge from that code (or from a
static's value, or a vtable, which are nodes or blobs with edges of their own). So if no
node of `R(t)` changed, and no edge, the same instructions run on the same constant data.
What is left is state that is not code: the hazards under "what the graph cannot see".

### What each kind of edit does, and why the graph sees it

| Edit | How it shows |
|---|---|
| Body of a function | Its code differs: serial of that node. Every test reaching it reruns. |
| Signature, callers updated | The callee's code and each caller's code differ. |
| Generic function | Every instance is its own node, compiled from the body: all instances differ, and only tests that reach one rerun. |
| Generic instantiated only in tests | The instance is a node only the test reaches. |
| Trait objects | A vtable is a data request of the function that makes the `dyn` value, with an edge to each method. A test reaches a method by `dyn` call only if it reaches code that made such a vtable. Over-approximate (every method of every vtable it can make) and never under. |
| Function pointers, closures | Taking the address is a function request of the code that takes it. Closures are instances. |
| Statics, consts | A static is a node whose serial is its value; a `const` is evaluated into the code that uses it. Promoted constants are blobs of the function. |
| Drop glue | `InstanceDef::DropGlue` instances, requested by the code that drops. |
| Type or layout change | Offsets, sizes and drop glue are in the code of every function that touches the type, so their serials differ. A function that only passes a pointer through does not change, and its behaviour does not either. |
| Name resolution or trait selection changing without a body edit (a new impl, a new inherent method that shadows, an import) | The caller's code now names another callee: its serial differs. |
| Macro body | The expansion is the body that is compiled. |
| `#[inline]`, `#[cold]`, attributes that change code | In the code. |
| Lines moved (comment or blank line above) | Code is unchanged; panic locations change. They are part of the fingerprint, so tests below the insertion that reach a panic site in the file rerun. This is deliberate: `#[track_caller]`, `Location::caller()` and panic messages make lines observable. |
| `#[ctor]`, `inventory`, `linkme`, `#[used]`, `#[no_mangle]`, `link_section` statics | Nothing in the test names them; the linker or the loader does. They are **ambient**: every such static and exported function of the program, and what they reach, is part of every test's fingerprint. |
| The harness, `main`, the allocator, the panic handler | rustc's harness is constant until cargo builds again (epoch, below). `#[global_allocator]` and `#[panic_handler]` statics and functions are ambient. |
| A test added, removed, or its `#[ignore]`/`#[should_panic]` changed | The list of tests is rustc's to write. Today that refuses the build; with the harness written by rustd (see "next") the attributes become part of the fingerprint. |

### Code the daemon did not compile

- **Dependencies and the standard library.** Built by rustc, constant until cargo builds
  again. Generic functions of dependencies instantiated with workspace types are instances
  and are in the graph. A call from dependency code back into the workspace goes through a
  function pointer or vtable that workspace code made (an edge), or through a symbol by
  name (`#[no_mangle]`: ambient).
- **Workspace functions left to rustc** (rustd could not compile them, rustc's copy is
  linked). If the function compiled far enough to have requests, those are its edges.
  If not, what it calls is unknown: a test that reaches one **depends on everything**: its
  fingerprint includes the hash of every node of the program.
- **Workspace crates the program links as rustc built them** (today: the libraries under a
  unit-test executable). They are constant until an edit to them, which makes the build
  cargo's, which starts a new epoch.

### What the graph cannot see

Each line says: tracked, conservative (rerun), or out of scope, and why.

| Hazard | Treatment |
|---|---|
| Dependency change, feature or cfg change, toolchain, `RUSTFLAGS`, manifest, lock file, build script, proc macro | **Tracked as epoch.** Each makes the build cargo's (existing guards in `cargo/state.rs`). The fingerprint includes the stamp of the cargo build the program is linked against, so every result recorded before it is void. |
| `include_str!`, `include_bytes!`, `env!` | **Tracked.** Their value is in the code. Included files are watched by the daemon already. |
| Files read at run time: fixtures, snapshot files, golden files | **Conservative.** Every file under the package directory and the workspace root that is not a Rust source the daemon loaded, outside `target` and `.git`, is stamped (path, size, mtime). A change reruns every test of every package that could see it (the package's own directory: that package; elsewhere in the workspace: all). Files outside the workspace (`/etc`, `$HOME`, `/tmp`) are **out of scope**. |
| Environment variables | **Conservative.** The fingerprint includes a hash of the whole environment the tests are run with, minus a short written list of variables that change with every shell (`_`, `SHLVL`, `OLDPWD`, and the ones cargo and rustd set per run). A test that reads one of those is a known gap. |
| Arguments after `--` | **Tracked.** Flags that change what a test does (`--include-ignored`, `--ignored`, `--test-threads`, `--nocapture`…) are part of the key. Name filters only select. |
| Programs the tests execute | **Conservative.** Integration tests run the package's binaries (`CARGO_BIN_EXE_*`, or by path). Every test of a test executable includes the whole-program hash of every binary the same invocation builds. For ripgrep that means the integration suite reruns on any change `rg` can reach, which is the truth: every one of those tests runs `rg`. |
| State shared between tests of one executable (a `static` one test writes and another reads; thread-locals when tests share a thread) | **Conservative.** Tests that reach the same writable static (`CompiledStatic::writable`, or a thread-local) are one group: the group reruns together and a member's fingerprint includes all of the group's. State inside dependencies (a global logger, `std::env::set_var`, the working directory, a file one test writes and another reads) is **out of scope**: the graph has no node for it. |
| Test ordering | Follows from the above: with groups run together, a cached test's recorded result came from a process in which every test that shares visible state with it also ran. Order within libtest is not deterministic under cargo either. |
| Time, randomness, network, other processes, hardware | **Out of scope**, the same position as Go's test cache: a test that reads the clock can differ between two runs of `cargo test` with no edit at all. Two mitigations: a failing result is never remembered, so a flaky failure is always run again; and `--rerun-all` runs everything. |
| `#[should_panic]`, `#[ignore]` | Part of rustc's harness: a change refuses the build today (cargo's build, new epoch). An ignored test is reported as ignored without a fingerprint. |
| Doc tests | Each is its own program. Not selected in the prototype: they run every time, as today. The same fingerprint applies per example (whole-program hash) and is listed under "next". |
| Integration tests in other crates | Each test target is a program with its own graph; nothing special. |
| `harness = false` | The executable is the test. One result per executable, keyed by the whole-program hash; reported as cached when it passed. |
| Benchmarks (`#[bench]`) | Run as tests by libtest; treated as tests. |
| Output of a cached test | Not replayed. A passing test prints nothing under libtest unless `--nocapture`, which is part of the key and disables caching in the prototype. |

### Alternatives dropped

- **Selection by source file or by crate** (what cargo does, what `cargo-nextest` archives
  do). Sound but coarse: one edit to a hub crate reruns everything, and it knows nothing of
  generics or `dyn`.
- **Selection by the HIR-level call graph (name-resolved calls in bodies).** Cheaper to
  compute but misses drop glue, operators, auto-deref, coercions to `dyn`, closure
  captures, const evaluation, layout. Every one of those would need its own rule, and one
  forgotten rule is a wrongly skipped test. The machine-code graph has no such rules.
- **Selection by coverage** (run once instrumented, record which functions each test
  executed, rerun when one of those changes). More precise than reachability, but unsound
  for exactly the edits that matter: an edit that makes a test take a branch it did not
  take before reaches code the old run never recorded. Sound only if the fingerprint also
  covers every function reachable from the executed ones, which is reachability again.
- **Hash of the test executable.** Trivially sound, never skips anything after any edit.
- **Tracking what tests read at run time** (interposing `getenv`/`open` as Go's test cache
  does). This is the right answer to env and files and would replace the two conservative
  rules above with precise ones. Not built: it needs a preloaded library per platform and
  attribution to tests that share a process. Listed under "next".
- **Remembering failures.** A recorded failure could be reported without running. Dropped:
  the failure message is the point of running a failing test, and flaky tests fail
  intermittently. A failing test is always run.

## 2. Remembering results

Per test executable, next to the object the daemon builds for it:
`target/rustd/<key>/programs/<program>/bin.results`, beside `bin.reached`, the fingerprints
of the build that linked it. `<key>` already separates command lines (profile, features,
flags). The file maps a test name to the fingerprint it last passed under, and holds one
hash of everything the client alone knows; when that differs nothing in the file counts.
`cargo clean` removes it.

It is written by the client after a run, from libtest's own output of that run, only for
tests that reported `ok`. A crash of the test process, a timeout, or output the client
cannot parse records nothing.

The daemon computes fingerprints as part of the build that links the executable, and
answers them with the build. The client adds what only it knows (environment, arguments,
file stamps) and compares.

What is printed keeps libtest's shape, so tools that read it keep working, and says which
results are remembered:

    running 114 tests
    test flags::tests::parse ... ok
    test flags::tests::unknown ... ok (cached)
    ...
    test result: ok. 114 passed (112 cached); 0 failed; 0 ignored; 0 measured; 0 filtered out

`cargo rustd test --rerun-all` (or `RUSTD_TEST_CACHE=0`) runs everything and still records.
Not built: an `--explain` that prints, for each test that runs, the first changed node
that reaches it.

## 3. Test builds and `cfg(test)`

See the report for the measurements. The design question is the owner's: can one graph
hold the normal build and the test build of a crate, so that going from `build` to `test`
or `clippy` costs only what is really different?

Today a worker is one database with `cfg(test)` either on every workspace crate or on
none. Unit tests need the first, integration tests and programs the second, so
`cargo rustd test` runs two workers that share nothing in memory, and the libraries under a
unit-test executable are linked as rustc built them (rustc built them without `cfg(test)`,
the worker has them with). An edit to such a library makes the build cargo's.

The overlay design: rust-analyzer's crate graph gets, for each workspace target that has
unit tests, a second crate with the same root file and `cfg(test)` set, depending on the
normal crates of its dependencies. Then:

- one worker serves `build`, `run`, `test`;
- everything below the crate under test is shared between build and test exactly, as the
  same database crates;
- the crate under test exists twice. Parsing and item trees are per file and shared;
  def maps, inference and MIR are per crate and are not;
- its compiled functions are shared through the store where the code is byte-identical,
  if store keys do not carry the crate's build identity.

When is the test build "the normal crate plus additions"? When `cfg(test)` only adds items.
It is not when the crate has `cfg(not(test))`, `cfg_attr(test, …)`, a `#[cfg(test)]` field
or variant (layout), a `#[cfg(test)]` impl (trait selection in non-test code can change),
or a `#[cfg(test)]` import that shadows. These can be detected syntactically from item
trees, but the detection is not needed for correctness in this design: each crate is
analysed under its own cfg, and sharing happens only at the level of compiled code that
came out identical.

## 4. Lints

clippy's lints are written against rustc's HIR and type tables and cannot run on
rust-analyzer's. Running clippy beside the daemon is ruled out. The design is a small lint
interface over rust-analyzer's bodies and inference results:

- a lint is a function from (`db`, a body, its inference result) to diagnostics addressed
  by expression or pattern id;
- the set of lints for a body is one salsa query keyed by the body. salsa then gives
  "only bodies an edit touched" for free: the query reruns only when the body, its
  inference, or a signature it read changed;
- results are stored by expression id, and turned into file, line and column when
  reported, so an edit above a function does not invalidate its lints;
- item-level lints (naming, missing docs…) are a second query per module;
- rust-analyzer's own diagnostics (`ide-diagnostics`) are reported through the same
  channel, per file.

`cargo rustd clippy` asks the daemon for the diagnostics of the workspace's crates and
prints them in rustc's format. Agreement with real clippy is measured, not assumed; every
lint name printed is clippy's name for the same finding.

## 5. Watcher

`cargo rustd watch test|clippy`: the client stays connected; the daemon, which already
notices file changes, rebuilds on a change, answers fingerprints, and the client runs the
affected tests and reprints the summary. For lints the daemon sends the diagnostics that
changed. No polling by the client and no second process tree.

## Known limits of the approach

Collected here so they are not spread through the text; the report has the measured ones.

- State outside the program (time, network, randomness, files outside the workspace,
  variables on the skip list, state inside dependencies shared between tests) is not
  tracked.
- A selective run starts fewer tests in the process than a full run. A test that passes
  only when an unrelated test (no shared visible static) runs beside it is not detected.
- Reachability is an over-approximation: tests behind a hub function rerun together.
- Everything depends on serials being a faithful hash of the linked code. Functions left
  to rustc that could not be compiled have no serial; tests reaching them depend on
  everything.

## What the prototype is, and what building it showed

Built: section 1 and 2 for test executables with the standard harness that the daemon
links (`crates/hir-ty-codegen/reach.rs`, `crates/rustd/src/cargo/select.rs`), and the
campaign that checks it (`harness/incremental/`). Not built: sections 3 to 5.

What the code does differently from the text above, or more precisely:

- The graph needs no special case for trait objects, function pointers or drop glue. A
  vtable is a blob of the function that makes the trait object, and the blob's
  relocations are requests of that function; so are the functions a body takes the
  address of. Following the requests of each compiled function and static is the whole
  walk.
- A function of an `extern` block is a leaf. Exported functions and the statics a linker
  or a runtime finds (`#[used]`, `#[link_section]`) are reached by every test.
- The tests of a program that reach a writable or thread-local static in common are one
  group, and a test's fingerprint covers the code of its whole group. Statics written by
  what runs before `main` are common to every test: a crate with one registration that
  writes a static has all its tests in one group.
- The fingerprint of a function includes the lines its panic locations report. An edit
  that moves code down reruns every test that reaches code below it in the file. This is
  the largest loss of precision met; a test can observe a line (`Location::caller`), so
  dropping it needs a proof that the test does not.
- A node that did not compile makes the fingerprint of everything that reaches it
  unknown, and a test with an unknown fingerprint always runs.

Two ways a test build of the daemon was wrong before this work, found by the campaign and
unrelated to selection (`origin/perf-climb` at a8db50abc5 has both):

- A static the build takes from rustc (any static of a crate with a function left to
  rustc that the daemon cannot see into) kept rustc's value after its initializer was
  edited. The daemon now refuses such a build (`Frozen::out_of_date_static`).
- Code that only a `#[used]` or `#[link_section]` static points to (a function run before
  `main`) stayed rustc's after an edit. For a program with tests the daemon now refuses
  the build when what such statics reach changed or is not all known.

What `cargo rustd test` still gives to cargo or refuses, each of which runs every test or
none, and never a remembered result:

- a package with a library and a binary: the binary's unit tests link the library as
  rustc built it, so every edit to the library is cargo's build. This is the common case
  and the reason for section 3;
- a test added, removed, or given other attributes (the harness lists tests, and is
  rustc's);
- `harness = false` targets, doc tests, the unit tests of a proc-macro crate;
- edits to build scripts, proc macros and manifests (cargo builds, everything runs).
