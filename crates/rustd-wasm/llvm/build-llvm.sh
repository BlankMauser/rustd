#!/bin/sh
# Builds LLVM as static libraries for wasm32-wasip1, with the WebAssembly target only, for
# the `llvm` feature of `rustd-wasm`. Nothing it makes is committed.
#
#     crates/rustd-wasm/llvm/build-llvm.sh [Release|MinSizeRel]
#
# Needs cmake, ninja, curl, and a native `llvm-tblgen` of the same LLVM version
# (`llvm-20-dev` from apt.llvm.org has one). Everything goes under `$LLVM_WASM_DIR`
# (default `~/llvm-wasm`): wasi-sdk, the LLVM sources, a build tree per build type and the
# prefix `install-<type>` that `LLVM_SYS_201_PREFIX` is then set to.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
build_type=${1:-Release}
root=${LLVM_WASM_DIR:-$HOME/llvm-wasm}
llvm_version=20.1.8
wasi_sdk_version=34
native_tools=${LLVM_NATIVE_TOOL_DIR:-/usr/lib/llvm-20/bin}

wasi_sdk=$root/wasi-sdk-$wasi_sdk_version.0-x86_64-linux
source=$root/llvm-project-$llvm_version.src
build=$root/build-$build_type
prefix=$root/install-$build_type
mkdir -p "$root"

if [ ! -d "$wasi_sdk" ]; then
    curl -sL "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-$wasi_sdk_version/wasi-sdk-$wasi_sdk_version.0-x86_64-linux.tar.gz" \
        | tar xz -C "$root"
fi
if [ ! -d "$source" ]; then
    curl -sL "https://github.com/llvm/llvm-project/releases/download/llvmorg-$llvm_version/llvm-project-$llvm_version.src.tar.xz" \
        | tar xJ -C "$root" --wildcards "llvm-project-$llvm_version.src/llvm" \
            "llvm-project-$llvm_version.src/cmake" "llvm-project-$llvm_version.src/third-party"
    for patch in "$here"/patches/*.patch; do
        [ -e "$patch" ] && patch -d "$source" -p1 < "$patch"
    done
fi

# wasi-libc leaves these out unless asked; LLVM's Support library is written against them.
emulated="-D_WASI_EMULATED_SIGNAL -D_WASI_EMULATED_MMAN -D_WASI_EMULATED_PROCESS_CLOCKS -D_WASI_EMULATED_GETPID"

# `UNIX=ON`: LLVM's cmake knows no platform called WASI, and wasi-libc is close to POSIX.
cmake -S "$source/llvm" -B "$build" -G Ninja \
    -DCMAKE_TOOLCHAIN_FILE="$wasi_sdk/share/cmake/wasi-sdk-p1.cmake" \
    -DCMAKE_BUILD_TYPE="$build_type" \
    -DCMAKE_INSTALL_PREFIX="$prefix" \
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
    -DLLVM_INCLUDE_TESTS=OFF -DLLVM_INCLUDE_EXAMPLES=OFF -DLLVM_INCLUDE_BENCHMARKS=OFF \
    -DLLVM_INCLUDE_DOCS=OFF -DLLVM_INCLUDE_RUNTIMES=OFF

ninja -C "$build"
