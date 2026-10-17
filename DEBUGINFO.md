# Debug info

Branch `rustd-debuginfo`. Everything here is Linux x86-64 ELF. Mach-O and Windows are design
notes at the end. The first round (line tables, backtraces, the splice path, Cranelift
variables) was merged into `rustd-collate`; the second round is on top of `rustd-collate` at
91c7292532 and is marked "round two" where it changed something.

## What there is now

Every function rustd compiles is described in DWARF 4: a `DW_TAG_subprogram` per function
with its name, linkage name, declaration file and line and its range of code, and a
`.debug_line` program that gives a file, line and column to every instruction that came from
a statement. The producer string starts with `rustd`, which is how the harness tells rustd's
units from the ones rustc wrote for the dependencies.

Round two: an object now has one compilation unit per 256 functions instead of one for the
whole object. With a single unit of 11,647 functions GNU `addr2line` took 1.1 s for its first
lookup in the corpus binary (15 ms in the cargo build), because it reads a unit's whole line
program to answer for one address. `perf` gives `addr2line` one second and then gives up, so
under LLVM perf lost the lines of some frames. With units of 256 the lookups are quick and
perf has a line for every frame again.

The two backends get there differently.

Cranelift: each MIR statement and terminator sets a `SourceLoc` on the instructions built for
it. The location is an index into a table of spans kept per function, not a line, so an edit
above a function does not change its compiled code or its cache entry. After emission the
source-location ranges of the machine buffer become rows of (code offset, span index). Lines
are looked up from spans only when the function is put into an object, and gimli's writer
produces `.debug_info`, `.debug_abbrev`, `.debug_line`, `.debug_str`, `.debug_line_str` and
`.debug_ranges` with their relocations (`crates/hir-ty/src/mir/codegen/debuginfo.rs`).

LLVM: the lowering from Cranelift IR to LLVM IR creates a `DISubprogram` per function through
LLVM's DIBuilder and puts a debug location on every instruction, with the same span index in
the place of the line number. LLVM writes a `.debug_line` for its module, rustd reads it back
with gimli into the same (offset, span) rows, and from there the path is the one Cranelift
uses. So the optimizer decides which instruction belongs to which statement, and the final
sections are written by the same code for both backends.

Names are what rustc's demangler prints for its v0 symbols: `krate::module::f::<u8>`,
`<T as Trait>::f`, `f::{closure#0}`, with default generic arguments left out and closures
numbered in rustc's order.

The end of a scope, where its drops run, is given the place of the closing brace, as rustc
does. Before this the drops had the line where the scope's expression begins.

Debug info follows the profile; see "Profile settings" below.

## Variables and types (round two)

Both backends now describe parameters and `let` bindings, and they describe them from the
same data. A function's code carries, next to its line rows, a list of the stack slots that
hold named bindings and where each slot is: a `DW_AT_frame_base` expression and, per slot,
either an offset from the frame base or a list of (range of code, location expression).

Cranelift: a binding that lives in one stack slot is at a fixed offset from `rbp`.

LLVM: the lowering gives every such slot an `alloca` and a `dbg.declare` through DIBuilder,
under a made-up variable named after the slot number and with a made-up integer type of the
slot's size. LLVM then does what it does for clang: where the optimizer keeps the slot in
memory it writes a frame offset, where it moves the value into registers or drops it, it
writes a location list or nothing. rustd reads LLVM's `.debug_info` back (frame base, each
slot's `DW_AT_location`, location lists resolved to ranges of the function) and throws LLVM's
unit away. The real names, types and scopes are then written by the same writer as for
Cranelift. So LLVM decides where a value is and whether it still exists, and rustd never
guesses: a slot LLVM says nothing about is described with no location, and gdb prints
`<optimized out>` for it.

A parameter passed by reference to a caller's copy (a large array or struct by value) is
described as the pointer's slot dereferenced.

Types. A function's variables refer to a table of types that is independent of DWARF:
base types, pointers, arrays, and aggregates with fields at their layout offsets. What is
described in full:

- integers, floats, `bool`, `char`
- references and raw pointers; `&str` and `&[T]` as `data_ptr` and `length`, which is the
  shape rustc writes and gdb's Rust support understands
- arrays, tuples, structs, tuple structs, unions
- enums: fieldless ones as `DW_TAG_enumeration_type` with their discriminant values; the rest
  as a `DW_TAG_variant_part` with the discriminant member and one `DW_TAG_variant` per
  variant, for both tagged and niche layouts (`Option<&T>` is a niche: the `None` variant is
  the default variant, as rustc writes it)
- `Option`, `Result`, `Box`, `Vec`, `String` and the other library types follow from the above:
  they are described field by field with rustc's type names (`alloc::vec::Vec<i32,
  alloc::alloc::Global>`) and with their generic parameters as
  `DW_TAG_template_type_parameter`, which is what the pretty-printers the toolchain ships
  (`rust-gdb`) look for. The harness compares what `rust-gdb` prints for a `Vec`, a `String`
  and a slice of the `Vec` with what it prints for the cargo build.

A closure, a `dyn` pointer and whatever else there is are still described by name and size
only.

With `debug = "line-tables-only"` (or `1`, `limited`) the variables and types are left out
and only the line tables and subprograms are written.

## Tail calls, inlining and `#[track_caller]` (round two)

The frame that was missing under LLVM is a tail call, and I left it as it is. In the corpus
the panic at `cases/src/stabilized.rs:229` goes through

    fn or_die(v: Option<u32>) -> u32 {
        v.unwrap_or_else(|| diverge(77))
    }

and LLVM at -O2 compiles `or_die` to two instructions, `lea` of the closure and
`jmp unwrap_or_else`. There is no return address of `or_die` on the stack when the closure
panics, so no unwinder can show the frame, with any debug info. rustc does the same when it
optimizes: a three-function copy of this shape with `#[inline(never)]`, built with
`rustc -O -g`, prints `diverge`, `or_else`, `main` and no `or_die`. The frame is in the cargo
build only because that is a dev build. Turning tail
calls off for LLVM (`disable-tail-calls`) would bring the frame back and cost what the
optimization saves in every build, so the harness proves the explanation instead of the code
changing: for every frame that is in the cargo build's backtrace and not in rustd's, it finds
the function's range in rustd's debug info, disassembles it, and requires a jump out of the
function. A function that is missing and has no such jump is reported as `FRAMES LOST` and
fails the run. Under Cranelift nothing may be missing at all.

Inlining. Both backends compile one function per module (`llvm::emit` creates a module with
the one function), so no rustd function is inlined into another and there are no
`DW_TAG_inlined_subroutine` entries to write. The new `callers` part of the harness checks
what follows from that: a chain of an `#[inline(always)]` function inside an `#[inline]`
one, which rustc inlines even in a dev build and reports as inlined frames, prints the same
three frames from rustd, where they are real frames.

`#[track_caller]`. The same part panics through two nested `#[track_caller]` functions,
through one that reads `Location::caller()` into its message, through a `#[track_caller]`
trait method called on a `dyn` object, through `Option::unwrap` in a closure, and by
indexing out of bounds. The panic location (file, line, column), the message and the frames
are compared with the cargo build.

That part found one difference, which is fixed: an index out of bounds on an array or slice
was reported at the wrong column (`items[at]` at the bracket, and `let item = items[at];` at
the pattern), where rustc reports the start of the index expression. The bracket is right
only for a call of `Index::index` (a `Vec`), which is what the rule was written for. The
bounds check now takes its location from the index expression.

## Profile settings (round two)

The settings come from the build-model's plan (`crates/rustd/src/plan.rs`, which takes them
from cargo's unit graph); `Cargo.toml` is not parsed again and `profile.rs` is not touched.
`Profile::described` turns `debuginfo`, `split_debuginfo` and `strip` into one of three
answers, nothing, lines, or full, or into a refusal:

| setting | what rustd does |
|---|---|
| `debug = false`, `0`, `"none"` | no debug sections |
| `debug = 1`, `"limited"`, `"line-tables-only"`, `"line-directives-only"` | line tables and subprograms, no variables or types |
| `debug = true`, `2`, `"full"` | everything |
| any other `debug` value | refused: "rustd does not know the level" |
| `strip = "debuginfo"` or `"symbols"` (`true`) | no debug sections, whatever `debug` says |
| `split-debuginfo = "off"`, or not set | as above |
| `split-debuginfo = "packed"` or `"unpacked"` with debug info on | refused, see below |
| `split-debuginfo` set with debug info off or stripped | accepted, there is nothing to split |

The refusal reads: "`split-debuginfo = "packed"`: rustd writes what a debugger reads into the
object and the program, and has no `.dwo`, `.dwp` or `.dSYM` to keep it in. Build with
`split-debuginfo = "off"`, or with `debug = false`". The build fails with it when it is
planned. I refused rather than implemented it because split DWARF on ELF needs DWARF 5
skeleton units and a `.dwp` packer (see "What is open"), and silently writing unsplit debug
info into the binary is not what the profile asked for.

`strip = "symbols"` also asks for the symbol table to go. That is the linker's work and is
not done by this code.

When several packages are built with different profiles, the most any of the packages cargo
was asked to build wants is used for all of rustd's code. The harness changes the setting
nine times in one workspace and checks the object after each. `RUSTD_DEBUGINFO=0|lines|1`
overrides it for measuring. A project cargo does not plan still falls back to
`profile.rs`, which only knows on and off.

## What is independent of the object format, and where a new one plugs in

There are three layers.

1. What a function's code says about itself. This is produced by the backends and stored in
   the cache with the code: line rows as (code offset, span index), and `SlotPlaces`, the
   frame base and the location of each named stack slot. The location expressions are DWARF
   expression bytes, because that is what LLVM hands back; for Cranelift they are a frame
   offset. Nothing here knows the object format, and nothing here has a line number in it.

2. The description of a function, `FunctionDebug` in
   `crates/hir-ty/src/mir/codegen/debuginfo.rs`: its name and linkage name, where it is
   declared, the rows with file, line and column looked up, a table of types (`DebugType`:
   base, pointer, array, aggregate with fields, tags and variants), variables with a type
   index and a place, and scopes. `debuginfo/types.rs` builds the type table from rustd's
   types and layouts. This layer is the format-independent part: it is the same for ELF,
   Mach-O and COFF, and it is what a CodeView writer would consume. The one DWARF-shaped
   thing in it is the location expression bytes of a variable that is not at a plain frame
   offset; a CodeView writer handles `VariablePlace::Frame` directly and has to translate, or
   leave out, the `Ranges` case (LLVM-optimized locals).

3. The writer, `DebugContext` in the same file. `add_function` turns a `FunctionDebug` into
   DWARF entries with gimli, and `emit` writes the sections and their relocations into the
   object. This is the only part that is specific, and it is specific in two different ways:
   to DWARF, and to ELF.

A new object format plugs in at `DebugContext::new` and `DebugContext::emit`. `new` returns
`None` for anything but ELF today, which turns debug info off for the object; `emit` adds
the sections by their ELF names with absolute relocations against function symbols and
section symbols.

- Mach-O keeps DWARF, so layers 1 to 3 stay. `emit` has to name the sections
  `__DWARF,__debug_*`, and use the relocation forms Mach-O has (section-relative
  references inside `__DWARF` are written as plain offsets, not relocations). Then the
  linker's debug map and `dsymutil` do the rest; see the Mach-O notes.
- Windows needs a second writer next to `DebugContext` that takes the same `FunctionDebug`
  values and writes `.debug$S` and `.debug$T`; see the Windows notes. `ObjectCodegen` holds an
  `Option<DebugContext>`; that becomes an enum of the two writers, picked from the target's
  binary format in one place.
- The splice path appends debug sections to a linked ELF (`crates/rustd/src/splice`); that
  code is per format already and is separate from the above.

The harness is split the same way. In `harness/debuginfo.py` the comparison of backtraces
(`panics`, `own`, `compare`), the fixtures, the gdb and rust-gdb drivers, the profile checks
and the cost measurement do not look at the file format. What reads ELF or DWARF is in
functions of its own: `verify` and `described` (llvm-dwarfdump), `frames_of` and `unwinding`
(`.eh_frame_hdr`), `sections` (ELF section headers), `unit_of`, and `jumps_away` (dwarfdump
and objdump). Round two only added functions and parts (`callers`, `locals` under LLVM,
`types`, `profile`); the existing ones kept their names and arguments, except that `compare`
takes the binary as an optional last argument and `panics` keeps the message next to the
location.

## The splice path

Decision: append, do not patch.

The sections a debugger reads are not loaded, and each one is a single run of the file found
through the section headers. There is no room to grow one in place, and reserving room at
link time would mean guessing a size and still running out. A splice already starts from the
linker's file every time (it undoes the previous splice and truncates) and the patch object
is cumulative, so nothing has to be patched incrementally either.

So after the splice has written its loaded segments, for every `.debug_*` section the patch
object has, it writes a new copy at the end of the file: the executable's bytes first, then
the patch's, with the patch's relocations applied. A reference into another debug section
becomes the old size of that section plus the addend; a reference to code becomes the address
the splice placed that code at. A new section header table goes after that, with those
sections pointing at their new copies, and `e_shoff` in the ELF header is updated. The patch
is a compilation unit of its own, so the executable's units are untouched. The old code of a
replaced function is still in the file behind its jump and what the executable says about it
stays true.

If the executable has no section of a kind the patch has, or the section is compressed
(`SHF_COMPRESSED`), the splice is refused with that reason and rustd links in full instead.

A function is put in the patch again not only when its code changes but when any of its
lines change, so adding a line at the top of a file re-describes the functions below it. Its
compiled code comes from the cache; only the line lookup is redone.

The code is in `crates/rustd/src/splice/debug.rs`.

## What was measured

All on one machine, Linux x86-64, Rust 1.99.0, LLVM 20, at the head of this branch.

    cargo build -p rustd --release
    LLVM_SYS_201_PREFIX=/usr/lib/llvm-20 cargo build -p rustd --release --features llvm --target-dir target-llvm
    cd harness
    RUSTD_BIN=$PWD/../target/release/rustd python3 debuginfo.py
    RUSTD_BACKEND=llvm RUSTD_OPT=2 RUSTD_EMIT=fast RUSTD_BIN=$PWD/../target-llvm/release/rustd python3 debuginfo.py

`debuginfo.py` exits 0 under both backends ("all right").
`--only corpus|stress|rung2|edits|callers|locals|types|profile|cost` runs one part. The
programs used are the corpus runner, the `stress` workspace (three crates), `rung2`, and
small programs the script writes itself.

What it checks, and what it printed:

Backtraces. Each program is built by cargo and by rustd, each deliberate panic is run with
`RUST_BACKTRACE=1`, and the frames that belong to the workspace's crates are compared by
name, file, line and column. The panic location itself is compared as well, which is the
`#[track_caller]` check: the corpus and the edits program panic through `#[track_caller]`
functions.

| program | Cranelift | LLVM -O2 |
|---|---|---|
| corpus runner | 84 panics, 444 frames, all the same | 84 panics, 443 frames; 1 panic one frame short, the function seen to leave by a jump |
| stress | 1 panic, 3 frames, same | 1 panic, 2 frames; one short, seen to leave by a jump |
| rung2 | 1 panic, 3 frames, same | same |
| callers (round two) | 6 panics, 19 frames, same | 6 panics, 18 frames; one short (`sline::inner`, which ends in `expect`), seen to leave by a jump |

Under Cranelift the comparison is exact. Under LLVM at -O2 a panic passes if rustd's frames
are the cargo build's with some left out and every function left out is one that leaves by a
jump; see "Tail calls" above.

One name is normalized by the script: rustc calls the code that drops a coroutine
`core::ptr::drop_glue::<X>` and rustd has the coroutine's own poll function do it, so the
frame is named `X`. The file, line and column agree. This affects 14 of the 84 corpus panics.

`llvm-dwarfdump --verify`: "No errors." for every binary, including after each splice.

Unwind tables against debug ranges. For every range in a rustd unit the script looks for the
frame the unwinder would find (through `PT_GNU_EH_FRAME` and the search table, not the
section header) and checks that it starts at the same address and does not end after the
range. Corpus: 11647 functions, 0 without a frame, 0 where the frame ends past the range.
Three functions have neither a frame nor lines; they are `#[naked]` functions, which are
assembly only. Stress: 1834 functions, rung2: 3039, edits: 71 to 79 after each splice, all
agree.

gdb, batch mode: break at the file:line of the call into the panicking function, check gdb
stops there, that the pc is in a rustd unit, that `info line` gives that line, then `step`
four times and check it reaches other lines. Passes for corpus, stress, rung2 and for the
binary after the last splice under both backends. `print` of locals is the `locals` part
below.

perf: `perf record -F 5000 --call-graph dwarf` on the corpus runner, then
`perf script -F ip,sym,srcline`. Cranelift: 79 frames of rustd functions in 36 samples, none
without a line, 24 samples unwound back to `main`. LLVM: 46 frames in 22 samples, none without a line, 15 back to `main`. The machine needed
`sudo sysctl kernel.perf_event_paranoid=1`; where perf is not allowed the script says "not
run" and does not fail. No flamegraph tool is installed here, so no SVG was produced; the
`perf script` output is what `stackcollapse-perf` and `inferno` read.

Splice (`edits`): a program is built once under the daemon and edited six times: nothing, a
line added above everything, the panicking function made longer, its caller changed, a new
function added in the call chain, the added line removed. All 6 steps were spliced, and after
each the backtrace matches a fresh cargo build of the same source (7 or 8 frames),
dwarfdump verifies, and the unwind check agrees. Same under both backends.

Variables (`locals`): gdb stops in a function with three parameters and six locals and
prints each (`u32`, `f32`, `&str`, a struct, a tuple struct, an array, a tuple, and a
dereferenced reference). A value passes if it is right or if gdb says `<optimized out>`; a
wrong value or an unknown name fails. Cranelift: 9 of 9 right. LLVM -O2: 6 of 9 right and 3
`<optimized out>`. In the larger programs rustd describes 15,946 parameters and locals across
11,647 functions in the corpus, 3,006 in 1,834 for stress, 3,139 in 3,039 for rung2, the same
count under both backends.

Types (`types`, round two): the same program is built by cargo and by rustd, `rust-gdb` stops
at the same line in both and prints 21 values: a `u32`, a `&str`, a `[u64; 5]` passed by
value, a struct, a tuple struct, a tuple, three variants of an enum with fields (struct-like,
tuple-like, unit), a fieldless enum with explicit discriminants, `Some(42)`, `None`,
`Option<&Point>` both ways (a niche), `Result<u8, String>`, a `Vec<i32>`, a `String`, a slice
of the `Vec`, a union, a dereferenced reference and a sum. What rustd's binary prints has to
be what the cargo build's prints, character for character apart from addresses. Cranelift:
21 of 21. LLVM -O2: 20 of 21 and one `<optimized out>` (the second field of the tuple struct,
which the optimizer splits apart).

Profile (`profile`, round two): the nine settings in the table under "Profile settings", in
one workspace one after the other. All followed or refused as the table says, under both
backends.

lldb and addr2line were tried by hand on the locals program and are not in the script:
`addr2line -f -C -e bin 0x47387` gave `sline::area` and `src/main.rs:5`; `lldb-20 -b` with a
breakpoint on a line stopped, `frame variable` printed parameters and locals, `next` stepped
to the next line.

Cost (`cost`), corpus runner, best of three, compile and link of the `cases` crate object
(11,647 functions), with `RUSTD_DEBUGINFO=0` and without it:

| | Cranelift | LLVM -O2 |
|---|---|---|
| compile + link, no debug info | 5.48 s | 6.60 s |
| compile + link, debug info | 6.22 s (+13.4%) | 7.29 s (+10.4%) |
| object, no debug info | 41,502,552 bytes | 37,201,080 bytes |
| object, debug info | 48,150,544 bytes (+16.0%) | 44,799,664 bytes (+20.4%) |
| of that, sections a debugger reads | 3,249,209 | 3,442,559 |
| of that, their relocations | 3,398,783 | 4,156,025 |
| linked binary, no debug info | 86,944,048 bytes | 83,560,976 bytes |
| linked binary, debug info | 90,193,256 bytes | 87,003,536 bytes |

Round one had Cranelift at +21.9% with line tables and a few variables. With types and all
the variables of round two added it measured 5.41 s against 6.87 s (+27%) before the work
below, and +13.4% after. The object grew more than in round one (+10.7% then) because types
and variables are in it now. The LLVM figure moved between +7.8% and +12.7% over the runs of
this round; the +12.7% was taken while other builds ran on the machine, so read it as
"about a tenth". Timings vary by a few points from run to run.

Where the time went, from `perf record -g` on the daemon during the Cranelift build with
debug info (samples, so shares and not exact times): of about 420 ms of CPU in describing
functions, 180 was in working out types (`Types::describe`, `Types::ty`, `Types::fields`,
`Types::parameters`), 84 in turning a span into file, line and column, 68 in
`DebugContext::add_function`, 36 in copying type tables, and another 70 at the end in gimli
writing the sections and the object writer taking their relocations.

What was changed for it:

- The line starts of a file are a tracked query (`line_starts`), computed once for a file.
  Before, each span counted newlines from the start of the file.
- The types of one environment are described once for the whole object (`ObjectCodegen`
  keeps a `Types` for each `StoredParamEnvAndCrate`), with the name and the normalized form
  of a type remembered. Before, every function described its types from nothing.
- A function is given a copy of only the part of the type table it uses, with the indices
  moved (`add_types`).
- Units of 256 functions (this was done for `addr2line` and perf, which took over a second
  for the first lookup in one unit of 11,647 functions; it does not make the build faster).

What is left of the cost is mostly real work: the type graphs, the line rows of each
function, and about 3.4 MB of relocations for 3.2 MB of sections, which the object writer and
then the linker both go through. The relocations could be cut by describing addresses
relative to one symbol for a unit (DWARF 5 `DW_FORM_addrx` and range lists against a base);
not done.

Regression gate, both backends, on the tree that was committed (the report was edited after):

| | Cranelift | LLVM -O2 |
|---|---|---|
| `difftest.py corpus --bin runner --crate cases --layouts` | MATCH, 0 of 893 layouts differ | MATCH, 0 of 893 |
| `programs.py` | 22 of 22 | 22 of 22 |
| `stress.py` | 99 of 99, 0 restarts | 99 of 99, 0 restarts |
| `breakers.py` | 43 of 43, 5 open | 43 of 43, 5 open |
| `unwinds.py` | 8 of 8 | 8 of 8 |
| `splices.py` | 71 of 71 | 71 of 71 |
| `asmedits.py` | 10 of 10 | 10 of 10 |
| `includes.py` | 9 of 9 | 9 of 9 |
| `worktrees.py` | 58 of 58 | 58 of 58 |
| `keys.py` | 36 of 36, 0 open | 36 of 36, 0 open |
| `debuginfo.py` | all right | all right |
| `storekills.py --kills 100` | PASS | PASS |
| `storecorrupts.py --rounds 4` | PASS | PASS |
| `storefull.py` | PASS | PASS |
| `storeshared.py` | PASS, 180 builds | PASS, 180 builds |

`buildmodel.py` runs both backends itself, with the binary built with `--features llvm`:
180 of 180 rows as expected, 0 WRONG. That is 176 in the full run plus the four
`cross_musl` and `cross_aarch64` rows run again on their own, because the 1.99.0 toolchain
on this machine did not have those two targets installed during the full run and cargo
itself could not build the cases.

`cargo fmt --all -- --check` is clean. `cargo test --release -p hir-ty -p hir-def -p parser
-p syntax -p rustd`: 930 passed, 0 failed, 1 ignored. `cargo clippy -p hir-ty -p rustd` has
no warning in the files this branch added.

## The gdb rows on the GitHub runner

In CI (run 39034233840) every `gdb at <file>:<line>` row was `NOT STOPPED`, with gdb's
`[Inferior 1 (process N) killed]` shown as the reason, under both backends. The debug info
was right and the harness was wrong.

The runner is `ubuntu-24.04` with gdb 15.1; this machine is Ubuntu 22.04 with gdb 12.1. I
ran the corpus row in an `ubuntu:24.04` container with its gdb. gdb set the breakpoint,
stopped at it and stepped, and printed the stop as

    Breakpoint 1.1, ... at cases/src/lib.rs:1962

where gdb 12 prints `Breakpoint 1, ...`. gdb from version 13 on numbers the location when a
breakpoint has more than one, and a line of a generic function or one with code in two
places has more than one. The script looked for `Breakpoint 1, `, did not find it, and
reported the last line gdb printed, which is the inferior being killed when the batch ends.
It now accepts `Breakpoint 1.N, ` as well. The locals and types rows already passed in CI
because they read values and not the stop line.

The other suspects, as seen in the container: `personality` is refused there
(`warning: Error disabling address space randomization: Operation not permitted`) and gdb
goes on with ASLR left on, which does not matter for a breakpoint by file and line; there
was no debuginfod prompt in batch mode; the breakpoint was not pending; ptrace worked. gdb
also warns that the `gdb_load_rust_pretty_printers.py` script named by `.debug_gdb_scripts`
is not on its path, as it does for any Rust binary run under plain `gdb`.

With that change pushed (commit 22e1ec56d6, run 39044537945) both harness jobs pass on the
runner and every gdb row is `stopped`: corpus `lib.rs:1962`, stress, rung2 `main.rs:134` and
the binary after the last splice, with locals 9 of 9 and types 21 of 21 under Cranelift and
6 of 9 with 3 optimized out and 20 of 21 with 1 optimized out under LLVM, the same as here.
No row is skipped on CI. The `Coverage` workflow fails on this branch as it does on
`rustd-collate`; I did not look into it.

## What is open

Inline frames. There are no `DW_TAG_inlined_subroutine` entries because nothing is inlined;
see "Tail calls, inlining and `#[track_caller]`". If rustd starts inlining (LLVM across
functions, or MIR inlining) the entries have to be added at the same time, or backtraces
will lose frames.

Scopes. Variables have no `DW_TAG_lexical_block`, so a local shows in the debugger from the
start of the function, with garbage in it until it is assigned. Because of that, a name that
the body binds more than once is not described at all (the debugger could not tell which one
is meant). Locals that Cranelift keeps in a register, locals of closures and of `async`
bodies, and unsized locals are not described either. In the corpus this leaves 15,946
described; I did not count how many rustc describes for the same code. The writer and the
data model have lexical blocks (`Scope`, `DW_TAG_lexical_block`); what is missing is the
lowering recording which code belongs to which block.

Closures, coroutines and `dyn` pointers are described by name and size only. A closure's
captures have a layout rustd knows, so that is the next type to add.

A project that cargo does not plan gets its setting from `profile.rs`, which reads `debug` as
on or off and does not know `strip` or `split-debuginfo`.

After a splice, the `.eh_frame` section header still covers only the linker's frames. The
frames of spliced code are in the appended segment and are in the `.eh_frame_hdr` search
table, which is what the unwinder and perf use, and backtraces through spliced code are
right. A tool that reads `.eh_frame` by section name (`llvm-dwarfdump --eh-frame`) does not
see them. Spliced functions also have no `.symtab` entry; gdb names them from DWARF.

A breakpoint set by function name can land on the old copy of a replaced function, which is
still described by the executable's unit. Breaking by file:line finds the new code.

An edit that changes only a type (say a field's name) without changing the code of a function
that has a local of that type does not re-describe that function in the patch, so the
debugger shows the old field name until the next full link.

The coroutine drop frame name (`X` where rustc says `core::ptr::drop_glue::<X>`), described
above. Fixing it means emitting a real drop-glue function for coroutines instead of polling
with a null context.

Split debug info is refused, not done. On ELF it needs DWARF 5 (or the GNU extension to 4):
a skeleton unit in the object with `DW_AT_dwo_name`, the real unit in a `.dwo` with
`.debug_*.dwo` sections and an address table so the `.dwo` has no relocations, and for
`packed` a `.dwp` made from the `.dwo` files after the link. I have not checked how much of
that gimli's writer does. The splice path would have to keep the `.dwo` of patched code next to the binary as
well. A week, most of it the splice path and the harness. Compressed debug sections in the
executable make the splice fall back to a full link.

Under LLVM a variable the optimizer keeps in registers for only part of a function is
described for those ranges only, and one it splits into pieces (SROA) is `<optimized out>`
entirely. My reading, not measured, is that LLVM can only describe the pieces
(`DW_OP_piece`) of a variable whose type it knows the fields of, and rustd gives it a made-up
integer of the slot's size. Telling LLVM the real types through DIBuilder would then fix it,
and would mean two writers of types. Not started.

gdb prints a warning about `.debug_gdb_scripts` for these binaries; it comes from rustc's
std objects and is the same for the cargo build.

## Mach-O (design only)

On macOS the linker does not copy DWARF into the executable. It writes a debug map (`N_OSO`
stabs naming each object file, `N_FUN` for functions) and `dsymutil` later gathers the DWARF
from the objects into a `.dSYM` bundle; lldb can also read the objects directly through the
debug map.

For a full link this is mostly free: the gimli writer already produces the sections, and
they need to go into `__DWARF,__debug_*` with Mach-O relocations (`object` supports this; the
relocation kinds for section-relative references differ, `X86_64_RELOC_UNSIGNED` /
`ARM64_RELOC_UNSIGNED` and subtractor pairs). The backtrace in a Rust binary on macOS reads
the dSYM or the objects through the debug map, so rustd's object has to stay on disk where
`N_OSO` says or `dsymutil` has to be run after the link.

For a splice the debug map is the natural place: add an `N_OSO` entry for the patch object
plus `N_FUN` entries at the spliced addresses, and leave the DWARF in the patch object.
The Mach-O splice would have to rewrite the symbol table in `__LINKEDIT` for this; I have
not checked how much of that it already does. A stale `.dSYM` next
to the binary would win over the debug map, so the splice has to remove or regenerate it.

Estimate: one week for full-link objects with backtrace and lldb checks on aarch64 macOS, one
more for the splice and for a macOS variant of `debuginfo.py` (lldb in place of gdb,
`dwarfdump --verify`, `atos`). I have not run any of this.

## Windows (design only)

MSVC targets use CodeView: `.debug$S` (symbols and line tables per function) and `.debug$T`
(types) in the COFF object, which `link.exe` or `lld-link` merge into a PDB.

LLVM can write CodeView itself (module flag `CodeView`, the same DIBuilder metadata), so the
LLVM backend is the short path, but today rustd reads LLVM's line table back and writes the
sections itself, and that part would need a CodeView reader or a mode where LLVM's sections
are kept as they are. Cranelift has no CodeView writer, and as far as I know rustc's Cranelift
backend does not have one either. Writing `.debug$S` line tables and `S_GPROC32` records by hand is
well specified (the `C13` line format) and small for lines and names; types (`.debug$T`,
TPI/IPI hashing) are a lot more work.

The splice is the hard part. A PDB is a multi-stream file that is not designed to be
appended to; the realistic options are to rewrite the PDB after each splice (the `pdb` crate
only reads; LLVM's PDB writer is C++ and not exposed through the C API), or to emit
`*-windows-gnu`-style DWARF in the PE for spliced code only, which WinDbg and Visual Studio
do not read. Windows backtraces in Rust go through `dbghelp` and need the PDB.

Estimate: two weeks for line tables and procedure symbols in full links under both backends
with a `cdb`-driven harness, two to four more for the splice depending on which PDB route is
taken, and types on top of that. This is the least certain number here.
