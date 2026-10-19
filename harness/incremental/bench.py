#!/usr/bin/env python3
"""Wall time of a test run after an edit, three ways, in two copies of one project:

    cargo         `cargo test ARGS`, warm and incremental
    rustd, all    `cargo rustd test ARGS --rerun-all`: the daemon builds, every test runs
    rustd         `cargo rustd test ARGS`: the daemon builds, the tests the edit reaches run

    bench.py NAME --source DIR --work DIR --args "..." --edit FILE:LINE:OLD:NEW ... [--runs 7]

Each edit is made and undone `--runs` times, and each of the two is one sample for each
way, so the three ways see the same edits in the same order. A sample is the whole command,
build and tests. `none` as an edit is a run with nothing changed. The copies are warmed by
two runs each, which are not samples. Prints medians and writes every sample to
WORK/NAME.bench.jsonl.
"""

import json, os, shutil, statistics, subprocess, sys, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUSTD = os.environ.get("RUSTD_BIN") or str(HERE.parent.parent / "target/release/rustd")
WAYS = {"cargo": ("b", ["cargo", "test"], []), "rustd, all": ("a", ["cargo", "rustd", "test"], ["--rerun-all"]),
        "rustd": ("a", ["cargo", "rustd", "test"], [])}


def main():
    argv = sys.argv[1:]
    name = argv[0]
    flag = lambda f, d=None: argv[argv.index(f) + 1] if f in argv else d
    source, work = Path(flag("--source")).resolve(), Path(flag("--work")).resolve()
    args, runs = flag("--args", "").split(), int(flag("--runs", "7"))
    edits = [argv[i + 1] for i, a in enumerate(argv) if a == "--edit"]
    copies = {side: work / side / name for side in "ab"}
    for copy in copies.values():
        if copy.exists():
            shutil.rmtree(copy)
        shutil.copytree(source, copy, symlinks=True, ignore=shutil.ignore_patterns("target", ".git"))
    base = {k: v for k, v in os.environ.items() if not k.startswith("RUSTD_")}
    envs = {"b": base, "a": dict(base, RUSTD_HOME=str(work / "rustd-home"), RUSTD_FIRST_BUILD="1",
                                 PATH=f"{Path(RUSTD).parent}:{base['PATH']}")}
    log, rows = open(work / f"{name}.bench.log", "w"), open(work / f"{name}.bench.jsonl", "w")

    def sample(way, label):
        side, command, more = WAYS[way]
        start = time.time()
        done = subprocess.run([*command, *args, "--no-fail-fast", *more], cwd=copies[side], env=envs[side],
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors="replace")
        took = time.time() - start
        log.write(f"\n$ [{label}, {way}] exit {done.returncode}, {took:.3f}s\n{done.stdout}")
        cached = done.stdout.count(" (cached)\n")
        ran = done.stdout.count(" ... ok\n") + done.stdout.count(" ... FAILED\n")
        return dict(way=way, label=label, secs=round(took, 3), code=done.returncode, ran=ran, cached=cached,
                    by_cargo="building with cargo" in done.stdout or "rustd did not build" in done.stdout)

    for _ in range(2):
        for way in WAYS:
            sample(way, "warm-up")
    print(f"{name}: cargo test {' '.join(args)}; medians of {runs * 2} samples (min-max), seconds")
    for edit in edits:
        samples = {way: [] for way in WAYS}
        for turn in range(runs * 2):
            if edit != "none":
                rel, line, old, new = edit.split(":", 3)
                old, new = (old, new) if turn % 2 == 0 else (new, old)
                for copy in copies.values():
                    lines = (copy / rel).read_text().split("\n")
                    assert old in lines[int(line) - 1], (edit, lines[int(line) - 1])
                    lines[int(line) - 1] = lines[int(line) - 1].replace(old, new, 1)
                    (copy / rel).write_text("\n".join(lines))
            # The selective run first: `--rerun-all` after it has nothing left to build.
            for way in ("cargo", "rustd", "rustd, all"):
                row = sample(way, edit)
                samples[way].append(row)
                rows.write(json.dumps(row) + "\n")
        rows.flush()
        print(f"  {edit}")
        for way, got in samples.items():
            secs = [it["secs"] for it in got]
            ran, cached = statistics.median(it["ran"] for it in got), statistics.median(it["cached"] for it in got)
            by_cargo = sum(it["by_cargo"] for it in got)
            print(f"    {way:11} {statistics.median(secs):7.2f}  ({min(secs):.2f}-{max(secs):.2f})  ran {ran:.0f}, cached {cached:.0f}"
                  + (f", {by_cargo} of {len(got)} built by cargo or refused" if by_cargo else ""), flush=True)


main()
