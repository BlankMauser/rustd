# The test suites of 1100 crates.io crates through rustd

Branch `rustd-sweep-2`: `rustd-collate` at `ce52276cf5` with `rustd-apps-merged` (`14fad54fcd`)
and `rustd-sweep-1000` (`f4fca15318`) merged in. Measured on one 8-core, 31 GB Linux x86-64 VM
with rustc 1.99.0. This file is updated with each push; a section that says "in progress" has
numbers from a sweep that is still running.

## The list

`harness/sweep1000.txt` holds 1100 `name-version` lines: the most downloaded crates on
crates.io, each at its newest version, as the crates.io API returned them on 2026-10-17. The
session that started this work did not commit its list, so the numbers it reported at 550
crates (402 match, 27 mismatch, 118 untested) were measured on a list that may differ from
this one in versions and in order.

## The gate at the merge

Every harness below was run under Cranelift and then under LLVM (`RUSTD_OPT=2 RUSTD_EMIT=fast`)
on the merge commit, one backend at a time.

| harness | Cranelift | LLVM |
|---|---|---|
| `difftest.py corpus --bin runner --crate cases --layouts` | match, 0 of 893 layouts differ | match, 0 of 893 |
| `programs.py` | 22 of 22 | 22 of 22 |
| `stress.py` | 99 of 99, 0 restarts | 99 of 99, 0 restarts |
| `breakers.py` | 109 of 109, 6 open, 0 skipped | 109 of 109, 6 open, 0 skipped |
| `unwinds.py` | 8 of 8 | 8 of 8 |
| `RUSTD_SPLICE=1 splices.py` | 71 of 71 | 71 of 71 |
| `asmedits.py` | 10 of 10 | 10 of 10 |
| `includes.py` | 9 of 9 | 9 of 9 |
| `RUSTD_SPLICE=1 worktrees.py` | 62 of 62 | 62 of 62 |
| `keys.py` | 46 of 46 | 46 of 46 |
| `debuginfo.py` | all right | all right |
| `buildmodel.py` | 194 of 194 | (runs both) |
| `storekills.py --kills 100` | pass | pass |
| `storecorrupts.py --rounds 4` | pass | pass |
| `storefull.py` | pass | pass |
| `storeshared.py` | pass, 180 builds | pass, 180 builds |

`cargo fmt --all -- --check` is clean and `cargo test --release -p hir-ty -p hir-def -p parser
-p syntax -p rustd` passes. `cargo clippy -p rustd --all-targets -- --cap-lints warn` prints
147 warnings for the `rustd` binary; they were not compared against a clippy run of
`rustd-collate`, so "no new warnings" is not established.

The merge made one breaker that was listed as open pass: `backtrace_line`. It is now in the
passing set.

## Totals

In progress. The Cranelift sweep is running; the LLVM pass follows it.

## How to rerun

```
LLVM_SYS_201_PREFIX=/usr/lib/llvm-20 cargo build -p rustd --release --features llvm
cd harness
RUSTD_BIN=$PWD/../target/release/rustd CLEAN=1 JOBS=3 BACKENDS="cranelift llvm" \
    ./cratesweep.sh ~/sweep/out $(cat sweep1000.txt)
./sweepreport.py ~/sweep/out/cranelift
```

The sweep runs every crate under the first backend before it starts the second. A crate with a
`<name>.status` in `~/sweep/out/<backend>` is not run again; delete the file to rerun it.
`cratetest.py vendor/<name>` runs one crate and `--bisect` names the functions that differ.
