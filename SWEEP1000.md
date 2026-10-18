# The test suites of 1100 crates.io crates through rustd

Branch `rustd-sweep-2`: `rustd-collate` at `3d47ed59bf` with `rustd-sweep-1000` (`f4fca15318`)
merged in. Measured on one 8-core, 31 GB Linux x86-64 VM with rustc 1.99.0. This file is
updated with each push; a section that says "in progress" has numbers from a sweep that is
still running.

## The list

`harness/sweep1000.txt` holds 1100 `name-version` lines: the most downloaded crates on
crates.io, each at its newest version, as the crates.io API returned them on 2026-10-17. The
session that started this work did not commit its list, so the numbers it reported at 550
crates (402 match, 27 mismatch, 118 untested) were measured on a list that may differ from
this one in versions and in order.

## The gate at the merge

Every harness below was run under Cranelift and then under LLVM (`RUSTD_OPT=2 RUSTD_EMIT=fast`)
on the merge with `rustd-collate` at `3d47ed59bf` (`bf8c5edbee`), one backend at a time, with the binary of
`LLVM_SYS_201_PREFIX=/usr/lib/llvm-20 cargo build -p rustd --release --features llvm`.

| harness | Cranelift | LLVM |
|---|---|---|
| `difftest.py corpus --bin runner --crate cases --layouts` | match, 0 of 893 layouts differ | match, 0 of 893 |
| `programs.py` | 22 of 22 | 22 of 22 |
| `stress.py` | 99 of 99, 0 restarts | 99 of 99, 0 restarts |
| `breakers.py` | 129 of 129, 7 open or left to rustc, 0 skipped | 129 of 129, 7 open or left to rustc, 0 skipped |
| `unwinds.py` | 8 of 8 | 8 of 8 |
| `RUSTD_SPLICE=1 splices.py` | 71 of 71 | 71 of 71 |
| `asmedits.py` | 10 of 10 | 10 of 10 |
| `includes.py` | 9 of 9 | 9 of 9 |
| `RUSTD_SPLICE=1 worktrees.py` | 62 of 62 | 62 of 62 |
| `keys.py` | 46 of 46 | 46 of 46 |
| `fuzz/fuzz.py smoke` | 2 programs, 505 functions, 0 differences | 2 programs, 505 functions, 0 differences |
| `backends.py corpus runner cases --bin runner --levels 2f` | (LLVM only) | prints the same as cargo's |
| `debuginfo.py` | all right | all right |
| `buildmodel.py` | 194 of 194 | (runs both) |
| `storekills.py --kills 100` | pass | pass |
| `storecorrupts.py --rounds 4` | pass | pass |
| `storefull.py` | pass | pass |
| `storeshared.py` | pass, 180 builds | pass, 180 builds |
| `storeguards.py` | all right | all right |
| `selects.py` | 3 of 3 | 3 of 3 |
| `cargo_rustd.py --small` | pass | (Cranelift leg only) |

`cargo fmt --all -- --check` is clean and `cargo test --release -p hir-ty -p hir-def -p parser
-p syntax -p rustd` passes.

Not run: `remote.py` (no minio on this machine) and `worktrees.py --join --together` and
`--overlay`. `cargo_rustd.py --small` did not finish on the previous push (it sat idle at
ripgrep's cold build and was stopped); it passes on this one, in 196 seconds, and the stall
has not been seen again or explained.

Clippy (`cargo clippy -p rustd --all-targets -- --cap-lints warn`) was compared with
`rustd-collate` at `f191c6e42a` on the previous push: the same 97 kinds of warning, three more
instances, two of which are fixed here. It was not compared again after the later merges.

Where `rustd-collate` and this branch had fixed the same thing, one fix was kept:

| what | kept | case |
|---|---|---|
| An alias that names the lifetime of a binder around it, in monomorphization | `rustd-collate`'s: normalize with placeholders and put the bound lifetimes back | `alias_under_binder` (collate's) and `alias_under_binder_projection` (this branch's, renamed) |
| A variant reexported through a private import of its enum | this branch's: the visibility the enum is declared with, kept by the module scope | both `nameres` tests, `variant_alias_reexported` and `variant_reexported_through_private_enum_import` |
| A tuple struct's constructor as a promotable aggregate | identical in both | `promoted_constructor` |

An unset `env!` variable both records the read for the build key (collate's) and is an
expansion error (this branch's). The file name under a stable name goes through collate's
`stored` (a path under cargo's home is named the same on every machine), inside this branch's
`file_name`.

The open breakers are `raw_write_then_match`, `future_unwind_flags`,
`fragment_edition_through_macro`, `assert_long_expression` and `assert_message`; rustd
prints something else than rustc in each. `caller_in_constant` and `empty_array_by_impl` are
left to rustc with a reason. `unreachable_arm_move` was fixed in `rustd-collate`.

## Totals

In progress, and not a result for this head. What follows is the Cranelift sweep on the
binary of an earlier push (`a9b62fbfce`), stopped at 578 of the 1100 crates for this merge.
It is run again from the first crate on this head, and LLVM after it.

| | Cranelift, 578 of 1100 crates |
|---|---|
| tests print the same as under rustc | 443 |
| differ | 8 |
| untested | 126 |
| timed out at 25 minutes | 1 (base64) |
| tests run and compared | 36,880 |
| functions from rustd | 130,580 of 130,612 |

The 32 functions left to rustc:

| reason | functions | crates |
|---|---|---|
| `UnresolvedMethod`, and two layouts of `GenericArray` (the next-solver cause `harness/BREAKERS.md` has as open) | 15 | generic-array |
| `MissingFunctionDefinition` | 6 | ring |
| `NotSupported` | 4 | encode_unicode, ring |
| LLVM intrinsic `x86.aesni.aeskeygenassist` | 3 | aes |
| calling convention `EfiApi` | 3 | r-efi |
| layout of an array | 1 | wit-bindgen |

The 8 that differ have not been reduced yet. From their logs:

| crate | what differs | first reading |
|---|---|---|
| tracing-opentelemetry | `layer::tests::otel_context_propagation` fails under rustd only: a span id that should be zero is not | a candidate for a real bug |
| sysinfo | `check_refresh_process_update` fails under rustc and passes under rustd | the test counts processes of the machine; likely not rustd |
| openssl | four tests fail under both and print differently | not looked into |
| semver-parser | `generated_code_is_fresh` fails under both | not looked into |
| num_enum, proc-macro-crate | tests fail under both: they want a manifest beside the crate that the published crate does not have | the harness should not count a test that fails the same way under rustc |
| hyper-timeout | `test_upload_timeout` | a timeout test; not looked into |
| jobserver | nothing in the log names a test | not looked into |

Of the 126 untested, 83 have no test binary at all (no tests, or the tests do not build under
cargo either) and 42 have one in which no function of the crate is reached. tokio is the last:
171 test binaries, all left out because they need features that are off by default.

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
