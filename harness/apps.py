#!/usr/bin/env python3
"""apps.py: whole applications built by rustd, dependencies included, against cargo's build.

usage: apps.py <name> [--stage build,workload,perf,tests,edits] [--json out.json]
       apps.py --table <dir of json>

<name> is an entry of apps/apps.json. The checkout (APPS_DIR/<name>, cloned at `rev`) gets
every dependency of the registry as a path dependency (`cargo vendor` and a `[patch]` table in
`.cargo/config.toml`), which makes each a crate rustd compiles. The stages:

build     cargo builds the program for the link line; rustd compiles every crate linked into
          it and links its objects ahead of rustc's. Then the same with rustd as cargo's
          `RUSTC_WRAPPER`, where rustc builds only what runs during the build: that one is
          timed from nothing and is the program the other stages use when it links.
workload  each command line of `runs`, cargo's program against rustd's: status, stdout, stderr.
perf      `bench` timed with cargo's debug and release programs and rustd's; sizes.
tests     the test binaries of `tests`: those of a library are linked again with rustd's
          objects, those that start the program are run with rustd's program in its place.
edits     the daemon holds the program; each edit of `edits` is timed to a linked program and
          to cargo's, and `check` has to print the same from both.

The backend is the environment's, as for the other harnesses: `RUSTD_BACKEND`, `RUSTD_OPT`,
`RUSTD_EMIT`.
"""
import argparse, json, os, re, resource, shlex, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import difftest, linkline
from difftest import Fixture, RUSTD, bucket, host, sysroot_rlibs

APPS_DIR = os.environ.get("APPS_DIR") or os.path.join(HERE, "vendor", "apps")
SPECS = os.path.join(HERE, "apps", "apps.json")
# A dependency at a path is linted as the workspace is, and one that denies warnings must
# still build.
FLAGS = "-C link-dead-code --cap-lints allow"
STAGES = ["build", "workload", "perf", "tests", "edits"]


def sh(argv, cwd, env=None, check=True, timeout=None):
    """Runs a command, returning (seconds, result)."""
    began = time.monotonic()
    ran = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)
    if check and ran.returncode != 0:
        raise SystemExit(f"`{shlex.join(argv)}` failed:\n{(ran.stdout + ran.stderr)[-3000:]}")
    return time.monotonic() - began, ran


def checkout(name, spec):
    """The application's source at the pinned commit, with its dependencies beside it."""
    ws = os.path.join(APPS_DIR, name)
    if not os.path.isdir(ws):
        os.makedirs(ws)
        if "repo" in spec:
            for argv in (["git", "init", "-q"], ["git", "fetch", "-q", "--depth", "1", spec["repo"], spec["rev"]],
                         ["git", "checkout", "-q", "FETCH_HEAD"],
                         ["git", "submodule", "update", "-q", "--init", "--recursive", "--depth", "1"]):
                sh(argv, ws)
        else:
            shutil.copytree(os.path.join(HERE, "apps", spec["source"]), ws, dirs_exist_ok=True)
    return os.path.join(ws, spec.get("dir", ""))


def localize(ws):
    """Makes every package of the registry or of a git repository one at a path."""
    config = os.path.join(ws, ".cargo", "config.toml")
    marker = "# apps.py: dependencies at paths\n"
    if os.path.exists(config) and marker in open(config).read():
        return
    vendor = os.path.abspath(ws).rstrip("/") + "-vendor"
    sh(["cargo", "vendor", "--versioned-dirs", vendor], ws)
    meta = json.loads(sh(["cargo", "metadata", "--format-version", "1"], ws)[1].stdout)
    tables = {}
    for package in meta["packages"]:
        if not package["source"]:
            continue
        kind, url = package["source"].split("+", 1)
        key = "crates-io" if kind == "registry" else json.dumps(re.split("[?#]", url)[0])
        directory = os.path.join(vendor, f"{package['name']}-{package['version']}")
        entry = f'{json.dumps(package["name"] + "-" + package["version"])} = {{ package = "{package["name"]}", path = {json.dumps(directory)} }}'
        tables.setdefault(key, []).append(entry)
    os.makedirs(os.path.dirname(config), exist_ok=True)
    with open(config, "a") as out:
        out.write("\n" + marker)
        for key, entries in tables.items():
            out.write(f"[patch.{key}]\n" + "\n".join(sorted(entries)) + "\n")
    sh(["cargo", "metadata", "--format-version", "1"], ws)


class App(Fixture):
    """A program and every crate linked into it."""

    def __init__(self, ws, spec, alone):
        self.spec = spec
        super().__init__(ws, [], ["-p", spec["package"], *spec.get("cargo_args", [])])
        self.alone = alone
        self.reports = {}
        self.scrubbed = spec.get("scrub", [])
        self.argv0 = spec.get("bin", spec["package"])
        self.run_env = {**os.environ, **spec.get("env", {})}

    def rustc_build(self):
        """Cargo's build, with rustd in rustc's place for every crate when the program is
        rustd's alone: a dependency at a path is not of the workspace to cargo."""
        if self.alone:
            os.environ.update(RUSTC_WRAPPER=RUSTD)
        try:
            began = time.monotonic()
            super().rustc_build()
            self.cargo_secs = time.monotonic() - began
        finally:
            os.environ.pop("RUSTC_WRAPPER", None)
        linked = {os.path.basename(r)[3:].rsplit("-", 1)[0] for r in self.rlibs
                  if r.endswith(".rlib") and "/rustlib/" not in r}
        program = os.path.basename(self.reference).rsplit("-", 1)[0].replace("-", "_")
        self.crates = [",".join([program, *sorted(linked - {program})])]

    def rustd_build(self, deny=()):
        """As the fixture's, keeping what the process took: seconds and peak resident memory."""
        crate = self.crates[0]
        obj, report, log = (os.path.join(self.work, f"all.{ext}") for ext in ("o", "tsv", "stderr"))
        symbols = [x for rlib in self.rlibs + sysroot_rlibs() for x in ("--symbols", rlib)]
        argv = [RUSTD, self.ws, *(x for c in crate.split(",") for x in ("--crate", c)), "--out", obj,
                "--report", report, *self.rustd_args, *symbols]
        began = time.monotonic()
        with open(log, "w") as err:
            child = subprocess.Popen(argv, stdout=subprocess.DEVNULL, stderr=err)
            _, status, usage = os.wait4(child.pid, 0)
        self.rustd_secs, self.rustd_kb = time.monotonic() - began, usage.ru_maxrss
        if status != 0:
            raise SystemExit(f"rustd crashed:\n{open(log).read()[-3000:]}")
        self.reports = {crate: [l.rstrip("\n").split("\t") for l in open(report)]}
        self.objects = [obj]
        began = time.monotonic()
        self.link(self.objects)
        self.link_secs = time.monotonic() - began

    def behaviour(self, binary):
        try:
            run = subprocess.run([self.argv0, *self.run_args], executable=binary, capture_output=True,
                                 stdin=subprocess.DEVNULL, timeout=600, cwd=self.ws, env=self.run_env)
        except subprocess.TimeoutExpired:
            return "timeout", b"", b""
        return run.returncode, self.scrub(run.stdout), self.scrub(run.stderr)

    def scrub(self, output):
        """What a program measures (elapsed times) differs from run to run."""
        for pattern in self.scrubbed:
            output = re.sub(pattern.encode(), b"<scrubbed>", output)
        return output

    def summary(self):
        """Functions compiled and left, the ones left by reason."""
        rows = self.reports[self.crates[0]]
        reasons, examples = {}, {}
        for row in rows:
            if row[0] == "skipped" and bucket(row[2]) != "no rustc symbol for function":
                why = cause(row[2])
                reasons[why] = reasons.get(why, 0) + 1
                examples.setdefault(why, f"{row[1]}: {row[2][:300]}")
        return {"compiled": sum(r[0] == "compiled" for r in rows), "left": sum(reasons.values()),
                "reasons": reasons, "examples": examples, "instances": int(rows[0][2]),
                "codegen_s": round(int(rows[0][4]) / 1e6, 1), "crates": len(self.crates[0].split(","))}


def cause(reason):
    """Why a function was left, without what differs from one function to the next: the
    chain of callees it was found through, and the names in backquotes."""
    last = re.split(r"callee `[^`]*`: ", reason)[-1]
    last = re.sub(r"^(unsupported|frontend|backend): ", "", last)
    return re.sub(r"`[^`]*`", "`..`", last)[:120]


def build(ws, spec, result, alone):
    """One build of the program by rustd; returns the fixture, or None with the failure noted."""
    key = "alone" if alone else "ahead"
    fx = App(ws, spec, alone)
    try:
        fx.rustc_build()
        fx.rustd_build()
    except SystemExit as failed:
        result[key] = {"error": str(failed)[-1500:], "work": fx.work, **(fx.summary() if fx.reports else {})}
        missing = sorted(set(re.findall(r"undefined symbol: (.*)", str(failed))))
        print(f"{key}: FAILED {str(failed)[-300:]}")
        for symbol in missing[:20]:
            print(f"  undefined {symbol[:200]}")
        reasons(result[key])
        return None
    result[key] = {**fx.summary(), "cargo_s": round(fx.cargo_secs, 1), "rustd_s": round(fx.rustd_secs, 1),
                   "link_s": round(fx.link_secs, 1), "peak_mb": fx.rustd_kb // 1024,
                   "size": os.path.getsize(fx.binary), "work": fx.work}
    row = result[key]
    print(f"{key}: {row['compiled']} functions from rustd over {row['crates']} crates, {row['left']} left; "
          f"rustd {row['rustd_s']} s, link {row['link_s']} s, peak {row['peak_mb']} MB")
    reasons(row)
    return fx


def reasons(row):
    """Prints why functions were left, the most frequent reason first."""
    for reason, count in sorted(row.get("reasons", {}).items(), key=lambda it: -it[1]):
        print(f"  {count:5} {reason}\n        {row['examples'][reason]}")


def reference(ws, spec, profile):
    """Cargo's own build of the program without the harness's flags, timed when it is from
    nothing; returns (program, seconds or None)."""
    target = os.path.join(ws, "target-" + profile)
    name = spec.get("bin", spec["package"])
    program = os.path.join(target, profile, name)
    cold = not os.path.exists(program)
    env = {**os.environ, "CARGO_TARGET_DIR": target, "RUSTFLAGS": "--cap-lints allow"}
    argv = ["cargo", "build", "-p", spec["package"], *spec.get("cargo_args", []),
            *(["--release"] if profile == "release" else [])]
    secs, _ = sh(argv, ws, env)
    return program, round(secs, 1) if cold else None


def setup(ws, spec):
    """What the command lines read is made once."""
    done = os.path.join(ws, ".apps-setup")
    if not os.path.exists(done):
        for line in spec.get("setup", []):
            sh(["bash", "-c", line], ws)
        open(done, "w").close()


def workload(fx, spec, debug, result):
    differing = []
    for run in spec["runs"]:
        fx.run_args = run
        want, got = fx.behaviour(debug), fx.behaviour(fx.binary)
        if want != got:
            differing.append(shlex.join(run))
            tail = got[2].decode(errors="replace").strip().splitlines()[-2:]
            print(f"  DIFFERS `{shlex.join(run)}`: rustc status {want[0]}, rustd status {got[0]}: "
                  f"{difftest.symptom(want, got)} {' | '.join(tail)[:300]}")
    result["workload"] = {"runs": len(spec["runs"]), "differing": differing}
    print(f"workload: {len(spec['runs']) - len(differing)} of {len(spec['runs'])} command lines the same")


def best(fx, binary, argv, rounds=3):
    """The fastest of a few runs, in seconds."""
    times = []
    for _ in range(rounds):
        began = time.monotonic()
        subprocess.run([fx.argv0, *argv], executable=binary, cwd=fx.ws, capture_output=True,
                       stdin=subprocess.DEVNULL, env=fx.run_env)
        times.append(time.monotonic() - began)
    return round(min(times), 3)


def perf(fx, spec, debug, release, result):
    argv = spec["bench"]
    result["perf"] = {"bench": shlex.join(argv), "rustd_s": best(fx, fx.binary, argv),
                      "debug_s": best(fx, debug, argv), "release_s": best(fx, release, argv),
                      "debug_size": os.path.getsize(debug), "release_size": os.path.getsize(release)}
    row = result["perf"]
    print(f"perf `{row['bench']}`: rustd {row['rustd_s']} s, rustc debug {row['debug_s']} s, release {row['release_s']} s")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("name", nargs="?")
    ap.add_argument("--stage", default="build,workload,perf")
    ap.add_argument("--json")
    ap.add_argument("--table")
    a = ap.parse_args()
    if a.table:
        import appstable
        return appstable.main(a.table)
    spec = json.load(open(SPECS))[a.name]
    stages = a.stage.split(",")
    out = os.path.abspath(a.json) if a.json else None
    ws = checkout(a.name, spec)
    localize(ws)
    os.environ["RUSTFLAGS"] = FLAGS
    backend = os.environ.get("RUSTD_BACKEND", "cranelift")
    result = {"app": a.name, "backend": backend, "rev": spec.get("rev")}
    debug, result["debug_cold_s"] = reference(ws, spec, "debug")
    setup(ws, spec)
    ahead = build(ws, spec, result, alone=False)
    alone = build(ws, spec, result, alone=True) if ahead else None
    fx = alone or ahead
    result["program"] = "alone" if alone else "ahead" if ahead else None
    ok = fx is not None
    if fx and "workload" in stages:
        workload(fx, spec, debug, result)
        ok = ok and not result["workload"]["differing"]
    if fx and "perf" in stages and "bench" in spec:
        release, result["release_cold_s"] = reference(ws, spec, "release")
        perf(fx, spec, debug, release, result)
    if fx and "tests" in stages:
        import appstests
        ok = appstests.run(fx, ws, spec, result) and ok
    if fx and "edits" in stages:
        import appsedits
        ok = appsedits.run(fx, ws, spec, result) and ok
    result["ok"] = ok
    if out:
        json.dump(result, open(out, "w"), indent=1)
    print("OK" if ok else "FAILED")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
