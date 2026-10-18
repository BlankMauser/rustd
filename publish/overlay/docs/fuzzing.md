# The fuzzer

Every other harness runs programs somebody wrote. The fuzzer makes programs up, builds each
with cargo and with rustd, and compares what they print. It found sixteen bugs the other
harnesses had not, eleven of them miscompiles, so it is worth knowing how to run it and what
it does not cover. The full account, with every finding, is in
[reports/FUZZ.md](reports/FUZZ.md).

## Running it

    cd harness
    export RUSTD_BIN=$PWD/../target/release/rustd
    python3 fuzz/fuzz.py smoke                                  # 2 programs, fixed seed; in CI
    python3 fuzz/fuzz.py run --seed S --rounds N --jobs 8 --out DIR
    python3 fuzz/fuzz.py repro S-ROUND [CASE ...]               # one program again, or some of its cases
    python3 fuzz/fuzz.py check fuzz/found/*.rs                  # every stored reduction against this rustd
    python3 fuzz/fuzz.py report DIR [--coverage]                # add up the logs of a run

By default each program is built under both backends, so `RUSTD_BIN` should be a build with
`--features llvm`; `--backends cranelift` runs one. LLVM is run with
`RUSTD_OPT=2 RUSTD_EMIT=fast`. The exit status is 1 when there is a finding.

`run` also takes `--cases` (cases per program, 30 by default), `--first` (the number of the
first round), `--reduce` (seconds a reduction may take, 0 to only count) and `--work`
(where the programs are built).

A round is one program of about 290 functions. It costs one cargo build and one rustd build
per backend. The smoke run took 31 s under Cranelift and 53 s under LLVM on the machine
these documents were checked on. A full-size run of 360 programs is long; use `--jobs`.

## What a program is

`fuzz/gen.py` writes a program from a seed: `fuzz/prelude.rs` followed by one module per
case. A case is a set of types, traits and helper functions and a `run` function. Everything
a case computes is fed into one 64-bit checksum. `main` runs each case inside
`catch_unwind` and prints one line per case with its name, its checksum and whether it
ended or panicked. Panic messages are printed too, so the place a panic is reported at is
compared as well.

The source of case N of seed S depends only on S and N. A case can therefore be made again
on its own, and round N of a seed is the same program whatever `--jobs` is.

The generator has to produce programs on which two correct compilers cannot disagree:

- Arithmetic is wrapping, checked, saturating or overflowing. Plain `+`, `/`, `<<`,
  indexing and `unwrap` appear only where the panic is caught and it is known whether they
  panic.
- No address, NaN bit pattern, sign of a zero or hash order is observed. Maps are
  `BTreeMap`.
- Raw pointers stay inside the object they came from and read only what was written.
  Unions and `MaybeUninit` are read only through the field that was written.
- Every loop has a counter the generated code cannot assign, calls go only to functions
  defined earlier, and recursion counts down.
- Half the programs are built as edition 2021 and half as 2024, because drop order differs
  between them.

It does not generate `async`, threads, macros of its own, inline assembly, SIMD intrinsics,
`dyn Any`, or any I/O other than the checksum lines. Programs are a single crate and use
only `std`.

## What counts as a finding

- A case whose line differs from cargo's: the checksum, the panic location, the panic
  message, whether it panics, or the program dying.
- A function rustd's report lists as left to rustc.
- A rustd build that did not finish.

Each finding is reduced in two steps, first over the cases of the program and then over the
lines of the one case left, keeping a candidate only while rustc still accepts it and the
same kind of finding still shows. The result is written to
`fuzz/found/<kind>-<key>-<seed>-<case>.rs` with a header naming the seed, the edition, the
backend and both outputs. A cause already seen in the same run is counted and not reduced
again. Reduction is bounded by time, so a reduced file is small but not always minimal.

## From a finding to a fix

1. `fuzz.py repro SEED-ROUND CASE` to see it again.
2. Read the file under `fuzz/found/`. Reduce it further by hand to a function that shows
   the cause.
3. Add that as a breaker with the cause written at the top
   ([harness.md](harness.md), "Adding a breaker"). The breaker is the regression test; the
   file under `found/` is the evidence.
4. Fix it, run `fuzz.py check fuzz/found/*.rs` and the gate.
5. Run a new seed. Several times, a run that was otherwise clean found one to three more
   bugs of the same family as the one just fixed.

## What it has found

Sixteen distinct problems, on Linux x86-64 with Rust 1.99.0. Eleven were miscompiles: the
program ran and did the wrong thing. Most of those were the order in which temporaries and
partly moved values are dropped, in `if let`, `let ... else`, `match` guards, `for` loops in
tail position and when an operand panics. One was an index that was not bounds-checked when
its value was unused. One was LLVM miscompiling valid IR, which rustd works around by
turning a vectoriser pass off. Three were wrong panic text and two were functions rustd
refused.

Thirteen are fixed and one is worked around. Two are open and listed in
[support.md](support.md): the text of a long `assert!` condition, and the drop order of an
array behind an unreachable match arm.

The long runs are tabulated in [results.md](results.md). Up to the eighth, about one program
in a few hundred still found something. The ninth, 360 programs and 106,116 generated
functions on commit 27a94f2770, found nothing, and so did a shorter run of 60 programs
after the last merge.

## What it does not cover

- It builds whole programs. It does not edit a program and splice the change in, so the
  daemon's incremental path, the patch object and the splice are not fuzzed.
- Only Linux x86-64 has been run. The ABI shapes the generator aims at are the System V
  ones. The smoke run is in the Linux x86-64 CI jobs only.
- Single-crate programs using only `std`: nothing about dependencies, features, build
  scripts or proc macros.
- The constructs listed above that the generator does not make.
