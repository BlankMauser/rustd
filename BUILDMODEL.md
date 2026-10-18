# Cargo's build model in rustd

Branch `rustd-build-model`. The second round starts from `rustd-collate` at 0151c01218
merged in. Linux x86-64, Rust 1.99.0, LLVM 20.

This is a survey of which parts of cargo's build model rustd follows, with a small project
for each part under `harness/buildmodel/` and a script, `harness/buildmodel.py`, that builds
every one with cargo and rustc alone and again with rustd's code linked in, runs both and
compares what they print and return. Where a case has steps, one input is changed at a time
under the same daemon and each step is compared with cargo again.

## Result

Measured with

    LLVM_SYS_201_PREFIX=/usr/lib/llvm-20 cargo build -p rustd --release --features llvm
    cd harness
    RUSTD_BIN=... python3 buildmodel.py --backend cranelift --jobs 6 --table table-cranelift.md
    RUSTD_BIN=... RUSTD_OPT=2 RUSTD_EMIT=fast python3 buildmodel.py --backend llvm --jobs 6 --table table-llvm.md

Result: 97 of 97 as expected under Cranelift and 97 of 97 under LLVM, 0 WRONG. The two
tables are the same but for the backend column: 92 supported and 5 refused. Of the 5, 2 are
projects cargo itself turns down, 2 are `lto` and 1 is doctests. The cases have 31
invalidation steps between them: 29 come out the same as cargo's rebuild and 2 are refused
with a reason (an edit to a crate the daemon was not asked to build, see below). None is
"start it again" any more.

The first round ended at 90 cases per backend, 82 supported and 8 refused, with 16 steps of
which 7 were refused.

A row is one of three things. "supported": the two programs print and return the same, and
every function of the crates the case names was compiled by rustd. "refused": rustd said why
it does not build this and nothing ran differently; that includes a function it left to
rustc with a reason. "WRONG": a different output, a crash, or a refusal without a reason. The
script fails on a WRONG row and on any row that is not what its `case.json` expects, so a
feature that stops working, or a refusal that starts to pass, is seen.

## Second round

### 1. A checkout is loaded again in place

Before, a change under a running daemon to `Cargo.toml`, `Cargo.lock`,
`.cargo/config.toml`, `RUSTFLAGS`, or a build-script input that changes what the script
says was answered with "start it again". Now the daemon loads that checkout's workspace
again and goes on.

How it works. `inputs.rs` keeps, for each checkout, the files cargo reads of it (manifests,
lock file, configuration files), the flag variables of the client that asked, and what the
build scripts said. Before a request is served, the checkouts it is about are asked whether
any of that is stale. For a stale one, cargo is asked again for the workspace of that
checkout only (`Checkouts::reload`), and the new crate graph is put in place of the old
with `load-cargo`'s `Loaded::replace`. Other checkouts are not looked at. Each crate is
then compared with what it was before (its data, cfg and environment): a crate that is the
same keeps its salsa state, and since the disk store is keyed on a crate's content, its
stored code is still found. Crates that differ are compiled again. When the graph itself
changed, the programs of that checkout start over: their functions are compiled again or
taken from the store, and the client's link line, sent with the request, is the new one.

A request and a reload never interleave. The daemon serves builds one at a time; a request
that arrives while a build runs waits, and staleness is checked and the reload done before
that request's build starts. So a build sees the model from before the reload or the one
after it, whole.

Two things had to be fixed on the way. Files that are new to a checkout after a reload (a
new `build.rs`, a new member) had no source root and the daemon panicked; `load-cargo` now
puts them in the root of the directory they are in. And variables that were only meant for
running cargo were also given to crates as if `env!` could read them; `project-model` now
keeps the two apart (`extra_env_for_commands_only`).

Measured. The 7 steps that were refused are "same" now (`config_env`, the `RUSTFLAGS` and
configuration rustflags cases, the manifest and build-script cases in the table). New
cases, all "same" under both backends:

- `reload_dep_added`: a dependency is added, then removed again.
- `reload_lock_version`: the version of a dependency in `Cargo.lock` goes up, then down.
- `reload_feature_toggled`: a feature of a dependency and a default feature of the package
  are turned on in the manifest, then both off, with 6
  clients asking at once after each change.
- `reload_member_added`: a new workspace member is added and depended on.
- `reload_build_script_added`: a `build.rs` appears that sets a cfg and an env variable,
  then says something else.
- `reload_git_checkout`: `git checkout` between two commits that differ in all of the
  above, there, back and there again, with 6 clients asking at once each time.

`worktrees.py` has a new step where the manifest of one of several checkouts changes in a
way no crate reads; it passes 62 of 62 (plain, `--join`, `--together`) and 71 of 71
(`--overlay`), which includes its counts of what each checkout compiles.

Inferred, not measured: that "6 clients at once" exercises every interleaving. It shows
that six requests arriving together after a change all get the new program; the argument
that none can see a mix is the one above, from how the daemon orders builds, not a test
that tries to catch one mid-reload. How long a reload takes was not measured either; it is
one `cargo metadata` and the build scripts of that checkout.

One thing this turned up, which was silently wrong before and is a refusal now. A daemon
asked to build only `app` (`--crate app`) took an edit to a path dependency of `app`,
answered "ok", and the program still ran rustc's old copy of the dependency. That is now
refused: "... is of `dep`, which the program uses and the daemon was not asked to build:
rustc's copy of it is from before the edit, name the crate with `--crate`". Cases
`dep_not_named` and the second step of `reload_member_added` (an edit inside a member that
did not exist when the daemon was told what to build). After a reload the refusal is not
given, because the client has just sent a link line from a fresh cargo build.

### 2. Debug info from the unit graph

`crates/rustd/src/profile.rs`, which read `Cargo.toml` itself, is deleted, and the `toml`
dependency of `rustd` with it. `Plan::debug()` in `plan.rs` says whether the packages the
build was asked for have debug info, from the profile of their units in cargo's unit graph,
with `-g` and `-C debuginfo=` of the rustflags over it. That sees `.cargo/config.toml`
profiles and per-package overrides because cargo has already applied them. It is kept per
checkout and read again on a reload. `RUSTD_DEBUGINFO` still overrides it. `debuginfo.py`
passes under both backends.

### 3. Overflow checks and debug assertions per package

Both rows that were refused (`overflow_checks_package`, `release_package_checks`) are
supported.

`debug-assertions` was already per crate, since it is a cfg. For overflow checks, an
instance of a function now carries whether it is compiled with checks (`Instance::checked`
in `codegen.rs`). A function that is neither generic nor `#[inline]` takes the setting of
the crate that defines it. A generic or `#[inline]` function takes the setting of the code
that asks for it, so the same generic function can exist twice in one program, once
checked and once not. The stable name of a checked instance ends in " checked", and that
name is what the store is keyed on, so the two never share an entry; the single
whole-build overflow setting is gone from `Entry::key`.

`keys.py` has 10 more cases (46 of 46 right, 0 open, both backends): overflow checks
changed for a dependency only, for the program crate only, for a generic function used
from two crates with different settings, the same in release, and debug assertions for one
package. Each builds with an empty store, changes the setting, builds with the warm store
in place and in a copy elsewhere, and compares with cargo.

Known differences that are not covered by a case:

- With `-Z share-generics`, which is on by default at `opt-level` 0, rustc may reuse an
  instantiation that an upstream crate already made, with the upstream crate's setting.
  rustd always uses the setting of the crate that asks. The fixtures do not show a
  difference; a program built so that it depends on which copy rustc reuses could.
- In release rustc can decide by itself that a small non-generic function is inlinable
  across crates and compile it in the caller, with the caller's setting. rustd goes by
  `#[inline]` only. The fixture's function for "defined in the dependency" is
  `#[inline(never)]` for that reason.
- The `ub_checks` intrinsic is a constant false, as before.

### 4. Doctests

Refused, with what is missing named. `rustd build --doc` and the `doctest` case (which now
has a plain doctest, a `should_panic`, a `no_run` and a `compile_fail`) say: rustdoc takes
each example out of the documentation, compiles it as a crate of its own and runs it, and
a stable rustdoc tells nobody where that crate and its link line are (`--persist-doctests`
and `--test-builder` are unstable). rustd lacks the two things that would stand in for it:
taking the examples out itself (hidden `#` lines, the `main` put around an example, the
`should_panic`, `no_run`, `compile_fail`, `ignore` and edition marks), and linking a
program of its own for each, where today it only replaces functions of a program cargo
linked.

### 5. `[target.'cfg(..)'] rustflags`

Supported (`config_rustflags_cfg`). The keys of `[target]` tables are matched with
`cargo-platform` against the triple and the cfg of the target as rustc prints it, and the
flags of every table that matches are joined the way cargo does. The refusal is gone.

### `lto`

Still refused, same reason as before.

## First round: what was wrong and what was done

The first run of the suite, before any fix, had 19 WRONG rows of 87 under Cranelift.

Fixed at the root:

- Profiles. rustd had no notion of one and compiled everything as dev. It now takes
  `--release` and `--profile`, asks cargo for the unit graph of the build (`plan.rs`), and
  gives each package the `debug_assertions`, overflow checks and panic strategy cargo builds
  it with. Those are part of the crate's cfg, so of its hash in `content.rs`, and the
  overflow setting is in `cached.rs` `Entry::key`, so a dev entry is never served to a
  release build. `harness/keys.py` "the release profile" is closed and has three more
  cases (overflow checks, debug assertions, a custom profile).
- `env!` and `option_env!` of a variable of the process. They only saw what cargo and build
  scripts set. The client now sends its environment with each request, the macros read the
  crate's own variables and then those of the client, and the values a crate read are part
  of its identity. A variable nothing reads does not invalidate anything; two clients with
  different values do not share code. `keys.py` "a variable of the process read by
  option_env!" is closed.
- `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, and `[build] rustflags` / `[target.<triple>]
  rustflags` of `.cargo/config.toml`. `-C debug-assertions`, `-C overflow-checks`,
  `-C panic` and `-C opt-level` come after the profile's flags on rustc's command line and
  win; rustd ignored them. They are now read the way cargo orders them and applied over the
  profile of every package.
- `--features`, `--no-default-features`, `--all-features`, `-p` and `--workspace`. The
  features of a package are now those of the unit cargo builds, not those of `cargo
  metadata`, which unifies over the whole workspace. That is what makes resolver 1, 2 and 3
  and "one member built" against "the workspace built" come out as cargo has them.
- `CARGO_PRIMARY_PACKAGE` was never set. It is set for the packages the build was asked for.
- `CARGO_PKG_RUST_VERSION` was `1.70.0` where the manifest says `1.70`. It is now what the
  manifest has, as cargo passes it.
- `--target aarch64-unknown-linux-gnu`: `std::env::consts::ARCH` printed
  `UNRESOLVED_ENV_VAR`, because the build script of `std`, which sets `STD_ENV_ARCH`, is
  never run. The variable is now set on the sysroot's `std` from the target's cfg.
- `#![no_main]`: rustd wrote a `main` that calls `lang_start` for any crate with a function
  named `main` at its root, which is wrong for a crate that exports its own. A crate with
  `#![no_main]` gets none.
- `#![no_std]` binary that defines `rust_eh_personality` itself (the usual way on stable):
  the Cranelift backend declared the runtime's personality up front, with a signature the
  program's own function did not have, and failed. The personality is now named once every
  function is declared, and the program's own is used when it has one.
- Files a build script writes into `OUT_DIR` were taken for files that never change, so
  generated code was stale after the script ran again. They are watched now.

Turned into a refusal in the first round: `lto`; and four that the second round has since
dealt with (per-package overflow checks, `[target.'cfg(..)'] rustflags`, changes that
needed a restart, doctests).

Two rows are refusals by cargo itself (`env!` of an unset variable, a `rust-version` newer
than the toolchain); the case checks that rustd turns the project down too.

## What is measured and what is inferred

Measured: the table below; the regression gate; `keys.py`.

Inferred, not measured:

- "supported" for `opt-level`, `codegen-units` and `strip` means the program behaves the
  same. rustd's own optimisation level is its own (`RUSTD_OPT`), not the profile's, and the
  suite does not compare speed or size.
- `-C target-cpu` and `-C target-feature` are supported in that `cfg(target_feature)` and
  the program's output agree with rustc. Whether rustd's backends emit the wider
  instructions was not looked at.
- The cross rows run under qemu (aarch64) and natively (musl). Other targets were not run.
- The points marked as inferred under "A checkout is loaded again in place" and the known
  differences under "Overflow checks and debug assertions per package".

## Still open

- Doctests (refused).
- LTO (refused).
- An edit to a crate the daemon was not asked to build is refused, not built. A member or
  dependency that appears on a reload cannot be added to what a running daemon builds; the
  client has to name it when it starts one.
- Overflow checks under rustc's shared generics and its automatic cross-crate inlining, as
  described above.
- `Flags::of` in `plan.rs` asks `cargo config get` for the configuration, which is how
  project-model already reads it; it is an unstable cargo command.

## Regression gate

Run from `harness/` with `RUSTD_BIN` set to the release binary built with `--features llvm`
from the head of the branch, once with `RUSTD_BACKEND=cranelift` and once with
`RUSTD_BACKEND=llvm RUSTD_OPT=2 RUSTD_EMIT=fast`. The numbers were the same under both.

| command | result |
|---|---|
| `python3 difftest.py corpus --bin runner --crate cases --layouts` | MATCH, 803/803 functions from rustd, 0 of 892 layouts differ |
| `python3 programs.py` | 22 of 22 right |
| `python3 stress.py` | 99 of 99 right, 0 restarts |
| `python3 breakers.py` | 25 of 25 right, 0 open, 0 functions skipped |
| `python3 unwinds.py` | 8 of 8 right |
| `python3 splices.py` | 71 of 71 right |
| `python3 asmedits.py` | 10 of 10 right |
| `python3 includes.py` | 9 of 9 right |
| `python3 worktrees.py` | 62 of 62 right |
| `python3 keys.py` | 46 of 46 right, 0 open |
| `python3 debuginfo.py` | all right |
| `python3 buildmodel.py --backend <backend> --jobs 6` | 97 of 97 as expected, 0 WRONG |
| `python3 storecorrupts.py` | PASS, 40 damaged stores detected, discarded and rebuilt |
| `python3 storefull.py` | PASS, 6 full disks |
| `python3 storekills.py` | PASS, 300 kills |
| `python3 storeshared.py` | PASS, 180 builds on a shared store |
| `cargo fmt --all -- --check` | clean |
| `cargo test --release -p hir-ty -p hir-def -p parser -p syntax -p rustd` | 929 passed, 0 failed |

The two backends were run side by side. The Cranelift `difftest.py` of that run failed with
"no linker line": both runs build the same `corpus` directory, and cargo had nothing left
to link for the second. Run again by itself it gave the line in the table. Everything else
is from the side-by-side run.

`cargo clippy --all-targets -- --cap-lints warn` on the crates touched gives warnings and no
errors; they were not sorted into old and new.

## The table

One row per case. "invalidation" is what each step of the case came to under one daemon.

| feature | case | backend | result | detail | invalidation |
|---|---|---|---|---|---|
| benches (`harness = false`, the `bench` profile) | `bench` | both | supported |  |  |
| `codegen-units = 1` | `codegen_units` | both | supported |  |  |
| `codegen-units = 1` in dev | `codegen_units_dev` | both | supported |  |  |
| `.cargo/config.toml`: `[env]` | `config_env` | both | supported |  | the value in the configuration changes: same |
| `.cargo/config.toml`: `[env]` under a variable of the process, and `force` | `config_env_overridden` | both | supported |  |  |
| `.cargo/config.toml`: `[target.*] linker` and `runner` | `config_linker_runner` | both | supported |  |  |
| `.cargo/config.toml`: `[build] rustflags` | `config_rustflags` | both | supported |  |  |
| `.cargo/config.toml` `[target.'cfg(..)'] rustflags` | `config_rustflags_cfg` | both | supported |  |  |
| `.cargo/config.toml` `[target.<triple>] rustflags` | `config_rustflags_target` | both | supported |  |  |
| crate types `bin` and `lib` | `crate_bin_lib` | both | supported |  |  |
| crate type `cdylib` | `crate_cdylib` | both | supported |  |  |
| crate type `dylib` | `crate_dylib` | both | supported |  |  |
| crate types `dylib` and `rlib` together | `crate_dylib_rlib` | both | supported |  |  |
| crate type `proc-macro` (rustc's build of it is loaded; its expansion is compiled by rustd) | `crate_proc_macro` | both | supported |  |  |
| crate type `rlib` | `crate_rlib` | both | supported |  |  |
| crate type `staticlib` | `crate_staticlib` | both | supported |  |  |
| `--target aarch64-unknown-linux-gnu` | `cross_aarch64` | both | supported |  |  |
| `--target x86_64-unknown-linux-musl` | `cross_musl` | both | supported |  |  |
| `debug-assertions = false` in dev | `debug_assertions_off` | both | supported |  |  |
| `debug-assertions = true` in release | `debug_assertions_on` | both | supported |  |  |
| per-package `debug-assertions` | `debug_assertions_package` | both | supported |  |  |
| git dependency | `dep_git` | both | supported |  |  |
| edit to a dependency the daemon was not asked to build | `dep_not_named` | both | supported |  | the dependency's source changes: refused (outside/dep/src/lib.rs is of `dep`, which the program uses and the daemon was not asked to build: rustc's copy of it is from before the edit, name the crate with `--crate`) |
| `[patch]` | `dep_patch` | both | supported |  | the patched source changes: same |
| path dependency outside the workspace | `dep_path` | both | supported |  | the dependency's source changes: same |
| a workspace member that is also a registry dependency at the same version | `dep_registry_member` | both | supported |  |  |
| renamed dependency (`package = ...`) | `dep_renamed` | both | supported |  |  |
| `[replace]` | `dep_replace` | both | supported |  |  |
| vendored dependency (`[source] directory`) | `dep_vendored` | both | supported |  |  |
| doctests | `doctest` | both | refused | rustdoc takes each example out of the documentation, compiles it as a crate of its own and runs it, and a stable rustdoc tells nobody where that crate and its link line are (`--persist-doctests`, `--test-builder` are unstable); rustd neithe |  |
| editions 2015, 2018, 2021 and 2024 in one graph | `editions_mixed` | both | supported |  |  |
| `CARGO_PKG_*`, `CARGO_MANIFEST_DIR`, `OUT_DIR`, `CARGO_CRATE_NAME`, `CARGO_BIN_NAME` | `env_cargo_pkg` | both | supported |  | the version in the manifest changes: same |
| `env!` of a variable of the process | `env_env` | both | supported |  | the variable changes: same |
| `env!` of a variable that is not set | `env_env_unset` | both | refused | cargo turns it down too |  |
| `option_env!` of a variable of the process | `env_option_env` | both | supported |  | the variable changes: same; another variable, which nothing reads, changes: same |
| examples | `example` | both | supported |  |  |
| `--all-features` | `features_all` | both | supported |  |  |
| `[features]`: defaults, `default-features = false` on a dependency | `features_default` | both | supported |  |  |
| `--no-default-features` | `features_none` | both | supported |  |  |
| optional dependency through `dep:` | `features_optional_dep` | both | supported |  |  |
| `--features` enabling a dependency's feature (`lib/fancy`) | `features_selected` | both | supported |  |  |
| weak `opt?/loud` with the optional dependency off | `features_weak_off` | both | supported |  |  |
| weak `opt?/loud` with the optional dependency on | `features_weak_on` | both | supported |  |  |
| integration tests | `integration_test` | both | supported |  |  |
| `links` and `DEP_<name>_<key>` metadata | `links` | both | supported |  |  |
| `lto = true` | `lto_fat` | both | refused | `lto` is on: rustc compiles the whole program again as it links it and leaves no library on the link line for rustd's functions to be linked against |  |
| `lto = "off"` | `lto_off` | both | supported |  |  |
| `lto = "thin"` | `lto_thin` | both | refused | `lto` is on: rustc compiles the whole program again as it links it and leaves no library on the link line for rustd's functions to be linked against |  |
| proc macro: attribute | `macro_attribute` | both | supported |  |  |
| proc macro: derive, with a dependency of its own | `macro_derive` | both | supported |  |  |
| proc macro: function-like | `macro_function` | both | supported |  |  |
| multiple `[[bin]]` targets | `multiple_bins` | both | supported |  |  |
| `#![no_main]` with std | `no_main` | both | supported |  |  |
| `#![no_std]` binary with `#![no_main]` | `no_std_no_main` | both | supported |  |  |
| per-dependency `opt-level` (`[profile.dev.package."*"]`) | `opt_level_dependencies` | both | supported |  |  |
| `opt-level = 3` in dev | `opt_level_dev` | both | supported |  |  |
| per-package `opt-level` (`[profile.dev.package.dep]`) | `opt_level_package` | both | supported |  |  |
| `overflow-checks = false` in dev | `overflow_checks_off` | both | supported |  |  |
| `overflow-checks = true` in release | `overflow_checks_on` | both | supported |  |  |
| per-package `overflow-checks` | `overflow_checks_package` | both | supported |  |  |
| `panic = "abort"` | `panic_abort` | both | supported |  |  |
| `panic = "abort"` in release | `panic_abort_release` | both | supported |  |  |
| `panic = "unwind"` (the default) | `panic_unwind` | both | supported |  |  |
| profile: custom (`--profile fast`, inherits release) | `profile_custom` | both | supported |  |  |
| profile: dev | `profile_dev` | both | supported |  |  |
| profile: release | `profile_release` | both | supported |  |  |
| per-package checks turned on in release | `release_package_checks` | both | supported |  |  |
| reload: a new `build.rs` | `reload_build_script_added` | both | supported |  | the package gets a build script: same; the script says something else: same |
| reload: a dependency is added to the manifest and removed again | `reload_dep_added` | both | supported |  | a dependency is added: same; and removed: same |
| reload: a feature is toggled in the manifest | `reload_feature_toggled` | both | supported |  | a feature of a dependency is turned on: same; a default feature of the package is turned on: same; and both off: same |
| reload: `git checkout` between commits that differ in dependencies, lock file, features, members and build script | `reload_git_checkout` | both | supported |  | the other commit is checked out: same; and the first again: same; and the other once more: same |
| reload: a dependency's version is bumped in `Cargo.lock` | `reload_lock_version` | both | supported |  | the lock file has the newer version: same; and the older again: same |
| reload: a new workspace member | `reload_member_added` | both | supported |  | a member is added and depended on: same; the new member's source changes: refused (tool/src/lib.rs is of `tool`, which the program uses and the daemon was not asked to build: rustc's copy of it is from before the edit, name the crate with `--crate`) |
| `rust-version` the toolchain meets | `rust_version_met` | both | supported |  |  |
| `rust-version` newer than the toolchain | `rust_version_unmet` | both | refused | cargo turns it down too |  |
| `RUSTFLAGS` with `--cfg` | `rustflags_cfg` | both | supported |  |  |
| `RUSTFLAGS=-C debug-assertions=off` over the profile | `rustflags_debug_assertions` | both | supported |  | `RUSTFLAGS` of the client is another: same |
| `RUSTFLAGS=-C opt-level=3` in dev: the checks cargo left to rustc go off | `rustflags_opt_level` | both | supported |  |  |
| `RUSTFLAGS=-C overflow-checks=off` over the profile | `rustflags_overflow_checks` | both | supported |  |  |
| `RUSTFLAGS=-C panic=abort` over the profile | `rustflags_panic_abort` | both | supported |  |  |
| `RUSTFLAGS=-C target-cpu=x86-64-v3` | `rustflags_target_cpu` | both | supported |  |  |
| `RUSTFLAGS=-C target-feature=+avx2,+fma` | `rustflags_target_feature` | both | supported |  |  |
| build script: `cargo:rustc-link-lib` and `rustc-link-search` | `script_link_lib` | both | supported |  |  |
| build script: generated code through `OUT_DIR` | `script_out_dir` | both | supported |  | `rerun-if-changed` input gains a row: same |
| build script: `cargo:rerun-if-changed` | `script_rerun_if_changed` | both | supported |  | the named file changes: same; the build script itself changes: same |
| build script: `cargo:rerun-if-env-changed` | `script_rerun_if_env_changed` | both | supported |  | the variable is set: same; the variable changes: same |
| build script: `cargo:rustc-cfg` | `script_rustc_cfg` | both | supported |  | the file the script reads turns the cfg off: same; and on again: same |
| build script: `cargo:rustc-env` | `script_rustc_env` | both | supported |  | the file the script reads has another value: same |
| `strip = "debuginfo"` with `debug = true` | `strip_debuginfo` | both | supported |  |  |
| `strip = true` | `strip_symbols` | both | supported |  |  |
| target-specific dependencies and `cfg(target_*)` on the host | `target_specific_deps` | both | supported |  |  |
| feature unification across members, one package built (`-p app`) | `unify_members` | both | supported |  |  |
| feature unification, resolver 1 (host and target unify) | `unify_resolver1` | both | supported |  |  |
| feature unification, resolver 2 (host apart from target) | `unify_resolver2` | both | supported |  |  |
| feature unification, resolver 3 (edition 2024) | `unify_resolver3` | both | supported |  |  |
| feature unification across members, whole workspace built | `unify_workspace` | both | supported |  |  |
| unit tests | `unit_test` | both | supported |  |  |

`buildmodel.py` is not in `.github/workflows/rustd.yaml`: the cross rows need
`aarch64-linux-gnu-gcc`, `qemu-aarch64` and `musl-gcc` and the two rustup targets, which the
workflow does not install.
