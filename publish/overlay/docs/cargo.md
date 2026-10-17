# Using rustd with cargo

There are two ways to use rustd from cargo. `cargo rustd <command>` is an explicit
subcommand, described here. The second, switching rustd on globally so that plain `cargo`
goes through it, has its own section at the end, which is a placeholder for now.

Everything in this file was checked against the code in `crates/rustd/src/cargo/` and run on
Linux x86-64. The design notes it was written from are in
[reports/CARGO.md](reports/CARGO.md).

## Install

`cargo rustd` is cargo's way of running a program called `cargo-rustd` on your `PATH`. That
program is built with rustd and runs the `rustd` executable in the same directory, so the two
have to be installed side by side:

    cargo +1.99.0 build -p rustd --release
    cp target/release/rustd target/release/cargo-rustd ~/.cargo/bin/

The project you build must use Rust 1.99.0, the toolchain rustd is pinned to. Use a
`rust-toolchain.toml`, `rustup override set 1.99.0`, or `RUSTUP_TOOLCHAIN=1.99.0`.

## Commands

    cargo rustd build [cargo's flags]
    cargo rustd run   [cargo's flags] [-- program arguments]
    cargo rustd test  [cargo's flags] [test names] [-- test arguments]
    cargo rustd check [cargo's flags]
    cargo rustd status
    cargo rustd stop

`build`, `run` and `test` take the flags cargo takes and leave the same files in the same
places under `target/`. `check` is `cargo check` and nothing else: rustd has no diagnostics
of its own. `status` prints what the daemon holds, and for every workspace how many builds
the daemon served and how many went to cargo, with the reasons. `stop` ends the daemon. Any
other subcommand is passed to cargo as typed.

## What a build does

The first build of a workspace is cargo's own, run with rustd as the
`RUSTC_WORKSPACE_WRAPPER`. The wrapper runs rustc exactly as cargo asked and records, for
each crate of the workspace, the arguments, the files rustc wrote and, for a program, the
line the linker was run with. Diagnostics, progress and the exit status are cargo's.

When that build succeeds, the command tells the daemon to load the workspace and returns
without waiting. The daemon is started if it is not running. Until the workspace is loaded,
builds are cargo's, and each one says so:

    rustd: the daemon is still loading the workspace (951ms so far); building with cargo

Once the daemon is ready, a build after an edit does not start cargo at all. The daemon
compiles the functions that changed and either splices them into the executable or links
again, and the result is put where cargo would have put it. The status line shows which path
a build took:

    Finished `dev` profile [rustd] target(s) in 0.24s

`[rustd]` means the daemon built it. A cargo build prints cargo's usual line.

`run` then runs the program and `test` runs the test executables the way cargo does: the
same working directory, the variables cargo sets, cargo's `Running` lines and the same exit
status.

Loading takes from under a second to most of a minute depending on the workspace. Measured
on one machine, it was 2.5 s for just, 6.9 s for ripgrep and 42.4 s for nushell (see
[results.md](results.md)).

## When cargo builds instead

rustd hands a build to cargo whenever it cannot be sure its own result would be what cargo's
is. It prints one line with the reason, `rustd: <reason>; building with cargo`, and counts
it in `cargo rustd status`. The build then behaves as cargo's because it is cargo's.

A whole invocation goes to cargo when any of these hold (`Invocation::only_cargo` in
`cargo/cli.rs`):

| Condition | Reason printed |
|---|---|
| `--release`, or `--profile` other than `dev` and `test` | rustd builds the dev profile only |
| `cargo rustd test --doc` | doc tests are built by rustdoc |
| `--message-format`, `--build-plan`, `--unit-graph` | `--message-format` is cargo's to answer |
| a `-Z` flag | unstable cargo flags |
| `--all-features` | not loaded by the daemon yet |
| `--no-default-features` | not loaded by the daemon yet |
| more than one `-p`, or `--exclude` | a selection of several packages is not loaded by the daemon yet |
| more than one `--target` | several targets in one build |
| `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS` or `CARGO_BUILD_RUSTFLAGS` set | the daemon loads the workspace without it |
| `RUSTC_WORKSPACE_WRAPPER` set | it is set already |
| `--target` or `CARGO_BUILD_TARGET` naming a target that is not ELF, or one rustd does not know | programs for the triple are not linked through `cargo rustd` yet |

A single build goes to cargo when:

- the daemon is still loading the workspace;
- a manifest, `Cargo.lock`, a cargo configuration file, the toolchain file, or something a
  build script reads has changed since cargo last built. Cargo builds, and the daemon loads
  the workspace again afterwards. On a large workspace that reload is long: 36 to 51 s on
  nushell;
- a `#[test]` function was added or removed, or its attributes changed. The list of tests in
  a test executable is rustc's;
- a library changed that unit tests are linked against as rustc built it;
- rust-analyzer finds an error in the source. The daemon refuses it, cargo runs, and you get
  rustc's diagnostics and cargo's exit status. This costs 0.1 to 0.5 s over cargo alone.
  Errors rust-analyzer does not find are not caught here; see "Known problems";
- the daemon refused the build or failed.

Doc tests are always run by `cargo test --doc`.

The fallback is per invocation or per program, not per crate. If one program of an
invocation cannot be the daemon's, cargo builds the invocation.

These flags are handled on the daemon's path: `--target` naming the host, a single `-p`,
`--features`, `--bin`, `--example`, `--test`, `--lib`, `--manifest-path`, `--workspace`,
`--no-run`, `--no-fail-fast`, `-q`, test filters and the arguments after `--`.

## The daemon

There is one daemon per user. It is started by the first `cargo rustd` that needs it, in a
session of its own, and it outlives the command that started it. It exits after 30 minutes
without a request; `RUSTD_IDLE_SECS` sets another time and is read when the daemon starts
(0 means never).

The daemon holds no compiler state itself. It owns worker processes, each holding one
rust-analyzer database for one repository, toolchain, target, feature set and `cfg(test)`
setting. Git worktrees of one repository share a worker, so what they have in common is
analysed once. Different repositories get different workers.

A new worktree still starts with a cargo build, because it has a target directory of its
own. What it saves is the daemon's side: the worktree joins the worker that is already
loaded instead of loading the workspace again. With a ripgrep checkout loaded, the first
`cargo rustd build` in a new worktree of it was cargo's and took 16 s, the worktree showed
as ready under the same worker, and the first edit there was built by the daemon in 0.4 s. A `cargo rustd test` can use
two workers, because unit tests are built with `cfg(test)` and integration tests without.

`cargo rustd status` shows the daemon, its workers and their log files:

    daemon: pid 10969, version 0.0.0-3da7208-18df73efa25a74db, exits after 1800s idle
      worker: pid 13169, /dev/shm/ripgrep/, log /home/user/.cache/rustd/workers/f30b80a4e02dffde/log
        /dev/shm/ripgrep: ready
    /dev/shm/ripgrep (dev, 1f59ed7b049b7fa0): 3 builds by the daemon, 0 by cargo

A worker for a large workspace holds a lot of memory: 1.3 GB for ripgrep and 9.4 GB for
dioxus-cli at the end of the agent-style edit runs in [results.md](results.md).

A client and a daemon built from different sources do not talk to each other. A daemon that
gets a build request from another version stops its workers and exits, and the client
starts its own.

### Files

The daemon's directory is `$RUSTD_HOME` if set, else `$XDG_RUNTIME_DIR/rustd`, else
`~/.cache/rustd`. It holds `daemon.sock`, `daemon.sock.lock`, `daemon.log`,
`workers/<id>/` with each worker's socket and log, and `states/`. The directory must belong
to you and be closed to group and others (mode 0700); otherwise the client and the daemon
refuse to use it. The socket is 0600, and a connection from another user is refused before
its request is read.

In a project, rustd keeps its records next to cargo's output, so `cargo clean` removes
them too:

    target/<profile>/.rustd-units/     what the wrapper recorded of each rustc run
    target/rustd/<key>/                state of one command line, and the daemon's objects

Cargo keeps what it builds through a workspace wrapper apart from what it builds without
one. `cargo build` and `cargo rustd build` in one checkout therefore each compile the
workspace's crates once, and share the dependencies.

## Environment variables

These are read by `cargo rustd` and passed to the worker it starts. They are part of which
build a command line is, so changing one starts from a cargo build again.

| Variable | Meaning | Default |
|---|---|---|
| `RUSTD_BACKEND` | `cranelift` or `llvm`. `llvm` needs a rustd built with `--features llvm` | `cranelift` |
| `RUSTD_OPT` | LLVM optimisation level, 0 to 3. Functions are still compiled one at a time | 2 |
| `RUSTD_EMIT` | LLVM instruction selection: `optimized` or `fast` | `optimized`; `fast` at level 0 |
| `RUSTD_SPLICE` | `0` links in full after every edit instead of splicing | splice |

These are read by the daemon when it starts, from the environment of whichever command
started it. To change one, run `cargo rustd stop` first.

| Variable | Meaning | Default |
|---|---|---|
| `RUSTD_HOME` | the daemon's directory (also read by every client) | see above |
| `RUSTD_IDLE_SECS` | seconds without a request before the daemon exits; 0 is never | 1800 |
| `RUSTD_CACHE` | directory of the disk store; without it nothing is kept on disk | not set |
| `RUSTD_REMOTE` and the other store variables | see [cache.md](cache.md) | not set |

The disk store is off unless `RUSTD_CACHE` is set. That is easy to miss: a daemon started by
a shell without the variable keeps no store, whatever later shells have set.

## Known problems

- **The daemon builds programs rustc would reject.** It refuses source in which
  rust-analyzer finds an error, and those builds go to cargo. It has no borrow checker, so
  a borrow or move error is not found: with a warm daemon, `cargo rustd run` built and ran
  a program that uses a value after moving it. Nothing is reported until cargo next builds
  that crate. Run `cargo rustd check` when you want rustc's verdict.
- Only Linux has been run. The code compiles for macOS and Windows, but every build there
  would go to cargo (the link step is written for ELF), and the command has not been built
  or run on either. The socket's permission and peer checks do nothing on Windows.
- `cargo rustd` was measured with the Cranelift backend only. LLVM has been run through the
  harness gate, not through `harness/cargo_rustd.py`.
- In ten runs of just's test suite through `harness/cargo_rustd.py` there were two
  differences from cargo that are not explained. One was a test failing in the
  daemon-built executable and passing under cargo. The other was between two executables
  cargo built, which points at the tests being sensitive to load, but that was not shown.
- A function rustc emitted no symbol for is assumed to be dead code. One bug came from that
  assumption being wrong (test functions, fixed); no other is known, and it is not proved
  that there is none.
- The check that only your user can reach the daemon is tested for the same user only.
- `cargo rustd` is not in the CI workflow. `harness/cargo_rustd.py` needs the network and
  about 20 minutes.

## Plain `cargo`, switched on globally

<!-- GLOBAL-MODE: this section is a placeholder. The global switch is being written on
     another branch and was not present at the commit these docs were written against
     (f191c6e42a). Fill it from that branch's report: how to switch it on and off, what it
     changes in cargo's configuration, how it differs from `cargo rustd`, and how to tell
     which one built a program. -->

Not documented yet.
