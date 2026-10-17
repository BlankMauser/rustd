#!/usr/bin/env python3
"""The doctests of real crates, run by `cargo test --doc` with rustdoc and with rustd in
rustdoc's place: the same tests must end the same way and print the same.

    RUSTD_BIN=../target/release/rustd python3 doctests.py [name-version ...]

Each crate is unpacked by `vendor.sh`. A crate rustd leaves to rustdoc is counted as
refused, with its reason; a difference is WRONG and fails the run."""
import os
import re
import subprocess
import sys

from buildmodel import doctest_report

HERE = os.path.dirname(os.path.abspath(__file__))
RUSTD = os.path.abspath(os.environ.get("RUSTD_BIN", os.path.join(HERE, "..", "target", "release", "rustd")))
CRATES = ["semver-1.0.28", "either-1.19.0", "smallvec-1.16.3"]


def tested(crate, target_dir, **more):
    """How the crate's doctests ended, what they printed, and what was said beside."""
    run = subprocess.run(["cargo", "test", "--doc"], cwd=crate, capture_output=True, text=True, timeout=1800,
                         env={**os.environ, "CARGO_TARGET_DIR": os.path.join(crate, target_dir), **more})
    return run.returncode, doctest_report(run.stdout), run.stderr


def row(spec):
    """The crate's line: its verdict and what there is to say of it."""
    subprocess.run(["sh", os.path.join(HERE, "vendor.sh"), spec], check=True)
    crate = os.path.join(HERE, "vendor", spec.rsplit("-", 1)[0])
    want = tested(crate, "target-doc-ref")
    got = tested(crate, "target-doc-rustd", RUSTDOC=RUSTD, RUSTD_RUSTDOC="rustdoc")
    left = re.search(r"rustd: the doctests are left to rustdoc: (.*)", got[2])
    if left:
        return "refused", left.group(1)
    if want[:2] != got[:2]:
        lines = [f"cargo's `{w}`, rustd's `{g}`" for w, g in zip(want[1], got[1]) if w != g]
        return "WRONG", (lines[0] if lines else f"cargo's ends with {want[0]}, rustd's with {got[0]}")[:300]
    built = re.findall(r"doctests: compiled (\d+) of (\d+) fns", got[2])
    if not built:
        return "WRONG", "rustd did not say what it built for the doctests"
    tests = sum(l.startswith("test ") and not l.startswith("test result") for l in want[1])
    return "same", f"{tests} doctests, {sum(int(n) for n, _ in built)} of {sum(int(n) for _, n in built)} functions of the library from rustd"


def main():
    wrong = 0
    for spec in sys.argv[1:] or CRATES:
        verdict, said = row(spec)
        wrong += verdict == "WRONG"
        print(f"{spec:24} {os.environ.get('RUSTD_BACKEND', 'cranelift'):9} {verdict:8} {said}", flush=True)
    print(f"{wrong} WRONG")
    sys.exit(1 if wrong else 0)


if __name__ == "__main__":
    main()
