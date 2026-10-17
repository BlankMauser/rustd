#!/bin/bash
# cratesweep.sh <out-dir> <name-version>...: vendors each crate and runs its own tests against
# rustd, JOBS crates at a time. One log, one json and one status line per crate in <out-dir>.
# A crate that already has a status there is not run again, so a stopped sweep picks up where
# it was; delete `<out-dir>/<name>.status` to run a crate again.
here=$(cd "$(dirname "$0")" && pwd)
out=$1; shift; mkdir -p "$out"
one() {
    name=$(printf %s "$1" | sed -E 's/-[0-9]+\.[0-9]+\.[0-9]+.*$//')
    [ -e "$out/$name.status" ] && return
    start=$(date +%s)
    rm -f "$out/$name.json"
    "$here/vendor.sh" "$1" > "$out/$name.log" 2>&1 &&
        timeout -k 20 "${TIMEOUT:-1800}" python3 "$here/cratetest.py" "$here/vendor/$name" --json "$out/$name.json" >> "$out/$name.log" 2>&1
    code=$?
    # CLEAN=1: a sweep over many crates fills the disk with their target directories.
    [ -n "$CLEAN" ] && rm -rf "$here/vendor/$name/target"
    result=$(grep -aE '^(MATCH|MISMATCH|UNTESTED): ' "$out/$name.log" | tail -1)
    [ "$code" = 124 ] || [ "$code" = 137 ] && result="TIMEOUT after ${TIMEOUT:-1800}s"
    [ -z "$result" ] && result="FAILED $(tail -1 "$out/$name.log" | cut -c1-150)"
    echo "$name: $result"
    printf '%s\t%s\t%s\n' "$1" "$(($(date +%s) - start))" "$result" > "$out/$name.status"
}
export -f one; export here out
printf '%s\n' "$@" | xargs -P "${JOBS:-2}" -I{} bash -c 'one {}'
