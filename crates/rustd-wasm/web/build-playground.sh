#!/bin/sh
# Builds the page into `assets`: the playground, a Dioxus app with `dioxus-code-editor` for
# the source. Needs `dx` (`cargo install dioxus-cli`) and a `clang` that targets wasm32, for
# the parser the highlighting uses.
set -eu
cd "$(dirname "$0")/playground"
rm -rf target/dx/playground/release/web/public
dx build --web --release
# The module asks for its files at `/assets`.
rm -rf ../assets
cp -r target/dx/playground/release/web/public/assets ../assets
# `playground.js` imports it under one name, whatever the build called it.
echo "export * from \"./$(basename ../assets/playground-*.js)\";" > ../assets/playground.js
