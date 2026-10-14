#!/usr/bin/env python3
"""Edit latency of the daemon on crates of a dioxus checkout.

usage: dxedits.py CHECKOUT crate[,crate...] SYMBOLS_FILE

Starts the daemon on the crates, waits for the first build, applies each edit and reports the
time the daemon took to rebuild and how many functions it compiled again.
"""
import os, re, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
os.environ.setdefault("RUSTUP_TOOLCHAIN", open(os.path.join(HERE, "toolchain")).read().strip())
os.environ["RUSTD_STATS"] = "1"
RUSTD = os.path.join(HERE, "../target/release/rustd")
BODY = "let _rustd_edit = 1u32;"
NEW_FN = "\nfn rustd_new_item(x: u32) -> u32 { x + 1 }\n"


def main():
    root, crates, symbols = sys.argv[1], sys.argv[2].split(","), open(sys.argv[3]).read().split()
    core = os.path.join(root, "packages/core/src")
    edits = [
        ("blank line at top", f"{core}/virtual_dom.rs", lambda t: "\n" + t),
        ("comment appended", f"{core}/virtual_dom.rs", lambda t: t + "// edit\n"),
        ("1 function body (mark_dirty)", f"{core}/virtual_dom.rs",
         lambda t: t.replace("pub fn mark_dirty(&mut self, id: ScopeId) {\n", "pub fn mark_dirty(&mut self, id: ScopeId) {\n" + BODY + "\n", 1)),
        ("1 function body (rebuild, generic)", f"{core}/virtual_dom.rs",
         lambda t: t.replace("pub fn rebuild(&mut self, to: &mut impl crate::MultiWriter) {\n", "pub fn rebuild(&mut self, to: &mut impl crate::MultiWriter) {\n" + BODY + "\n", 1)),
        ("new private fn", f"{core}/virtual_dom.rs", lambda t: t + NEW_FN),
        ("new private fn in lib.rs", f"{core}/lib.rs", lambda t: t + NEW_FN),
    ]
    work = os.path.join(root, "work")
    log_path = os.path.join(work, "dxedits.log")
    open(log_path, "w").close()
    out = open(log_path, "a")
    log = open(log_path)
    args = [x for c in crates for x in ("--crate", c)]
    start = time.time()
    daemon = subprocess.Popen([RUSTD, os.path.join(root, "packages/cli"), *args, "--out", os.path.join(work, "dx.o"), "--watch", *symbols],
                              stderr=out, stdout=out)

    def wait(pattern, seen):
        while True:
            log.seek(0)
            found = re.findall(pattern, log.read())
            if len(found) > seen:
                return found
            if daemon.poll() is not None:
                log.seek(0)
                raise SystemExit(f"daemon died:\n{log.read()[-3000:]}")
            time.sleep(0.01)

    done = 0
    try:
        wait(r"watching \d+ files", 0)
        rss = int(re.search(r"VmRSS:\s+(\d+)", open(f"/proc/{daemon.pid}/status").read())[1]) // 1024
        print(f"warmup: {time.time() - start:.1f}s, {rss} MB")
        log.seek(0)
        print(re.search(r"cold: .*", log.read())[0])
        for name, path, apply in edits:
            original = open(path).read()
            edited = apply(original)
            assert edited != original, name
            for label, text in ((name, edited), ("  (revert)", original)):
                time.sleep(0.3)
                open(path, "w").write(text)
                found = wait(r"edit \(apply (\S+)\): compiled (\d+) of (\d+) fns \(\+(\d+) instances\) in (\S+) .*\n(?:   .*\n)*stats: (\d+) of (\d+) functions compiled.*\n.*\n(?:link: .*\n)?edit-to-binary: (\S+)", done)
                done = len(found)
                b = found[-1]
                print(f"{label:38} edit-to-object {b[7]:>8}  recompiled {b[5]:>5} of {b[6]}")
        rss = int(re.search(r"VmRSS:\s+(\d+)", open(f"/proc/{daemon.pid}/status").read())[1]) // 1024
        print(f"rss after edits: {rss} MB")
    finally:
        daemon.kill()


main()
