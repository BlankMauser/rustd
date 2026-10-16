#!/usr/bin/env python3
"""includes.py: a daemon follows edits to a file that only `include!` names.

The file is not a `.rs` file, so nothing loads it until the macro is expanded. Each step edits
it or the file that includes it, asks the daemon for the program and compares what it prints
with what `cargo build` of the same source prints.
"""
import os, shlex, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from difftest import Fixture, RUSTD, sysroot_rlibs, RUNNER, profile_dir

TOML = """[package]
name = "sinc"
version = "0.1.0"
edition = "2021"
"""
DATA_TEXT = """pub const TABLE: [u32; 3] = [3, 5, 8];
pub fn pick(i: usize) -> u32 {
    TABLE[i % 3] * 2
}
macro_rules! make {
    ($name:ident, $v:expr) => {
        pub fn $name() -> u32 {
            $v + pick(1)
        }
    };
}
"""
MAIN_TEXT = """mod data {
    include!("table.rs.data");
    make!(made, 100);
}

fn main() {
    println!("{} {} {:?}", data::pick(2), data::made(), data::TABLE);
}
"""
DATA, MAIN = "src/table.rs.data", "src/main.rs"
# (what the step is, [(file, old, new)], whether cargo rejects it)
STEPS = [
    ("nothing edited", [], False),
    ("a body in the included file", [(DATA, "* 2", "* 7")], False),
    ("a constant in the included file", [(DATA, "[3, 5, 8]", "[4, 6, 9]")], False),
    ("a macro in the included file", [(DATA, "$v + pick(1)", "$v * 2 + pick(0)")], False),
    ("the included file broken", [(DATA, "* 7", '* "x"')], True),
    ("mended", [(DATA, '* "x"', "* 11")], False),
    ("a function added to it and used", [
        (DATA, "macro_rules!", "pub fn more() -> u32 {\n    77\n}\nmacro_rules!"),
        (MAIN, "fn main() {", 'fn main() {\n    println!("{}", data::more());'),
    ], False),
    ("the including file alone", [(MAIN, "make!(made, 100)", "make!(made, 200)")], False),
    ("the included file again", [(DATA, "77", "78")], False),
]


def follow(files, steps):
    """Edits a crate called `sinc` a step at a time under a daemon, against cargo."""
    ws = tempfile.mkdtemp(prefix="rustd-includes-")
    os.makedirs(os.path.join(ws, "src"))
    for path, text in (("Cargo.toml", TOML), *files):
        open(os.path.join(ws, path), "w").write(text)
    work = tempfile.mkdtemp(prefix="rustd-includes-out-")
    socket, log_path = os.path.join(work, "rustd.sock"), os.path.join(work, "daemon.log")
    reference = os.path.join(work, "reference")
    fx = Fixture(ws, ["sinc"], ["-p", "sinc"])
    fx.rustc_build()
    obj, patch, binary = (os.path.join(work, f"sinc.{n}") for n in ("o", "patch.o", "bin"))
    link = list(fx.link_args)
    first = next(i for i, a in enumerate(link) if a.endswith((".rlib", ".rcgu.o")))
    link[first:first] = [patch, obj, "-Wl,--allow-multiple-definition"]
    link[link.index("-o") + 1] = binary
    symbols = [x for rlib in fx.rlibs + sysroot_rlibs() for x in ("--symbols", rlib)]
    # RUSTD_SPLICE: edits are added to the linked binary where that can be done.
    splice = ["--splice", binary] if os.environ.get("RUSTD_SPLICE") else []
    ask = ["--crate", "sinc", "--out", obj, "--patch", patch, "--link-cmd", shlex.join(link), *splice, *symbols]
    out = open(log_path, "a")
    daemon = subprocess.Popen([RUSTD, ws, "--socket", socket, *ask], stderr=out, stdout=out)
    while "watching" not in open(log_path).read():
        if daemon.poll() is not None:
            raise SystemExit(f"the daemon did not start: {log_path}")
        time.sleep(0.05)

    def run(path):
        try:
            ran = subprocess.run([*RUNNER, path], capture_output=True, text=True, timeout=30)
        except Exception as error:
            return f"did not run: {error}"
        return ran.stdout + (f"[exit {ran.returncode}]" if ran.returncode else "")

    wrong = 0
    for what, edits, broken in steps:
        for path, old, new in edits:
            text = open(os.path.join(ws, path)).read()
            assert old in text, (what, old)
            open(os.path.join(ws, path), "w").write(text.replace(old, new, 1))
        time.sleep(0.02)
        began = time.time()
        reply = subprocess.run([RUSTD, "sync", socket, *ask], capture_output=True, text=True, timeout=120)
        took = (time.time() - began) * 1000
        reply = (reply.stdout + reply.stderr).strip()
        cargo = subprocess.run(["cargo", "build", "-q"], cwd=ws, capture_output=True, text=True,
                               env={**os.environ, "CARGO_TARGET_DIR": reference})
        assert (cargo.returncode != 0) == broken, cargo.stderr[-2000:]
        if daemon.poll() is not None:
            raise SystemExit(f"the daemon died: {log_path}")
        if broken:
            verdict = "refused" if not reply.startswith("ok") else "ANSWERED OK"
        elif not reply.startswith("ok"):
            verdict = "REFUSED"
        else:
            verdict = "same" if run(binary) == run(os.path.join(profile_dir(reference), "sinc")) else "DIFFERENT OUTPUT"
        wrong += verdict.isupper()
        print(f"{what:40} {verdict:16} {took:6.0f} ms  {reply[:90]}", flush=True)
    daemon.kill()
    print(f"{len(steps) - wrong} of {len(steps)} right; log {log_path}")
    sys.exit(1 if wrong else 0)


if __name__ == "__main__":
    follow([(DATA, DATA_TEXT), (MAIN, MAIN_TEXT)], STEPS)
