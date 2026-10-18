# A differential fuzzer for rustd

Every harness before this one runs programs that somebody wrote. This one makes programs up.
`harness/fuzz/gen.py` writes valid, deterministic, terminating Rust programs from a seed, and
`harness/fuzz/fuzz.py` builds each one with cargo and with rustd under both backends, runs the
binaries and compares what they print. A case whose output differs, a function rustd leaves to
rustc, or a rustd crash is a finding; the driver reduces it and writes it to
`harness/fuzz/found/` with the seed that makes it again.

Branch `rustd-fuzz`, from `rustd-collate` at 6d12f21c86, with `rustd-collate` at ce52276cf5
merged in at the end (27920a4f69, see "The merge"). Everything below was measured on Linux
x86-64 with Rust 1.99.0 and LLVM 20.1.8, with a release rustd built with `--features llvm`.
macOS, Windows, aarch64 and wasm were not run.

## Result

The fuzzer found sixteen distinct problems. Fourteen are fixed in rustd, one is worked around (it is a
bug in LLVM, not in rustd), and one is open. Each has a reduced case in `harness/breakers/`.

| # | What | Backends | Status | Breaker case |
|---|---|---|---|---|
| 1 | A panic's column was wrong for a parenthesised expression, for built-in indexing, and on a line with wide or zero-width characters | both | fixed, f1e4a5011f | `panic_columns`, `panic_column_width` |
| 2 | An index out of bounds inside a larger statement was reported at the statement, or at `<rustd>:0:0` | both | fixed, 80ca599faf | `panic_index_operand` |
| 3 | `assert!` of a condition longer than a line prints the condition on one line; rustc breaks it over several | both | open | `assert_message` |
| 4 | When an operand of an array, tuple, struct literal or call panicked, the operands already made were dropped in the wrong order relative to the statement's temporaries | both | fixed, dcb6b1fa7a | `unwind_operand_order` |
| 5 | A function with a C-convention aggregate argument passed on the stack whose size is not a multiple of eight was left to rustc (Cranelift panicked) | cranelift | fixed, ca26ccf5d1 | `stack_aggregate_odd_size` |
| 6 | An array of repeated `[i128; 5]` rows came out with part of it unwritten | llvm | worked around, 02796328b3 | `repeat_rows` |
| 7 | Before the 2024 edition, the temporaries of the tail of an `if` branch were dropped at the end of the enclosing statement instead of the end of the branch | both | fixed, fa04742c8d | `if_branch_temporaries` |
| 8 | A closure that matches on a field of a variable without binding anything, and uses another field, was left to rustc (`UnresolvedUpvar`) | both | fixed, 94141b5783 | `uncaptured_scrutinee` |
| 9 | `let _ = items[at]` and `match items[at] { _ => .. }` did not check the index, so an index out of bounds did not panic | both | fixed, a5360940ee | `unread_index` |
| 10 | When the pattern of a `let ... else` did not match, the value and the temporaries of the initializer were dropped after the `else` ran instead of before | both | fixed, e04c495c9c | `let_else_temporaries` |
| 11 | An array whose pattern would move an element out, where the pattern did not match, was dropped from the back instead of front to back | both | fixed, e04c495c9c and a1f0466b5d | `let_else_temporaries`, `unmatched_array_drop` |
| 12 | Before the 2024 edition, when a `for` loop was the tail of a block, the temporaries of what it iterates over were dropped after the block's locals instead of where the loop ends | both | fixed, 876f964f06 | `for_tail_temporaries` |
| 13 | Before the 2024 edition, the temporaries of what an `if let` matches on were dropped at the end of the branch instead of with those of the expression the `if` is in | both | fixed, 302a786299 | `if_let_temporaries` |
| 14 | When an arm bound part of an array by value (`[_, middle @ .., _] if guard`) and its guard was false, the elements put back into the array were never dropped | both | fixed, 49c4fddd1f | `guard_subslice_restored` |
| 15 | An array matched by an arm that would move elements out, where rustc can tell from the arms before it that no value reaches that arm, is dropped from the back instead of front to back | both | fixed, 20d97d4d73 | `unreachable_arm_move` |
| 16 | When an operand of an aggregate or call panicked and the expression was the tail of a block, the operands already made were dropped in the wrong order relative to the temporaries (finding 4 again, outside a statement) | both | fixed, 27a94f2770 | `unwind_operand_tail`, `unwind_operand_order` |

Numbers 4, 6, 7 and 9 to 16 are miscompiles: the program ran and did the wrong thing. Numbers 1 to 3
are wrong panic text. Numbers 5 and 8 are functions rustd refused, with the reason stated, so the
program was still right.

## The long runs

All with `python3 fuzz/fuzz.py run --seed S --rounds N --cases 30 --jobs 8 --reduce 600 --out DIR`,
both backends, counts from `fuzz.py report DIR`. "Generated" counts the functions the generator
wrote; "compiled" is the number of functions rustd's own report says it compiled, which also
has the prelude's functions and leaves out generic functions nobody calls. Each run used the
rustd of the time, so the earlier ones show the bugs that were then fixed.

| Run | Seed, rounds | rustd | Programs run | Functions generated | Compiled, Cranelift | Compiled, LLVM | Differences | Crashes | Left to rustc |
|---|---|---|---:|---:|---:|---:|---|---:|---|
| L1 | `L1`, 136 | with fixes 1, 2, 4 | 136 | 39,720 | 41,089 | 38,984 | Cranelift 4, LLVM 7 (all checksum; findings 6, 7) | 0 | Cranelift 12, `backend panic: StructArgument size is not properly aligned` (finding 5); LLVM 0 |
| L2 | `L2`, 119 (stopped) | with fixes 1 to 7 | 119 | 35,230 | 34,195 | 34,195 | 0 | 0 | 1 per backend, `mir: UnresolvedUpvar` (finding 8) |
| L3 | `L2`, 360 | with fixes 1 to 8 | 360 | 105,869 | 102,426 | 102,426 | 5 per backend: 4 checksum (findings 10, 11), 1 panic location (finding 9) | 0 | 0 |
| L4 | `L4`, 120 | e04c495c9c | 119 | 35,679 | 33,854 | 33,854 | 0 | 0 | 0 |
| L5 | `L5`, 150 | a1f0466b5d | 150 | 44,031 | 43,069 | 43,069 | 0 | 0 | 0 |
| L6 | `L6`, 210 | a1f0466b5d | 209 | 62,034 | 59,182 | 59,182 | 0 | 0 | 0 |
| L7 | `L7`, 360 | 876f964f06 | 360 | 106,202 | 102,898 | 102,898 | 8 per backend, all checksum (findings 13, 14, 15) | 0 | 0 |
| L8 | `L8`, 360 | 49c4fddd1f | 360 | 105,649 | 102,081 | 102,081 | 1 per backend, checksum (finding 16) | 0 | 0 |
| L9 | `L9`, 360 | 27a94f2770 | 360 | 106,116 | 101,848 | 101,848 | 0 | 0 | 0 |
| L10 | `L10`, 60 | 27920a4f69 (merged) | 60 | 17,297 | 16,734 | 16,734 | 0 | 0 | 0 |

L9 is the run on the last binary before the merge described below: 360 programs, none rejected,
10,800 cases, 106,116 functions generated and 101,848 compiled by rustd under each backend, no
difference, no crash, nothing left to rustc. L5 and L6 were meant to be that run (106,065
functions together, clean), and so were L7 and L8; each time a run that was otherwise clean
turned up one to three more drop-order bugs, which were fixed before the next. So the
honest reading is that about one program in a few hundred still found something up to L8,
and L9 is the first full-size run that found nothing.

L3 ran the seed of L2 again from its first round, so the 119 programs of L2 are the
first 119 of L3 and should not be counted twice; without L2 the runs are 2,116 programs and
622,597 generated functions. In L1 the two backends compiled different numbers of functions
because of the functions left to rustc and because the count is per build of the daemon's
report; I did not look further into that difference. Smaller runs before L1 (seeds `s1` and `s2`,
while the generator was being written) found the first findings, the `s1-` and `s2-` files
under `found/`; they are not counted here.

Two of the 2,116 programs were not compared, both through faults of the generator and not of
rustd, and both fixed:

- `L4-51` was rejected by rustc (E0502): a closure borrowed a trait object and the argument
  next to it called a `&mut self` method of the same object. The argument of a call no longer
  changes what the closure before it borrows (a1f0466b5d); `fuzz.py repro L4-51` then builds,
  301 functions generated, 299 compiled by rustd under each backend, no finding.
- `L6-176` built, but rustc's binary aborted: the program called `offset_from` on pointers to
  `()`, which panics for a zero-sized element, in a function that cannot unwind. The generator
  now leaves the distance out for such an element. With that, `fuzz.py repro L6-176` ran and
  showed finding 12 in case `c3` under both backends.

## Prior art

I read how the existing generators work before writing one. None of them was run here.

- rustlantis generates MIR directly, as `#![feature(custom_mir)]` source. That needs a nightly
  compiler, and it enters the compiler below the level rustd starts from: rustd lowers source
  through its own HIR and MIR, so a program written as MIR does not test that lowering, and
  1.99 stable rejects it.
- rustsmith generates source programs, but from a small language. Going by its own description
  its programs have no trait objects, closures by capture kind, slice patterns, raw pointers, `repr`
  attributes, `catch_unwind` or observable drop order, which is most of the list this task
  asks for. It is also a Kotlin program, which would add a JVM to the harness.
- From csmith I took swarm testing. Every case turns a random fifth of the constructs off and
  weights the rest differently (`weight` in `gen.py`), so that cases differ in kind and not only
  in detail.

So the generator is new: `harness/fuzz/gen.py`, 2,565 lines of Python, no dependencies.

## The generator

A program is `prelude.rs` followed by one module per case. A case is a set of types, traits and
helper functions and a `run` function. Everything a case computes is fed to one 64-bit checksum
(the `Mix` trait in `prelude.rs`, implemented for every type the generator uses), and `main` runs
each case inside `catch_unwind` and prints one line per case: the name, the checksum, and whether
it ended or panicked. Panic messages go to stdout too through a panic hook, so the place a
panic is reported at is compared as well. The source of case N of seed S depends only on (S, N),
so one case can be made again alone.

What keeps a program free of undefined behaviour and of anything two correct compilers may
disagree on:

- Arithmetic is wrapping, checked, saturating or overflowing. Plain `+`, `/`, `<<`, indexing and
  `unwrap` appear only where the panic is caught, and the values are chosen so that it is known
  whether they panic.
- No address, NaN bit pattern, sign of a zero from `min`/`max`, or hash order is observed. Maps
  are `BTreeMap`.
- Raw pointers stay inside the object they came from and read only what was written. Unions and
  `MaybeUninit` are read only through the field that was written.
- Every loop has a counter the generated code cannot assign. Calls go only to functions defined
  earlier, recursion counts down, and a case's work is bounded by an estimate.
- Half the programs are built as edition 2021 and half as 2024, because drop order differs.

### Coverage

Each construct the generator emits is counted by name; there are 400 counters. The
table groups them by the list in the task. The counts are from run L3
(360 programs; `fuzz.py report DIR --coverage` prints every counter).

| Construct | Times generated | Counters |
|---|---:|---|
| Type choices: an integer, any of the twelve widths | 16812988 | 1 |
| Type choices: `i128`/`u128` only (a separate choice, in addition to the row above) | 17346223 | 1 |
| Wrapping, checked, saturating, overflowing arithmetic | 209515 | 11 |
| Shifts | 63324 | 4 |
| Other integer operations (bits, bytes, ilog, isqrt, clamp) | 60992 | 7 |
| Casts (`as`, `From`, `TryFrom`) | 135476 | 10 |
| Floats | 54410 | 7 |
| `bool` and `char` | 72453 | 9 |
| Tuples | 148119 | 3 |
| Structs (plain, generic, update syntax) | 125808 | 4 |
| Enums with data, fieldless enums, niches (`NonZero`, `Option<Box>`) | 107278 | 4 |
| `repr(C)`, `repr(u8)` and other integer reprs, `repr(align)`, packed | 33138 | 9 |
| Arrays | 119355 | 8 |
| Slices | 42487 | 12 |
| References | 98679 | 5 |
| Raw pointers, unions, `MaybeUninit`, transmute | 167476 | 18 |
| Patterns: literals, ranges, bindings, `@`, or-patterns | 562826 | 10 |
| Patterns: slices | 181844 | 8 |
| Patterns: guards | 88300 | 1 |
| `match`, `if let`, `let else` | 346107 | 8 |
| Loops (`loop`, `while`, `while let`, `for`) | 78612 | 5 |
| Labelled `break`/`continue`, `break` with a value, labelled blocks | 81987 | 3 |
| Closures: capture by reference | 48316 | 4 |
| Closures: capture by mutable reference | 29516 | 3 |
| Closures: capture by move, `FnOnce`, returned and boxed closures | 82087 | 8 |
| Generics (functions, structs, enums, const generics, bounds) | 246508 | 10 |
| Traits (impls, default methods, derives, qualified calls) | 56231 | 8 |
| Trait objects | 46602 | 6 |
| `Option` | 160135 | 15 |
| `Result` and `?` | 47797 | 8 |
| `Box` | 21238 | 2 |
| `Vec` | 138732 | 23 |
| `String` and `str` | 95946 | 31 |
| Iterators | 185441 | 27 |
| Drops with observable order | 28518 | 8 |
| Panics caught with `catch_unwind` | 73220 | 16 |
| Function pointers | 33247 | 4 |
| Calls by argument count (0 to 8 and more) | 84088 | 8 |
| ABI: scalar integer argument or return | 72693 | 2 |
| ABI: scalar float argument or return | 11957 | 2 |
| ABI: integer pair | 8665 | 2 |
| ABI: float pair | 357 | 2 |
| ABI: mixed float/integer pair | 844 | 2 |
| ABI: in memory (large aggregate) | 40817 | 2 |
| ABI: zero-sized | 6527 | 2 |
| `extern "C"` functions | 7802 | 1 |
| Statics, consts, `size_of`/`align_of` | 41433 | 8 |
| Formatting (`Debug`, `Display`), `BTreeMap` | 28517 | 3 |

"Times generated" is the sum of the counters in the row, and "Counters" how many there are.
The first two rows count each time the generator picked a type, including the picks it made
while looking for a type that fits a place, which is why they are so large; every other row
counts constructs written into a program. Four counters are in two rows (`closure.by_reference` under references and closures,
`cast.enum_to_int` under casts and enums, `raw.packed_field` under raw pointers and reprs,
`generic.const_array` under arrays and generics), so the column does not add up to a total. 49 of the 400 counters are in no row because they are not on
the task's list or are too general to mean anything (`stmt.let`, `expr.literal`, `type.bool`
and the like).

The integer types by how often their name appears in the source of the first twenty of those
programs (5,827 functions): `i8` 8,321, `i16` 5,661, `i32` 5,669, `i64` 6,059, `i128` 14,784,
`isize` 5,388, `u8` 9,049, `u16` 5,682, `u32` 6,463, `u64` 5,774, `u128` 14,686, `usize` 6,566,
`f32` 17,524, `f64` 17,115.

What the generator does not make: `async`, threads, macros of its own, inline assembly, SIMD
intrinsics, `dyn Any`, trait objects with generic methods, specialization-like overlap, and
any I/O other than the checksum lines. Float pairs returned in two SSE registers are generated
least of the ABI classes (see the table).

## The driver

    cd harness
    export RUSTD_BIN=/path/to/release/rustd
    python3 fuzz/fuzz.py smoke                              # 2 programs of 25 cases, fixed seed, for CI
    python3 fuzz/fuzz.py run --seed L2 --rounds 360 --jobs 8 --out DIR
    python3 fuzz/fuzz.py repro L2-1 c27                     # one program again, or some of its cases
    python3 fuzz/fuzz.py check fuzz/found/*.rs              # every reduction against this rustd
    python3 fuzz/fuzz.py report DIR --coverage              # the counts of a run

A round is one program: 30 cases by default, about 290 functions. It is one cargo project built
through `Fixture` from `difftest.py`, once by cargo and once by rustd per backend
(`--backends cranelift,llvm`; llvm runs with `RUSTD_OPT=2 RUSTD_EMIT=fast`), so a round costs
one cargo build and two rustd builds. Rounds are split over `--jobs` worker processes; round N
of a seed is the same program whatever the number of jobs.

A finding is one of: a case whose lines differ (told apart as checksum, panic location, panic
message, whether it panics, or the program dying), a function that rustd's report lists as skipped, or a rustd build that did not finish.
A cause seen before in the same run is counted and not reduced again. Reduction is delta
debugging in two steps: first over the cases of the program, then over the lines of the one
case left, by brace-balanced units, keeping a candidate only when rustc still accepts it and the
same kind of finding with the same key still shows. `--reduce` bounds the seconds spent per
finding. The result goes to `fuzz/found/<kind>-<key>-<seed>-<case>.rs` with a header that names
the seed, the edition, the backend and both outputs.

The smoke run is in CI (`.github/workflows/rustd.yaml`, in the Linux harness job after `keys.py`,
per backend, on x86-64 only since nothing else was fuzzed). On this
machine, with nothing else running, it took 57 s for both backends with the binary of 27a94f2770
and 89 s with the merged one
(`time python3 fuzz/fuzz.py smoke`): 475 functions generated, 505 compiled by rustd per
backend, no finding.

## The findings

**1. Panic columns.** rustd took the column of a panic from the start of the expression
without its parentheses, reported built-in indexing at the bracket where rustc reports it at
the start of the base, and counted columns in characters where rustc counts display width (a
tab is four, a wide character two, a combining mark zero). The fix distinguishes built-in
indexing from an overloaded `Index` and counts width with `unicode-width`, the crate rustc
uses. Reductions: `differs-panic-location-s1-2-c13.rs`.

**2. Index out of bounds inside a statement.** When the index expression was an operand of a
larger statement (an argument of `assert!`, the value of a `let` with a conversion around it),
the panic was reported at the whole statement or at `<rustd>:0:0`. The bounds check now carries
the span of the index expression. Reductions: `differs-panic-location-s2-2-c27.rs`,
`s2-3-c10`, `s2-28-c1`, `s2-36-c24`, `s2-43-c12`.

**3. `assert!` message (open).** The message of a failed `assert!` is the condition as
rustc's pretty printer writes it back, and that printer breaks a condition longer than a line
over several lines. rustd writes it on one line. Matching it needs `rustc_ast_pretty`, which
is not a crate rustd can depend on, or a port of its line-breaking algorithm. The generator
keeps `assert!` conditions short so that this does not drown other findings. The case is in
`OPEN` in `breakers.py`.

**4. Drop order when an operand panics.** In
`let x = [Loud(1), sum(&Loud(20), &Loud(30)), Loud(3), none().unwrap()];` rustc drops, on the
unwind, 3, 50, 1 and then 30, 20: the operands belong to the array expression, and the
`&Loud(..)` temporaries live to the end of the statement. rustd dropped 1 last. The same
happened for tuples, struct literals and call arguments. In a block's tail expression both
kinds of temporary are in one scope and the order is plain reverse creation, which rustd
already did, so the fix applies to statements only. Reduction: `differs-checksum-s2-23-c24.rs`.

**5. Odd-sized stack aggregate.** An `extern "C"` function whose aggregate argument no longer
fits in registers gets it on the stack. rustd gave Cranelift the aggregate's exact size, and
Cranelift asserts a multiple of eight, so the function was left to rustc with `backend panic:
StructArgument size is not properly aligned`. The slot is now rounded up, as the System V ABI
says. Reductions: the three `left-backend-panic-structargument-*.rs`. This one was hit 12
times in 136 programs.

**6. LLVM's SLP vectorizer (worked around).**
`[[-18i128; 5], [524287; 5], ...]` built under LLVM at `-O2` had rows whose later elements were
never written. The IR rustd gives LLVM is right: `opt-20 -O2 -opt-bisect-limit` on the dumped
module puts the change at the SLP vectorizer pass, which turns the stores into vector stores
with poison lanes. This is LLVM 20.1.8 miscompiling valid IR. rustd now turns the SLP
vectorizer off (`-vectorize-slp=false`); the loop vectorizer stays on. I did not measure what
that costs in generated code speed, and I did not check whether a later LLVM has the bug.
rustc 1.99 uses its own LLVM and did not show it on the same source. Reductions:
`differs-checksum-L1-45-c17.rs`, `L1-77-c19`, `L1-142-c12`, `s2-49-c12`.

**7. Temporaries of an `if` branch's tail, before 2024.** Before the 2024 edition the
temporaries of a block's tail expression live as long as those of the expression the block is
in. rustd applied that to every block. A branch of an `if` is where rustc ends them anyway, so
in `seen(&if c { a } else { pick(&Loud(4), &Loud(6)).clone() })` rustc drops 6 and 4 before
`seen` runs and rustd dropped them after. Each branch now has its own scope. Match arms, loop
bodies and the right side of `&&`/`||` were checked by hand and were already right. The
breakers crate is edition 2024, so the case lives in a new 2021 crate,
`harness/breakers/edition2021`. Reductions: `differs-checksum-L1-12-c3.rs`, `L1-98-c28`.

**8. Uncaptured match scrutinee.** In `|| match x.1 { (_, _) if g => a, _ => x.0 }` nothing
reads `x.1`, so the closure captures `x.0` only. rustd's MIR still had a fake read of `x.1`,
found no capture to map it to and gave up on the closure and, through it, on its callers.
Fake reads of places that are not captured are now removed; rustc does not emit them either.
Reduction: `left-mir-unresolvedupvar-L2-1-c27.rs`.

**9. Index of an element that is not read.** rustd checks an index where it computes the
address of the element, and it made no code at all for a place that is matched on by a
pattern that binds and tests nothing. So `let _ = items[at];`, `let (_, _) = items[at];` and
`match items[at] { _ => .. }` went on with `at` out of bounds where rustc panics. Such a place
is now still computed when it has a run-time index. The breaker has it for arrays, slices, a
field, a deref, a nested index and inside a closure. Reduction:
`differs-panic-location-L2-154-c15.rs` (the driver called it a panic location because the next
panic in the case was a different one).

**10. `let ... else` temporaries.** When the pattern of `let PAT = EXPR else { .. };` does not
match, rustc drops the value of `EXPR` and its temporaries, also the ones that would have
lived to the end of the block (`let (Loud(1), b) = &(Loud(n), Loud(5)) else ..`), and then runs
the `else`. rustd ran the `else` first and dropped them on the way out of it. They are now
dropped where the `else` starts. Reductions: `differs-checksum-L2-354-c5.rs`, `L2-372-c9`,
`L2-431-c24`.

**11. Order in which an unmatched array is dropped.** rustc drops an array that has lost
elements to a pattern from the back, and an array that is whole front to back, and it decides
which at compile time: where a drop is, can an element have been moved out on some way there.
rustd's MIR has no such analysis; it keeps flags at run time and used the from-the-back order
for every drop of a local that some pattern in the function moves out of. So
`let [a, Loud(7)] = [Loud(1), Loud(n)] else { .. }` with `n` not 7 dropped `n` and then 1. There
is now a small forward analysis over the MIR (`MirBody::parted_at_block_ends`): a drop that no
move out of a part of the local can reach drops the local as one value. That fixed `let else`
and `match`. `if let` needed one more change: rustd lowered `if let` to a `bool` and a switch
on it, which put the matched and the unmatched way through one block and so lost what the
analysis needs. An `if` whose whole condition is a `let` now goes straight from the pattern to
its branches. A let chain (`if let A = x && c`) still goes through the `bool`. I tried one by hand (a
four-element array pattern followed by `&& on`, with the pattern matching, not matching, and
matching with `on` false) and it printed what rustc's build prints under Cranelift; the
fuzzer does not generate let chains, so that is the only evidence for them.
Reduction: `differs-checksum-L2-463-c8.rs`, and the three of finding 10.

**12. Temporaries of a `for` in tail position, before 2024.** rustc writes `for` as a `match`
on the iterator around a `loop` and ends the temporaries of the whole thing where the loop
ends. That only shows before the 2024 edition and when the loop is the tail of a block (a
function body that ends in a `for` is one): the temporaries of any other tail are dropped
after the block's locals, those of a `for` before. rustd treated the loop as any other tail.
The `match` a `for` is written as now has a scope of its own for its temporaries. It was found
late because it needs a `for` in tail position whose iterable makes a temporary with a `Drop`,
in a 2021 program, and it came out of `L6-176` only after the generator fault in that
program was fixed (see the long runs). Reduction: `differs-checksum-L6-176-c3.rs`.

**13. Temporaries of an `if let`, before 2024.** Before the 2024 edition the temporaries of
the expression an `if let` matches on are not the `if`'s: they live as long as the temporaries
of the expression the `if` is in, so to the end of the statement, or past the block's locals
when the `if let` is the tail of a block. Only what the pattern binds ends with the branch.
rustd dropped all of it at the end of the branch, the 2024 rule. Before 2024 the scrutinee's
temporaries are now handed to the scope around the `if`. `L7` hit this in six programs (rounds
50, 81, 87, 171, 274 and 290). Reductions: `differs-checksum-L7-50-c6.rs`, `L7-87-c6`,
`L7-81-c9`, `L7-171-c9`.

**14. A subslice put back after a failed guard.** In
`match array { [_, middle @ .., Loud(3..=40)] if on => .., _ => .. }` rustc moves the middle
elements into `middle` before it runs the guard, and when the guard is false moves them back
so the next arms see the array whole. rustd did both moves, but its note of which elements of
the array are gone was only cleared for an assignment to a field or one element, not to a
subslice. The elements were back and still marked as moved, so the array was dropped without
them and their destructors never ran. An assignment to a subslice now clears the note for
each of its elements, the same set a move out of the subslice sets. Both editions, both
backends. `L7` hit it in one program (round 174). Reduction:
`differs-checksum-L7-174-c9.rs`.

**15. An arm that moves and that rustc's tree does not lead to.** In

    match items { [.., Loud::Char(_)] => 1, [Loud::Char(..), middle @ .., Loud::Char('Z')] => 2, _ => 3 }

the second arm can never be taken: the first takes every array that ends in a `Char`. rustc
builds one tree of tests for all the arms, the second arm's bindings end up in a block no
edge leads to, and its drop analysis sees an array nothing is moved out of and drops it front
to back. rustd tries the arms one after the other, so its MIR has a path through the second
arm's move, and the array was dropped as one that may have lost elements: from the back.

What decides is not whether a value can reach the arm but whether rustc's tree has an edge
to it, and those differ. I measured with rustc alone (`RUSTC_BOOTSTRAP=1 rustc
-Zdump-mir=FN -Zdump-mir-dir=DIR` writes the MIR as built, which is the ground truth here):
an arm behind an overlapping range, an arm behind a wildcard arm with a guard, and an arm
after arms for every variant are all unreachable for any value and all still have an edge,
so the array is dropped from the back; an arm covered through an or-pattern, through a
nested pattern or through an integer literal has none.

So the fix follows rustc's construction. `mir/lower/match_tree.rs` (about 490 lines) turns
each arm into what rustc calls a candidate, the list of places its pattern tests, and sorts
the candidates the way `rustc_mir_build`'s match builder does: take the first test of the
first candidate, sort the candidates the outcome says something about into its branches,
leave the rest for after, expand or-patterns where rustc does, and stop at a candidate that
matches without a guard. It builds nothing; it only says which arms are reached. That is
the part of rustc's roughly 4,000 lines of match lowering that decides reachability, and
it was contained. rustd still lowers the arms one after the other; the bindings of an arm
the tree does not reach go into a block listed in `MirBody::unreached`, and the analysis of
what may be moved out where a drop is does not enter those blocks. If the simulation is
wrong about an arm, the cost is the order of a drop: every part is still dropped once, by
its flag. A pattern with something the simulation does not follow (a constant named by a
path, a deref pattern) makes every arm of that `match` count as reached, as before.

The breaker case has nine functions, the six that rustc drops front to back and three it
does not, and matches rustc under both backends. `L7` hit this in one program (round 230).
Reduction: `differs-checksum-L7-230-c24.rs`.

**16. Operands of a tail when a later one panics.** The fix of finding 4 moved the operands
of an aggregate or call in front of the statement's temporaries on the way out of a panic,
and only did so inside a statement. `L8-231` had the same thing in the tail of a closure's
block, in a 2021 program. I measured what rustc does for each position (a closure with
`(Loud(1), sum(&Loud(20), &Loud(30)), Loud(3), none().unwrap())` as a statement, as the tail
of a block, as the body without braces, and nested in another call, in both editions) and
the rule is: operands belong to the expression they are operands of, temporaries to the
nearest expression that ends temporaries, and each scope is dropped in the reverse of the
order its values were made. So the operands go first (`3, 50, 1, 30, 20`) everywhere except
where the expression itself ends its temporaries, which is the body of a closure without
braces and, from the 2024 edition, the tail of a block; there everything is in one scope
(`3, 50, 30, 20, 1`). rustd now keeps the set of those expressions and applies the fix of
finding 4 to every other aggregate or call. Reduction: `differs-checksum-L8-231-c11.rs`.

`fuzz.py check fuzz/found/*.rs` with the binary of 20d97d4d73: all 30 files give the same
output as rustc under both backends, with no function left to rustc
(`RUSTD_BIN=rustd python3 fuzz/fuzz.py check fuzz/found/*.rs`, 60 lines of `same`).

## The merge

`rustd-collate` moved to ce52276cf5 while this was running, and I merged it in (27920a4f69).
It conflicted where it mattered: the same weeks' work on rustc's own test suites had fixed
drop order in the same functions, differently. Both sides had a fix for the temporaries of an
`if let` before 2024, for `let ... else`, and for the order an array is dropped in after a
pattern gave its elements back. What I kept:

- From this branch, in `mir/lower.rs`: an `if let` that goes straight from the pattern to its
  branches, a scope of its own for the temporaries of each branch of an `if`, and its
  `lower_let_else`. The other side's `end_of_branch`, `forget_branch`, `outlive_if_let` and
  `lower_let_else` are gone; its breaker cases for them (`if_let_outlives`, `let_else_drops`,
  `tail_tuple_drops`, `guard_let_drops`, `repeat_moved`) pass with this branch's code.
- From `rustd-collate`, in `mir/codegen.rs`: the analysis of which fields may be moved out
  where a drop is (`moved_where_blocks_end`), the stack argument modes (which include the fix
  of finding 5), and scope ends as the place of drops. This branch's
  `MirBody::parted_at_block_ends` answered nearly the same question and was still there
  beside it after the merge. It is gone since 20d97d4d73: the other analysis alone passes
  every breaker case, including the ones written for this one.
- The edition 2021 breakers are one crate, `breakers2021`, with both sides' cases.
- `OPEN` in `breakers.py` is both sides' open cases.

Measured on the merged binary: the required harnesses below, `fuzz.py check` (29 of the 30
files the same, the one of finding 15 differs, as before the merge), the smoke run (no
finding), and one more run, shorter than the others: `--seed L10 --rounds 60`, 60 programs,
none rejected, 1,800 cases, 17,297 functions generated, 16,734 compiled by rustd under each
backend, no difference, no crash, nothing left to rustc. So the 100,000 functions per backend
without a finding were run on 27a94f2770, the head before the merge, and the merged head has
the gate, the reductions and 17,000 functions behind it.

## The required harnesses

Run from `harness/` with `RUSTD_BIN` at the release binary of the merged head (27920a4f69), once with
`RUSTD_BACKEND=cranelift` and once with `RUSTD_BACKEND=llvm RUSTD_OPT=2 RUSTD_EMIT=fast`. The
results were the same under both:

| Command | Result |
|---|---|
| `python3 difftest.py corpus --bin runner --crate cases --layouts` | exit 0, 0 of 893 layouts differ |
| `python3 programs.py` | 22 of 22 right |
| `python3 stress.py` | 99 of 99 right, 0 restarts |
| `python3 breakers.py` | 59 of 59 right, 6 open, 0 functions skipped |
| `python3 unwinds.py` | 8 of 8 right |
| `python3 splices.py` | 71 of 71 right |
| `python3 asmedits.py` | 10 of 10 right |
| `python3 includes.py` | 9 of 9 right |
| `python3 worktrees.py` | 62 of 62 right |

The table is as of 20d97d4d73 (finding 15 fixed), run again under both backends; on the
merge commit itself the breakers were 58 of 58 right with 7 open, the rest the same. The 6
open of `breakers.py` are finding 3 of this report, three that
`rustd-collate` lists as open and two it lists as left to rustc. Before the merge, on
27a94f2770, the same nine passed under both backends with 40 of 40 breakers right and 2 open.

`cargo fmt --all -- --check` is clean. `cargo test --release -p hir-ty -p hir-def -p parser -p
syntax -p rustd` on the merged head: 512, 24, 330, 33 and 56 tests passed in the test binaries
that have tests, 0 failed, 1 ignored.

The other harnesses `rustd-collate` has in CI now (`keys.py`, `backends.py`, `debuginfo.py`,
`buildmodel.py`, the `store*.py` ones, `remote.py`) are not in the required list and I did
not run them on the merged head.

## Open, and not done

- Finding 3, the `assert!` message, is open for the reason above.
- Finding 6 is a workaround. The honest fix is in LLVM; the cost of running without the SLP
  vectorizer was not measured.
- The fuzzer only tests a whole build. It does not edit a program and splice the change in, so
  the daemon's incremental path is not fuzzed.
- Only Linux x86-64 was run. The ABI classes the generator aims at are the System V ones.
- The generator's programs are single-crate and use only `std`.
- A reduction is bounded by time (`--reduce`, 600 s in the long runs), so a reduced file is
  small but not always minimal. The breaker cases were written by hand from them.
- The CI jobs for Windows, macOS and AArch64 that came with the merge do not run the fuzzer.
