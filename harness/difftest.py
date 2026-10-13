#!/usr/bin/env python3
"""Differential test: build a binary with rustc, rebuild chosen crates with rustd, compare behaviour.

usage: difftest.py <workspace> --bin <package> --crate <name> [--crate <name> ...] [--bisect]

Exit code 0 when stdout, stderr and exit status match. With --bisect, each rustd function is
also linked in on its own to name the ones that change behaviour.
"""
import argparse, glob, json, os, shlex, subprocess, sys, tempfile

# rustd computes layouts with rust-analyzer's copy of rustc_abi, so everything it links against
# has to come from the rustc that copy was taken from.
os.environ.setdefault("RUSTUP_TOOLCHAIN", open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "toolchain")).read().strip())

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RUSTD = os.path.join(ROOT, "target/release/rustd")


def sysroot_rlibs():
    sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
    host = subprocess.check_output(["rustc", "-vV"], text=True).split("host: ")[1].split()[0]
    lib = f"{sysroot}/lib/rustlib/{host}/lib"
    return [p for n in ("core", "alloc", "std") for p in glob.glob(f"{lib}/lib{n}-*.rlib")]


class Fixture:
    def __init__(self, workspace, crates, target, touch=None):
        """`target` selects what cargo links: ["-p", "app"] or ["--test", "name"]."""
        self.ws, self.crates, self.target_args = os.path.abspath(workspace), crates, target
        self.work = tempfile.mkdtemp(prefix="rustd-diff-")
        self.run_args = []
        self.layout_log = os.path.join(self.work, "layouts.tsv")
        if touch is None:
            meta = json.loads(subprocess.check_output(
                ["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=self.ws))
            touch = next(t["src_path"] for p in meta["packages"] if p["name"] == target[1]
                         for t in p["targets"] if "bin" in t["kind"])
        self.touch = touch

    def rustc_build(self):
        """Builds with cargo and captures the linker line so rustd objects can be spliced in."""
        os.utime(self.touch)
        out = subprocess.run(
            ["cargo", "rustc", *self.target_args, "--", "--print", "link-args", "-C", "save-temps"],
            cwd=self.ws, capture_output=True, text=True)
        if out.returncode != 0:
            raise SystemExit(f"cargo failed:\n{out.stderr[-3000:]}")
        line = next(l for l in (out.stdout + out.stderr).splitlines() if '"cc"' in l)
        args = shlex.split(line)
        self.link_args = [a for a in args[args.index("cc"):] if not a.endswith("symbols.o")]
        self.reference = self.link_args[self.link_args.index("-o") + 1]
        self.rlibs = [a for a in self.link_args if a.endswith(".rlib")]

    def rustd_build(self, deny=()):
        """Compiles each crate with rustd and links the objects ahead of rustc's rlibs."""
        deny_file = os.path.join(self.work, "deny.txt")
        open(deny_file, "w").write("\n".join(deny))
        symbols = [x for rlib in self.rlibs + sysroot_rlibs() for x in ("--symbols", rlib)]
        objects, self.reports = [], {}
        for crate in self.crates:
            obj, report = (os.path.join(self.work, f"{crate}.{ext}") for ext in ("o", "tsv"))
            run = subprocess.run(
                [RUSTD, self.ws, "--crate", crate, "--out", obj, "--report", report,
                 "--deny", deny_file, "--isolate", os.path.join(self.work, f"{crate}.isolated"), *symbols],
                capture_output=True, text=True, env={**os.environ, "RUSTD_LAYOUTS": self.layout_log})
            open(os.path.join(self.work, f"{crate}.stderr"), "w").write(run.stderr)
            if run.returncode != 0:
                raise SystemExit(f"rustd crashed on {crate}:\n{run.stderr[-2000:]}")
            objects.append(obj)
            self.reports[crate] = [l.rstrip("\n").split("\t") for l in open(report)]
        self.link(objects)

    def link(self, objects):
        args = list(self.link_args)
        first = next(i for i, a in enumerate(args) if a.endswith(".rlib"))
        args[first:first] = objects + ["-Wl,--allow-multiple-definition"]
        self.binary = os.path.join(self.work, "bin-rustd")
        args[args.index("-o") + 1] = self.binary
        run = subprocess.run(args, capture_output=True, text=True)
        if run.returncode != 0:
            raise SystemExit(f"link failed:\n{run.stderr[-3000:]}")

    def compiled(self):
        return [r[1] for rows in self.reports.values() for r in rows if r[0] == "compiled"]

    def behaviour(self, binary):
        try:
            run = subprocess.run([binary, *self.run_args], capture_output=True, timeout=120)
            return run.returncode, self.normalize(run.stdout), run.stderr
        except subprocess.TimeoutExpired:
            return "timeout", b"", b""

    def normalize(self, stdout):
        return stdout

    def matches(self, expected):
        return self.behaviour(self.binary) == expected


def isolate(fx, expected):
    """Links each rustd function into the rustc binary on its own; returns the ones that change behaviour."""
    culprits = []
    for crate in fx.crates:
        directory = os.path.join(fx.work, f"{crate}.isolated")
        for line in open(os.path.join(directory, "index.tsv")):
            index, name = line.rstrip("\n").split("\t")
            fx.link([os.path.join(directory, f"{index}.o")])
            if not fx.matches(expected):
                got = fx.behaviour(fx.binary)
                culprits.append({"function": name, "symptom": symptom(expected, got)})
    return culprits


def symptom(expected, got):
    if got[0] != expected[0]:
        return f"exit status {got[0]}"
    want, have = expected[1].decode(errors="replace").splitlines(), got[1].decode(errors="replace").splitlines()
    for w, h in zip(want, have):
        if w != h:
            return f"expected `{w}`, got `{h}`"
    return "output length differs"


def bucket(error):
    """Failure reason without the payload, so the same gap in different functions counts once."""
    import re
    reason = error.rsplit("`: ", 1)[-1]
    reason = re.sub(r"^(unsupported|frontend|backend): ", "", reason)
    for prefix in ("drop glue", "unsizing", "aggregate of", "expected scalar", "cast "):
        if reason.startswith(prefix):
            return {"aggregate of": "odd-sized aggregate in a register", "cast ": reason.split("(")[-1].rstrip(")") + " cast"}.get(prefix, prefix)
    return re.split(r"[({]", reason)[0].strip()[:60]


def memcheck(fx):
    """Runs the rustd binary under valgrind: the first invalid access is usually the miscompile,
    where the crash or wrong output is only what it led to. Returns the error blocks."""
    import re, shutil
    if not shutil.which("valgrind"):
        return None
    log = os.path.join(fx.work, "valgrind.log")
    try:
        subprocess.run(["valgrind", "-q", "--num-callers=30", f"--log-file={log}", fx.binary, *fx.run_args],
                       capture_output=True, timeout=900)
    except subprocess.TimeoutExpired:
        pass
    text = re.sub(r"^==\d+== ?", "", open(log).read(), flags=re.M)
    errors = [e for e in text.split("\n\n") if e.strip()]
    print(f"  valgrind: {len(errors)} errors ({log})")
    if errors:
        print("\n".join("    " + line[:200] for line in errors[0].splitlines()))
    return errors


def compare(fx, name, do_bisect, do_memcheck=False, do_layouts=False):
    fx.rustc_build()
    expected = fx.behaviour(fx.reference)
    fx.rustd_build()
    ok = fx.matches(expected)
    result = {"fixture": name, "match": ok, "crates": {}}
    for crate, rows in fx.reports.items():
        n = sum(r[0] == "compiled" for r in rows)
        total = sum(r[0] in ("compiled", "skipped") for r in rows)
        reasons = {}
        for r in rows:
            if r[0] == "skipped":
                key = bucket(r[2])
                reasons[key] = reasons.get(key, 0) + 1
        no_symbol = reasons.pop("no rustc symbol for function", 0)
        result["crates"][crate] = {
            "compiled": n, "roots": total - no_symbol, "not_roots": no_symbol,
            "instances": int(rows[0][2]), "codegen_ms": int(rows[0][4]) / 1000, "skipped": reasons}
        print(f"{crate}: {n}/{total - no_symbol} functions from rustd")
    print("MATCH" if ok else "MISMATCH")
    if not ok:
        got = fx.behaviour(fx.binary)
        print(f"  rustc: status {expected[0]}\n{expected[1].decode(errors='replace')}")
        print(f"  rustd: status {got[0]}\n{got[1].decode(errors='replace')}{got[2].decode(errors='replace')[-600:]}")
        do_memcheck = True
        if do_bisect:
            result["culprits"] = isolate(fx, expected)
            for c in result["culprits"]:
                print(f"  MISCOMPILED {c['function']}: {c['symptom']}")
    if do_memcheck:
        errors = memcheck(fx)
        result["valgrind_errors"] = None if errors is None else len(errors)
        if errors:
            result["match"] = False
    if do_layouts:
        import layoutdiff
        compared, wrong = layoutdiff.diff(fx.ws, fx.crates, fx.layout_log)
        print(f"  layouts: {len(wrong)} of {compared} differ from rustc")
        for name, want, have in wrong:
            print(f"    {name}: rustc (size, align) {want}, rust-analyzer {have}")
        result["layouts"] = {"compared": compared, "wrong": [w[0] for w in wrong]}
        if wrong:
            result["match"] = False
    return result


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("workspace")
    ap.add_argument("--bin", required=True)
    ap.add_argument("--crate", action="append", required=True)
    ap.add_argument("--bisect", action="store_true")
    ap.add_argument("--memcheck", action="store_true", help="also fail on valgrind errors when the output matches")
    ap.add_argument("--layouts", action="store_true", help="compare type sizes and alignments with rustc's")
    ap.add_argument("--json")
    a = ap.parse_args()

    fx = Fixture(a.workspace, a.crate, ["-p", a.bin])
    result = compare(fx, a.bin, a.bisect, a.memcheck, a.layouts)
    if a.json:
        json.dump(result, open(a.json, "w"), indent=1)
    sys.exit(0 if result["match"] else 1)


if __name__ == "__main__":
    main()
