#!/bin/sh
# Builds the source pane of the pages into `assets`: `dioxus-code-editor`, with the
# highlighting of `dioxus-code`. The pages have a plain text area without it.
# Needs `dx` (`cargo install dioxus-cli`) and a `clang` that targets wasm32, for the parser.
set -eu
cd "$(dirname "$0")/editor"
rm -rf target/dx/editor/release/web/public
dx build --web --release
# The module asks for its files at `/assets`.
rm -rf ../assets
cp -r target/dx/editor/release/web/public/assets ../assets
# The page imports it under one name, whatever the build called it.
echo "export * from \"./$(basename ../assets/editor-*.js)\";" > ../assets/editor.js
