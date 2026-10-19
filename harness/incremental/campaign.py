#!/usr/bin/env python3
"""campaign.py: does `cargo rustd test` skip only tests whose result an edit cannot change?

    campaign.py PROJECT --source DIR --work DIR [--mutations N] [--seed S] [--limit N]

PROJECT is `hazards` (the workspace beside this file, one case per way a result can depend
on something), or the name of a checkout in --source to mutate: N lines of its source are
changed one at a time (`==` to `!=`, `true` to `false`, ...) and changed back.

Two copies are made in --work: `a`, which `cargo rustd test` builds and runs, and `b`, the
reference, which plain `cargo test` builds with rustc into a target directory of its own.
After every edit, made to both, three commands run:

    sel   a: cargo rustd test ARGS --no-fail-fast               (what is being judged)
    all   a: cargo rustd test ARGS --no-fail-fast --rerun-all   (every test of the same build)
    ref   b: cargo test ARGS --no-fail-fast                     (rustc's build, every test)

A step fails when a test `sel` did not run ("cached") has another result in `all` or in
`ref` (a miss), or when the final results of `sel` and `ref` differ. A step where rustd
says it cannot make the build, and runs nothing, is counted apart as refused. A test whose
result in `ref` differs from the step before and that `sel` did not run is counted as
recalled: `sel` had its result from an earlier step with the same code. Every step is one line of
JSON in --work/PROJECT.jsonl, and the full output of every command is in --work/PROJECT.log.
"""
import json, os, random, re, shutil, subprocess, sys, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUSTD = os.environ.get("RUSTD_BIN") or str(HERE.parent.parent / "target/release/rustd")
TOOLCHAIN = os.environ.get("RUSTUP_TOOLCHAIN", "1.99.0")

LINE = re.compile(r"^test (.+?) \.\.\. (ok|FAILED|ignored)(?:, [^(]*)?( \(cached\))?\s*$")
RUNNING = re.compile(r"^\s*Running (.+?) \(.*?([^/]+?)-[0-9a-f]{16}\)\s*$")
DOC = re.compile(r"^\s*Doc-tests (\S+)")


def parse(text):
    """{program: {test: (result, cached)}} and the programs with no harness of their own."""
    programs, current = {}, None
    for line in text.splitlines():
        line = re.sub(r"\x1b\[[0-9;]*m", "", line)
        m = RUNNING.match(line) or DOC.match(line)
        if m:
            current = programs.setdefault(" ".join(m.groups()), {})
            continue
        m = LINE.match(line)
        if m and current is not None:
            name = m.group(1).removesuffix(" - should panic")
            current[name] = (m.group(2), bool(m.group(3)))
    return programs


def run(cmd, cwd, env, log, label):
    start = time.time()
    done = subprocess.run(cmd, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    took = time.time() - start
    log.write(f"\n$ [{label}] {' '.join(cmd)}  (in {cwd}, {took:.2f}s, exit {done.returncode})\n{done.stdout}")
    log.flush()
    refused = next((l.strip() for l in done.stdout.splitlines() if "rustd did not build" in l), None)
    return dict(code=done.returncode, took=round(took, 3), results=parse(done.stdout), refused=refused,
                said=sorted({l.strip() for l in done.stdout.splitlines() if re.match(r"^(\x1b\[[0-9;]*m)*rustd:", l)}))


def flat(results):
    return {(p, t): r for p, tests in results.items() for t, r in tests.items()}


class Campaign:
    def __init__(self, name, source, work, args):
        self.name, self.args = name, args
        self.a, self.b = work / "a" / name, work / "b" / name
        for copy in (self.a, self.b):
            if copy.exists():
                shutil.rmtree(copy)
            shutil.copytree(source, copy, ignore=shutil.ignore_patterns("target", ".git"))
        self.log = open(work / f"{name}.log", "w")
        self.rows = open(work / f"{name}.jsonl", "w")
        home = work / "rustd-home"
        base = {k: v for k, v in os.environ.items() if not k.startswith("RUSTD_")}
        base["RUSTUP_TOOLCHAIN"] = TOOLCHAIN
        self.plain = dict(base)
        self.rustd = dict(base, RUSTD_HOME=str(home), RUSTD_FIRST_BUILD="1",
                          PATH=f"{Path(RUSTD).parent}:{base['PATH']}")
        self.before, self.failures, self.refusals, self.steps = None, [], [], 0

    def step(self, label, kind, extra_args=(), extra_env=None):
        args = ["test", *self.args, *extra_args, "--no-fail-fast"]
        envs = [dict(e, **(extra_env or {})) for e in (self.rustd, self.rustd, self.plain)]
        sel = run(["cargo", "rustd", *args], self.a, envs[0], self.log, f"{label} sel")
        all_ = run(["cargo", "rustd", *args, "--rerun-all"], self.a, envs[1], self.log, f"{label} all")
        ref = run(["cargo", *args], self.b, envs[2], self.log, f"{label} ref")
        s, a, r = flat(sel["results"]), flat(all_["results"]), flat(ref["results"])
        cached = {k for k, v in s.items() if v[1]}
        misses = sorted(k for k in cached if (k in a and a[k][0] != s[k][0]) or (k in r and r[k][0] != s[k][0]))
        changed = set()
        if self.before is not None:
            changed = {k for k in set(r) | set(self.before) if r.get(k, ("-",))[0] != self.before.get(k, ("-",))[0]}
        # Served from memory though the result is not that of the step before: right when the
        # code is back to what it was when the result was recorded, which `misses` decides.
        recalled = sorted(changed & cached)
        final = sorted(k for k in set(s) | set(r) if s.get(k, ("-",))[0] != r.get(k, ("-",))[0])
        unstable = sorted(k for k in set(s) & set(a) if k not in cached and s[k][0] != a[k][0])
        self.before = r
        row = dict(step=self.steps, label=label, kind=kind, tests=len(r), cached=len(cached), ran=len(s) - len(cached),
                   changed=len(changed), failed_ref=sum(v[0] == "FAILED" for v in r.values()),
                   misses=[list(k) for k in misses], recalled=[list(k) for k in recalled],
                   final_differs=[list(k) for k in final], unstable=[list(k) for k in unstable],
                   codes=[sel["code"], all_["code"], ref["code"]], secs=[sel["took"], all_["took"], ref["took"]],
                   said=sel["said"])
        # A build rustd says it cannot make ran nothing and answered nothing: a gap, not a miss.
        refused = sel["refused"] if not s and sel["code"] != 0 else None
        ok = bool(refused) or (not misses and not final and (sel["code"] == 0) == (ref["code"] == 0))
        row["ok"], row["refused"] = ok, refused
        if refused:
            self.refusals.append(label)
        if not ok:
            self.failures.append(label)
        self.rows.write(json.dumps(row) + "\n")
        self.rows.flush()
        self.steps += 1
        print(f"{'FAIL' if not ok else 'gap ' if refused else 'ok  '} {self.steps:3} {label:44} tests {len(r):4} cached {len(cached):4} ran {row['ran']:4} "
              f"changed {len(changed):3} | sel {sel['took']:6.2f}s all {all_['took']:6.2f}s ref {ref['took']:6.2f}s"
              + (f" | {'; '.join(sel['said'])[:90]}" if sel["said"] else "") + (f" | {refused[-110:]}" if refused else "")
              + (f" | MISS {misses[:3]}" if misses else "") + (f" | recalled {len(recalled)}" if recalled else "")
              + (f" | DIFFERS {final[:3]}" if final and not refused else ""), flush=True)

    def write(self, rel, text):
        for copy in (self.a, self.b):
            path = copy / rel
            if text is None:
                path.unlink()
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)

    def edit(self, label, kind, rel, old, new, revert=True, **how):
        before = (self.a / rel).read_text() if (self.a / rel).exists() else None
        if old is None:
            after = new
        else:
            assert before.count(old) >= 1, f"{label}: `{old}` is not in {rel}"
            after = before.replace(old, new, 1)
        # Cargo compares times to the second on some filesystems.
        time.sleep(1.1)
        self.write(rel, after)
        self.step(label, kind, **how)
        if revert:
            time.sleep(1.1)
            self.write(rel, before)
            self.step(f"{label} (undone)", kind, **how)


def hazards(c):
    lib, shapes = "src/lib.rs", "src/shapes.rs"
    c.step("first run", "none")
    c.step("no edit", "none")
    e = c.edit
    e("body of a tested function", "body", lib, "    a + b\n", "    a + b + 1\n")
    e("body no test reaches", "body", lib, "    5\n}", "    6\n}", revert=False)
    e("function in a table of pointers", "fn pointer", lib, "    x * 3\n", "    x * 4\n")
    e("which pointers the table holds", "static", lib, "[double, triple]", "[triple, double]")
    e("const", "const", lib, "pub const LIMIT: u32 = 10;", "pub const LIMIT: u32 = 11;")
    e("static", "static", lib, 'GREETING: &str = "hi"', 'GREETING: &str = "yo"')
    e("const fn in an array length", "const", lib, "    4\n}\npub static BUFFER", "    5\n}\npub static BUFFER")
    e("what two tests add to a shared static", "shared state", lib, "fn step() -> u32 {\n    1", "fn step() -> u32 {\n    5")
    e("initial value of a thread local", "static", lib, "    3\n}\n\n// Registration", "    4\n}\n\n// Registration")
    e("function run before main", "ctor", lib, "    11\n}", "    12\n}")
    e("function drop calls", "drop glue", lib, "    9\n}", "    8\n}")
    e("body of Drop::drop", "drop glue", lib, "self.0.set(dropped_value());", "self.0.set(dropped_value() + 1);")
    e("generic only tests instantiate", "generic", lib, "if item > best", "if item < best")
    e("macro_rules body", "macro", lib, "$x * $x", "$x + $x")
    e("field type (layout)", "layout", lib, "pub len: u32,", "pub len: u64,")
    e("field added to a struct no test measures", "layout", lib, "    pub first: u8,", "    pub first: u8,\n    pub more: u8,", revert=False)
    e("enum variants reordered", "layout", lib, "    Stop,\n    Go,", "    Go,\n    Stop,")
    e("type alias", "type", lib, "pub type Id = u8;", "pub type Id = u16;")
    e("field order under a derive", "type", lib, "    pub major: u32,\n    pub minor: u32,", "    pub minor: u32,\n    pub major: u32,")
    e("import names another function", "name resolution", lib, "use first::pick;", "use second::pick;")
    e("impl overrides a default method", "trait selection", lib, "impl Describe for Fancy {}",
      'impl Describe for Fancy {\n    fn describe(&self) -> &\'static str {\n        "fancy"\n    }\n}')
    e("inherent method shadows a trait method", "trait selection", lib, "impl Describe for Plain {}",
      'impl Describe for Plain {}\nimpl Plain {\n    pub fn describe(&self) -> &\'static str {\n        "inherent"\n    }\n}')
    e("default method body", "trait", lib, '        "default"', '        "changed"')
    e("associated const", "const", lib, "const RATE: u32 = 2;", "const RATE: u32 = 3;")
    e("file read by include_str!", "include_str", "fixtures/included.txt", "hello", "bye")
    e("file read when the test runs", "fixture", "fixtures/read.txt", "runtime", "other")
    e("file rewritten with the same text", "fixture", "fixtures/read.txt", "runtime", "runtime", revert=False)
    e("new file nothing reads", "fixture", "fixtures/new.txt", None, "new\n")
    c.step("environment variable set", "env", extra_env={"HAZ_MODE": "set"})
    c.step("environment variable unset again", "env")
    e("message of an expected panic", "should_panic", lib, 'panic!("zero")', 'panic!("none")')
    e("expected panic no longer happens", "should_panic", lib, "if x == 0 {", "if x == u32::MAX {")
    e("#[ignore] removed", "ignore", lib, "    #[ignore]\n", "")
    e("signature and callers' types", "signature", lib, "pub fn scale(x: u32, by: u32) -> u32 {\n    x * by",
      "pub fn scale(x: u32, by: u64) -> u64 {\n    x as u64 * by + 1")
    e("build script output", "build script", "build.rs", "HAZ_BUILD=7", "HAZ_BUILD=8")
    e("proc macro", "proc macro", "macros/src/lib.rs", "42u32", "43u32")
    e("workspace dependency body", "dependency", "helper/src/lib.rs", "x + 100", "x + 200")
    e("workspace dependency generic body", "dependency", "helper/src/lib.rs", "(x.clone(), x.clone())", "{ let y = x.clone(); (y.clone(), y) }")
    e("exported symbol called by name", "symbol", lib, "    x + 1\n}\nextern", "    x + 2\n}\nextern")
    e("closure body", "body", lib, "y + 5", "y + 6")
    e("function compiled only for tests", "test-only", lib, "    21\n", "    22\n")
    e("cfg(test) version of a function", "cfg", lib, '    "test"\n', '    "tests"\n')
    e("cfg(not(test)) version of a function", "cfg", lib, '    "normal"\n', '    "norm"\n', revert=False)
    e("trait object: method of one impl", "dyn", shapes, "3 * self.0 * self.0", "4 * self.0 * self.0")
    e("trait object: method of another impl", "dyn", shapes, "self.0 * self.0\n", "self.0 + self.0 + 1\n")
    e("test added", "test list", lib, "    #[test]\n    fn independent()", "    #[test]\n    fn added() {\n        assert_eq!(add(1, 1), 2);\n    }\n    #[test]\n    fn independent()")
    e("body of a test", "test body", lib, "assert_eq!(1 + 1, 2);", "assert_eq!(1 + 1, 3);")
    e("function added and called", "new function", lib, '    format!("hazards {name}")\n}',
      '    format!("hazards {name}{}", suffix())\n}\nfn suffix() -> &\'static str {\n    "!"\n}')
    e("program an integration test runs", "binary", "src/main.rs", 'println!("{}",', 'println!("{}!",')
    e("integration test body", "test body", "tests/it.rs", "assert_eq!(hazards::add(1, 2), 3);", "assert_eq!(hazards::add(1, 2), 4);")
    e("test without the harness", "harness = false", "tests/custom.rs", "add(2, 3), 5", "add(2, 3), 6")
    e("doc test", "doc test", lib, "//! assert_eq!(hazards::add(1, 2), 3);", "//! assert_eq!(hazards::add(1, 2), 4);")
    e("comment added", "no change", lib, "pub fn add(", "// Adds.\npub fn add(", revert=False)
    e("lines added above everything", "no change", lib, "use std::cell::Cell;", "\n\n\nuse std::cell::Cell;", revert=False)
    c.step("feature turned on", "feature", extra_args=["--features", "extra"])
    c.step("feature turned off again", "feature")
    e("type error", "broken", lib, "    a + b\n", '    a + "x"\n')


OPERATORS = [(" == ", " != "), (" != ", " == "), ("true", "false"), ("false", "true"), (" && ", " || "),
             (" < ", " >= "), (" > ", " <= "), (" + 1", " + 2"), ("Some(", "None.or(")]


def mutations(c, count, seed):
    """One-line changes to code that is not test code, each made and undone."""
    places = []
    for path in sorted(c.a.rglob("*.rs")):
        rel = path.relative_to(c.a)
        if {"tests", "benches", "examples", "target", "fuzz"} & set(rel.parts) or rel.name == "build.rs":
            continue
        for at, line in enumerate(path.read_text().splitlines()):
            if "#[cfg(test)]" in line or line.startswith("mod tests"):
                break
            code = line.split("//")[0]
            if code.strip().startswith(("#", "use ", "assert", "debug_assert")):
                continue
            places += [(str(rel), at, old, new) for old, new in OPERATORS if old in code]
    random.Random(seed).shuffle(places)
    c.step("first run", "none")
    c.step("no edit", "none")
    for rel, at, old, new in places[:count]:
        lines = (c.a / rel).read_text().split("\n")
        before = "\n".join(lines)
        lines[at] = lines[at].replace(old, new, 1)
        time.sleep(1.1)
        c.write(rel, "\n".join(lines))
        label = f"{rel}:{at + 1} `{old.strip()}`→`{new.strip()}`"
        c.step(label, "mutation")
        time.sleep(1.1)
        c.write(rel, before)
        c.step(f"{label} (undone)", "mutation")


def main():
    argv = sys.argv[1:]
    flag = lambda name, default=None: argv[argv.index(name) + 1] if name in argv else default
    name = argv[0]
    work = Path(flag("--work")).resolve()
    work.mkdir(parents=True, exist_ok=True)
    if name == "hazards":
        c = Campaign(name, HERE / "hazards", work, flag("--args", "-p hazards -p helper").split())
        hazards(c)
    else:
        c = Campaign(name, Path(flag("--source")).resolve(), work, flag("--args", "").split())
        mutations(c, int(flag("--mutations", "25")), int(flag("--seed", "1")))
    print(f"{c.steps} steps, {len(c.failures)} failed, {len(c.refusals)} refused by rustd"
          + "".join(f"\n  failed: {it}" for it in c.failures) + "".join(f"\n  refused: {it}" for it in c.refusals))
    sys.exit(1 if c.failures else 0)


if __name__ == "__main__":
    main()
