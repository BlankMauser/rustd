#!/bin/sh
# Builds what the page loads into `pkg/`: the compiler as wasm64, and the source of `core`.
# Needs the `rust-src` component. Serve this directory afterwards, with any static server:
#
#     python3 -m http.server -d crates/rustd-wasm/web 8377
set -eu
cd "$(dirname "$0")/.."
target=../../target
mkdir -p web/pkg

# wasm64 is a tier 3 target, so `std` is built along with the compiler.
RUSTC_BOOTSTRAP=1 cargo build --release --lib --target-dir "$target" \
    -Zbuild-std=std,panic_abort --target wasm64-unknown-unknown
cp "$target/wasm64-unknown-unknown/release/rustd_wasm.wasm" web/pkg/

cargo run --release --target-dir "$target" -- \
    bundle "$(rustc --print sysroot)/lib/rustlib/src/rust/library" web/pkg/core.bundle
