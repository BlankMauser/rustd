# LLVM as the code producer inside the browser build

Branch `rustd-llvm-wasm`, cut from `rustd-wasm`. This is a report of an experiment: can LLVM
(WebAssembly target only) run inside the compiler that the page loads, and emit the wasm for
the program in the editor, in place of the hand-written Cranelift-IR-to-wasm lowering
(`wasm.rs`)? And what does it cost?

## Verdict

Practical, with these costs.

It works. LLVM 20.1.8 built for `wasm32-wasip1` links into `rustd-wasm`, the compiler loads in
Chrome, and all five canvas demos (mandelbrot, balls, fountain, life, sort) compile through
LLVM and run, at O0 and at "O1 passes + fast selection", with no function falling back to the
hand-written lowering.

What it costs, at O1 passes + fast selection, against the hand-written lowering (Chrome,
medians of 5 runs, details below):

- Download: the compiler goes from 25.7 MB to 64.8 MB raw, 3.8 MB to 12.4 MB with brotli
  (11.1 MB if LLVM is built MinSizeRel).
- Start: `WebAssembly.compile` goes from 28 ms to 77 ms, instantiate + load of the core
  bundle from 57 ms to 121 ms.
- First build of a demo: 12% to 73% longer (balls 3.4 s to 5.4 s). LLVM itself takes about
  2.0 ms per function, 1.15 ms at O0.
- A rebuild after a one-line edit: codegen 12 ms to 16-18 ms, which nobody will notice.
- Memory: the compiler's linear memory grows by 6 to 9 MiB (208 to 214 MiB on balls).
- The compiler has to be built for `wasm32-wasip1`, not `wasm32-unknown-unknown`, with a
  75-line WASI shim in the page, and LLVM needs a 428-line patch to its Support library.

What it buys: the emitted module is 53% to 59% smaller and the demos' frames run 1.75x to
4.75x faster.

What was not shown: anything beyond the five demos. See "What is not done".

## What was built

### 1. LLVM 20 for wasm32

`crates/rustd-wasm/llvm/build-llvm.sh [Release|MinSizeRel]` reproduces it. It downloads
wasi-sdk and the LLVM sources into `~/llvm-wasm` (or `$LLVM_WASM_DIR`), applies
`patches/0001-support-wasi.patch`, configures and builds the static libraries. Nothing it makes
is committed.

The flavour that works is `wasm32-wasip1`, with wasi-sdk's libc and libc++ (the `noeh` libc++,
no exceptions). `wasm32-unknown-unknown` has no libc, and LLVM cannot be built without one.
Emscripten was not tried: wasi-sdk worked, and its output is a plain wasm module with a small
import list, which is closer to what the page already loads.

Versions:

| | |
|---|---|
| LLVM | 20.1.8 (sources from the llvm-project release tarball) |
| wasi-sdk | 34.0 (`clang version 23.1.0-wasi-sdk`) |
| native `llvm-tblgen` | 20.1.8, from `llvm-20-dev` (`/usr/lib/llvm-20/bin`) |
| cmake / ninja | 3.22.1 / 1.10.1 |
| Rust | 1.99.0, targets `wasm32-wasip1` and `wasm32-unknown-unknown`, `rust-src` |

The cmake flags, as in the script (`$emulated` is
`-D_WASI_EMULATED_SIGNAL -D_WASI_EMULATED_PROCESS_CLOCKS -D_WASI_EMULATED_MMAN -D_WASI_EMULATED_GETPID`):

```sh
cmake -S "$source/llvm" -B "$build" -G Ninja \
    -DCMAKE_TOOLCHAIN_FILE="$wasi_sdk/share/cmake/wasi-sdk-p1.cmake" \
    -DCMAKE_BUILD_TYPE="$build_type" \
    -DCMAKE_C_FLAGS="$emulated" -DCMAKE_CXX_FLAGS="$emulated" \
    -DUNIX=ON \
    -DLLVM_TARGETS_TO_BUILD=WebAssembly \
    -DLLVM_DEFAULT_TARGET_TRIPLE=wasm32-unknown-unknown \
    -DLLVM_HOST_TRIPLE=wasm32-wasip1 \
    -DLLVM_TABLEGEN="$native_tools/llvm-tblgen" \
    -DLLVM_NATIVE_TOOL_DIR="$native_tools" \
    -DLLVM_ENABLE_THREADS=OFF \
    -DLLVM_ENABLE_ZLIB=OFF -DLLVM_ENABLE_ZSTD=OFF -DLLVM_ENABLE_TERMINFO=OFF \
    -DLLVM_ENABLE_LIBXML2=OFF -DLLVM_ENABLE_LIBEDIT=OFF -DLLVM_ENABLE_LIBPFM=OFF \
    -DLLVM_ENABLE_FFI=OFF \
    -DLLVM_ENABLE_PIC=OFF -DLLVM_ENABLE_PLUGINS=OFF -DLLVM_BUILD_LLVM_DYLIB=OFF \
    -DBUILD_SHARED_LIBS=OFF \
    -DLLVM_ENABLE_BACKTRACES=OFF -DLLVM_ENABLE_CRASH_OVERRIDES=OFF \
    -DLLVM_ENABLE_UNWIND_TABLES=OFF -DLLVM_ENABLE_ASSERTIONS=OFF \
    -DLLVM_INCLUDE_TOOLS=OFF -DLLVM_BUILD_TOOLS=OFF \
    -DLLVM_INCLUDE_UTILS=OFF -DLLVM_BUILD_UTILS=OFF \
    -DLLVM_INCLUDE_TESTS=OFF -DLLVM_BUILD_TESTS=OFF \
    -DLLVM_INCLUDE_EXAMPLES=OFF -DLLVM_BUILD_BENCHMARKS=OFF \
    -DLLVM_INCLUDE_DOCS=OFF -DLLVM_INCLUDE_RUNTIMES=OFF
ninja -C "$build" $(ninja -C "$build" -t targets all | sed -n 's/^\(lib\/libLLVM[^:]*\.a\):.*/\1/p')
```

What did not work on the way, and what was done about it:

- LLVM's Support library does not compile for WASI as released. It wants
  `machine/endian.h`, `sys/wait.h`, `pwd.h`, sockets, `getsid`, `alarm`, `fchown`, `umask`,
  and crash recovery built on `setjmp`. The patch (12 files, 428 lines) puts `__wasi__` guards
  and stubs around those. This patch is the part that has to be carried across LLVM versions.
- A plain `ninja` stops at linking `llvm-tblgen` for wasm (`mmap`, `munmap`, `posix_madvise`
  unresolved). Nothing needs that executable, so the script builds the `libLLVM*.a` targets
  only: 97 archives, 208 MB on disk for Release.

### 2. Linking it into `rustd-wasm`

`crates/rustd-wasm/web/build-llvm.sh [Release|MinSizeRel] [out-dir]` builds the compiler with
`--features llvm --target wasm32-wasip1` into `web/pkg-llvm`. The ordinary `web/build.sh` is
untouched and its output is byte-for-byte the same size as before (25,713,529).

The browser crate's target had to change for this build, from `wasm32-unknown-unknown` to
`wasm32-wasip1`. LLVM's objects are built against wasi-libc and call into it; a
`wasm32-unknown-unknown` Rust library has no libc to give them and a different idea of
`malloc`. Building the whole compiler for `wasm32-wasip1` makes Rust's std and LLVM share one
libc and one allocator, and nothing has to be bridged by hand.

The problems, in the order they came up:

- `llvm-sys`'s build script runs a native `llvm-config`. `crates/rustd-wasm/llvm/bin/llvm-config`
  is a 33-line shell script that answers its questions for the wasm build tree.
  `LLVM_SYS_201_PREFIX` points at it.
- `llvm-sys` wants `libffi` and `libstdc++`. inkwell's `no-libffi-linking` feature and
  `LLVM_SYS_LIBCPP=c++` deal with those; libc++ and libc++abi come from wasi-sdk
  (`-l static=c++ -l static=c++abi`, the `noeh` directory first).
- C++ exceptions and RTTI: none are needed. LLVM is built without exceptions and wasi-sdk's
  `noeh` libc++ matches.
- The WASI emulation archives (`wasi-emulated-mman`, `-signal`, `-process-clocks`, `-getpid`)
  and `libdl` have to be linked.
- Static constructors: the module is linked with wasi-sdk's `crt1-reactor.o` and the page
  calls `_initialize` once. Without it the first call into LLVM traps with "null function or
  function signature mismatch". (`-mexec-model=reactor` is a clang flag; `rust-lld` rejects it.)
- `--cfg no_salsa_async_drops`, which `.cargo/config.toml` gives the other wasm target, has to
  be passed for this one too.

What the page must supply: the module imports 24 functions of `wasi_snapshot_preview1`
(environment, clock, random, the `fd_*` and `path_*` file calls, `proc_exit`, `sched_yield`).
`web/wasi.js` (75 lines) implements the environment, the clock, random bytes and `fd_write` to
the console, and answers everything else with `ENOSYS`. No file is ever opened. The environment
is how the page passes `RUSTD_BACKEND` and `RUSTD_OPT`.

### 3. LLVM emitting the program's wasm

Feature `llvm` of `rustd-wasm` turns on `hir-ty/llvm-wasm` (`wasm` + `llvm` +
`inkwell/target-webassembly`). The native `llvm` feature and its x86-64 path are unchanged;
everything new is under `cfg(feature = "wasm")`. The hand-written lowering stays in the build
and is the default; `RUSTD_BACKEND=llvm` selects LLVM, and a function LLVM cannot do is
compiled by the hand-written lowering, as Cranelift takes over natively.

- `llvm.rs`: target `wasm32-unknown-unknown`, cpu `mvp` with
  `+bulk-memory,+mutable-globals,+sign-ext,+nontrapping-fptoint,+multivalue`, ABI
  `experimental-mv` so that a 128-bit integer is returned as two results, as the hand-written
  lowering has it.
- `RUSTD_OPT=1-fast` is new (`Optimization::Quick`): the `default<O1>` pipeline, and the
  target machine at `OptimizationLevel::None`. It exists natively too.
- `llvm/emitted_wasm.rs` (427 lines) reads the wasm object with `wasmparser` (already in the
  lock file through other crates; `object` reads wasm too but does not give relocations in
  a form that was less work). It takes the one function body, turns direct calls into the
  table calls the page's linker expects, turns relocations into rustd's, and data segments
  into constants. One trap: the buffer inkwell hands back has a trailing NUL that is not part
  of the object.
- `llvm/lower.rs`: no x86 attributes, memory routines become `memcpy`/`memmove`/`memset`
  intrinsics, calls that can unwind are plain calls (nothing unwinds in the wasm build).
  128-bit multiplies, 64-bit multiplies with overflow, and 128-bit shifts by an unknown
  amount are expanded into 64-bit operations in the IR, because LLVM would otherwise call
  `__multi3` / `__lshrti3` from compiler-rt and the page has no such library. Before that
  expansion 6 or 7 functions of balls and fountain fell back to the hand-written lowering.

`web/llvm.html` is a page that builds one demo with one producer, draws it, and prints what
that cost: `llvm.html?demo=mandelbrot&backend=llvm&opt=1-fast`. `web/bench.mjs` drives it in
headless Chrome.

To reproduce everything:

```sh
crates/rustd-wasm/llvm/build-llvm.sh Release        # LLVM, into ~/llvm-wasm
crates/rustd-wasm/web/build.sh                      # the usual compiler, and core.bundle
crates/rustd-wasm/web/build-llvm.sh Release         # the compiler with LLVM, web/pkg-llvm
cd crates/rustd-wasm/web
python3 -m http.server 8377 &
RUSTD_PKG=pkg-llvm RUSTD_BACKEND=llvm RUSTD_OPT=1-fast node check.mjs    # Node
npm install playwright-core
PRODUCERS="cranelift,cranelift::pkg-llvm,llvm:0,llvm:1-fast" node bench.mjs 5   # Chrome
```

## Measurements

Everything in this section was measured. Machine: 8-core x86-64 Linux VM, 31 GB. Chrome is
Chrome for Testing 137.0.7118.2, headless, driven by `playwright-core`; every run is a new
browser process, so nothing is cached. Node is v24.21.0. Numbers are medians.

### Compiler size (bytes; `gzip -9`, `brotli -q 9`)

| compiler | raw | gzip | brotli |
|---|---:|---:|---:|
| hand-written lowering only (`pkg`) | 25,713,529 | 5,213,539 | 3,814,420 |
| + LLVM, Release (`pkg-llvm`) | 64,817,640 | 16,631,266 | 12,379,654 |
| + LLVM, MinSizeRel | 59,642,176 | 14,656,700 | 11,078,991 |

The LLVM builds still contain the hand-written lowering. `core.bundle` (7.0 MB, 1.05 MB
brotli) is the same for all.

### Start of the compiler (Chrome, served uncompressed from localhost)

| compiler | runs | fetch ms | `WebAssembly.compile` ms | instantiate + core bundle ms |
|---|---:|---:|---:|---:|
| `pkg` | 25 | 88.5 | 28.4 | 57.0 |
| `pkg-llvm` Release | 75 | 147.1 | 76.8 | 121.3 |
| `pkg-llvm` MinSizeRel | 25 | 139.8 | 73.1 | 119.4 |

### Building and running the demos (Chrome, 5 runs each)

"First build" is the round trip from the page to the worker and back for the first build,
which compiles the demo and everything of `core` it uses. "Codegen" is the compiler's own
figure for that build and includes lowering MIR to Cranelift IR, which both producers share.
"LLVM ms/fn" is LLVM's own tally (lower, verify, optimize, emit, read) over the bodies it
compiled. "Frame" is the mean over 300 calls of the demo's `frame` with drawing stubbed out,
after 20 clicks. "Memory" is the size of the compiler's linear memory after the first build.
"Fallback" is the number of functions LLVM left to the hand-written lowering.

| demo | producer | first build ms | codegen ms | functions | LLVM ms/fn | module bytes | fallback | frame ms | memory MiB |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| balls | hand-written | 3418.5 | 1259.1 | 747 | - | 680,788 | - | 0.014 | 208 |
| balls | LLVM O0 | 4709.7 | 2480.4 | 740 | 1.16 | 710,009 | 0 | 0.012 | 216 |
| balls | LLVM O1 + fast | 5428.8 | 3310.9 | 702 | 2.00 | 296,705 | 0 | 0.008 | 214 |
| fountain | hand-written | 3292.0 | 1257.4 | 750 | - | 723,905 | - | 0.206 | 210 |
| fountain | LLVM O0 | 4813.4 | 2541.1 | 743 | 1.15 | 751,635 | 0 | 0.173 | 217 |
| fountain | LLVM O1 + fast | 5680.3 | 3358.8 | 705 | 2.09 | 337,987 | 0 | 0.054 | 214 |
| life | hand-written | 2825.7 | 803.7 | 404 | - | 262,105 | - | 2.095 | 193 |
| life | LLVM O0 | 3520.3 | 1393.5 | 398 | 1.13 | 262,771 | 0 | 1.724 | 201 |
| life | LLVM O1 + fast | 3990.3 | 1859.8 | 382 | 2.05 | 107,491 | 0 | 0.441 | 200 |
| mandelbrot | hand-written | 2662.8 | 447.1 | 43 | - | 19,662 | - | 0.872 | 182 |
| mandelbrot | LLVM O0 | 2997.2 | 627.1 | 43 | 3.14 | 19,039 | 0 | 0.568 | 191 |
| mandelbrot | LLVM O1 + fast | 2979.7 | 767.7 | 42 | 5.95 | 8,845 | 0 | 0.195 | 191 |
| sort | hand-written | 2747.4 | 782.4 | 397 | - | 265,499 | - | 0.025 | 193 |
| sort | LLVM O0 | 3515.4 | 1395.3 | 389 | 1.10 | 264,783 | 0 | 0.017 | 202 |
| sort | LLVM O1 + fast | 3976.0 | 1852.6 | 375 | 2.01 | 108,909 | 0 | 0.011 | 200 |

About 2.0 to 2.3 s of every first build is analysis of the program and `core`, the same for
both producers.

Where LLVM's time goes, balls, first build (948 bodies; Chrome, 5 runs, ms):

| | lower to IR | verify | O1 passes | emit | read object | total |
|---|---:|---:|---:|---:|---:|---:|
| O0 | 80 | 41 | 0 | 863 | 24 | 1100 |
| O1 + fast | 80 | 41 | 1000 | 678 | 22 | 1900 |

The compiler prints these totals rounded to 0.1 s once past a second, so 1000 and 1100 are
coarse.

A rebuild after a one-line edit (Chrome, 5 runs):

| demo | producer | round trip ms | codegen ms | functions | patch bytes |
|---|---|---:|---:|---:|---:|
| balls | hand-written | 70.2 | 12.3 | 2 | 2,952 |
| balls | LLVM O1 + fast | 72.5 | 16.0 | 2 | 963 |
| mandelbrot | hand-written | 66.5 | 12.2 | 2 | 1,956 |
| mandelbrot | LLVM O1 + fast | 67.5 | 17.9 | 2 | 1,131 |

The other rows of the matrix:

- The hand-written lowering in the LLVM build of the compiler (so, `wasm32-wasip1` instead of
  `wasm32-unknown-unknown`, same producer) emits identical modules and is 0% to 13% slower at
  codegen (balls 1259 to 1427 ms), with 7-8 MiB more memory.
- LLVM built MinSizeRel is within noise of Release for compile time (balls O1 + fast codegen
  3290 vs 3311 ms) and emits identical modules.

Node (v24.21.0, 1 run each, `check.mjs`, which builds and runs every demo and checks what it
draws): passes with the hand-written lowering, with LLVM O0 and with LLVM O1 + fast. First
build of balls 4.9 s at O0 and 5.4 s at O1 + fast.

## What I infer, not measured

- The hand-written lowering's own cost per function was not timed on its own. From the codegen
  column, switching to LLVM O1 + fast adds about 2.7 ms per function of the module on balls
  ((3311 - 1259) / 747), of which LLVM's tally accounts for 2.0 ms per body; the rest is
  probably the context and module made per function, but that was not profiled.
- LLVM in wasm at 2.0 ms per function is in the same range as the native 1.6 ms quoted for
  dx. They are different programs and machines, so that is only a rough agreement.
- Mandelbrot's high per-function figure (6 ms) is mostly the first functions through LLVM
  paying for cold code and lazy compilation of the compiler's own wasm; with 38 bodies there is
  little to spread it over. I did not separate that out.
- On a real connection the extra 8.6 MB of brotli is the cost users would feel most. The fetch
  times above are localhost and say nothing about that.
- Half of O1 + fast's time is the pass pipeline and a third is emit. A shorter hand-picked
  pipeline would likely cut the first figure; this was not tried.
- Dropping `wasm.rs` looks possible, since no demo needs the fallback any more, but the
  compiler was never built without it, so the size it would give back is unknown.

## What is not done

- Only the five canvas demos were run. The Dioxus demo and the playground page were not tried
  with LLVM, and no test suite was run through it. The 128-bit expansions are exercised only
  by whatever those demos happen to call.
- The main page (`index.html`, the playground) still loads the usual compiler. Only
  `llvm.html` and `check.mjs` use the LLVM build.
- Anything LLVM would call compiler-rt for beyond the multiplies and shifts handled here
  (128-bit division, some float conversions) will fall back to the hand-written lowering today,
  and would need either expanding the same way or a small runtime linked into the page if
  `wasm.rs` were removed.
- Emscripten as the runtime, and LLVM versions other than 20.1.8, were not tried.
