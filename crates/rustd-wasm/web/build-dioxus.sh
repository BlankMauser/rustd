#!/bin/sh
# Builds what the Dioxus demo loads from `pkg/`, after `build.sh`: Dioxus and `std` as a module
# rustc builds, and the workspace of the program for rustd to compile in the page.
# Needs the toolchain of `dioxus/rust-toolchain.toml`, and `wasm-bindgen` of the version
# `dioxus/Cargo.lock` has (`cargo install wasm-bindgen-cli --version 0.2.122`).
set -eu
cd "$(dirname "$0")/.."
target=../../target
tool="$PWD/$target/release/rustd-wasm"
cargo build --release --target-dir "$target"

cd web/dioxus
cargo build --release -p base
base=target/wasm32-unknown-unknown/release
# Everything of every crate, exported under its symbol: the program links against any of it.
# rustc gives its own arguments to the linker after the libraries, where `--whole-archive`
# does nothing, so the linker runs here.
"$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/host: //p')/bin/rust-lld" -flavor wasm \
    --whole-archive "$base/libbase.a" --no-entry --no-gc-sections --allow-undefined \
    --export-all --export-table --growable-table --export=__stack_pointer --emit-relocs \
    -o "$base/base.wasm"
wasm-bindgen "$base/base.wasm" --target web --keep-lld-exports --out-dir ../pkg/dioxus --out-name base

"$tool" pack . app ../pkg/dioxus.pack
"$tool" host "$base/base.wasm" ../pkg/dioxus.pack

# Two things wasm-bindgen's JavaScript needs for a module nothing was removed from: the names
# of functions LLVM made module-private (`name.llvm.123`) do not parse with their dots, and
# the functions describing bindings, which it normally deletes once read, keep their import.
sed -i -E -e 's/wasm\.(_R[A-Za-z0-9_]+\.llvm\.[0-9]+)/wasm["\1"]/g' \
    -e 's/(^|[^"A-Za-z0-9_.])(_R[A-Za-z0-9_]+)\.llvm\.([0-9]+)/\1\2$llvm$\3/g' \
    -e 's/^( *)"\.\/base_bg\.js": import0,$/&\n\1__wbindgen_placeholder__: { __wbindgen_describe() {}, __wbindgen_describe_cast() {} },/' \
    ../pkg/dioxus/base.js
