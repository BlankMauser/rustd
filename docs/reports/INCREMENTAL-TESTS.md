# Tests that run only when what they can run changed: report

Branch `explore/incremental-test-clippy`, off `perf-climb` at a8db50abc5. The design is
`docs/incremental-tests.md`. This says what was built, what was measured and how, and what
does not hold up. Raw logs are in the archive attached to the session, named below.

Short version: the selection is sound in everything measured (no remembered result was
ever wrong in 188 compared scripted steps), but on the two real projects it never skipped a single
test after a source edit. It only pays when nothing changed, and on the synthetic crate.
The lints, the watcher and the shared `cfg(test)` graph are design only. One check of
`harness/cargo_rustd.py --small` that passes on `perf-climb` fails on this branch.

## What was built

- `crates/hir-ty-codegen/reach.rs`: for each `#[test]` function, a 128-bit print of
  everything it can run, taken over the graph of compiled functions, instances and statics
  the daemon already holds (requests of each node, strongly connected parts condensed,
  panic locations included). Tests that reach a writable or thread-local static in common
  are one group and rerun together. Functions of `extern` blocks are leaves. A node that did
  not compile makes the print unknown, and such a test always runs.
- The worker writes `<object>.reached` after each linked build (40 ms on ripgrep when warm).
- `crates/rustd/src/cargo/select.rs`: `cargo rustd test` lists the tests, runs with
  `--exact` the ones whose print, group or surroundings differ from when they last passed,
  and prints the rest as `test x ... ok (cached)`, with `N passed (C cached)` in the
  summary. Only passes and ignores are remembered; a failure always runs again. Results
  are in `<object>.results`. `--rerun-all` or `RUSTD_TEST_CACHE=0` runs everything.
  The surroundings are one hash: cargo's build generation, the plan, the environment,
  stamps of the non-source files of the package and workspace, and the print of every
  program of the build that is not a test.
- Two refusals the campaign made necessary, both for builds that were wrong on
  `perf-climb` too: a static linked as rustc built it whose initializer was edited, and
  code only a `#[used]`/`#[link_section]` static reaches. A changed list of tests is also
  refused (the harness is rustc's).
- `harness/incremental/campaign.py` (edits, compared with plain `cargo test` in a second
  copy), `harness/incremental/hazards/` (a crate with one case per hazard of the design),
  `harness/incremental/bench.py` (timings).

## Correctness of selection

Method: two copies of a project. After each edit, in copy A `cargo rustd test` and then
`cargo rustd test --rerun-all`; in copy B plain `cargo test`. A miss is a test printed as
cached whose result differs from either of the other two runs. Mutations are one-operator
changes to non-test code, each made and then undone, seed 1.

| project | steps | misses | result-changing steps (tests flipped) | refused by rustd | steps with a cached test |
|---|---|---|---|---|---|
| hazards (synthetic, 48 tests) | 106 | 0 | 90 (210) | 7 | 65 |
| ripgrep 15.1.0 (443 tests) | 42, 2 hung | 0 | 12 (1310) | 0 | 0 |
| just (1443 tests, `--lib --test integration`) | 42 | 0 | 39 (9388) | 1 | 3 |

- hazards: 0 failed. The 7 refusals are an edited static, a const fn in an array length
  of a static, a thread-local's initial value, a function run before `main`, `#[ignore]`
  removed, a test added, and a type error. Each printed an error and ran nothing.
- ripgrep: 0 failed. Two mutations made `rg` loop for ever in all three runs; the campaign
  kills a run after 120 s and does not compare those steps. 37 of the 40 compared steps
  were built by cargo ("a library its unit tests are linked with as rustc built it"),
  because the mutations fell in the library crates.
- just: the campaign prints 4 failed. In all four every test ran (0 cached), and the
  `test result` totals of the three runs are equal in all 42 steps; the differing tests
  are lines where a child's output landed on the test's line (`test x ... All tests
  passed.`) and the campaign's parser read no result. So they are the parser's, not
  results that differ, but the per-test comparison of those four steps is by totals only.
- 106 + 40 + 42 = 188 compared steps, 0 misses. That is evidence the prints are
  conservative enough, not a proof: on the real projects almost nothing was served from
  memory after an edit, so the real-project steps mostly test that rustd's build agrees
  with cargo's, and the selection itself is exercised by hazards (65 steps with cached
  tests; 54 results over 7 steps recalled after an undo).

## Performance

`bench.py`: two copies, each warmed twice. Each edit is made then undone five times (10
samples a way). A sample is the wall time of the whole command, build and tests, on this
VM, default linker on both sides, debug profile. Order within a turn: `cargo test`,
`cargo rustd test`, `cargo rustd test --rerun-all`; the last runs in the same copy after
the selective run, so it has nothing left to build and is the test time alone. Medians
(min-max), seconds:

| project, edit | cargo test | cargo rustd test | of which ran / cached | rustd --rerun-all |
|---|---|---|---|---|
| ripgrep, nothing changed | 0.94 (0.93-0.97) | 0.04 (0.04-0.12) | 0 / 443 | 0.95 |
| ripgrep, body in `crates/core` (`hiargs.rs:642`) | 1.47 (1.43-2.02) | 1.36 (1.30-3.54) | 443 / 0 | 0.94 |
| ripgrep, body in a library (`printer/src/util.rs:434`) | 1.67 (1.60-1.97) | 2.26 (2.08-2.70), built by cargo | 443 / 0 | 15.84 |
| just, nothing changed | 1.83 (1.67-2.06) | 0.06 (0.05-0.11) | 6 / 1437 | 2.07 |
| just, body (`src/unindent.rs:32`) | 3.33 (2.80-3.59) | 2.51 (2.41-6.77) | 1436 / 0 | 1.80 |
| hazards, nothing changed | 0.02 | 0.03 | 0 / 48 | 0.03 |
| hazards, body (`src/lib.rs:191`) | 0.18 (0.16-0.23) | 0.06 (0.05-0.25) | 42 / 5 | 0.03 |

(The six tests just reruns with nothing changed are its six that fail here under plain
cargo too; failures are not remembered.)

What this says:

- With nothing changed, the run is 20 to 30 times faster. That is the case of an agent
  running the tests again to be sure.
- After any source edit to ripgrep or just, every test ran. The gain over cargo there is
  the daemon's faster build, which `perf-climb` already has. Selection added nothing.
- An edit to a library of ripgrep is slower than cargo (cargo builds, then rustd's
  bookkeeping), and the run after it costs 15 s because the daemon loads cargo's build
  again. Both are from before this work but this is where most real edits land.

Why nothing was skipped, in order of weight:

1. Integration tests that run the built binary (all 329 of ripgrep's, all 936 of just's).
   The print of every non-test program is part of every test's surroundings, so any edit
   that changes the binary invalidates every result. The graph cannot say which of the
   binary's code a test's child process runs.
2. A package with a library and a binary: the binary's unit tests link the library as
   rustc built it, so a library edit is cargo's build and nothing is remembered.
3. Groups. ripgrep's 114 unit tests are one group but for two (shared `OnceLock`
   statics). just's 507 unit tests are 245 groups, the largest 263; for the `unindent`
   edit 324 of 507 prints changed, so with (1) out of the way 183 unit tests would have
   been skipped, of a suite that takes 0.05 s.

## Required commands, final state

- `cargo fmt --all -- --check`: clean.
- `cargo test --release -p hir-ty -p rustd`: 41 + 96 passed, 0 failed.
- `cargo clippy --release -p hir-ty -p rustd --all-targets -- --cap-lints warn`: exit 0.
  `select.rs` adds six `println!` warnings and one `Command::new` warning of the kinds
  `run.rs` already has.
- `RUSTD_BIN=... harness/cargo_rustd.py --small`: **FAIL**, one check:
  `edit: cargo rustd test --test it: 101 for 0`. `perf-climb` passes it. The fixture's
  `util` has a `thread_local!`; after `util` is edited, the program of `--test it` joins
  the daemon, links rustc's copy of the thread-local's static from before the edit, and
  the new refusal fires because nothing says whether that static changed. The refusal is
  right to be suspicious (on `perf-climb` an edited initializer there is linked stale,
  silently) but here the static did not change, so it is a regression for that case.

## Known unsoundness and gaps

Unsound, or not shown sound:

- Out of scope by design and so able to skip wrongly: time, network, randomness, files
  outside the package and workspace directories, anything under `target/`, state inside
  dependencies the daemon does not compile, a test that depends on which other tests ran
  beside it without sharing a static the graph sees.
- Vtables, function pointers and drop glue are followed because they are relocations in
  compiled requests; the hazards crate has a case for each and none missed, but there is
  no argument beyond that. Layout, name resolution and trait selection changes are caught
  only through the serials of the code they change; same evidence, same limit.
- Functions the daemon leaves to rustc are leaves. If one is in an edited crate the build
  is refused, so this should not skip wrongly, but it rests on that refusal being complete.

Gaps (rustd does less than asked):

- No lint API, no lints, no `cargo rustd clippy`, no comparison with clippy. Design only
  (section 4 of the design).
- No watcher. Design only (section 5).
- No shared `cfg(test)` graph. Section 3 of the design has what was found (a unit-test
  worker sets `cfg(test)` on every workspace crate, so nothing is shared with the normal
  build and libraries go to rustc) and what it would take. Not measured beyond that.
- The fallback to cargo is still there, and announced (`rustd: ...; building with cargo`),
  not an error: library edits under a binary's unit tests, `harness = false`, doc tests,
  proc-macro crates' tests, build script, proc macro and manifest edits, command lines
  that pick targets (`--lib --bins --tests`). After one, every test runs and nothing is
  remembered. This does not meet "no fallback".
- Refusals that run nothing: a test added or removed or its attributes changed; an
  edited static rustc's copy is linked for; code reached only from `#[used]` statics; the
  thread-local case above, which is over-broad.
- `--explain` (why a test ran) is not built.
- The campaign's parser loses a result when a child prints on the test's line.

## What I would do next

1. Fix the thread-local refusal before anything else is merged: compile thread-locals in
   the daemon instead of importing rustc's, or keep per-file edit times so a program that
   joins late can tell whether rustc saw the static's source.
2. The `cfg(test)` overlay (one worker, each member library twice in the crate graph).
   It removes the most common fallback and is what the owner asked about; without it most
   real edits never reach the selector.
3. Per-test tracing of what a spawned binary runs. Static reachability cannot split
   integration tests that exec the program. Recording, per test, which functions the child
   entered (the daemon compiles every function and can add an entry counter cheaply) gives
   a dependency set that is exact for deterministic tests, and falls back to the whole
   program when a test spawns something untraced. This is where the time is on both real
   projects.
4. Only then lints and the watcher; the per-body cache they need is the same store.

As it stands I would not merge this: the measured gain on real projects is the
no-change case only, and it costs one harness regression.

## Logs

`hazards.out`, `rg.out`, `just.out` (one line per step), `*.jsonl` (per step: cached, ran,
changed, misses, seconds), `hazards.log`, `rg.log`, `just.log` (every command's output),
`bench-*.out` and `*.bench.jsonl` (every timing sample), `test.txt`, `clippy.txt`,
`harness-small.txt`, `harness-base.txt` (the same harness on `perf-climb`).
