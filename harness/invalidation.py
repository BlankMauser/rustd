#!/usr/bin/env python3
"""How much of a crate rust-analyzer redoes after different kinds of edit.

usage: invalidation.py [crate] [--only TEXT] [--perf DIR] [--check]

`--only` keeps the scenarios whose name contains TEXT, plus one per entry of BODIES. `--perf`
records the daemon with `perf` during each edit, into DIR/<n>.data. `--check` compares the object
the daemon wrote after each edit with the one a new rustd process writes from the same source.

Starts the daemon on bench2 for one crate (default regex_automata) with RUSTD_STATS set, applies
each scenario, and records the time to apply the change, the time spent in MIR queries, the
total rebuild time, where it went, and how many functions were compiled again. Writes INVALIDATION.md.
"""
import os, re, signal, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
os.environ.setdefault("RUSTUP_TOOLCHAIN", open(os.path.join(HERE, "toolchain")).read().strip())
os.environ["RUSTD_STATS"] = "1"
from difftest import Fixture, RUSTD, sysroot_rlibs
from bench import millis

V = os.path.join(HERE, "vendor")
A, S = os.path.join(V, "regex-automata/src"), os.path.join(V, "regex-syntax/src")
BODY = "let _rustd_edit = 1u32;"


def insert_after(path, line, text):
    """Inserts `text` after 1-based `line`."""
    def apply(lines):
        lines.insert(line, text + "\n")
    return path, apply


def top(path):
    def apply(lines):
        lines.insert(0, "\n")
    return path, apply


def substitute(path, old, new):
    def apply(lines):
        text = "".join(lines)
        assert old in text, old
        lines[:] = text.replace(old, new).splitlines(keepends=True)
    return path, apply


BODIES = [(f"{A}/util/look.rs", 291), (f"{A}/util/sparse_set.rs", 151), (f"{A}/nfa/thompson/builder.rs", 149),
          (f"{A}/meta/strategy.rs", 383), (f"{A}/nfa/thompson/pikevm.rs", 165)]
NEW_FN = "\npub fn rustd_new_item(x: u32) -> u32 { x + 1 }\n"
SCENARIOS = {
    "comment appended to one file": [insert_after(f"{A}/util/look.rs", 10**9, "// edit")],
    "blank line at top of one file": [top(f"{A}/nfa/thompson/builder.rs")],
    "blank line at top of 5 files": [top(p) for p, _ in BODIES],
    "1 function body": [insert_after(*BODIES[0], BODY)],
    "5 function bodies in 5 files": [insert_after(p, l, BODY) for p, l in BODIES],
    "new private fn added": [insert_after(f"{A}/util/look.rs", 10**9, NEW_FN.replace("pub ", ""))],
    "signature of a private method": [
        substitute(f"{A}/nfa/thompson/builder.rs", "    fn memory_usage(&self) -> usize {\n        match *self {\n            State::Empty",
                   "    fn memory_usage(&self, _extra: ()) -> usize {\n        match *self {\n            State::Empty"),
        substitute(f"{A}/nfa/thompson/builder.rs", "state.memory_usage()", "state.memory_usage(())")],
    "upstream crate: 1 function body": [insert_after(f"{S}/hir/mod.rs", 1008, BODY)],
    "upstream crate: blank line at top": [top(f"{S}/hir/mod.rs")],
    "upstream crate: new pub fn": [insert_after(f"{S}/hir/mod.rs", 10**9, NEW_FN)],
    "both crates: 3 bodies each + new pub fn upstream": [
        *[insert_after(p, l, BODY) for p, l in BODIES[:3]],
        insert_after(f"{S}/ast/mod.rs", 36, BODY), insert_after(f"{S}/ast/mod.rs", 10**9, NEW_FN),
        insert_after(f"{S}/hir/mod.rs", 1008, BODY), insert_after(f"{S}/hir/literal.rs", 10**9, "// edit")],
}


def main():
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("crate", nargs="?", default="regex_automata")
    ap.add_argument("--only")
    ap.add_argument("--perf")
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()
    crate = a.crate
    scenarios = SCENARIOS
    if a.only:
        scenarios = {f"body in {os.path.basename(p)}": [insert_after(p, l, BODY)] for p, l in BODIES}
        scenarios.update(SCENARIOS)
        scenarios = {name: it for name, it in scenarios.items() if a.only in name}
    ws = os.path.join(HERE, "bench2")
    fx = Fixture(ws, [crate], ["-p", "bench2"])
    fx.rustc_build()
    symbols = [x for rlib in fx.rlibs + sysroot_rlibs() for x in ("--symbols", rlib)]
    log = open(os.path.join(fx.work, "daemon.log"), "w+")
    obj = os.path.join(fx.work, "rustd.o")
    mismatches = 0
    daemon = subprocess.Popen([RUSTD, ws, "--crate", crate, "--out", obj, "--watch", *symbols],
                              stderr=log, stdout=log)

    def wait(pattern, seen):
        while True:
            log.seek(0)
            found = re.findall(pattern, log.read())
            if len(found) > seen:
                return
            if daemon.poll() is not None:
                log.seek(0)
                raise SystemExit(f"daemon died:\n{log.read()[-3000:]}")
            time.sleep(0.01)

    originals, rows, edits = {}, [], 0

    def settle():
        """Waits until the daemon has finished every build the writes set off."""
        nonlocal edits
        wait(r"edit-to-binary: \S+", edits)
        while True:
            time.sleep(0.3)
            log.seek(0)
            text = log.read()
            edits = len(re.findall(r"edit-to-binary: \S+", text))
            if edits == len(re.findall(r"change: \d+ files", text)):
                return

    def write(changes):
        nonlocal edits
        files = {}
        for path, apply in changes:
            originals.setdefault(path, open(path).read())
            lines = files.setdefault(path, originals[path].splitlines(keepends=True))
            apply(lines)
        time.sleep(0.2)
        for path, lines in files.items():
            open(path, "w").write("".join(lines))
        settle()

    def restore(paths):
        nonlocal edits
        time.sleep(0.2)
        for path in paths:
            open(path, "w").write(originals[path])
        settle()

    try:
        wait(r"watching \d+ files", 0)
        for n, (name, changes) in enumerate(scenarios.items()):
            before = edits
            perf = a.perf and subprocess.Popen(
                ["perf", "record", "--call-graph", "dwarf,16384", "-F", "499", "-q", "-p", str(daemon.pid), "-o", f"{a.perf}/{n}.data"],
                stderr=subprocess.DEVNULL)
            write(changes)
            if perf:
                perf.send_signal(signal.SIGINT)
                perf.wait()
            log.seek(0)
            text = log.read()
            builds = re.findall(
                r"edit \(apply (\S+)\): compiled (\d+) of (\d+) fns \(\+(\d+) instances\) in (\S+) .*\n(?:   .*\n)*stats: (\d+) of (\d+) functions compiled, \d+ reused\nphases: compile (\S+), resolve (\S+), define (\S+), object (\S+), \d+ rounds\nedit-to-binary: (\S+)",
                text)[before:edits]
            apply = sum(millis(b[0]) for b in builds)
            total = sum(millis(b[4]) for b in builds)
            phases = [sum(millis(b[i]) for b in builds) for i in (7, 8, 9, 10)]
            rows.append((name, len(builds), apply, total, sum(int(b[5]) for b in builds),
                         int(builds[-1][6]), phases, sum(millis(b[11]) for b in builds)))
            print(rows[-1], flush=True)
            if a.check:
                scratch = os.path.join(fx.work, "scratch.o")
                subprocess.run([RUSTD, ws, "--crate", crate, "--out", scratch, *symbols],
                               check=True, capture_output=True)
                same = open(scratch, "rb").read() == open(obj, "rb").read()
                print("  from scratch:", "same object" if same else "DIFFERENT OBJECT", flush=True)
                mismatches += not same
            restore({path for path, _ in changes})
        log.seek(0)
        text = log.read()
        rss = int(re.search(r"VmRSS:\s+(\d+)", open(f"/proc/{daemon.pid}/status").read())[1]) // 1024
    finally:
        daemon.kill()
        for path, text_ in originals.items():
            open(path, "w").write(text_)
    cold = re.search(r"cold: compiled (\d+) of (\d+) fns \(\+(\d+) instances\) in (\S+) .*\n(?:   .*\n)*stats: (\d+) of (\d+) functions compiled", text)
    load = re.search(r"load workspace: (\S+)", text)
    load = load[1] if load else "?"
    out = [f"# What an edit invalidates ({crate})", "",
           "Generated by `harness/invalidation.py`. The daemon holds bench2 (regex-syntax and regex-automata, vendored).",
           f"Cold: load {load}, {cold[1]} of {cold[2]} functions + {cold[3]} instances in {cold[4]}. Daemon RSS {rss} MB.",
           "", "Each row is one save, followed by a restore that is not counted. Times in ms.", "",
           "| Edit | Builds | Apply change | Codegen | of which compile / resolve / define / object | Edit to binary | Functions recompiled |",
           "|---|---|---|---|---|---|---|"]
    for name, builds, apply, total, compiled, bodies, phases, e2b in rows:
        split = " / ".join(f"{p:.0f}" for p in phases)
        out.append(f"| {name} | {builds} | {apply:.1f} | {total:.0f} | {split} | {e2b:.0f} | {compiled} of {bodies} |")
    if not a.only:
        open(os.path.join(HERE, f"INVALIDATION-{crate}.md"), "w").write("\n".join(out) + "\n")
    print("\n".join(out))
    if mismatches:
        raise SystemExit(f"{mismatches} edits gave an object that differs from a build from scratch")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))
    main()
