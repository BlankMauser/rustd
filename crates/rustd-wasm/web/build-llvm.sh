#!/bin/sh
# As `build.sh`, with LLVM in the compiler, into `pkg-llvm/`. Run `llvm/build-llvm.sh` and
# `build.sh` first: this links the LLVM the one built, and uses the `core.bundle` of the other.
#
# The compiler is built for wasm32-wasip1 here, as LLVM needs a libc and a libc++, and the
# ones of wasi-sdk are what it was built against. The page gives it the WASI imports.
set -eu
cd "$(dirname "$0")/.."
target=../../target
root=${LLVM_WASM_DIR:-$HOME/llvm-wasm}
sdk=$root/wasi-sdk-34.0-x86_64-linux
lib=$sdk/share/wasi-sysroot/lib/wasm32-wasip1
mkdir -p web/pkg-llvm

export LLVM_SYS_201_PREFIX="$PWD/llvm"
export LLVM_SYS_LIBCPP=c++
export CC_wasm32_wasip1="$sdk/bin/clang"
export AR_wasm32_wasip1="$sdk/bin/llvm-ar"
# The first is what `.cargo/config.toml` gives the other wasm32 target: there are no threads.
# `noeh` is the libc++ without exceptions, as LLVM is built. The reactor start file makes
# the module one whose constructors run once, in `_initialize`, and not in every export.
export RUSTFLAGS="--cfg no_salsa_async_drops -L native=$lib/noeh -L native=$lib -l static=c++ -l static=c++abi \
    -l static=wasi-emulated-mman -l static=wasi-emulated-signal \
    -l static=wasi-emulated-process-clocks -l static=wasi-emulated-getpid \
    -C link-arg=$lib/crt1-reactor.o"

cargo build --release --lib --features llvm --target-dir "$target" --target wasm32-wasip1
cp "$target/wasm32-wasip1/release/rustd_wasm.wasm" web/pkg-llvm/
cp web/pkg/core.bundle web/pkg-llvm/
