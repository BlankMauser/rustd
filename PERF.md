# Where rustd's time goes

Measurements of `cargo rustd` against plain cargo on three pinned workspaces, what the daemon
says about its own time, profiles of the slow cases, and what was changed because of them.
Everything here was run on one machine: x86-64 Linux, 8 cores, 31 GB, Rust 1.99.0, LLVM 20.
What was measured and what is inferred from it are kept apart; inferences are marked.

## The harness

`harness/bench.py` (the edit loop that had this name is now `harness/editbench.py`) takes a
pinned checkout of ripgrep 15.1.0, cargo 0.92.0 or nushell 0.108.0, copies it, and under each
backend, with a store, a daemon and a home of its own, runs:

- **cold**: `cargo rustd build` from nothing, then the wait until `cargo rustd status` says the
  workspace is ready. Cargo's own cold build is started only after that, so the two do not
  share the cores.
- **no-op**: `cargo rustd build` with nothing changed.
- **body**: a statement is added to one function of the program's own crate.
- **signature**: a method gets another parameter and every caller the argument.
- **leaf**: a statement is added to a function in a crate of the workspace that the others
  depend on.
- **generic**: a statement is added to a generic function that is used at many types.

Every edit is made and taken back three times, with plain cargo building the same source into
its own target directory after each, and after the first of each kind the program is run and
has to print what cargo's prints. For each phase the JSON has the wall time of the command
(the median for edits, with each sample and the slowest), cargo's time, the most memory the
daemon's workers held (`VmHWM` summed), who built it (the daemon, or cargo with the daemon's
stated reason), whether the edit was spliced or linked in full and why, how many functions
were compiled, and the daemon's own timings from `RUSTD_STATS`. An edit the daemon turns down
is measured once and reported as built by cargo; it is not retried, since each refusal has the
daemon load the workspace again.

    cd harness
    python3 bench.py ripgrep cargo nushell --out result.json        # both backends
    python3 bench.py cargo --backend llvm --work DIR --reference DIR2
    python3 bench.py --compare bench-baseline.json result.json --threshold 25

`--compare` exits 1 when a phase is slower, holds more memory or compiles more functions than
the baseline by more than the threshold, when the daemon built something in the baseline and
cargo builds it now, when a spliced edit is no longer spliced, when a phase is missing, or
when a program printed something else than cargo's.

`harness/bench-baseline.json` is ripgrep under both backends on the machine above. The CI job
`bench (ripgrep)` runs the same and compares at 200%, with `continue-on-error`; it has not
run yet at the time of writing. The runner is not this machine, so only the counts and the
splice/full-link/refusal fields mean something there until a baseline is taken on a runner.

How steady it is, measured: ripgrep's edits were within 20% over three separate runs of the
harness (body 0.124, 0.161, 0.154 s under Cranelift). The cold phase is not steady: 17.1,
19.8 and 27.1 s for ripgrep under Cranelift in three runs with nothing changed between them.
Cargo and nushell need about 12 GB of disk each per backend and 25 to 40 minutes, which is
why CI has ripgrep alone.

## What the harness measured

Seconds, `cargo rustd` then plain cargo, on the branch before any fix. Edits are the median of
three; "first" is the first of the three where it differed much from the others.

| Cranelift | ripgrep | cargo | nushell |
|---|---|---|---|
| cold | 17.1 / 5.4 | 173.2 / 66.2 | 218.9 / 54.8 |
| no-op | 0.02 / 0.03 | 0.04 / 0.18 | 0.03 / 0.32 |
| body | 0.12 / 0.48, spliced | 0.39 / 5.40, spliced | 1.09 / 2.20, spliced |
| signature | 0.13 / 0.63, spliced | refused, 43.3 / 10.2 | refused, 11.2 / 9.6 |
| leaf | 0.08 / 0.84, spliced | 0.41 / 7.23 (first 41.9, full link) | refused, 40.3 / 8.5 |
| generic | 0.08 / 0.82, spliced | 35.7 / 7.8, full link | 1.27 / 10.3 (first 30.6, full link) |
| peak RSS | 2.1 GB | 8.4 GB | 10.2 GB |

| LLVM (`RUSTD_OPT=2 RUSTD_EMIT=fast`) | ripgrep | cargo | nushell |
|---|---|---|---|
| cold | 20.1 / 5.3 | 225.6 / 52.8 | 311.4 / 66.6 |
| no-op | 0.04 / 0.03 | 0.07 / 0.18 | 0.05 / 0.30 |
| body | 0.17 / 0.47, spliced | 0.37 / 5.00, spliced | 2.46 / 3.10, spliced |
| signature | 0.18 / 0.62, spliced | refused, 31.1 / 9.7 | refused, 11.8 / 8.4 |
| leaf | 0.09 / 0.88, spliced | 0.43 / 7.79 (first 26.7, full link) | refused, 34.1 / 8.6 |
| generic | 0.09 / 0.79, spliced | 31.1 / 14.3, full link | 2.14 / 10.6 (first 21.9, full link) |
| peak RSS | 2.1 GB | 7.9 GB | 9.3 GB |

The cold column is cargo's build through the wrapper plus the daemon's load and first build
beside it; the program is usable when cargo's part is done (5.4, 48 and 66 s under Cranelift),
and the rest is the daemon getting ready for the first edit.

What these say, before any profile:

- Where the daemon builds an edit and splices it, it is 2 to 14 times faster than cargo.
- A "refused" edit is one the daemon turned down with a reason and cargo built. The time is
  cargo's build plus the daemon's attempt, and after it the daemon loads the workspace again
  from the start, because what cargo linked is no longer what the daemon's objects are of
  (`Daemon::warm` in `crates/rustd/src/cargo/daemon.rs` replaces the worker when the
  generation differs). On cargo that reload is about 150 s during which every build is cargo's.
- Cargo's signature edit (`GlobalContext::cli_unstable`, 113 places in 49 files) is refused
  with "`publish` has errors: frontend: mir: HasErrors". Nushell's signature and leaf edits are
  refused with the same for `MatchPattern::eq`, a derived `PartialEq` the frontend cannot
  lower in the cold build either.
- The first edit to the checkout after the daemon loaded with a full store compiles about half
  of all functions again and links in full (cargo leaf 109,223 functions, nushell generic
  104,924): the first build takes the checkout's code from the store, and a patch can only be
  made against code compiled in this process (`hydrate` in `CodegenUnit::compile`,
  `Base::outdated`).
- Cargo's generic edit (`GlobalContext::get`) takes 31 to 37 s every time, compiles 199 to 435
  functions, and still writes the whole object again.

The sections below take these one at a time.

## What the daemon says about its own time

With `RUSTD_STATS=1` the daemon already printed, for every build, `timeline-us:` (the steps
of an edit one after another: settle, read, apply, roots, check, reach, resolve, define,
object, write, link, and what is left between them), `phases:`, `backend:` (the producer's
time summed over threads; LLVM's split into lower, verify, optimize, emit, read), `store:`,
`link:` (spliced or not and why) and `edit-to-binary:`; and for the first build `load
workspace:`, `resolve:` and `cold:`. Two things could not be told from those and were added,
on the same switch and as lines of the same kind:

- `steps: infer …, mir …, monomorphize …, translate …, describe …, store read …, store write …`:
  what the compiling threads spent, summed over the threads, on the types of a body, its MIR,
  putting an instance's types into a generic body, turning MIR into the producer's IR, debug
  info (lines and variables of each function, and writing the sections), and reading and
  writing entries of the store. `infer` is only what had not been asked for before codegen got
  to it; after an edit the types are computed in the `check` step of the timeline, so `infer`
  is then near zero.
- `round N: TIME, started over: WHY` for every pass over the program whose object was thrown
  away, and `discarded=` in `timeline-us` for their sum. `phases:` only ever described the last
  round, so a build of four rounds showed three quarters of its time as "compile-rest".

`bench.py` puts these in the JSON as `steps_ms` (the timeline), `threads_s` (the `steps:` and
`backend:` lines) and `started_over` (the reasons).

On ripgrep's cold build under Cranelift (7.1 s of compiling on 8 threads, 23,387 functions)
the threads spent: MIR 2.9 s, translation 3.8 s, Cranelift 6.2 s, store writes 2.1 s, debug
info 1.2 s, monomorphization 0.5 s, types 0.3 s, store reads 0.07 s. The same for cargo and
nushell, and the profiles, are not in this file yet.

## Open at this head

Not done yet from the list this file is for:

- Profiles (perf) of cargo's cold build, of debug info, and of the slowest edit of each kind.
- The 6 s that REMOTE.md calls unexplained. One thing is already clear from its own table:
  the "store costs 14 s" there subtracts a run with debug info from a run without. With debug
  info on in both, the store costs 28.8 s on cargo (101.9 s against 73.1 s). The run that
  tells whether the 6 s are in the store or in compiling (no store and no debug info) has not
  been made.
- No fix has been made. The candidates, from the measurements above, none of them small:
  a refusal costing a whole reload; the first edit after a start with a full store compiling
  half the program; cargo's generic edit starting over several times.
- The frontend errors behind the refused edits (`publish` in cargo after a signature edit is
  taken back, `MatchPattern::eq` in nushell from the start) have not been reduced to cases.
- The CI job has not run, and its baseline is from another machine than the runner.
