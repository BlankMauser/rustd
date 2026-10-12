#!/usr/bin/env python3
"""Differential test: build a binary with rustc, rebuild chosen crates with rustd, compare behaviour.

usage: difftest.py <workspace> --bin <package> --crate <name> [--crate <name> ...] [--bisect]

Exit code 0 when stdout, stderr and exit status match. With --bisect, each rustd function is
also linked in on its own to name the ones that change behaviour.
"""
import argparse, glob, json, os, shlex, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RUSTD = os.path.join(ROOT, "target/release/rustd")


def sysroot_rlibs():
    sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
    host = subprocess.check_output(["rustc", "-vV"], text=True).split("host: ")[1].split()[0]
    lib = f"{sysroot}/lib/rustlib/{host}/lib"
    return [p for n in ("core", "alloc", "std") for p in glob.glob(f"{lib}/lib{n}-*.rlib")]


class Fixture:
    def __init__(self, workspace, bin_pkg, crates):
        self.ws, self.bin, self.crates = os.path.abspath(workspace), bin_pkg, crates
        self.work = tempfile.mkdtemp(prefix="rustd-diff-")
        meta = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1"], cwd=self.ws))
        self.target = meta["target_directory"]
        self.main = next(t["src_path"] for p in meta["packages"] if p["name"] == bin_pkg
                         for t in p["targets"] if "bin" in t["kind"])

    def rustc_build(self):
        """Builds with cargo and captures the linker line so rustd objects can be spliced in."""
        os.utime(self.main)
        out = subprocess.run(
            ["cargo", "rustc", "-p", self.bin, "--", "--print", "link-args", "-C", "save-temps"],
            cwd=self.ws, capture_output=True, text=True, check=True)
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
                 "--deny", deny_file, "--isolate", os.path.join(self.work, f"{crate}.isolated"), *symbols], capture_output=True, text=True)
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
        subprocess.run(args, check=True, capture_output=True)

    def compiled(self):
        return [r[1] for rows in self.reports.values() for r in rows if r[0] == "compiled"]

    @staticmethod
    def behaviour(binary):
        try:
            run = subprocess.run([binary], capture_output=True, timeout=20)
            return run.returncode, run.stdout, run.stderr
        except subprocess.TimeoutExpired:
            return "timeout", b"", b""

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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("workspace")
    ap.add_argument("--bin", required=True)
    ap.add_argument("--crate", action="append", required=True)
    ap.add_argument("--bisect", action="store_true")
    ap.add_argument("--json")
    a = ap.parse_args()

    fx = Fixture(a.workspace, a.bin, a.crate)
    fx.rustc_build()
    expected = fx.behaviour(fx.reference)
    fx.rustd_build()
    ok = fx.matches(expected)
    result = {"fixture": a.bin, "match": ok, "crates": {}}
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
        if a.bisect:
            result["culprits"] = isolate(fx, expected)
            for c in result["culprits"]:
                print(f"  MISCOMPILED {c['function']}: {c['symptom']}")
    if a.json:
        json.dump(result, open(a.json, "w"), indent=1)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
