#!/usr/bin/env python3
"""breakers.py: every reduction in harness/breakers on its own, rustd's binary against cargo's.

usage: breakers.py [case ...]

The crates in CRATES are run through cratetest.py: what broke was in their tests.
Prints "N of N right" and exits non-zero when a case differs.
"""
import os, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from difftest import Fixture

CRATES = ["devdep"]
# What is reduced and not fixed: these are expected to differ, and are told apart when they do.
OPEN = {"future_size"}


def main():
    fx = Fixture(os.path.join(HERE, "breakers"), ["breakers"], ["-p", "breaker"])
    fx.rustc_build()
    fx.rustd_build()
    skipped = [r for rows in fx.reports.values() for r in rows if r[0] == "skipped" and "no rustc symbol" not in r[2]]
    for row in skipped:
        print(f"  skipped {row[1]}: {row[2][:300]}")
    names = sys.argv[1:] or subprocess.check_output([fx.reference], text=True).split()
    wrong = fixed = 0
    for name in names:
        fx.run_args = [name]
        want, got = fx.behaviour(fx.reference), fx.behaviour(fx.binary)
        same = want == got
        wrong += not same and name not in OPEN
        fixed += same and name in OPEN
        print(f"{'same     ' if same else 'open     ' if name in OPEN else 'DIFFERENT'} {name}", flush=True)
        if not same:
            print(f"    rustc: status {want[0]} {want[1].decode(errors='replace').strip()[:300]}")
            tail = got[2].decode(errors="replace").strip().splitlines()[-1:]
            print(f"    rustd: status {got[0]} {got[1].decode(errors='replace').strip()[:300]} {' '.join(tail)[:200]}")
    if not sys.argv[1:]:
        # What breaks in a crate's tests and not in a program: each a crate for cratetest.py.
        for crate in CRATES:
            run = subprocess.run([sys.executable, os.path.join(HERE, "cratetest.py"), os.path.join(HERE, "breakers", crate)],
                                 capture_output=True, text=True)
            same = run.returncode == 0 and "skipped" not in run.stdout
            wrong += not same
            names.append(crate)
            print(f"{'same     ' if same else 'DIFFERENT'} {crate} (its tests)", flush=True)
            if not same:
                print("    " + "\n    ".join(run.stdout.strip().splitlines()[-8:]))
    still = len(OPEN & set(names)) - fixed
    print(f"{len(names) - wrong - still} of {len(names) - still} right, {still} open, {len(skipped)} functions skipped")
    if fixed:
        print(f"{fixed} of the open cases are the same now: take them out of OPEN")
    sys.exit(1 if wrong or skipped or fixed else 0)


main()
