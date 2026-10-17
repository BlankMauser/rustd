# rustd

rustd is a Rust compiler daemon. One process per user stays running, keeps rust-analyzer's
analysis of your workspaces in memory and, when given a directory, compiled functions in a
store on disk, and answers builds from that state. After an edit it compiles the functions that changed, with Cranelift
or LLVM, and either links them in front of what rustc built or splices them into the
executable that is already there.

cargo and rustc start from the file system on every build: they parse, resolve, type-check
and generate code for a whole crate again when one function in it changed, and a second
checkout of the same repository starts from nothing. rustd keeps that work. Git worktrees of
one repository share one database, and a function compiled once is found again by the hash of
what it was compiled from, in any workspace on the machine or, through an S3 bucket, on
another machine.

It is meant for people and coding agents who build the same code many times, often in several
worktrees at once.

## Status

This is a prototype, published for the first time. Nobody outside the project has used it.
Read this section before relying on it.

- rustc is still required and still does the first build. rustd replaces the code of
  workspace crates in a program cargo and rustc linked; it does not produce `.rlib` or
  `.rmeta` files.
- It has no diagnostics and no borrow checker. Source with an error rust-analyzer finds is
  handed to cargo, so the message is rustc's. A borrow or move error is not found: the
  daemon builds and runs such a program, and you only hear of it from `cargo rustd check`
  or the next build cargo does.
- It is pinned to one toolchain, Rust 1.99.0. It links against rustc's output, so it has to
  agree with that rustc on symbol names, type layout and the unstable Rust ABI.
- `cargo rustd` serves dev and test builds on Linux (ELF). Release builds, other profiles,
  `RUSTFLAGS` and several other flags are handed to cargo unchanged. The command has not been
  built or run on macOS or Windows; the daemon underneath it has, through the harnesses.
- The code rustd generates is for the edit loop. Each function is compiled alone and nothing
  is inlined across functions, under either backend.
- When rustd cannot build something it says so in one line and cargo or rustc builds it. It
  is not supposed to guess. It has still been wrong: a fuzzer found sixteen problems,
  eleven of them miscompiles. Five known differences from rustc are open and listed in
  [docs/support.md](docs/support.md).

## Build from source

You need Rust 1.99.0 with the `rust-src` component. LLVM 20 is optional and adds the LLVM
backend.

    rustup toolchain install 1.99.0 --component rust-src
    git clone https://github.com/DioxusLabs/rustd
    cd rustd
    git submodule update --init
    python3 vendor/apply.py
    cargo +1.99.0 build -p rustd --release

`vendor/rust-analyzer` is rust-analyzer at the commit rustd was forked from, and
`vendor/apply.py` applies rustd's patches to it. With LLVM 20 installed (on Debian and Ubuntu,
`llvm-20-dev` from apt.llvm.org):

    LLVM_SYS_201_PREFIX=/usr/lib/llvm-20 cargo +1.99.0 build -p rustd --release --features llvm

The build leaves `rustd` and `cargo-rustd` in `target/release`. Put both in one directory on
your `PATH`; `cargo-rustd` runs the `rustd` next to it.

    cp target/release/rustd target/release/cargo-rustd ~/.cargo/bin/

## Quick start

### `cargo rustd`

In any cargo project, with 1.99.0 as the toolchain:

    cargo rustd build
    cargo rustd run -- --some-flag
    cargo rustd test
    cargo rustd check

The first build of a workspace is cargo's own, with rustd recording what rustc was asked to
do. It also starts the daemon, which loads the workspace in the background. Until it has,
builds go to cargo and a line on stderr says so:

    rustd: the daemon is loading the workspace; building with cargo

After that an edit is compiled by the daemon and no cargo process runs. `cargo rustd status`
shows what the daemon holds and how many builds it served or handed to cargo, by reason.
`cargo rustd stop` ends it; it also exits by itself after 30 minutes without a request.
`cargo rustd check` is `cargo check`.

Nothing is kept on disk between daemons unless `RUSTD_CACHE` names a directory when the
daemon starts. See [docs/cache.md](docs/cache.md) for the store and the S3 remote.

### Plain `cargo`, switched on globally

<!-- GLOBAL-MODE: to be filled from the cargo session's report. Not present at the commit
     this README was written against. -->

Not written yet. See [docs/cargo.md](docs/cargo.md).

## Results

Seconds for `cargo rustd` against plain cargo on the same edit, one run each, Linux x86-64,
8 CPUs, Cranelift. The edit adds a line to a function. From `harness/cargo_rustd.py`; the
full table, the commit and the other steps are in [docs/results.md](docs/results.md).

| Project | No-change build | Build after an edit | Daemon warm-up after the first build |
|---|---:|---:|---:|
| ripgrep 15.1.0 | 0.02 / 0.44 | 0.34 / 0.86 | 6.87 |
| fd 10.3.0 | 0.03 / 0.55 | 0.16 / 0.51 | 0.02 |
| bat 0.26.0 | 0.02 / 0.08 | 0.30 / 0.53 | 8.50 |
| hyperfine 1.19.0 | 0.03 / 0.39 | 0.13 / 0.34 | 5.28 |
| just 1.43.0 | 0.02 / 0.06 | 0.11 / 0.28 | 2.50 |
| axum 0.8.6 | 0.02 / 0.13 | 0.07 / 0.40 | 3.86 |
| nushell 0.108.0 | 0.04 / 0.26 | 0.96 / 1.65 | 42.36 |

The first build is not faster than cargo's, because it is cargo's. A build of source that
does not compile goes to cargo and costs 0.1 to 0.5 s more than cargo alone. On nushell the
daemon needs 36 to 51 s to load again after a manifest or build script changes, and builds
are cargo's for that long.

Correctness is measured by building the same program with rustc and with rustd and comparing
what they do. On the last CI run that covered all four native platforms, every function of
the test corpus was compiled by rustd under both backends and no type layout differed from
rustc's (822 functions on Linux x86-64, 825 on Linux AArch64, 830 on Windows, 828 on macOS).
Of rustc's own `tests/ui` run-pass tests, 2686 of 2690 behave the same under Cranelift; 1114
were left out, for reasons listed in the results.

## What is supported

| | Linux x86-64 | Linux AArch64 | Windows x86-64 MSVC | macOS arm64 | wasm32 |
|---|---|---|---|---|---|
| Cranelift | yes | yes | yes | yes | no (own lowering) |
| LLVM | yes | yes | yes | yes | no |
| Full link | yes | yes | yes | yes | older branch only |
| Splice | yes | yes | yes | yes, not with chained fixups | older branch only |
| Debug info | yes | yes | no | unverified | no |
| `cargo rustd` | yes | unverified | not run | not run | no |
| Disk store | yes | yes | yes | yes | unverified |
| S3 remote | yes, not real AWS | unverified | unverified | unverified | unverified |

"yes" means a harness passes there and a report says where it ran. The wasm32 column was
measured on an earlier branch with a nightly toolchain and has not been run since.
[docs/support.md](docs/support.md) has the source of every cell and the same for language and
build-model features.

## What is not supported

These are refused with a reason, and rustc or cargo builds them instead:

- doctests, and `lto = true` or `"thin"`;
- through `cargo rustd`: `--release` and any profile but dev and test, `RUSTFLAGS`,
  `--all-features`, `--no-default-features`, more than one `-p`, `--exclude`, more than
  one `--target`, `--message-format`, `-Z` flags, and a `RUSTC_WORKSPACE_WRAPPER` that is
  already set;
- a few language features, among them `sym` operands in `asm!` that is not naked, some
  variadic function definitions, and vectors in a C ABI;
- any edit to a manifest, `Cargo.lock`, cargo configuration, a build script or a proc macro:
  that build is cargo's, and the daemon reloads afterwards.

Not there at all: diagnostics, optimised output comparable to `--release`, debug info on
Windows, and targets other than the ones in the table.

## rust-analyzer

rustd's front end is rust-analyzer: its parser, name resolution, type inference, MIR,
monomorphisation, layout and const evaluation, and its cargo workspace loading. rustd adds a
code generator on top of rust-analyzer's MIR and the daemon around it. rust-analyzer is
included as a submodule pinned to one commit, with a patch series in `vendor/patches/` for
the files rustd changes. None of this would exist without the rust-analyzer team's work.
rustd is not part of the rust-analyzer project and is not endorsed by it; please do not
report rustd problems there.

Type layout is computed with rustc's own `rustc_abi` crate, copied unchanged from the 1.99.0
tag into `vendor/rustc_abi`.

## Licence

MIT or Apache-2.0, at your option, the same as rust-analyzer and rustc. See
[LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).

## Documentation

- [docs/cargo.md](docs/cargo.md): using `cargo rustd`
- [docs/support.md](docs/support.md): the support matrix
- [docs/cache.md](docs/cache.md): the disk store and the S3 remote
- [docs/architecture.md](docs/architecture.md): how it works
- [docs/results.md](docs/results.md): measurements and where each came from
- [docs/harness.md](docs/harness.md): the test harnesses
- [docs/contributing.md](docs/contributing.md): layout, patches, moving the rust-analyzer pin
- `docs/reports/`: the working reports the above were written from
