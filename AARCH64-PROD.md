# Linux AArch64: towards production

Branch `rustd-aarch64-prod`, cut from `rustd-collate`. This file is kept accurate for the head
of the branch. "Native" below means GitHub's `ubuntu-24.04-arm` runner, where rustd itself is an
AArch64 process; "qemu" means an x86-64 host that builds for `aarch64-unknown-linux-gnu` and runs
the result under `qemu-aarch64`. The two are never mixed in one number.

## Where it stands

The head this file describes is the merge of `rustd-collate` at f191c6e42a plus the commits of
this branch. `rustd-security` is not in it yet, so `storeguards.py` is not either.

The gate on that head is green on an x86-64 Linux host under both backends (the list is at the
end of this file). The native AArch64 numbers in this file are from the workflow run on the
first commit of this branch, 4f365bdcb0, and that run was not green: three things failed
natively, and the fixes for two of them are in this head but have not run natively yet.

Open on native AArch64:

- `remote.py`, one check of 91: after a store makes one pack of 24 small ones, a pull fetches one
  of the 24 again. It passes on x86-64 and under qemu. No cause is known. The harness now prints
  what the first pull and the merge did, so the next native run says more.
- `cargo_rustd.py` on fd: one unit test failed when the daemon built the test binary. The same
  failure showed on x86-64 once the harness ran fd there, and it is fixed in this head (see
  section 3). Not yet confirmed natively.
- `cargo_rustd.py`, the daemon's life, with LLVM: a worker was stopped as idle while it was still
  loading. Fixed in this head, not yet confirmed natively.
- perf unwinding in `debuginfo.py` has not run anywhere: neither the runner nor the local host
  had a usable `perf`. The workflow now installs it.
- cargo itself through `cargo rustd` has run on x86-64 only.
- A 32-bit target has not been tried.

## What passes natively

From the run on 4f365bdcb0. The harness matrix of the workflow, on `ubuntu-24.04-arm`, with Cranelift and with LLVM at -O2
with fast emit. The counts are the same under both backends unless two are given.

| Harness | Native AArch64 |
|---|---|
| `difftest.py corpus --layouts` | MATCH, 825 of 825 functions from rustd, 0 of 903 layouts differ |
| `programs.py` | 22 of 22 |
| `stress.py` | 99 of 99, 0 restarts |
| `breakers.py` | 43 of 43, 5 open |
| `unwinds.py` | 8 of 8 |
| `splices.py` | 71 of 71 |
| `RUSTD_SPLICE=1 farsplices.py` | 6 of 6 |
| `asmedits.py` | 10 of 10 |
| `includes.py` | 9 of 9 |
| `worktrees.py` | 62 of 62 |
| `keys.py` | 46 of 46, 0 open |
| `backends.py` (LLVM leg) | LLVM compiled all 825 functions and their 11,055 instances itself |
| `debuginfo.py` | all right under both backends, perf not run (see below) |
| `buildmodel.py` | 190 of 190 as expected, 4 rows not run (see below) |
| `storekills.py --kills 100` | pass |
| `storecorrupts.py --rounds 4` | pass |
| `storefull.py` | pass |
| `storeshared.py` | pass |
| `remote.py` with MinIO's arm64 binary | 90 of 91 (see below) |

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

On 4f365bdcb0, natively, with both backends: ripgrep, bat, hyperfine, just, axum and nushell
gave the same output and the same test results as cargo at every step. fd did not (section 3).
The flag table passed. The daemon's life passed with Cranelift and failed with LLVM on the idle
worker described below.

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

Natively 90 of 91 checks are right under both backends. The one that is wrong: "packs the
store made one of are not fetched again". The harness pushes 24 packs of 64 KiB, another
machine pulls them, its store makes one pack of them, and a second pull should fetch nothing
because every key is already there. Natively it fetches one pack (65,545 bytes) and passes over
23. On x86-64 and under qemu it fetches none and passes over 24. Whether the first pull, the
merge or the listing of keys lost the one is not known; the check now prints the first pull's
line and the merge's counts.

### `storeguards.py`

Not run: it comes with `rustd-security`, which was not in `rustd-collate` at this head.

## 2. `debuginfo.py`

All rows are right natively under both backends: the frames of each panic are cargo's,
`llvm-dwarfdump --verify` finds no errors, `.eh_frame` agrees with the function ranges for all
11,721 functions of the corpus, gdb stops at the breakpoint, walks up the frames and steps by
line, and the six edits are spliced into the linked binary with the debug sections kept and still
sound after each.

Locals, with Cranelift: a debugger is told of 12,983 parameters and locals in the corpus, and
gdb prints 9 of 9 in the locals case. With LLVM: 0, as on x86-64 ("LLVM's frames are not read
back yet"). Nothing differs between AArch64 and x86-64 here; the gap is the same one.

With LLVM at -O2 one corpus panic has a frame fewer than cargo's because the optimizer inlined
it (447 frames against 448); the harness counts and allows that, as on x86-64.

perf has not run natively. The runner had no `perf`, and the row says "not run". The workflow
now has a step that installs it and lowers `kernel.perf_event_paranoid`.

No run has taken place since that step was added.

## 3. Real applications through `cargo rustd`

The harness builds, runs and tests each project with `cargo rustd` and with cargo, cold, warm,
after an edit, with the edit broken and with it mended, and compares output and the result of
every test by name.

Natively (4f365bdcb0, both backends): ripgrep and bat are the same as cargo at every step. fd
is not: `filter::time::tests::is_time_filter_applicable` fails in the test binary the daemon
builds and passes in cargo's. cargo itself was not in that run.

The fd failure is fixed in this head. The test sets a thread-local clock and then calls a
function that reads it. In a test executable rustc gives a symbol other objects can link to
only to the functions another codegen unit calls; the rest are private to their object and are
run by rustc's test harness as rustc built them. rustd counted such a function as one rustc has
no copy of, so the thread-local it shares with a function rustd did compile was defined twice,
once by each side, and the test set one copy while the code read the other. rustd now reads the
private function symbols of rustc's objects too, leaves those functions to rustc, and treats
the statics they use as rustc's.

On x86-64 with this head, under both backends, ripgrep, fd, bat and cargo are the same as
cargo's own build at all 14 steps each, tests included. The fd failure showed on x86-64 as
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
| `debuginfo.py` | all right, perf not run |
| `buildmodel.py` (Cranelift leg, runs both backends) | 194 of 194 as expected |
| `storekills.py --kills 100`, `storecorrupts.py --rounds 4`, `storefull.py`, `storeshared.py` | pass |
| `remote.py` with MinIO | 91 of 91 |
| `cargo_rustd.py ripgrep fd bat cargo` | same as cargo at every step, flags and daemon checks pass |

Nothing of this branch ran under qemu for this head.
