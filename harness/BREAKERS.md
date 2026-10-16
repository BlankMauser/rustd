# What breaks rustd's codegen: a sweep of 93 crates and 7 binaries

Branch `rustd-breakers`, off `rustd-spike`. Everything below was measured on one 8-core, 31 GB
VM with `nightly-2026-05-10` (`harness/toolchain`). Each number is from a single run unless it
says otherwise, and each is followed by the command that printed it. "rustd-14" is
`cargo build -p rustd --release` at commit `4219d84566`; the later commits change only the
harness and this report.

## Summary

| | before (as found) | after (rustd-14) | command |
|---|---|---|---|
| crates with a result (the other 7 are under "Not tested") | | 86 of 93 | `cratesweep.sh ~/sweep/out6 $(cat versions.txt)` |
| crates whose tests behave as rustc's | not measured on the whole list, see below | 84 of 86 | `sweepreport.py ~/sweep/out6` |
| tests run and compared | | 10746 | same |
| functions compiled by rustd | | 41597 of 41692 (95 left to rustc) | same |
| binaries, command lines the same | 25 of 29 on the 3 that ran, 4 did not run | 68 of 68 over 7 binaries | `bintest.py <name>` |
| `harness/corpus` | 661 of 662, MATCH | 661 of 662, MATCH | `difftest.py corpus --bin runner --crate cases` |
| `harness/stress.py` | 99 of 99 | 99 of 99, 0 restarts | `stress.py` |
| reduced breakers | | 15 of 15 right, 1 open | `breakers.py` |

"Before" has no single number for the 93 crates: I stopped the first sweep with the
unmodified rustd after 29 crates, to restart it with the first fixes in. In those 29, rustd
crashed on futures, pin-project-lite and tokio and failed to link serde_json. Of those 29: 4965 tests, 9039 of 9539 functions, 7 crates failing
(`sweepreport.py ~/sweep/out1`). A full sweep with the first four fixes in gave 40986 of 41212
functions and 6 failing crates (`sweepreport.py ~/sweep/out2`); the final one is the table
above.

The two crates that still differ, both in tests that measure rustd's code rather than compute
with it:

- futures: `join_size`, `select_size`, `try_join_size`, `select_on_non_unpin_size` assert the
  size of a future. rustd lays an `async` block out without overlapping the variables of
  different await points, so its futures are larger (reduced: `future_size`, rustc
  `[12, 20, 24, 112]`, rustd `[192, 288, 304, 288]`).
- simd-json: `limit_fits_in_a_default_thread_stack` recurses to the parser's depth limit on a
  default thread stack. rustd's frames are larger and the test overflows. Not reduced.

## How it was run

- Libraries: `harness/cratesweep.sh <out> <crate-version>...` downloads each crate from
  crates.io and runs `harness/cratetest.py` on it, which builds every test binary of the
  crate with cargo, builds it again with rustd's object linked in front, runs both and
  compares the result of every test by name. `sweepreport.py <out>` adds up the JSON files.
  The final run: `RUSTD_BIN=~/sweep/rustd-14 CLEAN=1 JOBS=5 harness/cratesweep.sh ~/sweep/out6
  $(cat ~/sweep/versions.txt)`, about 31 minutes of wall time for everything but base64
  (start of the run to the last line of its log), with `stress.py` and the binaries running
  beside it for part of that.
- Binaries: `harness/bintest.py <name>` (new) clones the repository named in
  `harness/breakers/bins.json`, builds it with cargo and with rustd, runs each command line
  listed there with both and compares status, stdout and stderr.
- Reduced cases: `harness/breakers.py` (new) builds `harness/breakers/cases` with both and
  compares each case's output, then runs `cratetest.py` on the crates under
  `harness/breakers` that need a test of their own. It fails on any difference and on any
  function skipped that is not listed as open.

Two things in the harness itself had to be fixed before the numbers were right, and are in
the branch: `cratetest.py` counted a test binary that does not link the crate at all as the
crate's failure, and integration tests were loaded without dev-dependencies (below).

## The list, and why each is on it

| stress | crates |
|---|---|
| generics, trait objects, iterator adaptors | serde_json, itertools, nalgebra, either, anyhow, thiserror, petgraph, im, nom, winnow |
| const eval, const generics, type-level numbers | generic-array, typenum, arrayvec, heapless, bitvec, bitflags, hex-literal, static_assertions |
| unsafe, raw pointers, hand-rolled layouts | bumpalo, hashbrown, smallvec, slab, indexmap, tinyvec, bytes, triomphe, zerocopy, bytemuck, lru, smol_str |
| atomics, threads, lock-free | crossbeam-utils, crossbeam-queue, crossbeam-channel, crossbeam-epoch, arc-swap, once_cell, rayon, rayon-core, parking_lot, dashmap, thread_local, lazy_static |
| async, pinning | futures, futures-util, tokio, smol, async-channel, pin-project-lite |
| SIMD and CPU intrinsics | memchr, simd-json, aho-corasick, sha2, blake3, crc32fast, ahash, half |
| FFI, C layout, syscalls | memmap2, socket2, libc through most of the above |
| 128-bit integers, floats, formatting | num-bigint, num-traits, num-complex, ryu, rust_decimal, ordered-float, libm, fastrand, rand, rand_core, siphasher, fnv, byteorder |
| panics, unwinding, drop order | scopeguard, anyhow, crossbeam-queue (`drops`), rayon-core (`stack_overflow_crash`) |
| `dyn Any`, `TypeId` | downcast-rs, anyhow |
| text, tables, large statics | unicode-segmentation, unicode-width, unicode-normalization, regex-syntax, regex-automata, bstr, base64, percent-encoding, url, uuid, time, chrono, textwrap, strsim, smawk, camino, log |
| compression (tight loops, big frames) | adler2, miniz_oxide, flate2 |
| big binaries | ripgrep, fd, bat, just, hyperfine, zoxide, tokei |

Inline asm, thread locals, statics with interior pointers, closures and fn pointers are
already in `harness/corpus` (661 functions) and came up again only through the crates above.

## Binaries

| binary | command lines the same | functions from rustd | codegen ms |
|---|---|---|---|
| fd | 10 of 10 | 143 of 143 | 4664 |
| just | 12 of 12 | 553 of 553 | 13124 |
| hyperfine | 7 of 7 | 130 of 130 | 2915 |
| zoxide | 9 of 9 | 102 of 102 | 2555 |
| ripgrep | 12 of 12 | 1742 of 1742 | 5706 |
| bat | 10 of 10 | 274 of 274 | 5593 |
| tokei | 8 of 8 | 119 of 119 | 5778 |

`RUSTD_BIN=~/sweep/rustd-14 harness/bintest.py <name> --json <name>.json` for each; the
function counts and times are the sums of `compiled`, `roots` and `codegen_ms` over the
crates in that JSON.

The first run with the unmodified rustd (`~/sweep/bins1.log`) gave fd 9 of 10, ripgrep 9 of
12, zoxide 9 of 9, hyperfine 7 of 7, and no result for just, tokei and bat. Two of those were
rustd: fd returned status 1 where rustc's build returns 130 (cause 7 below), and tokei had 0
functions compiled (cause 2). The rest was the new harness, which did not yet pick the right
binary of a package with several, and compared `argv[0]` in help texts. I did not separate
the two more finely than that.

## Root causes, by how many crates each touched

Fixed. Each has a commit and a case that fails without it.

| # | cause | crates | effect before | reduced case | commit |
|---|---|---|---|---|---|
| 1 | Integration tests were loaded without dev-dependencies, so nothing from them resolved; and a macro that failed to expand at the end of a block was lowered as an empty block, so the function was compiled to return `()` | arc-swap (traced). crossbeam-queue, futures and pin-project-lite also failed in the first sweep and pass since; I did not trace those three one by one | arc-swap: 4 tests differ, and its `random` test binary segfaulted | `breakers/devdep` | `e1302a63a4` |
| 2 | A library and a binary, bench or test of the same name were taken for one crate | tokei, serde_json, ahash | 0 functions compiled; link failure; wrong `main` | `breakers/devdep` (`only_main`), tokei in `bins.json` | `414fa60d2f`, `c826da5e7e` |
| 3 | `format_args!` ends in `<Arguments>::new`, which is unstable; a stable `new` from a blanket trait in scope (yansi's `Paint`, through pretty_assertions) was picked instead | im (85 functions), any crate with pretty_assertions | every function that formats: `HasErrors` | `format_blanket_new` | `ffd6b4acd9` |
| 4 | `llvm.x86.pclmulqdq` (128, 256, 512) missing | crc32fast, simd-json | 10 functions left | `clmul` | `ea7d92397e` |
| 5 | `llvm.x86.xgetbv` missing | sha2, libm | 3 functions left | `xgetbv` | `1fbc416a11` |
| 6 | A method of `impl<T> Trait for T` in a crate rustc built no code of was named and compiled with nothing for `T` | downcast-rs, pin-project-lite | 10 functions reported as failures | `breakers/blanket` | `4219d84566` |
| 7 | Const eval read a niche-encoded enum's variant from the niche's start instead of the first niche variant | fd | wrong exit status | `niche_eq` | `99a4eebbd8` |
| 8 | `simd_cast`, `simd_as` missing | serde_json (zmij) | functions left | `simd_cast` | `1d53ea1572` |
| 9 | A large temporary and the binding it initializes each got a stack slot | rayon-core | `stack_overflow_crash` differs | `big_frame` | `52856e3d60` |
| 10 | `&[x; N]` of a constant was built on the stack instead of promoted | adler2 | 2 tests differ | `big_promoted` | `89ff27cf61` |
| 11 | In a macro, `1 as $ty << n` was parsed as the start of generic arguments | rand | 6 tests differ | `cast_then_shift` | `d74cf4ee84` |
| 12 | A macro call's location included the comments and attributes before it | simd-json | 3 tests differ (panic line numbers) | `macro_item_location` | `357e80d227` |
| 13 | Unsizing a struct whose last field is an associated type of a parameter | heapless | 22 functions left | `unsize_assoc_field` | `35715f72aa` |
| 14 | Const eval had no `bswap` or `bitreverse` | bitvec | 5 functions left | `const_bswap` | `35715f72aa` |

On 6: these methods are generic, they were never functions rustd could compile alone (11 of
them were listed, 10 failed and one happened to compile because it never touched `T`). The fix
stops counting them as roots, which is why the total number of functions went from 41703
(`sweepreport.py ~/sweep/out3`, the sweep before this fix) to 41692 while the number compiled
went up. Their instances are
compiled where they are used, as before.

Open. 95 functions in 11 crates, and 2 crates with differing tests.

| cause | functions | crates | reduced | difficulty |
|---|---|---|---|---|
| rust-analyzer does not infer a type or resolve a method that rustc does (`UnresolvedMethod`, `HasErrors`, `{type error}` in a layout, `MissingFunctionDefinition`, `TypeError`, monomorphization errors) | 80 | bitvec 41, generic-array 15, itertools 9, im 6, nalgebra 3, petgraph 3, ryu 2, zerocopy 1 | no | Open-ended. Each is a separate inference bug in hir-ty, mostly around associated types of type-level numbers (typenum, bitvec's `BitStore`) and types known only from a later use. Days each, and upstream rust-analyzer work rather than codegen work. |
| Generic function inside a `const _` block inside a function, twice in one function: compiled without its parameters, and both get one name | 9 | pin-project-lite | no, a plain copy of the pattern compiles | Small once reproduced, likely a day: the name needs the block's index, and the function is generic. |
| `llvm.x86.vcvtps2ph` (F16C) missing. Fixed since: a function of the instruction, one for each rounding | 3 | half | `f16c` | Done; `half` itself not rerun. |
| Comparing two references to a struct that ends in a slice. Fixed since: the derive's `discriminant_value` read the wide reference as one scalar | 2 | triomphe | `unsized_tail_cmp` | Done; `triomphe` itself not rerun. |
| A call resolved to a trait method with no body (`into`) | 1 | zerocopy | no | Unknown, probably the first row again. |
| Futures are larger than rustc's: no overlap of variables across await points | 4 tests | futures | `future_size` | Large, a week or more: coroutine layout as rustc does it, with liveness across suspension points. Changes no behaviour other than size. |
| Frames are larger than rustc's, a deep recursion overflows the stack | 1 test | simd-json | no | Medium to large: Cranelift without optimization gives each MIR local a slot. Fix 9 removed the worst case. |

The first row is four fifths of what is left. I did not reduce those to cases: each of the
functions I looked at (itertools `array_windows`, ryu `test_random`, bitvec `load`) fails for
a different reason inside type inference, and the two reductions I tried (an empty `vec![]`
in a destructured array, `parse()` typed by the other side of `assert_eq!`) compiled fine on
their own.

## Not tested, and why

| crate | reason | checked with |
|---|---|---|
| bstr | cargo cannot build the tests: `src/unicode/data/*Test.txt` are not in the published crate | `cargo test --no-run` in `harness/vendor/bstr` |
| blake3 | same: `reference_impl` is not in the published crate | `cargo test --no-run` |
| rust_decimal | a dev-dependency needs the MySQL client library (`mysqlclient-sys` build script fails) | `cargo test --no-run` |
| smol | `rustix` does not compile on this nightly (`rustc_layout_scalar_valid_range_*` attributes) | `cargo test --no-run` |
| tokio | the tests do not build with default features | `cargo test --no-run`; see below for `--features full` |
| static_assertions | has no functions and no tests that run | sweep log: 1 test binary, 0 tests |
| base64 (unit tests only) | did not finish in the 1800 s the sweep allows; its integration tests are the same (107 of 107 functions) | see below |

tokio with `--features full` (`RUSTD_BIN=~/sweep/rustd-14 cratetest.py harness/vendor/tokio
--features full --json ~/sweep/tokio-full.json`, one run, after the sweep): 155 test binaries,
1018 tests, 3271 of 3273 functions from rustd. 8 tests differ, all of them assertions on the
size of a future (`join_size`, `join_size_biased`, `try_join_size`, `try_join_size_biased`,
`pointer_64_tests::struct_size_1..3`, `buffer_not_included_in_future`), the futures cause
above. The 2 functions left are `io_join::method_delegation` and `io_split::method_delegation`
(`layout of Runtime: HasErrorType`, the type inference row). 18 further test binaries,
the library's unit tests among them, do not build with cargo under `full` alone (they want
`test-util`), so tokio's unit tests are not covered. These numbers are not in the totals above.

base64: the sweep reports it as failed. Its two integration test binaries match; the unit
test binary ran past the 1800 s limit of `cratetest.py` and was killed, so it has no result.
I saw its tests passing while it ran but did not find out which build is the slow one or
whether it would finish. It is counted among the 7 without a result, not among the 86.

## Every crate

`harness/sweepreport.py ~/sweep/out6 --md`. "codegen ms" is the time rustd spent in codegen
over the crate's test binaries, not wall time.

| crate | result | test binaries | tests | functions from rustd | codegen ms | left to rustc, or differing |
|---|---|---|---|---|---|---|
| adler2 | same | 1 | 6 | 23/23 | 164 |  |
| ahash | same | 3 | 38 | 185/185 | 596 |  |
| aho_corasick | same | 1 | 163 | 856/856 | 2406 |  |
| anyhow | same | 15 | 73 | 215/215 | 8589 |  |
| arc_swap | same | 4 | 52 | 123/123 | 3981 |  |
| arrayvec | same | 4 | 52 | 66/66 | 1075 |  |
| async_channel | same | 3 | 41 | 54/54 | 1853 |  |
| bitflags | same | 1 | 54 | 824/824 | 1175 |  |
| bitvec | same | 1 | 141 | 203/244 | 4677 | 16 mir: HasErrors; 15 mir: UnresolvedMethod; 6 mir: MissingFunctionDefinition |
| bumpalo | same | 1 | 5 | 33/33 | 310 |  |
| bytemuck | same | 10 | 33 | 73/73 | 750 |  |
| byteorder | same | 1 | 680 | 1140/1140 | 1634 |  |
| bytes | same | 13 | 1055 | 1561/1561 | 4329 |  |
| camino | same | 2 | 9 | 713/713 | 549 |  |
| chrono | same | 4 | 291 | 1847/1847 | 6700 |  |
| crc32fast | same | 2 | 12 | 108/108 | 847 |  |
| crossbeam_channel | same | 15 | 391 | 628/628 | 16021 |  |
| crossbeam_epoch | same | 2 | 40 | 128/128 | 678 |  |
| crossbeam_queue | same | 3 | 23 | 29/29 | 1046 |  |
| crossbeam_utils | same | 7 | 62 | 423/423 | 3333 |  |
| dashmap | same | 1 | 20 | 44/44 | 477 |  |
| downcast_rs | same | 3 | 32 | 152/152 | 1349 |  |
| either | same | 1 | 8 | 13/13 | 1057 |  |
| fastrand | same | 3 | 32 | 77/77 | 740 |  |
| flate2 | same | 6 | 80 | 369/369 | 1797 |  |
| fnv | same | 1 | 1 | 8/8 | 327 |  |
| futures_util | same | 1 | 1 | 50/50 | 1162 |  |
| futures | DIFFERS | 31 | 84 | 106/106 | 11307 | differs: join_size select_on_non_unpin_size select_size try_join_size |
| generic_array | same | 1 | 3 | 4/19 | 5490 | 13 mir: UnresolvedMethod; 1 layout of Alias; 1 layout of GenericArray<i32, |
| half | same | 1 | 45 | 298/301 | 596 | 2 LLVM intrinsic `x86.vcvtps2ph.128`; 1 LLVM intrinsic `x86.vcvtps2ph.256` |
| hashbrown | same | 7 | 113 | 406/406 | 3764 |  |
| heapless | same | 3 | 227 | 431/431 | 3721 |  |
| hex_literal | same | 2 | 8 | 11/11 | 113 |  |
| im | same | 1 | 106 | 121/127 | 9137 | 3 layout of TestError<; 3 mir: TypeError |
| indexmap | same | 5 | 183 | 281/281 | 6055 |  |
| itertools | same | 14 | 450 | 800/809 | 22532 | 6 mir: NotSupported; 3 layout of [[ |
| lazy_static | same | 3 | 1 | 1/1 | 138 |  |
| libm | same | 1 | 116 | 1200/1200 | 1255 |  |
| log | same | 3 | 33 | 243/243 | 890 |  |
| lru | same | 1 | 59 | 68/68 | 720 |  |
| memchr | same | 1 | 142 | 718/718 | 1288 |  |
| memmap2 | same | 1 | 27 | 129/129 | 394 |  |
| miniz_oxide | same | 1 | 29 | 237/237 | 524 |  |
| nalgebra | same | 1 | 21 | 70/73 | 16077 | 3 mir: MissingFunctionDefinition |
| nom | same | 16 | 268 | 899/899 | 21913 |  |
| num_bigint | same | 9 | 168 | 3733/3733 | 4450 |  |
| num_complex | same | 1 | 92 | 401/401 | 688 |  |
| num_traits | same | 2 | 67 | 3985/3985 | 1913 |  |
| once_cell | same | 1 | 5 | 25/25 | 700 |  |
| ordered_float | same | 3 | 72 | 369/369 | 687 |  |
| parking_lot | same | 3 | 91 | 325/325 | 1111 |  |
| percent_encoding | same | 1 | 18 | 51/51 | 188 |  |
| petgraph | same | 24 | 346 | 827/830 | 16528 | 3 mir: UnresolvedMethod |
| pin_project_lite | same | 4 | 21 | 81/90 | 398 | 4 Duplicate definition of identifier: _RNvNvNvCs5zOgJOEO9oF_4t; 1 layout of Struct<#0, #1>: HasPlaceholder; 1 layout of PinRef<ReLateParam |
| rand | same | 1 | 104 | 298/298 | 1565 |  |
| rand_core | same | 4 | 11 | 36/36 | 475 |  |
| rayon_core | same | 8 | 112 | 481/481 | 8325 |  |
| rayon | same | 17 | 333 | 700/700 | 40674 |  |
| regex_automata | same | 1 | 204 | 2265/2265 | 8872 |  |
| regex_syntax | same | 1 | 147 | 985/985 | 2667 |  |
| ryu | same | 9 | 47 | 167/169 | 850 | 2 layout of |
| scopeguard | same | 1 | 8 | 14/14 | 164 |  |
| serde_json | same | 8 | 144 | 765/765 | 9952 |  |
| sha2 | same | 2 | 14 | 165/165 | 997 |  |
| simd_json | DIFFERS | 8 | 473 | 1895/1895 | 11015 | differs: limit_fits_in_a_default_thread_stack |
| siphasher | same | 1 | 13 | 148/148 | 216 |  |
| slab | same | 3 | 47 | 102/102 | 1232 |  |
| smallvec | same | 1 | 63 | 81/81 | 1369 |  |
| smawk | same | 5 | 21 | 23/23 | 1109 |  |
| smol_str | same | 3 | 26 | 162/162 | 2332 |  |
| socket2 | same | 1 | 10 | 267/267 | 591 |  |
| strsim | same | 2 | 96 | 133/133 | 890 |  |
| textwrap | same | 3 | 163 | 312/312 | 4053 |  |
| thiserror | same | 13 | 56 | 262/262 | 3159 |  |
| thread_local | same | 1 | 9 | 26/26 | 380 |  |
| time | same | 1 | 1 | 845/845 | 564 |  |
| tinyvec | same | 2 | 19 | 25/25 | 865 |  |
| triomphe | same | 1 | 63 | 77/79 | 1345 | 2 expected scalar |
| typenum | same | 2 | 1762 | 2089/2089 | 8189 |  |
| unicode_normalization | same | 1 | 13 | 62/62 | 651 |  |
| unicode_segmentation | same | 2 | 23 | 351/351 | 1687 |  |
| unicode_width | same | 2 | 35 | 107/107 | 1158 |  |
| url | same | 2 | 67 | 487/487 | 3093 |  |
| uuid | same | 1 | 62 | 323/323 | 306 |  |
| winnow | same | 1 | 270 | 833/833 | 6761 |  |
| zerocopy | same | 3 | 140 | 1346/1348 | 23135 | 1 call to `into`, which has no body and no rustc symbol; 1 mir: UnresolvedMethod |

10746 tests, 41597/41692 functions from rustd

    32  mir: UnresolvedMethod  (bitvec, generic_array, petgraph, zerocopy)
    16  mir: HasErrors  (bitvec)
     9  mir: MissingFunctionDefinition  (bitvec, nalgebra)
     9  mir: NotSupported  (bitvec, itertools)
     4  Duplicate definition of identifier: _RNvNvNvCs5zOgJOEO9oF_4t  (pin_project_lite)
     3  layout of TestError<  (im)
     3  mir: TypeError  (im)
     3  layout of [[  (itertools)
     2  LLVM intrinsic `x86.vcvtps2ph.128`  (half)
     2  layout of  (ryu)
     2  expected scalar  (triomphe)
     1  const eval: MirEvalError  (bitvec)
     1  layout of Alias  (generic_array)
     1  layout of GenericArray<i32,  (generic_array)
     1  LLVM intrinsic `x86.vcvtps2ph.256`  (half)
     1  layout of Struct<#0, #1>: HasPlaceholder  (pin_project_lite)
     1  layout of PinRef<ReLateParam  (pin_project_lite)
     1  layout of NotUnpin<#0>: HasPlaceholder  (pin_project_lite)
     1  layout of NotUnpinRef<ReLateParam  (pin_project_lite)
     1  layout of Struct7<#0>: HasPlaceholder  (pin_project_lite)
     1  call to `into`, which has no body and no rustc symbol  (zerocopy)

## Checks

- `cargo fmt --all -- --check`: clean.
- `cargo clippy -p hir-ty -p rustd --all-targets -- --cap-lints warn`: exit 0. It prints the
  warnings the base branch already has; none is in the lines this branch adds to `probe.rs`,
  `infer/path.rs`, `eval/shim.rs` or `codegen/asm.rs` (checked by filtering the output for
  those files; I did not diff the whole warning list against the base).
- `cargo xtask tidy`: exit 0.
- `cargo test -p hir-def -p hir-ty -p ide-diagnostics`: 510, 4 and 744 passed, 0 failed, at
  `ffd6b4acd9`. hir-ty has only 4 unit tests in this fork. Earlier in the branch, `ide` passed
  1342 tests after the macro changes; it was not rerun at the end.
- `harness/stress.py` with rustd-14: 99 of 99 right, 0 restarts.
- `harness/difftest.py corpus --bin runner --crate cases` with rustd-14: 661 of 662 functions,
  MATCH. The one left is the `TypeId` of a function pointer type rustc has no symbol for, as
  on the base branch.
- `harness/breakers.py` with rustd-14: 13 of 13 right, 3 open (`future_size`, `f16c`,
  `unsized_tail_cmp`), 0 functions skipped that are not listed as open.

## Unverified or open

- The dx benchmark (1839 of 1839 functions) was not rerun on this branch. The Dioxus target
  directory was wiped for disk space, and a rebuild did not fit beside the sweeps. Fixes 2, 3
  and 6 change which functions rustd names and how paths resolve, so that number should be
  rechecked before this is merged anywhere.
- Timing: only rustd's codegen time per crate was recorded. I did not measure rustd's wall
  time against cargo's per crate.
- The crate versions are the ones listed at the end of this report. The binaries are the
  default branch of each repository as cloned during the session, not pinned to a commit.
- macOS and aarch64 were not run. `xgetbv`, `clmul` and `f16c` are x86-64 only and return a
  constant elsewhere.
- The final sweep ran while `stress.py` and the seven binaries were running on the same
  machine, so the codegen times in the table are on a loaded machine.

## Versions

serde_json-1.0.154 itertools-0.15.0 nalgebra-0.35.0 either-1.19.0 anyhow-1.0.104
thiserror-2.0.21 generic-array-1.4.5 typenum-1.20.1 arrayvec-0.7.8 heapless-0.9.3
bumpalo-3.20.3 hashbrown-0.17.1 smallvec-1.16.3 crossbeam-utils-0.8.23 crossbeam-queue-0.3.14
crossbeam-channel-0.5.17 crossbeam-epoch-0.9.21 slab-0.4.12 indexmap-2.14.2 tinyvec-1.13.3
bytes-1.12.1 arc-swap-1.9.2 once_cell-1.21.4 rayon-1.12.0 rayon-core-1.13.0 parking_lot-0.12.5
futures-util-0.3.34 futures-0.3.34 tokio-1.53.3 smol-2.0.2 async-channel-2.5.0 pin-project-
lite-0.2.17 memchr-2.8.3 bytemuck-1.25.2 simd-json-0.18.1 aho-corasick-1.1.5 sha2-0.11.1
blake3-1.8.7 crc32fast-1.5.2 memmap2-0.9.11 socket2-0.6.5 num-bigint-0.5.1 ryu-1.0.23
rust_decimal-1.43.0 num-traits-0.2.19 fastrand-2.5.0 scopeguard-1.2.0 downcast-rs-2.0.2
thread_local-1.1.10 unicode-segmentation-1.13.3 regex-syntax-0.8.11 regex-automata-0.4.18
bstr-1.13.1 smol_str-0.3.6 base64-0.23.1 percent-encoding-2.3.2 url-2.5.8 uuid-1.28.0
time-0.3.55 chrono-0.4.45 nom-8.0.0 winnow-1.0.4 log-0.4.34 bitflags-2.13.2 textwrap-0.16.4
strsim-0.11.1 fnv-1.0.7 ahash-0.8.12 byteorder-1.5.0 static_assertions-1.1.0 lazy_static-1.5.1
bitvec-1.1.1 petgraph-0.8.3 im-15.1.0 ordered-float-5.5.0 half-2.7.1 num-complex-0.4.6
libm-0.2.16 adler2-2.0.1 miniz_oxide-0.9.1 flate2-1.1.10 lru-0.18.5 dashmap-6.2.1
triomphe-0.1.17 zerocopy-0.8.63 rand-0.10.3 rand_core-0.10.1 siphasher-1.0.4 hex-literal-1.1.0
unicode-width-0.2.2 smawk-0.3.3 unicode-normalization-0.1.25 camino-1.2.6
