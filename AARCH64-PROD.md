# Linux AArch64: towards production

Branch `rustd-aarch64-prod`, cut from `rustd-collate`. This file is kept accurate for the head
of the branch. "Native" below means GitHub's `ubuntu-24.04-arm` runner, where rustd itself is an
AArch64 process; "qemu" means an x86-64 host that builds for `aarch64-unknown-linux-gnu` and runs
the result under `qemu-aarch64`. The two are never mixed in one number.

## Where it stands

The head this file describes is this branch with `rustd-collate` at 14ee89181b merged in, which
brings `rustd-security` and with it `storeguards.py` and `selects.py`.

The gate on that head is green on an x86-64 Linux host under both backends (the list is at the
end of this file). The native AArch64 numbers below are from the workflow run on the commit
before that merge, 0a069156c8
([run 39087233000](https://github.com/DioxusLabs/rustd/actions/runs/39087233000)). In that run
the Cranelift harness job and both `projects` jobs were green natively. The LLVM harness job
failed on one row of `debuginfo.py`, which is open. `storeguards.py` and `selects.py` have not
run natively yet; the run on this head is their first.

Open on native AArch64:

- `debuginfo.py` with LLVM: the corpus has one function with lines and no entry in `.eh_frame`
  (section 2). It does not show when the same corpus is built for AArch64 on an x86-64 host.
- A 32-bit target has not been tried.

Open on every platform, seen while working here:

- `remote.py` has two checks that fail now and then on the runners, on either architecture:
  "packs the store made one of are not fetched again" and "twelve packs with three requests at
  a time: never more in flight". Neither has failed on the local host.

## What passes natively

From run 39087233000, on `ubuntu-24.04-arm`, with Cranelift and with LLVM at -O2 with fast emit.
The counts are the same under both backends unless two are given.

| Harness | Native AArch64 |
|---|---|
| `difftest.py corpus --layouts` | MATCH, 825 of 825 functions from rustd, 0 of 903 layouts differ |
| `programs.py` | 22 of 22 |
| `stress.py` | 99 of 99, 0 restarts |
| `breakers.py` | 58 of 58, 7 open |
| `unwinds.py` | 8 of 8 |
| `splices.py` | 71 of 71 |
| `RUSTD_SPLICE=1 farsplices.py` | 6 of 6 |
| `asmedits.py` | 10 of 10 |
| `includes.py` | 9 of 9 |
| `worktrees.py` | 62 of 62 |
| `keys.py` | 46 of 46, 0 open |
| `backends.py` (LLVM leg) | LLVM compiled all 825 functions and their 11,069 instances itself |
| `debuginfo.py` | Cranelift: all right, perf included. LLVM: 1 row wrong (section 2) |
| `buildmodel.py` | 190 of 190 as expected, 4 rows not run (see below) |
| `storekills.py --kills 100`, `storecorrupts.py --rounds 4`, `storefull.py`, `storeshared.py` | pass |
| `remote.py` with MinIO's arm64 binary | 91 of 91 |
| `cargo_rustd.py`, all eight projects, flags, daemon | same as cargo at every step |

The fuzzer's smoke run is x86-64 only in the workflow: only x86-64 has been fuzzed.

## 1. Harnesses that had never run natively

### `buildmodel.py`

190 rows as expected, none wrong. The harness runs both backends itself, so it is in the
Cranelift leg only.

Two cases are marked as cases of an x86-64 host (`"on": "x86_64"` in their `case.json`) and print
"not run" on AArch64, once per backend:

- `cross_musl` builds for `x86_64-unknown-linux-musl`. It is there for another libc of the same
  machine, linked by `musl-gcc`, whose output runs on the host without an emulator. On an AArch64
  host that target is another machine.
- `rustflags_target_cpu` sets `RUSTFLAGS=-C target-cpu=x86-64-v3`, a processor name that only an
  x86-64 compiler knows.

One more row changes its meaning without saying so: `cross_aarch64` builds for
`aarch64-unknown-linux-gnu`, which on this host is the host, so natively it is a build with the
host named and not a cross build. No row builds for another machine from an AArch64 host yet.

### `cargo_rustd.py`

The workflow has a `projects` job on `ubuntu-24.04-arm` for each backend. It runs every project
of the harness, the flag table and the daemon's life.

Natively, in run 39087233000, with both backends: ripgrep, fd, bat, cargo, hyperfine, just, axum
and nushell gave the same output and the same test results as cargo at every step, and the flag
table and the daemon's life passed.

Two things were wrong and are fixed:

- The flag case `cargo rustd run --target` named `x86_64-unknown-linux-gnu`. It now names the
  host.
- The daemon stopped a worker as idle while the worker was still loading the workspace, when
  loading took longer than the idle time. The harness sets the idle time to 3 seconds, and with
  LLVM on the arm runner loading takes longer than that, so the worker was gone before it had
  built anything. A worker that is loading is now not idle, its idle time starts when it has
  loaded, and the daemon does not end while it has a worker.

### `remote.py`

The MinIO step of the workflow now runs on both Linux architectures; the image it takes the
binary from publishes both. SeaweedFS is not installed on either runner, so that half is left
out there as on x86-64.

Natively 91 of 91 checks are right under both backends in run 39087233000.

The run before it had 90 of 91 natively, and I took that for an AArch64 fault. It is not: in
run 39087233000 the same check failed on the x86-64 runner and passed on both AArch64 jobs. The
check is "packs the store made one of are not fetched again": 24 packs of 64 KiB are pushed,
another machine pulls them, its store makes one pack of them, and a second pull should fetch
nothing. When it fails, the second pull fetches one pack and the store ends with 24 records in
2 packs, and the first pull printed nothing on its standard output, so the first pull is what
went wrong and stopped one pack short. Why is not known. The check now prints the first pull's
exit status and what it said on standard error.

A second check failed once on the x86-64 runner: "twelve packs with three requests at a time:
never more in flight". Not looked into.

### `storeguards.py`

In the gate and the workflow since `rustd-security` was merged. It passes on x86-64 under both
backends. It has not run natively yet.

## 2. `debuginfo.py`

Apart from the one row described below, all rows are right natively under both backends: the frames of each panic are cargo's,
`llvm-dwarfdump --verify` finds no errors, `.eh_frame` agrees with the function ranges for all
11,735 functions of the corpus (with Cranelift), gdb stops at the breakpoint, walks up the frames and steps by
line, and the six edits are spliced into the linked binary with the debug sections kept and still
sound after each.

Locals, with Cranelift: a debugger is told of 12,983 parameters and locals in the corpus, and
gdb prints 9 of 9 in the locals case. With LLVM: 0, as on x86-64 ("LLVM's frames are not read
back yet"). Nothing differs between AArch64 and x86-64 here; the gap is the same one.

With LLVM at -O2 one corpus panic has a frame fewer than cargo's because the optimizer inlined
it (447 frames against 448); the harness counts and allows that, as on x86-64.

perf, unwinding by `.eh_frame`, now runs: the workflow installs it and lets the user sample.
Natively with Cranelift it names 104 frames of rustd's functions in 108 samples, none without
a line, 28 samples unwound back to `main`. Natively with LLVM: 26 frames in 53 samples, none
without a line, 13 back to `main`.

One row is wrong natively with LLVM: "corpus: .eh_frame and the ranges" finds 1 of 11,735
functions that has lines and no entry in `.eh_frame`. With Cranelift natively there is none,
and with LLVM building the same corpus for AArch64 on an x86-64 host there is none either (6
functions with no entry, all six naked functions with no lines). Natively five of the six are
counted as assembly alone and the sixth has a line, so the likeliest reading is that one naked
function is given a line row natively, not that a compiled function lost its entry; that is a
guess. The row now names the function, so the next native run says which.

The perf row failed once on the x86-64 runner with LLVM ("NO LINE" for `runner::main`). That
was the harness: `main` of the corpus's runner is rustc's code, and rustc gives some of its
instructions line 0. The row no longer counts rustc's line 0 as a missing line of rustd's.

## 3. Real applications through `cargo rustd`

The harness builds, runs and tests each project with `cargo rustd` and with cargo, cold, warm,
after an edit, with the edit broken and with it mended, and compares output and the result of
every test by name.

Natively (run 39087233000, both backends): ripgrep, fd, bat and cargo are the same as cargo's
own build at every step. For cargo that run built cargo 0.99.0 and ran the unit tests of its
library (`cargo test --lib`).

fd was not the same in the run before: `filter::time::tests::is_time_filter_applicable` failed
in the test binary the daemon built and passed in cargo's. That is fixed. The test sets a thread-local clock and then calls a
function that reads it. In a test executable rustc gives a symbol other objects can link to
only to the functions another codegen unit calls; the rest are private to their object and are
run by rustc's test harness as rustc built them. rustd counted such a function as one rustc has
no copy of, so the thread-local it shares with a function rustd did compile was defined twice,
once by each side, and the test set one copy while the code read the other. rustd now reads the
private function symbols of rustc's objects too, leaves those functions to rustc, and treats
the statics they use as rustc's.

On x86-64 with this head, under both backends, ripgrep, fd, bat, `cargo` (0.92.0, the tests of
`cargo-platform`, as `rustd-collate` has it) and `cargo-lib` (0.99.0, the unit tests of cargo's
library) are the same as cargo's own build at all 14 steps each, tests included. The fd failure showed on x86-64 as
well before the fix, so it was not particular to AArch64.

Where the daemon hands a step to cargo rather than building it, the harness says so and it is
not a failure. That happens for the test binary of a package whose unit tests are linked with
its library as rustc built it (ripgrep after an edit, bat when warm), and in cargo itself for
an edit to a function rustd does not compile (`request_helper`).

## 4. Other targets, one attempt each

### `aarch64-unknown-linux-musl`

One attempt, under qemu on an x86-64 host, on an earlier head of this branch; the causes below
were not looked into.

What works as it is, under Cranelift: the corpus matches rustc (825 of 825 functions from rustd,
0 of 903 layouts differ), `breakers.py` 43 of 43, `unwinds.py` 8 of 8, `asmedits.py` 10 of 10,
`includes.py` 9 of 9, `farsplices.py` 6 of 6, `keys.py` 46 of 46. With LLVM the corpus matches
and `unwinds.py` is 8 of 8.

What breaks, in the order it showed:

1. `programs.py`: 9 of 22 under both backends. The first build of each program is right; a
   patched build prints different output.
2. `stress.py`: 38 of 99, with 61 restarts of the daemon.
3. `worktrees.py`: 12 of 62, different output after a checkout.
4. `splices.py` does not get as far as a count: the host's `strip` fails on the linked binary.

The common line is that code built from scratch is right and what comes after an edit is not,
which points at how a patch is linked into a statically linked musl executable and not at code
generation.

### `armv7-unknown-linux-gnueabihf`

Not tried yet.

## Local gate

Run on an x86-64 Linux host for the x86-64 target, each backend in turn (Cranelift, then LLVM
at -O2 with fast emit). Every harness passed under both; the counts are the same under both.

| Harness | Result |
|---|---|
| `difftest.py corpus --layouts` | MATCH, 822 of 822 functions from rustd, 0 of 893 layouts differ |
| `programs.py` | 22 of 22 |
| `stress.py` | 99 of 99, 0 restarts |
| `breakers.py` | 58 of 58, 7 open |
| `unwinds.py` | 8 of 8 |
| `splices.py` | 71 of 71 |
| `asmedits.py` | 10 of 10 |
| `includes.py` | 9 of 9 |
| `worktrees.py` | 62 of 62 |
| `keys.py` | 46 of 46, 0 open |
| `fuzz/fuzz.py smoke` | 2 programs, 0 differences, 0 crashes |
| `backends.py` (LLVM leg) | LLVM compiled all 822 functions and their 11,051 instances |
| `debuginfo.py` | all right; perf ran in the LLVM leg only (38 frames in 23 samples, none without a line) |
| `buildmodel.py` (Cranelift leg, runs both backends) | 194 of 194 as expected |
| `storekills.py --kills 100`, `storecorrupts.py --rounds 4`, `storefull.py`, `storeshared.py`, `storeguards.py`, `selects.py` | pass |
| `remote.py` with MinIO | 91 of 91 |
| `cargo_rustd.py ripgrep fd bat cargo cargo-lib` | same as cargo at every step, flags and daemon checks pass |

Nothing of this branch ran under qemu for this head.
