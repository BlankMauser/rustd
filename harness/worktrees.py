#!/usr/bin/env python3
"""worktrees.py: several checkouts of one workspace built by one daemon.

The stress workspace is copied COUNT times. The daemon is started in the first copy with the
others as `--also`; each step edits files of some checkouts and asks for the program of some by
`rustd sync SOCKET --project ...`. Every program asked for is run and compared with what
`cargo build` of that checkout prints.

With `--join` the daemon is started in the first copy alone, and the others join it when they
are first asked for. With `--together` the programs of a step are asked for at once.
"""
import os, shlex, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from difftest import Fixture, RUSTD, sysroot_rlibs

COUNT = 3
MID, APP = "mid/src/lib.rs", "app/src/main.rs"
AREA = "sbase::square!(self.side)"
SUM = "items.iter().sum()"
# (what the step is, [(checkout, file, old, new)], checkouts asked for in order, the ones cargo rejects)
STEPS = [
    ("nothing edited, the others asked for the first time", [], [1, 2, 0], []),
    ("nothing edited, all again", [], [0, 1, 2], []),
    ("one checkout edited", [(0, MID, AREA, AREA + " + 1")], [0, 1, 2], []),
    ("the same edit in another", [(1, MID, AREA, AREA + " + 1")], [1, 0, 2], []),
    ("a different edit to the same code in the third", [(2, MID, AREA, AREA + " + 7")], [2, 1, 0], []),
    ("two checkouts edited before either is asked",
     [(0, MID, AREA + " + 1", AREA + " + 2"), (1, APP, SUM, "items.iter().sum::<u32>() + 1")], [1, 0, 2], []),
    ("one checkout broken", [(1, MID, AREA + " + 1", AREA + ' + "x"')], [1, 0, 2, 1], [1]),
    ("another edited while it is broken", [(2, APP, SUM, "items.iter().sum::<u32>() + 5")], [2, 1, 0], [1]),
    ("mended", [(1, MID, AREA + ' + "x"', AREA + " + 4")], [0, 1, 2], []),
    ("a function added in one",
     [(2, APP, "fn main() {", "fn extra() -> u32 {\n    41\n}\n\nfn main() {\n    println!(\"extra {}\", extra());")],
     [2, 0, 1], []),
    ("one put back as another is", [(2, MID, AREA + " + 7", AREA + " + 2")], [2, 0], []),
]


def main():
    work = tempfile.mkdtemp(prefix="rustd-worktrees-")
    socket, log_path = os.path.join(work, "rustd.sock"), os.path.join(work, "daemon.log")
    crates = ["sapp", "sbase", "smid"]
    checkouts = []
    for at in range(COUNT):
        ws = os.path.join(work, f"checkout{at}")
        shutil.copytree(os.path.join(HERE, "stress"), ws, ignore=shutil.ignore_patterns("target"))
        fx = Fixture(ws, crates, ["-p", "sapp"])
        fx.rustc_build()
        obj, patch, binary = (os.path.join(work, f"sapp{at}.{n}") for n in ("o", "patch.o", "bin"))
        link = list(fx.link_args)
        first = next(i for i, a in enumerate(link) if a.endswith((".rlib", ".rcgu.o")))
        link[first:first] = [patch, obj, "-Wl,--allow-multiple-definition"]
        link[link.index("-o") + 1] = binary
        symbols = [x for rlib in fx.rlibs + sysroot_rlibs() for x in ("--symbols", rlib)]
        checkouts.append({"ws": ws, "obj": obj, "patch": patch, "bin": binary, "link": shlex.join(link),
                          "symbols": symbols, "reference": os.path.join(work, f"reference{at}")})
    first = checkouts[0]
    out = open(log_path, "a")
    daemon = subprocess.Popen(
        [RUSTD, first["ws"], *[x for c in crates for x in ("--crate", c)], "--out", first["obj"], "--socket", socket,
         "--patch", first["patch"], "--link-cmd", first["link"], *first["symbols"],
         *[x for other in checkouts[1:] if "--join" not in sys.argv for x in ("--also", other["ws"])]],
        stderr=out, stdout=out)
    while "watching" not in open(log_path).read():
        if daemon.poll() is not None:
            raise SystemExit(f"the daemon did not start: {log_path}")
        time.sleep(0.05)

    def run(path):
        try:
            ran = subprocess.run([path], capture_output=True, text=True, timeout=30)
        except Exception as error:
            return f"did not run: {error}"
        return ran.stdout + (f"[exit {ran.returncode}]" if ran.returncode else "")

    wrong = asked = 0
    for what, edits, order, broken in STEPS:
        for at, path, old, new in edits:
            path = os.path.join(checkouts[at]["ws"], path)
            text = open(path).read()
            assert old in text, (what, old)
            open(path, "w").write(text.replace(old, new, 1))
        time.sleep(0.02)
        print(what, flush=True)
        together = "--together" in sys.argv

        def ask(at):
            checkout = checkouts[at]
            return time.time(), subprocess.Popen(
                [RUSTD, "sync", socket, "--project", checkout["ws"], "--crate", "sapp", "--out", checkout["obj"],
                 "--patch", checkout["patch"], "--link-cmd", checkout["link"], *checkout["symbols"]],
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)

        # A program asked for twice in a step is asked for the second time after the others.
        asking = {at: ask(at) for at in order} if together else {}
        for at in order:
            checkout = checkouts[at]
            began, reply = asking.pop(at, None) or ask(at)
            reply = reply.communicate(timeout=120)[0].strip()
            took = (time.time() - began) * 1000
            cargo = subprocess.run(["cargo", "build", "-q", "-p", "sapp"], cwd=checkout["ws"], capture_output=True,
                                   text=True, env={**os.environ, "CARGO_TARGET_DIR": checkout["reference"]})
            assert (cargo.returncode != 0) == (at in broken), cargo.stderr[-2000:]
            if daemon.poll() is not None:
                verdict = "DAEMON DIED"
            elif at in broken:
                verdict = "refused" if not reply.startswith("ok") else "ANSWERED OK"
            elif not reply.startswith("ok"):
                verdict = "REFUSED"
            else:
                same = run(checkout["bin"]) == run(os.path.join(checkout["reference"], "debug", "sapp"))
                verdict = "same" if same else "DIFFERENT OUTPUT"
            asked += 1
            wrong += verdict.isupper()
            print(f"    checkout {at} {verdict:16} {took:6.0f} ms  {reply[:90]}", flush=True)
            if daemon.poll() is not None:
                raise SystemExit(f"the daemon died: {log_path}")
    daemon.kill()
    print(f"{asked - wrong} of {asked} right; log {log_path}")
    sys.exit(1 if wrong else 0)


if __name__ == "__main__":
    main()
