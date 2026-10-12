#!/bin/sh
# Unpacks a crates.io crate (name-version) into harness/vendor/<name> as its own workspace.
set -e
here=$(cd "$(dirname "$0")" && pwd)
name=${1%-*}
dest="$here/vendor/$name"
[ -d "$dest" ] && exit 0
mkdir -p "$here/vendor"
src=$(ls -d "$HOME"/.cargo/registry/src/*/"$1" 2>/dev/null | head -1)
if [ -z "$src" ]; then
    tmp=$(mktemp -d)
    (cd "$tmp" && cargo init -q --name fetch && cargo add -q "$name@=${1##*-}" && cargo fetch -q)
    src=$(ls -d "$HOME"/.cargo/registry/src/*/"$1" | head -1)
fi
cp -r "$src" "$dest"
chmod -R u+w "$dest"
rm -f "$dest/Cargo.lock"
[ -f "$dest/Cargo.toml.orig" ] && mv "$dest/Cargo.toml.orig" "$dest/Cargo.toml"
printf '\n[workspace]\n' >> "$dest/Cargo.toml"
