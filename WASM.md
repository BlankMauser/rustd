# rustd in the browser

This is the report for the `rustd-wasm` spike: can rustd's compiler run in a web page,
compile Rust there, and swap edited functions into a running program?

Short answer: yes, for a small `#![no_std]` program. The page in `crates/rustd-wasm/web`
loads rustd built as WebAssembly, compiles a Rust program (and the parts of `core` it uses,
from source) to WebAssembly, runs it against a canvas, and on each edit compiles only the
functions that changed and swaps them in while the program keeps its memory.

Everything below is marked as measured, read from code, or inferred. Timings are from one
Linux VM; where a number comes from a single run it says so.

## How to run it

    rustup component add rust-src
    crates/rustd-wasm/web/build.sh
    python3 -m http.server -d crates/rustd-wasm/web 8377

Then open `http://localhost:8377/` in Chrome. Code is on the left, the canvas on the
right, and the line at the bottom shows what the last build did. `node
crates/rustd-wasm/web/check.mjs` runs the same build, edit, edit sequence without a
browser and asserts on what the program draws.

The browser needs the memory64 proposal, since both the compiler and the code it emits are
wasm64 (see "Why wasm64"). I ran it in the Chrome on this VM and in Node 24. I did not try
Firefox or Safari.

## What was built

- `crates/hir-ty/src/mir/codegen/wasm.rs` (about 2,200 lines, behind the `wasm` feature of
  `hir-ty`): lowers one function of Cranelift IR to a WebAssembly function body.
- `crates/rustd-wasm`: a `Compiler` that holds a salsa database over an in-memory file set,
  a linker that turns the code generator's objects into a WebAssembly module, a small C ABI
  for the page, and a CLI that does the same natively.
- `crates/rustd-wasm/web`: the page, a worker that runs the compiler, and the runtime that
  holds the program's memory and function table.
- Small portability fixes in `stdx`, `paths` and `ide-db`, listed under "Blockers".

`rustd-wasm` is a workspace of its own, outside the main one. The `wasm` feature changes
what `codegen.rs` emits, and inside the main workspace cargo would unify that feature into
native `rustd`. With the feature off, `codegen.rs` compiles as before apart from
`std::time::Instant` becoming `stdx::time::Instant`, which is a re-export on native.

## 1. Does the front end run on wasm?

Yes.

Measured: `cargo check --target wasm32-unknown-unknown` passes for `hir-ty` and `ide`
without changes, including Cranelift as a dependency. The whole stack used here (`base-db`,
`hir-def`, `hir-ty`, `hir`, `ide-db`, MIR lowering, monomorphization, `codegen.rs`) builds
for `wasm64-unknown-unknown` and runs in Node and in Chrome. An earlier `wasm32-wasip1`
build also ran a small program in Node before I moved to wasm64; that path is not in the
branch. It name-resolves and type-checks `core` and the demo, lowers them to MIR and
generates code.

### Blockers hit, and what was done

- **Threads.** rust-analyzer drops syntax trees on a background thread, and spawning it
  fails on wasm. The repository already has a `no_salsa_async_drops` cfg for this;
  `.cargo/config.toml` now sets it for `wasm64-unknown-unknown`. Nothing else in the path
  needed a thread. Salsa itself ran fine on one thread. `codegen.rs` asks its
  `SymbolResolver` to run work in parallel; the default implementation does nothing, so
  codegen is serial here.
- **Clock.** `std::time::Instant::now()` panics on `wasm*-unknown-unknown`. `codegen.rs`
  and `ide-db` time their phases with it. Added `stdx::time::Instant`, which is the std one
  natively and reads an imported `rustd.now` on wasm.
- **`std::process`.** `stdx::process` already had a wasm stub, but gated on
  `target_arch = "wasm32"`. Changed to `target_family = "wasm"`.
- **Paths.** `AbsPathBuf::assert("/")` panics on `wasm*-unknown-unknown` because
  `Path::is_absolute` is false for everything there. `paths` now treats a path with a root
  as absolute on that target.
- **Filesystem and VFS.** Not a blocker. The `vfs` crate's `FileSet` and `VfsPath` take
  virtual paths; `vfs-notify` and the loader are not used. Files are set with
  `Compiler::set_file`.
- **Cargo metadata and project loading.** Bypassed, not solved. `project-model` is not
  compiled in; `rustd-wasm` builds a two-crate graph by hand (the program and `core`) with
  cfgs and a data layout for wasm64. A real project would need its crate graph computed
  somewhere else (a server, or ahead of time) and shipped as data.
- **Sysroot.** The sources of `core` (plus the `core_arch` and `core_simd` files it
  includes) are packed into one 6.7 MiB file by `rustd-wasm bundle` and loaded into the
  in-memory file set. `alloc` and `std` are not loaded.
- **Proc macros.** None are loaded; `core` needs none since its derives are built in. The
  proc-macro server loads native dylibs, which cannot work in wasm. The options I see are
  proc macros compiled to wasm and run as separate modules (this needs a wasm proc-macro
  ABI and a build of each macro crate, done outside the browser), or expanding ahead of
  time on a server. Neither was tried.
- **Build scripts.** Not run and cannot be. Their outputs would have to be shipped.
- **Memory.** Measured: the compiler's linear memory is 252 MiB after building the demo.
  Inferred: that is mostly `core`. Inferred: wasm32 caps a page at 4 GB, so a program that
  takes 7 GB natively (the dx-sized daemon) does not fit in wasm32. wasm64 lifts the
  address limit, but browsers still cap memory64 memories; I did not measure where Chrome
  stops. A realistic size for now is a small app plus `core`/`alloc` and a few
  dependencies, not a large workspace.

### Why wasm64

Two things in rustd assume the target's pointer is as wide as the compiler's own:
`codegen.rs` builds IR with `I64` pointers throughout, and the MIR interpreter, which
evaluates constants, refuses a target whose pointer size differs from the host's
(`mir/eval.rs`: "targets with different pointer size from host"). Compiling for wasm64
with a wasm64 build of the compiler satisfies both without touching either. The cost is
that memory64 is required and wasm64 is a tier 3 Rust target (`-Zbuild-std`).

Getting to wasm32 means parameterising `codegen.rs` on pointer width and letting the
interpreter run a 32-bit target on any host. That is mechanical but wide.

### Prior art

`rust-analyzer/rust-analyzer-wasm` ran the IDE half of rust-analyzer in a page, with a
packed copy of the standard library sources (its `rust-pack` directory). Checked through
the GitHub API: the repository is archived and its last push was 2022-06-14, so it is four
years stale. I did not build or read it beyond that. It is evidence that the analysis stack has run in a browser before, and nothing
more; it did no code generation. I did not reuse anything from it.

## 2. What does codegen produce in a browser?

Read from the Cranelift 0.136 sources that `hir-ty` depends on: `cranelift-codegen/src/isa`
has `x64`, `aarch64`, `riscv64`, `s390x`, `pulley32` and `pulley64`. There is no
WebAssembly backend.

### Options

**Wasm lowered from Cranelift IR (what this branch does).** `codegen.rs` builds Cranelift
IR as it does natively, and `wasm.rs` writes a wasm function from that IR instead of
calling a native ISA.

- Carried over: all of `codegen.rs` (9,200 lines), with one `emit` function swapped by
  cfg. Monomorphization, vtables, statics, drop glue, the object writer and the patcher
  that decides what an edit touched are used as they are.
- Speed: not measured against anything. Inferred: fair but not good. SSA values become
  wasm locals, stack slots live in linear memory, control flow is nested blocks with a
  dispatch loop for backward jumps, and every call is a `call_indirect`. The browser's
  optimising tier recovers some of it.
- Hot reload: every function has a slot in one `WebAssembly.Table`, every call goes
  through the table, and a function's address is its slot. A patch is a small module that
  imports the memory, the table and the stack pointer, and whose element segment writes
  the new functions into their slots. Statics keep their addresses and contents.
- ABI: Cranelift signatures map to wasm signatures directly; `i128` is two `i64`. Calls to
  functions nothing defines become imports from the page, which is how the demo draws.
- Not done: unwinding (a panic that would unwind aborts), SIMD types, and whatever opcodes
  the demo and its slice of `core` never reached.

**Wasm emitted straight from MIR.** A new backend beside `codegen.rs`. It would produce
better wasm (structured control flow from MIR, direct calls where a function cannot
change) but shares none of `codegen.rs`: layouts, ABI, intrinsics, vtables, drop glue and
the patcher's bookkeeping would all be written again. Hot reload and ABI as above.
Inferred: months, against the two thousand lines the IR route took.

**Pulley.** Cranelift can emit Pulley bytecode, and that carries `codegen.rs` over
completely. But the page then needs a Pulley interpreter, and what runs is interpreted
bytecode inside wasm. Not tried. Inferred: several times slower than emitted wasm, and
calling into the page (DOM, canvas) needs a trampoline per signature. Hot reload is easy
because code is data. I see no reason to pick it over emitted wasm now that emitted wasm
works.

**The MIR interpreter.** Nearly free and already runs in wasm; it evaluates the demo's
constants here. It is slow, it only runs a target with the host's pointer width, and it
has no path to calling host functions. Useful as a fallback for a function the code
generator rejects, not as the engine.

**Native code from Cranelift inside wasm.** Useless in a browser. Cranelift will produce
x86-64 bytes there, and nothing can execute them.

### Recommendation

Keep lowering Cranelift IR to wasm. It reuses everything, it works today, and the two
obvious speed problems (indirect calls everywhere, locals for every value) can be fixed
inside `wasm.rs` and the linker later. Revisit a MIR backend only if the generated code
turns out too slow for real UI work, which nothing here measured.

## 3. Where do std and dependencies come from?

For this demo, `core` is compiled from source by rustd in the page. Measured: the first
module has 271 functions: 8 from the demo, 59 non-generic functions of `core`, and 204
instances of generic ones. One function of `core` that the demo reaches was not compiled
(`core::fmt::num::imp::_fmt_inner`, integer formatting, reached only through panic
messages); it becomes an import that throws if called. I did not look into why.

That is a narrow slice. Formatting, `alloc`, collections and `std` were not attempted in
wasm, so I cannot say how much of them rustd's codegen handles on this target. Natively
rustd leans on rustc's copies for what it cannot compile; in the browser there is no such
fallback, so a function that fails to compile is simply missing.

The other route is to ship `core`, `alloc`, `std` and dependencies as wasm built by rustc
ahead of time, and link the program against them in the page. Inferred, not tried: the
linker here is about 500 lines because it only handles objects that rustd wrote; linking
rustc's wasm objects means reading the wasm object format and its relocations, and agreeing
on an ABI. rustc targets wasm32 by default while this spike is wasm64, and rustd's "every
call through the table" convention has to meet rustc's direct calls. A wasm build of
`wasm-ld` would do the format work but not the hot-swap part.

For a first real demo I would stay with source: add `alloc` (with a bump or dlmalloc
allocator written in Rust) and compile it the same way as `core`, and find out what breaks.
Prebuilt objects become worth it when first-build time on a large dependency tree hurts.

## 4. The prototype

All four steps work:

a. the analysis stack as wasm, in a page, type-checking a single-file program from memory;
b. MIR for that program, produced in wasm;
c. its functions run in the page as emitted wasm;
d. an edit recompiles only what changed and swaps it in without a reload.

The demo (`crates/rustd-wasm/web/demo.rs`) is `#![no_std]`: bouncing balls drawn through
four functions the page provides (`fill`, `rect`, `circle`, `text`). It uses a trait, an
`impl`, slices, iterators, `f64::clamp`, tuples, `match`, overflow checks and a
`static mut`. It is not web-sys and not a UI framework; both need `alloc`, `std` and proc
macros.

`check.mjs` asserts the behaviour that matters: after changing a constant only the
functions using it are compiled again, after changing a string only `frame` is, and a ball
dropped before the edits is still there afterwards.

### Measurements

Compiler and sources (measured, sizes in bytes):

| | raw | gzip -9 | brotli q9 |
|-|-|-|-|
| `rustd_wasm.wasm` | 23,566,999 | 4,543,855 | 3,403,701 |
| `core.bundle` | 7,003,958 | 1,354,134 | 1,053,260 |

The compiler is a plain `--release` build. No `wasm-opt`, no `opt-level = "z"`, no LTO.

Load and first build in Chrome, served from localhost (three page loads):

| | |
|-|-|
| fetch compiler and sources | 77 to 106 ms |
| instantiate compiler, load sources | 77 to 95 ms |
| first build, total | 4.5, 5.1 and 7.2 s |
| of which analysis (listing the functions, which runs name resolution over `core`) | 2.9 to 4.9 s |
| of which codegen (type inference, MIR, monomorphization, emitting 271 functions) | 1.5 to 2.3 s |
| of which link | 5 to 12 ms |
| first module | 143,980 bytes |
| compiler memory after the first build | 252 MiB |

Edits in Chrome after the first build. "Edit to running" is from the page handing the
source to the worker until the new functions are in the table; it leaves out the 60 ms the
page waits for typing to pause. Each row is one edit; the ranges are over the two or three
times I made that edit in different page loads:

| edit | functions swapped | module | codegen | edit to running |
|-|-|-|-|-|
| change `RADIUS` | 2 | 2,922 B | 11 to 49 ms | 16 to 204 ms |
| change the label string | 1 | 2,399 B | 20 ms | 24 ms (single run) |
| change `GRAVITY` | 1 | 2,307 B | 9.6 ms | 17 ms (single run) |
| change `RADIUS` and the label together | 3 | 5,233 B | 64 ms | 227 ms (single run) |

Analysis on an edit took 1 to 7 ms and link 1 to 3 ms in every run. The slow end of each
range is the first edit after the first build.

In Node 24, five runs of `check.mjs`: first build 6.3 to 8.3 s, the `RADIUS` edit 72 to
223 ms, the label edit 11 to 31 ms.

Per-function compile time, inferred from the above: 6 to 8 ms per function averaged over
the first build, most of it front-end work on `core` that is then cached; 10 to 30 ms per
function on a warm edit, where the cost is re-checking what the edit could have touched
rather than emitting code.

The first build varies between runs by more than I can explain (4.5 to 7.2 s in Chrome on
an otherwise idle machine, 11 s once while the machine was compiling). Treat it as rough.

## What does not work

- Anything outside `core`: no `alloc`, no `std`, no dependencies, no proc macros.
- Compile errors are not reported as diagnostics. Measured: with a type or syntax error
  the page lists the functions that could not be built with a reason from MIR lowering or
  codegen ("not compiled: frame: frontend: mir: HasErrors"), the old code keeps running,
  and fixing the error recovers. Such a build is not a patch: it emitted 50 to 220
  functions again, because a full build renumbers the generic instances.
- The demo has no `#[panic_handler]`; `panic_impl` is an import that throws.
- Panics abort; there is no unwinding. A trap stops the program until the next build.
- Functions declared inside a function body are not listed as roots.
- A changed `static` gets a fresh address and its initial value; the old one is leaked.
  Table slots and data are never reclaimed.
- Layout changes to live data are not handled. Changing the fields of `World` while balls
  exist reinterprets old bytes.
- wasm32 targets and browsers without memory64.
- `rustd-wasm` has no Rust unit tests; `check.mjs` is the only test, and it needs the
  wasm64 build. With the `wasm` feature on, `hir-ty` has dead-code warnings for the native
  emit path.

## From here to a real hot-reloading compiler in the browser

Sizes are rough guesses at effort, not measurements.

1. **`alloc` and a first slice of `std` from source** (1 to 2 weeks to learn what breaks,
   open-ended after). Needs an allocator and a wasm `std::sys` layer or a stub of one. This
   decides whether "compile everything from source" holds.
2. **Diagnostics in the page** (days). rust-analyzer already computes them; they need
   plumbing through the ABI and an editor that can show them.
3. **Proc macros** (weeks). A wasm proc-macro ABI, a host for it in the compiler, and
   prebuilt wasm for each macro crate. Any real UI framework needs this.
4. **A crate graph for real projects** (1 to 2 weeks). Cargo metadata, cfgs, features and
   build-script output computed outside the browser and shipped with the sources.
5. **wasm32 target** (2 to 4 weeks). Pointer width as a parameter in `codegen.rs`, and the
   interpreter running a target narrower than its host. Removes the memory64 requirement
   and makes rustc-built objects linkable.
6. **Linking rustc-built wasm** (2 to 4 weeks, after 5). Needed for dependencies too big to
   compile in the page and for `wasm-bindgen`-style glue.
7. **Code quality** (1 to 2 weeks for the first round). Direct calls to functions that
   have not changed, structured control flow, fewer locals.
8. **Unwinding** (1 to 2 weeks). The wasm exception-handling proposal maps onto
   `try_call`'s exceptional edges.
9. **Startup** (days to weeks). 3.4 MB compressed for the compiler is fine. The 3 s of
   analysis over `core` on every load is not; it needs a persisted database or a prebuilt
   snapshot of `core`'s analysis.
10. **Memory** (unknown). 250 MiB for `core` alone suggests `std` plus a framework will be
    over a gigabyte. Needs measuring before anything else is planned around it.
11. **Reclaiming slots and data, and state migration on layout changes** (1 to 2 weeks for
    the first, open-ended for the second).
