# cargo rustd: the drop-in entry point

`cargo rustd build|run|test|check` builds a workspace the way `cargo build|run|test|check`
does, with the same flags, and leaves the same files in the same places. The first build of a
workspace is cargo's. After it a daemon holds the workspace, and a build after an edit is the
daemon's: the changed functions are compiled and spliced or linked into the program cargo
built. Whenever the daemon cannot do that, cargo builds, and one line says why.

This file is the design (section 1, written before the code), then how it was measured and
what is still open.

## 1. Design

### What rustd needs from cargo, and where it comes from

rustd compiles the crates of the workspace and links its objects ahead of what rustc built of
the same crates. For that it needs, of every program (a binary, an example, a test
executable):

- the rlibs and objects rustc built, to read the symbols from and to link against;
- the line rustc ran the linker with;
- the crates of the workspace that are in it.

The harness (`Fixture.rustc_build` in `harness/difftest.py`) gets these with
`cargo rustc -- --print link-args -C save-temps`, which names one program at a time and has to
be told which. A user's `cargo build` names many, by flags only cargo understands. So the
source here is `RUSTC_WORKSPACE_WRAPPER`: cargo runs the wrapper in place of rustc for every
crate of the workspace and for none of the registry's, with the real command line.

`rustd` as the wrapper (`RUSTD_CARGO_UNITS` names a directory) runs rustc as asked, plus
`--print link-args=FILE -C save-temps` when the crate is linked into an executable. When rustc
succeeds it writes a record of the unit to the directory: the arguments, and copies of what
rustc wrote that a later build would write over (the rlib of a workspace crate, the objects
and the link line of a program). The copies are what the daemon reads and links against, so
a cargo build that runs beside the daemon never changes a file under it. Registry rlibs have
the hash of their inputs in their names and are read where they are.

Cargo keeps what it builds through a workspace wrapper apart from what it builds without one
(the wrapper is in the hash of the file names), and shares the registry's crates between the
two. `cargo build` and `cargo rustd build` in one checkout do not rebuild each other's crates.

The other design considered was the one in `stub.rs` (`RUSTD_WRAPS_RUSTC`), where rustc builds
a few lines in place of each workspace crate and all of the code is rustd's. It makes the cold
build faster, but a function rustd skips has no rustc copy to fall back on, so a crate with one
skip does not link. Until the skip list is empty for the crates people build, a drop-in entry
point cannot stand on it. The wrapper here is the same hook, so moving to it later changes the
wrapper and not the command.

### The command

`cargo-rustd` is a small binary next to `rustd` that runs `rustd cargo ...`. One invocation:

1. Reads the subcommand and its flags. The flags are cargo's and go to cargo unread, except
   the few that decide whether rustd has a part at all (`--release`, `--profile`, `--target`,
   `--message-format`), and the arguments after `--`, which are the program's.
2. Fast path. `target/rustd/<key>/state.json` is what the last cargo build of the same command
   left: the programs, where cargo put each, the daemon's files for each. `<key>` is a hash of
   the subcommand, the flags, the toolchain and the parts of the environment that change what
   cargo builds. When there is a state, nothing cargo would act on but source has changed (see
   "What only cargo can build"), and the daemon is warm for it, the daemon is asked for each
   program, and each one it built is put where cargo has it. No cargo process runs.
3. Slow path, when there is no state, or the daemon is not warm, or it turned the build down.
   One line says which: `rustd: <reason>; building with cargo`. Cargo runs with the user's
   flags, the wrapper, and `--message-format=json-render-diagnostics`: progress and
   diagnostics are cargo's and rustc's own on stderr, and the executables are read from the
   JSON on stdout. Cargo's exit status is the command's when it fails. When it succeeds the
   state is written and the daemon is told to load the workspace in the background; the
   command does not wait for it.
4. `run` runs the program and `test` runs the test executables, as cargo would: the working
   directory, the environment cargo sets, cargo's `Running` lines, the exit status. Doc tests
   are rustdoc's to build, so `cargo test --doc` runs them, and the line says so.

`check` is `cargo check`: rustd has no diagnostics of its own. A subcommand rustd knows nothing
of is passed to cargo.

A build that fails to compile takes the slow path by construction: the daemon turns down source
with errors, cargo runs, and the diagnostics and the exit status are rustc's and cargo's.

### What only cargo can build

rustd falls back to cargo, with the reason on one line, for:

- a profile that optimises (`--release`, `--profile`, `opt-level` above 0 in the manifest),
  `lto`, `panic = "abort"`, instrumentation: rustd's code is debug-quality and what rustc
  inlined of a function is not replaced by rustd's copy of it;
- `RUSTFLAGS` and `-C target-cpu`/`target-feature`: the daemon loads the workspace without
  them;
- a target rustd does not emit for, or links differently from ELF (the link line is rewritten
  for ELF linkers only so far);
- a change to a manifest, `Cargo.lock`, `.cargo/config.toml`, the toolchain file;
- a change to anything that runs during the build: a build script, a proc macro, a workspace
  crate one of those depends on, a file or variable a build script said to rerun for. A build
  script that names nothing to rerun for reruns on any change to its package, so any change
  to that package is cargo's;
- a function rustd skipped whose rustc copy is from before the edit (the daemon's own
  refusal);
- libraries that are not linked into an executable by this command (`cdylib`, `staticlib`):
  nothing to splice into.

These are counted: `cargo rustd status` prints, per workspace, how many builds the daemon
served and how many went to cargo for each reason.

### The daemon

One daemon per user. It listens on `$RUSTD_HOME/daemon.sock`, where `RUSTD_HOME` is
`$XDG_RUNTIME_DIR/rustd`, or `~/.cache/rustd` without one. Requests and answers are one JSON
object a line, typed on both sides.

- Start on demand. A client that finds no daemon starts `rustd daemon` in a session of its own
  (no terminal, output to `$RUSTD_HOME/daemon.log`), and waits for the socket. The daemon holds
  a lock on `$RUSTD_HOME/daemon.lock` for as long as it lives, so two clients that start one at
  the same moment get one.
- Survives its clients. It is not in the client's process group or session, and a client that
  goes away mid-request costs a failed write.
- Version. Every request carries the client's version: the crate version and a hash of the
  executable. A daemon of another version answers `restart`, stops its workers and exits; the
  client starts its own.
- Idle. With no request for `RUSTD_IDLE_SECS` (1800 by default, read when the daemon starts;
  0 is never) the daemon stops its workers and exits. A worker nobody asked for that long is
  stopped on its own.
- `cargo rustd status` and `cargo rustd stop` ask it what it holds and ask it to exit.

The daemon itself loads nothing. It owns workers: a worker is the existing `rustd <project>
--socket` process, which holds one database. One process has one target, one toolchain, one
`cfg(test)` setting and one feature set (they are inputs of loading), so a worker is keyed by
those and by the repository. Checkouts of one repository (git worktrees, which share a
`.git`) share a worker: the second joins the first's database with the existing `--project`
request, and what the two have in common is loaded once. Different repositories get different
workers: a crash or a reload of one does not cost the other its state.

A test build has up to two workers: programs and integration tests are built without
`cfg(test)` on the libraries, unit tests with it.

### When the daemon's state can be trusted

The daemon assumes the source it loads is the source rustc compiled. The client records the
time just before cargo starts; when the worker is loaded the daemon checks that no source file
rustc read (the dep-info of every recorded unit) is newer than that, and discards the worker
if one is. A later cargo build that recompiles a workspace crate makes a new generation of
records; a worker of an older generation is restarted on the new one.

### Layout

    $RUSTD_HOME/daemon.sock, daemon.lock, daemon.log
    $RUSTD_HOME/workers/<id>/sock, log
    <target-dir>/rustd/<key>/state.json
    <target-dir>/rustd/<key>/units/<crate>-<hash>/record.json, <stamp>/<copies>
    <target-dir>/rustd/<key>/programs/<name>/rustd.o, patch.o, link.args, bin

Code: `crates/rustd/src/cargo/` (`cli` the command, `wrapper` the rustc wrapper, `units` the
records, `plan` what a program needs from the daemon, `daemon` the per-user daemon and its
protocol, `client` the connection, `run` running programs and tests).
