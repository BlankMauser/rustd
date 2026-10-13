#!/usr/bin/env python3
"""Runs rustc's own run-pass tests through the differential harness.

usage: uitest.py <rust checkout> [--filter <substring>] [--limit N] [--jobs N] [--json out.json]

Each selected `tests/ui` program becomes a package whose library is the test and whose binary
calls its `main`. rustc builds it, rustd rebuilds the library, and the two binaries have to
behave the same. The summary lists mismatches first, then why functions were skipped, most
common reason first: that list is the worklist.
"""
import argparse, collections, concurrent.futures, glob, json, os, re, shutil, subprocess, sys, tempfile

import difftest

# Tests needing more than a plain `rustc file.rs && ./file` are left out.
DIRECTIVE = re.compile(r"^//@(?! *run-pass\b)", re.M)
UNSUITABLE = re.compile(
    r"^\s*(pub )?mod \w+;|extern crate|include(_str|_bytes)?!|env::args|process::|#!\[no_std\]|#!\[no_main\]"
    r"|#!\[crate_type|proc_macro|catch_unwind|#\[path", re.M)
MAIN = re.compile(r"^(pub )?fn main\(\)", re.M)


def select(checkout, needle):
    for path in sorted(glob.glob(f"{checkout}/tests/ui/**/*.rs", recursive=True)):
        if needle not in path:
            continue
        text = open(path, errors="replace").read()
        if re.search(r"^//@ *run-pass", text, re.M) and not DIRECTIVE.search(text) \
                and not UNSUITABLE.search(text) and MAIN.search(text):
            yield path


def run(path, root, keep):
    name = os.path.relpath(path, root)
    package = tempfile.mkdtemp(prefix="rustd-ui-")
    result = {"test": name, "outcome": "match", "skipped": {}, "compiled": 0, "roots": 0}
    try:
        os.makedirs(f"{package}/src")
        open(f"{package}/Cargo.toml", "w").write(
            '[package]\nname = "t"\nversion = "0.0.0"\nedition = "2015"\n[workspace]\n'
            '[profile.dev]\ndebug = false\n')
        open(f"{package}/src/lib.rs", "w").write(MAIN.sub("pub fn main()", open(path).read(), count=1))
        open(f"{package}/src/main.rs", "w").write("fn main() {\n    t::main()\n}\n")
        fx = difftest.Fixture(package, ["t"], ["-p", "t"], touch=f"{package}/src/main.rs")
        fx.target_args = ["--bin", "t"]
        fx.rustd_timeout = 120
        stage = "rustc"
        try:
            fx.rustc_build()
            expected = fx.behaviour(fx.reference)
            stage = "rustd"
            fx.rustd_build()
            rows = fx.reports["t"]
            for row in rows:
                if row[0] == "skipped" and "no rustc symbol for function" not in row[2]:
                    key = difftest.bucket(row[2])
                    result["skipped"][key] = result["skipped"].get(key, 0) + 1
            result["compiled"] = sum(r[0] == "compiled" for r in rows)
            result["roots"] = result["compiled"] + sum(result["skipped"].values())
            got = fx.behaviour(fx.binary)
            if got != expected:
                result.update(outcome="mismatch", detail=difftest.symptom(expected, got),
                              stderr=got[2].decode(errors="replace")[-300:])
        except SystemExit as failure:
            text = str(failure)
            outcome = "unbuildable" if stage == "rustc" else \
                "link" if text.startswith("link failed") else "crash"
            lines = [l for l in text.splitlines() if l.strip()]
            detail = next((l for l in lines if "panicked" in l or "error" in l), lines[-1] if lines else "")
            if "panicked" in detail:
                detail = lines[lines.index(detail) + 1] if lines.index(detail) + 1 < len(lines) else detail
            result.update(outcome=outcome, detail=detail.strip()[:200])
        finally:
            if not (keep and result["outcome"] not in ("match", "unbuildable")):
                shutil.rmtree(fx.work, ignore_errors=True)
            else:
                result["work"] = fx.work
    finally:
        if keep and result["outcome"] not in ("match", "unbuildable"):
            result["package"] = package
        else:
            shutil.rmtree(package, ignore_errors=True)
    return result


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("checkout")
    ap.add_argument("--filter", default="")
    ap.add_argument("--limit", type=int)
    ap.add_argument("--jobs", type=int, default=os.cpu_count())
    ap.add_argument("--keep", action="store_true", help="keep the packages of failing tests")
    ap.add_argument("--json")
    a = ap.parse_args()

    root = f"{os.path.abspath(a.checkout)}/tests/ui"
    tests = list(select(os.path.abspath(a.checkout), a.filter))[:a.limit]
    with concurrent.futures.ThreadPoolExecutor(a.jobs) as pool:
        results = []
        for result in pool.map(lambda t: run(t, root, a.keep), tests):
            results.append(result)
            if result["outcome"] not in ("match", "unbuildable"):
                print(f"{result['outcome']} {result['test']}: {result.get('detail', '')}", file=sys.stderr, flush=True)

    outcomes = collections.Counter(r["outcome"] for r in results)
    ran = [r for r in results if r["outcome"] != "unbuildable"]
    full = sum(r["outcome"] == "match" and not r["skipped"] for r in ran)
    print(f"{len(ran)} tests: {outcomes['match']} match ({full} with every function from rustd), "
          f"{outcomes['mismatch']} mismatch, {outcomes['crash']} rustd crashes, {outcomes['link']} link errors"
          f"; {outcomes['unbuildable']} left out")
    print(f"functions: {sum(r['compiled'] for r in ran)} of {sum(r['roots'] for r in ran)} from rustd")
    for outcome in ("mismatch", "crash", "link"):
        for r in ran:
            if r["outcome"] == outcome:
                print(f"  {outcome.upper()} {r['test']}: {r.get('detail', '')}")
    reasons, example = collections.Counter(), {}
    for r in ran:
        for reason, n in r["skipped"].items():
            reasons[reason] += n
            example.setdefault(reason, r["test"])
    print("skipped functions by reason:")
    for reason, n in reasons.most_common(40):
        print(f"  {n:4}  {reason}  (e.g. {example[reason]})")
    if a.json:
        json.dump(results, open(a.json, "w"), indent=1)
    sys.exit(1 if outcomes["mismatch"] or outcomes["crash"] or outcomes["link"] else 0)


if __name__ == "__main__":
    main()
