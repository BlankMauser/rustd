#!/usr/bin/env python3
"""Differential test using a crate's own integration tests.

usage: cratetest.py <crate-dir> [--crate <name>] [--bisect] [--json out.json]

Each `tests/*.rs` target is built by cargo, then relinked with rustd's objects for the crate in
front of rustc's rlib. Both binaries must report the same result for every test.
"""
import argparse, json, os, re, sys
from difftest import Fixture, compare


class TestFixture(Fixture):
    def __init__(self, crate_dir, crate, test):
        path = os.path.join(crate_dir, "tests", f"{test}.rs")
        super().__init__(crate_dir, [crate], ["--test", test], touch=path)
        self.run_args = ["--test-threads=1"]

    def normalize(self, stdout):
        return re.sub(rb"finished in [0-9.]+s", b"finished", stdout)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("crate_dir")
    ap.add_argument("--crate")
    ap.add_argument("--bisect", action="store_true")
    ap.add_argument("--json")
    a = ap.parse_args()
    crate = a.crate or os.path.basename(os.path.abspath(a.crate_dir)).replace("-", "_")
    tests = sorted(f[:-3] for f in os.listdir(os.path.join(a.crate_dir, "tests")) if f.endswith(".rs"))
    results = []
    for test in tests:
        print(f"--- {test}")
        fx = TestFixture(a.crate_dir, crate, test)
        result = compare(fx, test, a.bisect)
        expected = fx.behaviour(fx.reference)[1]
        result["tests_run"] = len(re.findall(rb"^test .* \.\.\. ", expected, re.M))
        results.append(result)
    ok = all(r["match"] for r in results)
    total = sum(r["tests_run"] for r in results)
    print(f"{'MATCH' if ok else 'MISMATCH'}: {len(results)} test binaries, {total} tests")
    if a.json:
        merged = {"fixture": crate, "match": ok, "tests_run": total,
                  "crates": results[0]["crates"] if results else {},
                  "culprits": [c for r in results for c in r.get("culprits", [])]}
        json.dump(merged, open(a.json, "w"), indent=1)
    sys.exit(0 if ok else 1)


main()
