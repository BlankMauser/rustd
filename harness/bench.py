#!/usr/bin/env python3
"""bench.py: what a build costs through `cargo rustd`, phase by phase, against plain cargo.

    bench.py [PROJECT...] [--backend cranelift|llvm]... [--runs N] [--out FILE] [--work DIR]
             [--reference DIR] [--keep]
    bench.py --compare BASELINE RESULT [--threshold PERCENT]
    bench.py --shapes [PROJECT...] [--mode rustd|plain] [--backend B] [--runs N] [--only SHAPE]... [--session N] [--think SECS]... [--out FILE]
    bench.py --report RESULT...

Each project is one pinned checkout (cloned into `RUSTD_PROJECTS` as cargo_rustd.py does, and
copied; the clone is never written to). Under each backend, with a store, a daemon and a home
of its own, and plain cargo building the same source into a target directory of its own (under `--reference` when
that is given, which lets the two be on different disks):
  cold       `cargo rustd build` from nothing, then the wait until the daemon has the
             workspace loaded and its first build linked
  no-op      `cargo rustd build` with nothing changed
  body       one function's body is edited
  signature  a function gets another parameter, and every caller the argument
  leaf       a body in a crate of the workspace that the others depend on
  generic    the body of a generic function that is used at many types
Every edit is made and taken back `--runs` times, or once when the daemon turned it down and
cargo built it; the time of each is the median. None of
them changes what the program does: after the first of each kind the program is run and has
to say what cargo's says.

For every phase the result has the wall time of the command, cargo's time for the same
step, the most memory the daemon's workers have held so far, who built it (the daemon, or
cargo and why), whether the daemon added the change to the linked program or linked again
(and why), how many functions it compiled, and what it timed itself: the steps of the build
one after another, what its threads spent on each step of compiling, and why a build started
over when it did.

`--shapes` is the larger measurement, of `cargo rustd` (`--mode rustd`) or of plain cargo
after `rustd enable` (`--mode plain`) against cargo alone, on the projects of `LARGE`: three
workspaces genws.py makes (10, 50 and 200 crates) and pinned checkouts. Cargo alone is
cargo as it is installed: no wrapper, the dev profile, incremental compilation, and a target
directory of its own that an earlier build of the same tree filled. A third build of every
step is cargo with mold as the linker (`MOLD`), in a third target directory. On top of the
phases above it has
  scatter3, scatter10, scatter30   a body edited in that many crates of the workspace at once
  new_fn, new_type                 a function or a type added to a library and used
  field                            a field added to a struct
  test_body, test_leaf             `cargo test` after an edit to the program and to a library
Every edit is made and taken back `--runs` times (5). After each build the program is run
(the tests' results are read) on each side and has to be what cargo alone gives, and
nothing is timed until the daemon and whatever it started have gone quiet. `--session N`
then times, in fresh copies with nothing built, a clean build followed by N edits one after
another, under cargo alone and under rustd with `--think` seconds between the edits.
`--report FILE...` prints the tables of results as Markdown.

`--compare` reads two results and exits 1 when a phase of the second is slower or larger
than in the first by more than the threshold, was built by cargo where the daemon built it,
was linked again where it was added, or is missing. Differences under 50 ms are not counted.

The rustd of a backend is `RUSTD_BIN` (cranelift) or `RUSTD_LLVM_BIN` (llvm).
"""
import glob, hashlib, json, os, platform, re, shutil, statistics, subprocess, sys, tempfile, time

import cargo_rustd, genws
from cargo_rustd import HERE, ROOT, TOOLCHAIN, Side, enabled_home, fetch, how_built, program_output, test_results, wait_warm

LET = "\n    let _bench_edit = 1u32;"


def body(file, opening):
    """An edit that adds a statement after the line that opens a function."""
    return [(file, opening, opening + LET)]


def parameter(file, name, returns, callers):
    """An edit that gives the method `name` a parameter and every call in `callers` the argument."""
    old = f"fn {name}(&self) -> {returns} {{"
    return [(file, old, old.replace("(&self)", "(&self, _bench_edit: ())")), (callers, f".{name}()", f".{name}(())")]


PROJECTS = {
    "ripgrep": dict(
        repo="BurntSushi/ripgrep", tag="15.1.0", rev="af60c2de9d85e7f3d81c78601669468cf02dabab",
        run=["-n", "--no-heading", "--sort", "path", "fn main", "crates/core/main.rs", "build.rs"],
        edits=dict(
            body=body("crates/core/main.rs", "fn search(args: &HiArgs, mode: SearchMode) -> anyhow::Result<bool> {"),
            signature=parameter("crates/printer/src/standard.rs", "write_line_term", "io::Result<()>", "crates/printer/src/standard.rs"),
            leaf=body("crates/matcher/src/lib.rs", "pub fn len(&self) -> usize {"),
            generic=[("crates/matcher/src/lib.rs", "        let mut last_end = at;", "        let _bench_edit = 1u32;\n        let mut last_end = at;")],
        ),
    ),
    "cargo": dict(
        repo="rust-lang/cargo", tag="0.92.0", rev="ea2d97820c16195b0ca3fadb4319fe512c199a43",
        run=["--version"],
        edits=dict(
            body=body("src/cargo/ops/cargo_compile/mod.rs", "pub fn compile<'a>(ws: &Workspace<'a>, options: &CompileOptions) -> CargoResult<Compilation<'a>> {"),
            signature=parameter("src/cargo/util/context/mod.rs", "cli_unstable", "&CliUnstable", "src/**/*.rs"),
            leaf=body("crates/cargo-util/src/paths.rs", "pub fn normalize_path(path: &Path) -> PathBuf {"),
            generic=body("src/cargo/util/context/mod.rs", "pub fn get<'de, T: serde::de::Deserialize<'de>>(&self, key: &str) -> CargoResult<T> {"),
        ),
    ),
    "nushell": dict(
        repo="nushell/nushell", tag="0.108.0", rev="da141be11b3acaa55b39321e5f9651d02aa2ed6f",
        run=["-n", "-c", "[3 1 2] | sort | str join ','"],
        edits=dict(
            body=body("src/main.rs", "fn main() -> Result<()> {"),
            signature=parameter("crates/nu-protocol/src/value/mod.rs", "coerce_bool", "Result<bool, ShellError>", "crates/**/*.rs"),
            leaf=body("crates/nu-path/src/components.rs", "pub fn components(path: &Path) -> impl Iterator<Item = Component<'_>> {"),
            generic=[("crates/nu-protocol/src/pipeline/pipeline_data.rs", "        F: FnMut(Value) -> Value + 'static + Send,\n    {",
                      "        F: FnMut(Value) -> Value + 'static + Send,\n    {\n        let _bench_edit = 1u32;")],
        ),
    ),
}

BACKENDS = {
    "cranelift": dict(RUSTD_BACKEND="cranelift"),
    "llvm": dict(RUSTD_BACKEND="llvm", RUSTD_OPT="2", RUSTD_EMIT="fast"),
}
DURATION = re.compile(r"([0-9.]+)(ns|µs|ms|s)\b")
UNITS = {"ns": 1e-9, "µs": 1e-6, "ms": 1e-3, "s": 1.0}
FLOOR_SECS = 0.05


def rustd_of(backend):
    if backend == "llvm":
        return os.environ.get("RUSTD_LLVM_BIN") or os.path.join(ROOT, "target-llvm/release/rustd")
    return cargo_rustd.RUSTD


def seconds(text):
    """The seconds a duration as Rust prints it stands for."""
    number, unit = DURATION.search(text).groups()
    return float(number) * UNITS[unit]


def write_edit(ws, changes, back):
    """Makes every change of an edit, or takes it back. The result is how many places changed."""
    places = 0
    for pattern, old, new in changes:
        if old is None:
            # Text for the end of the file.
            path = os.path.join(ws, pattern)
            text = open(path).read()
            assert text.endswith(new) == back, f"{path} does not end as the edit left it"
            open(path, "w").write(text[:-len(new)] if back else text + new)
            places += 1
            continue
        old, new = (new, old) if back else (old, new)
        for path in sorted(glob.glob(os.path.join(ws, pattern), recursive=True)):
            text = open(path).read()
            if old in text:
                places += text.count(old)
                open(path, "w").write(text.replace(old, new))
    assert places >= len(changes), f"an edit found only {places} places: {changes}"
    return places


class Daemon:
    """The daemon's workers for one checkout: their memory and what they logged."""

    def __init__(self, side):
        self.side, self.read, self.peak = side, {}, 0

    def workers(self):
        _, status = self.side.cargo("status")
        return [(int(pid), log) for pid, log in re.findall(r"worker: pid (\d+), .*, log (.*)$", status.stdout, re.M)]

    def peak_mb(self):
        """The most memory the workers have held at once so far, summed over those there are."""
        total = 0
        for pid, _ in self.workers():
            try:
                status = open(f"/proc/{pid}/status").read()
            except OSError:
                continue
            total += int(re.search(r"VmHWM:\s+(\d+) kB", status)[1])
        self.peak = max(self.peak, round(total / 1024))
        return self.peak

    def said(self, builds=0):
        """What the workers logged since the last call, once `builds` edits are in it."""
        for _ in range(100):
            logs = {log: open(log, "rb").read() for _, log in self.workers() if os.path.exists(log)}
            text = "".join(whole[self.read.get(log, 0):].decode(errors="replace") for log, whole in logs.items())
            if text.count("edit-to-binary:") >= builds:
                break
            time.sleep(0.02)
        self.read.update((log, len(whole)) for log, whole in logs.items())
        return text


def link_kind(text):
    """How the builds in `text` reached the program: added to it, or linked again and why."""
    links = re.findall(r"^link: \S+ \((.*)\)$", text, re.M)
    if not links:
        return None
    again = [l for l in links if not l.startswith("spliced")]
    if not again:
        return "spliced"
    why = re.search(r"not spliced: (.*)", again[0])
    return "full link" + (f": {why[1][:160]}" if why else "")


def daemon_steps(text):
    """The steps the daemon timed in `text`, in milliseconds, summed over its builds."""
    steps = {}
    for line in re.findall(r"^timeline-us: (.*)$", text, re.M):
        for name, micros in (pair.split("=") for pair in line.split()):
            # How long ago the file was saved is not the daemon's time.
            if name != "notice":
                steps[name] = round(steps.get(name, 0) + int(micros) / 1000, 2)
    return steps


def cold_steps(text):
    """The steps of loading a workspace and building it for the first time, in seconds."""
    steps = {}
    for name, pattern in (("load", r"^load workspace: (\S+)"), ("resolve", r"^resolve: \d+ crates in (\S+)"),
                          ("compile", r"^cold: compiled .* in (\S+) \("), ("emit", r"^cold: compiled .*\(\+(\S+) emit\)"),
                          ("link", r"^link: (\S+) \(")):
        found = re.findall(pattern, text, re.M)
        if found:
            steps[name] = round(sum(seconds(f) for f in found), 3)
    return steps


def thread_steps(text):
    """What the daemon's threads spent on each step of compiling a function and on each of
    the producer's, in seconds summed over the threads and the builds in `text`."""
    steps = {}
    for line in re.findall(r"^steps: (.*)$", text, re.M):
        for name, took in re.findall(r"([a-z ]+) ([0-9.]+(?:ns|µs|ms|s))(?:,|$)", line):
            steps[name.strip()] = round(steps.get(name.strip(), 0) + seconds(took), 3)
    for producer, took in re.findall(r"^backend: (\w+) compiled \d+ functions, \d+ bytes, (\S+) ", text, re.M):
        steps[producer] = round(steps.get(producer, 0) + seconds(took), 3)
    return steps


def rounds(text):
    """Why the builds in `text` started over, each time they did."""
    return re.findall(r"^round \d+: \S+, started over: (.*)$", text, re.M)


def compiled(text):
    """How many functions the builds in `text` compiled."""
    return sum(int(n) for n in re.findall(r"^stats: (\d+) of \d+ functions compiled", text, re.M))


def measure(name, spec, backend, runs, work, reference_dir):
    """Runs every phase of one project under one backend. The result is the phases by name."""
    rustd = rustd_of(backend)
    ws = os.path.realpath(os.path.join(work, f"{name}-{backend}"))
    shutil.copytree(fetch(name, spec), ws, symlinks=True, ignore=shutil.ignore_patterns("target", "target-ref", ".git"))
    env = cargo_rustd.base_env(os.path.join(work, f"home-{name}-{backend}"))
    env["PATH"] = os.path.dirname(rustd) + os.pathsep + env["PATH"]
    env.update(BACKENDS[backend], RUSTD_STATS="1", RUSTD_CACHE=os.path.join(work, f"store-{name}-{backend}"))
    ours, theirs = Side(ws, env, True), Side(ws, env, False)
    if reference_dir:
        theirs.env["CARGO_TARGET_DIR"] = os.path.join(reference_dir, f"target-ref-{name}-{backend}")
    daemon, phases, wrong = Daemon(ours), {}, []
    subprocess.run(["cargo", "fetch"], cwd=ws, env=env, capture_output=True)
    ours.cargo("stop")

    def both():
        (took, mine), (reference, other) = ours.cargo("build"), theirs.cargo("build")
        for side, ran in (("rustd", mine), ("cargo", other)):
            if ran.returncode != 0:
                raise SystemExit(f"{name} {backend}: the {side} build failed:\n{ran.stderr[-3000:]}")
        built = how_built(mine.stderr)
        if built != "daemon":
            # A build cargo did has the daemon load again; the next one is not timed against that.
            wait_warm(ours, ws)
        return took, reference, built

    def runs_right(label):
        _, mine = ours.cargo("run", "--", *spec["run"])
        if program_output(mine) != expected:
            wrong.append(f"{label}: the program says {program_output(mine)!r}")

    def report(label, phase):
        phases[label] = phase
        print(f"  {label:10} rustd {phase['wall_s']:8.3f}s  cargo {phase['cargo_s']:8.3f}s  rss {phase['rss_mb']:6} MB  "
              f"{phase['built'][:60]}  {phase.get('link') or ''}", flush=True)

    # Cargo's own cold build waits until the daemon is warm: the two would share the cores.
    took, mine = ours.cargo("build")
    warm, states = wait_warm(ours, ws)
    if mine.returncode != 0 or states != ["ready"]:
        raise SystemExit(f"{name} {backend}: the daemon did not get ready: {states}\n{mine.stderr[-3000:]}")
    said, built = daemon.said(), how_built(mine.stderr)
    reference, other = theirs.cargo("run", "--", *spec["run"])
    expected = program_output(other)
    report("cold", dict(wall_s=round(took + warm, 3), cargo_s=round(reference, 3), rss_mb=daemon.peak_mb(), built=built,
                        build_s=round(took, 3), warm_s=round(warm, 3), compiled=compiled(said), steps_s=cold_steps(said),
                        threads_s=thread_steps(said), started_over=rounds(said)))

    samples = [both() for _ in range(max(runs, 3))]
    said = daemon.said()
    report("no-op", dict(wall_s=round(statistics.median(s[0] for s in samples), 3), cargo_s=round(statistics.median(s[1] for s in samples), 3),
                         rss_mb=daemon.peak_mb(), built=samples[-1][2], compiled=compiled(said)))
    runs_right("no-op")

    for kind, changes in spec["edits"].items():
        made, back, first = [], [], None
        for turn in range(runs):
            places = write_edit(ws, changes, False)
            took, reference, built = both()
            said = daemon.said(1 if built == "daemon" else 0)
            made.append(dict(wall_s=took, cargo_s=reference, built=built, link=link_kind(said), compiled=compiled(said), steps_ms=daemon_steps(said),
                             threads_s=thread_steps(said), started_over=rounds(said)))
            if turn == 0:
                runs_right(kind)
                daemon.said()
            write_edit(ws, changes, True)
            took, reference, built = both()
            said = daemon.said(1 if built == "daemon" else 0)
            back.append(dict(wall_s=took, cargo_s=reference, built=built, link=link_kind(said), compiled=compiled(said)))
            # An edit the daemon turned down is one finding, not a time to take a median of,
            # and each has it load the workspace again.
            if made[-1]["built"] != "daemon":
                break
        # The slowest of them is the one to look at; the median is the one to compare.
        slowest = max(made, key=lambda s: s["wall_s"])
        median = lambda rows, key: round(statistics.median(r[key] for r in rows), 3)
        report(kind, dict(wall_s=median(made, "wall_s"), cargo_s=median(made, "cargo_s"), rss_mb=daemon.peak_mb(),
                          built=slowest["built"], link=slowest["link"], compiled=max(s["compiled"] for s in made), places=places,
                          steps_ms=slowest["steps_ms"], threads_s=slowest["threads_s"], started_over=slowest["started_over"], slowest_s=round(slowest["wall_s"], 3),
                          back_s=median(back, "wall_s"), back_cargo_s=median(back, "cargo_s"),
                          back_link=max(back, key=lambda s: s["wall_s"])["link"],
                          samples_s=[round(s["wall_s"], 3) for s in made]))
    ours.cargo("stop")
    return dict(phases=phases, wrong=wrong, tag=spec["tag"])


# The larger measurement (`--shapes`).

MOLD = os.environ.get("RUSTD_BENCH_MOLD") or shutil.which("mold")
SHAPES = ["body", "leaf", "scatter3", "scatter10", "scatter30", "signature", "generic", "new_fn", "new_type", "field", "test_body", "test_leaf", "test_leaf_lib"]
# The shapes a session goes through, each made and later taken back.
SESSION = ["body", "leaf", "scatter3", "generic", "signature"]
# Seconds after which a shape that has three runs is not run again.
SHAPE_BUDGET = 1500
BUILDERS = {"cargo", "rustc", "cc", "gcc", "clang", "ld", "ld.lld", "rust-lld", "lld", "mold", "collect2", "rustdoc", "cargo-rustd", "rustd-rustc"}
ADDED_FN = "\nfn bench_added(x: u32) -> u32 {\n    let mut acc = x;\n    for i in 0..4 {\n        acc = acc.wrapping_mul(31).wrapping_add(i);\n    }\n    acc\n}\n"
ADDED_TYPE = ("\nstruct BenchAdded {\n    parts: Vec<u32>,\n    name: String,\n}\n\nimpl BenchAdded {\n    fn total(&self) -> usize {\n"
              "        self.parts.iter().map(|p| *p as usize).sum::<usize>() + self.name.len()\n    }\n}\n")
PLAIN_FN = re.compile(r"^((?:    )?(?:pub(?:\(crate\))? )?fn \w+\([^()]*\)(?: -> [^{;()]+)? \{)\n", re.M)

LARGE = {
    "gen10": dict(generate=10, bin="app", run=["7"]),
    "gen50": dict(generate=50, bin="app", run=["7"]),
    "gen200": dict(generate=200, bin="app", run=["7"]),
    "nushell": dict(
        PROJECTS["nushell"], package="nu", bin="nu",
        edits=dict(
            PROJECTS["nushell"]["edits"],
            # A second body deep in the graph: the first is one the daemon hands to cargo.
            leaf2=body("crates/nu-utils/src/deansi.rs", "pub fn strip_ansi_unlikely(string: &str) -> Cow<'_, str> {"),
            field=[("crates/nu-path/src/components.rs", "pub fn components(path: &Path) -> impl Iterator<Item = Component<'_>> {",
                    "pub fn components(path: &Path) -> impl Iterator<Item = Component<'_>> {\n    let _bench_edit = BenchField { was: 1 }.was;"),
                   ("crates/nu-path/src/components.rs", None, "\nstruct BenchField {\n    was: u64,\n}\n")],
        ),
        tests=dict(test_leaf=("leaf", ["-p", "nu-path"]), test_leaf_lib=("leaf", ["-p", "nu-path", "--lib"])),
    ),
    "cargo": dict(
        PROJECTS["cargo"], package="cargo", bin="cargo",
        tests=dict(test_leaf=("leaf", ["-p", "cargo-util"]), test_leaf_lib=("leaf", ["-p", "cargo-util", "--lib"])),
    ),
    "ripgrep": dict(
        PROJECTS["ripgrep"], package="ripgrep", bin="rg",
        tests=dict(test_leaf=("leaf", ["-p", "grep-matcher"]), test_leaf_lib=("leaf", ["-p", "grep-matcher", "--lib"])),
    ),
    "helix": dict(
        cargo_rustd.PROJECTS["helix"], package="helix-term", bin="hx",
        edits=dict(
            body=body("helix-term/src/main.rs", "fn setup_logging(verbosity: u64) -> Result<()> {"),
            signature=parameter("helix-core/src/selection.rs", "primary", "Range", "helix-*/src/**/*.rs"),
            leaf=body("helix-stdx/src/env.rs", "pub fn env_var_is_set(env_var_name: &str) -> bool {"),
            generic=body("helix-stdx/src/env.rs", "pub fn binary_exists<T: AsRef<OsStr>>(binary_name: T) -> bool {"),
        ),
        tests=dict(test_leaf=("leaf", ["-p", "helix-stdx"]), test_leaf_lib=("leaf", ["-p", "helix-stdx", "--lib"])),
    ),
    "rust-analyzer": dict(
        cargo_rustd.PROJECTS["rust-analyzer"], package="rust-analyzer", bin="rust-analyzer", run=["--help"],
        edits=dict(
            body=body("crates/rust-analyzer/src/bin/main.rs", "fn actual_main() -> anyhow::Result<ExitCode> {"),
            signature=parameter("crates/syntax/src/lib.rs", "syntax_node", "SyntaxNode", "crates/**/*.rs"),
            leaf=body("crates/stdx/src/lib.rs", "pub fn to_lower_snake_case(s: &str) -> String {"),
            generic=body("crates/syntax/src/algo.rs", "pub fn find_node_at_offset<N: AstNode>(syntax: &SyntaxNode, offset: TextSize) -> Option<N> {"),
        ),
        tests=dict(test_leaf=("leaf", ["-p", "stdx"]), test_leaf_lib=("leaf", ["-p", "stdx", "--lib"])),
    ),
}


class Failed(Exception):
    """A step that cannot be timed: a build that did not succeed."""


def added(leaf, text, use):
    """The edit that adds `text` to the end of the file of the edit `leaf` and `use` to the
    function `leaf` edits."""
    file, opening, _ = leaf[0]
    return [(file, opening, opening + use), (file, None, text)]


def members(ws, env, package):
    """The libraries of the workspace that `package` is built from, by name: where the
    sources of each are."""
    listed = subprocess.run(["cargo", "metadata", "--format-version", "1", "--offline"], cwd=ws, env=env, capture_output=True, text=True)
    if listed.returncode != 0:
        listed = subprocess.run(["cargo", "metadata", "--format-version", "1"], cwd=ws, env=env, capture_output=True, text=True, check=True)
    data = json.loads(listed.stdout)
    ours = set(data["workspace_members"])
    packages = {p["id"]: p for p in data["packages"]}
    nodes = {n["id"]: n for n in data["resolve"]["nodes"]}
    start = [p for p in ours if packages[p]["name"] == package]
    reached, todo = set(), start[:1]
    while todo:
        at = todo.pop()
        if at in reached:
            continue
        reached.add(at)
        # What is only built for build scripts and tests is not in the program.
        todo += [d["pkg"] for d in nodes[at]["deps"] if any(k["kind"] is None for k in d["dep_kinds"])]
    found = {}
    for at in sorted(reached & ours):
        for target in packages[at]["targets"]:
            if target["kind"] == ["lib"] and packages[at]["name"] != package:
                found[packages[at]["name"]] = os.path.dirname(target["src_path"])
    return found


def scatter(ws, env, spec, n):
    """An edit to one body in each of `n` libraries of the workspace, spread over them by
    name, or `None` when there are not that many to edit."""
    if "generate" in spec:
        return genws.scattered(ws, n)
    places = []
    for name, src in sorted(members(ws, env, spec["package"]).items()):
        files = sorted(glob.glob(os.path.join(src, "**/*.rs"), recursive=True), key=lambda f: (not f.endswith("lib.rs"), f.count(os.sep), f))
        for path in files:
            text = open(path, errors="replace").read()
            head = text.split("#[cfg(test)]")[0]
            if "macro_rules!" in head or re.search(r"/tests?/|/benches/", path[len(src):]):
                continue
            # The opening has to be the one place that reads so, and not behind a `cfg`.
            for found in PLAIN_FN.finditer(head):
                before = head[:found.start()].splitlines()[-1:] or [""]
                if text.count(found[1]) == 1 and not before[0].strip().startswith("#["):
                    places.append((os.path.relpath(path, ws), found[1], found[1] + LET))
                    break
            else:
                continue
            break
    if n > len(places):
        return None
    return [places[i * len(places) // n] for i in range(n)]


def shapes_of(ws, env, spec):
    """Every shape of the project: the edit, the cargo command and its arguments. A shape
    that cannot be made is `None`."""
    build = spec.get("build", [])
    if "generate" in spec:
        made = genws.edits(ws)
        middle = json.load(open(os.path.join(ws, "genws.json")))["middle"]
        edits = dict(body=made["body"], leaf=made["deep"], signature=made["signature"], generic=made["generic"],
                     new_fn=made["new_fn"], new_type=made["new_type"], field=made["field"])
        tests = dict(test_body=(made["body"], ["-p", "app"]), test_leaf=(made["middle"], ["-p", middle]), test_leaf_lib=(made["middle"], ["-p", middle, "--lib"]))
    else:
        edits = dict(spec["edits"])
        leaf = edits["leaf"]
        edits["new_fn"] = added(leaf, ADDED_FN, "\n    let _bench_added = bench_added(1);")
        edits["new_type"] = added(leaf, ADDED_TYPE, "\n    let _bench_added = BenchAdded { parts: vec![1, 2], name: String::new() }.total();")
        tests = {label: (edits[edit], args) for label, (edit, args) in spec.get("tests", {}).items()}
    for n in (3, 10, 30):
        edits[f"scatter{n}"] = scatter(ws, env, spec, n)
    # A project may have further edits of its own, which are built after the others.
    shapes = {label: (edits.get(label), "build", build) for label in [l for l in SHAPES if not l.startswith("test_")] + [l for l in edits if l not in SHAPES]}
    shapes.update({label: (tests[label][0], "test", tests[label][1]) if label in tests else (None, "test", []) for label in SHAPES if label.startswith("test_")})
    return shapes


def populate(ws, name, spec):
    """Makes the checkout `ws`: generated, or a copy of the pinned clone."""
    if "generate" in spec:
        return genws.generate(ws, spec["generate"])
    shutil.copytree(fetch(name, spec), ws, symlinks=True, ignore=shutil.ignore_patterns("target", "target-ref", ".git"))


def tuned_env(env, ws):
    """Cargo alone with mold as the linker, into a target directory of its own."""
    flags = f"-C link-self-contained=-linker -C linker=clang -C link-arg=-fuse-ld={MOLD}"
    return dict(env, RUSTFLAGS=flags, CARGO_TARGET_DIR=os.path.join(ws, "target-mold"))


def quiet(limit=600):
    """Waits until nothing builds and no rustd process uses a processor. The result is the
    seconds it waited."""
    began, last, still = time.time(), None, 0
    while time.time() - began < limit:
        busy, ticks = False, 0
        for pid in filter(str.isdigit, os.listdir("/proc")):
            try:
                stat = open(f"/proc/{pid}/stat").read()
            except OSError:
                continue
            comm, rest = stat[stat.index("(") + 1:stat.rindex(")")], stat[stat.rindex(")") + 2:].split()
            if comm in BUILDERS:
                busy = True
            elif comm == "rustd":
                ticks += int(rest[11]) + int(rest[12])
        still = still + 1 if not busy and last is not None and ticks - last <= 1 else 0
        if still >= 2:
            break
        last = ticks
        time.sleep(0.1)
    return round(time.time() - began - 0.2, 3)


def touched(ws, changes):
    """The text of every file an edit may write."""
    return {path: open(path).read() for pattern, _, _ in changes for path in glob.glob(os.path.join(ws, pattern), recursive=True)}


def spread(samples, key):
    """The median, the least and the most of `key` over the samples that have it."""
    values = [s[key] for s in samples if s.get(key) is not None]
    return dict(median=round(statistics.median(values), 3), min=round(min(values), 3), max=round(max(values), 3), n=len(values)) if values else None


def counted(values):
    """How often each of `values` is there."""
    counts = {}
    for value in values:
        counts[str(value)] = counts.get(str(value), 0) + 1
    return counts


def sides(name, spec, backend, mode, work, tag=""):
    """A checkout of the project and the three builds of it: rustd's, cargo's and cargo's
    with mold."""
    rustd = rustd_of(backend)
    key = f"{name}-{backend}-{mode}{tag}"
    ws = os.path.realpath(os.path.join(work, key))
    made = populate(ws, name, spec)
    # The daemon listens on a socket under its home, and the path of a socket is at most
    # 107 bytes, so the home has a short name.
    home = os.path.join(work, "h" + hashlib.sha1(key.encode()).hexdigest()[:6])
    # The daemon has no part in a home other users can enter.
    os.makedirs(home, mode=0o700, exist_ok=True)
    env = dict(cargo_rustd.base_env(home), **spec.get("env", {}))
    env["PATH"] = os.path.dirname(rustd) + os.pathsep + env["PATH"]
    env.update(BACKENDS[backend], RUSTD_STATS="1", RUSTD_CACHE=os.path.join(work, f"store-{key}"))
    # difftest.py, which cargo_rustd.py imports, turns incremental compilation off for
    # whatever it starts. Here cargo is cargo as it is installed, on every side.
    env.pop("CARGO_INCREMENTAL", None)
    plain = mode == "plain"
    ours = Side(ws, enabled_home(home, env) if plain else env, "plain" if plain else True)
    stock = Side(ws, env, False)
    tuned = Side(ws, env, False) if MOLD and mode != "plain" and backend == "cranelift" else None
    if tuned:
        tuned.env = tuned_env(env, ws)
    subprocess.run(["cargo", "fetch"], cwd=ws, env=env, capture_output=True)
    ours.cargo("stop")
    return ws, env, ours, stock, tuned, made


def large(name, spec, backend, mode, runs, work, only):
    """Every shape of one project under one backend and one way of building. The result is
    the phases by name."""
    ws, env, ours, stock, tuned, made = sides(name, spec, backend, mode, work)
    daemon, phases, build = Daemon(ours), {}, spec.get("build", [])

    def disk(target):
        return int(subprocess.run(["du", "-sm", os.path.join(ws, target)], capture_output=True, text=True).stdout.split()[0])

    def output(target):
        ran = subprocess.run([os.path.join(ws, target, "debug", spec["bin"]), *spec["run"]], cwd=ws, env=env, capture_output=True, text=True, errors="replace", timeout=900)
        return ran.returncode, ran.stdout, ran.stderr

    def step(command, args):
        """One build on every side of the source as it is now, compared."""
        if shutil.disk_usage(ws).free < 8 << 30:
            raise Failed(f"less than 8 GB of disk is free; `target` is {disk('target')} MB and cargo's `target-ref` {disk('target-ref')} MB")
        quiet()
        took, mine = ours.cargo(command, *args)
        built = ours.how_built(mine.stderr)
        said = daemon.said(1 if built == "daemon" and command == "build" else 0)
        settle = quiet()
        reference, other = stock.cargo(command, *args)
        third = tuned.cargo(command, *args)[0] if tuned else None
        if command == "build":
            if mine.returncode != 0 or other.returncode != 0:
                raise Failed(f"the build failed: rustd {mine.returncode}, cargo {other.returncode}: {(other.stderr if other.returncode else mine.stderr)[-1500:]}")
            wrong = cargo_rustd.difference(output("target"), output("target-ref"))
        else:
            wrong = cargo_rustd.difference(test_results(mine), test_results(other))
            ran = sum(int(n) for n in re.findall(r"^test result: \w+\. (\d+) passed", other.stdout, re.M))
            if other.returncode != 0 or not ran:
                raise Failed(f"cargo's tests: exit {other.returncode}, {ran} passed: {other.stderr[-1500:]}")
        sample = dict(rustd_s=round(took, 3), cargo_s=round(reference, 3), mold_s=third and round(third, 3), built=built, link=link_kind(said),
                      compiled=compiled(said), settle_s=settle, cargo_crates=len(re.findall(r"^\s*Compiling ", other.stderr, re.M)),
                      steps_ms=daemon_steps(said), started_over=rounds(said), wrong=wrong)
        if built.startswith("cargo"):
            # A build cargo did has the daemon load again; the next one is not timed against that.
            sample["reload_s"] = round(wait_warm(ours, ws)[0], 3)
            daemon.said()
        return sample

    def summary(made, back):
        slowest = max(made, key=lambda s: s["rustd_s"])
        return dict(rustd=spread(made, "rustd_s"), cargo=spread(made, "cargo_s"), mold=spread(made, "mold_s"), built=counted(s["built"] for s in made),
                    link=counted(s["link"] for s in made), compiled=spread(made, "compiled"), cargo_crates=spread(made, "cargo_crates"),
                    settle=spread(made, "settle_s"), reload=spread(made, "reload_s"), rss_mb=daemon.peak_mb(), steps_ms=slowest["steps_ms"],
                    started_over=slowest["started_over"], wrong=[s["wrong"] for s in made + back if s["wrong"]],
                    back=dict(rustd=spread(back, "rustd_s"), cargo=spread(back, "cargo_s"), mold=spread(back, "mold_s"), built=counted(s["built"] for s in back),
                              link=counted(s["link"] for s in back)) if back else None,
                    samples=made, back_samples=back)

    def report(label):
        phase = phases[label]
        if "rustd" not in phase:
            print(f"  {label:10} {str(phase)[:300]}", flush=True)
            return
        print(f"  {label:10} rustd {phase['rustd']['median']:8.3f}s  cargo {phase['cargo']['median']:8.3f}s  mold {(phase['mold'] or {}).get('median', 0):8.3f}s  "
              f"n {phase['rustd']['n']}  rss {phase['rss_mb']:6} MB  {phase['built']}  {phase['link']}  {'WRONG ' + str(phase['wrong'][:1]) if phase['wrong'] else 'same'}", flush=True)

    # The first build, and the wait for the daemon. Cargo's own waits until that is over.
    took, mine = ours.cargo("build", *build)
    warm, states = wait_warm(ours, ws)
    settle = quiet()
    said = daemon.said()
    reference, other = stock.cargo("build", *build)
    third = tuned.cargo("build", *build)[0] if tuned else None
    fns = re.search(r"^cold: compiled (\d+) of (\d+) fns \(\+(\d+) instances\)", said, re.M)
    phases["cold"] = dict(rustd_s=round(took, 3), warm_s=round(warm + settle, 3), cargo_s=round(reference, 3), mold_s=third and round(third, 3), states=states,
                          rss_mb=daemon.peak_mb(), fns=fns and [int(g) for g in fns.groups()], steps_s=cold_steps(said), built=ours.how_built(mine.stderr),
                          disk_mb={t: disk(t) for t in ("target", "target-ref")})
    print(f"  cold       rustd {took:8.3f}s + {warm + settle:.1f}s until ready  cargo {reference:8.3f}s  mold {third or 0:8.3f}s  {states}  {phases['cold']['fns']}", flush=True)
    if mine.returncode != 0 or other.returncode != 0 or states != ["ready"]:
        _, status = ours.cargo("status")
        phases["cold"]["not_loaded"] = (mine.stderr[-1500:] if mine.returncode else "") + (other.stderr[-1500:] if other.returncode else "") + status.stdout[-1500:]
        ours.cargo("stop")
        return dict(phases=phases, made=made, tag=spec.get("tag"))

    try:
        if not only or "no-op" in only:
            phases["no-op"] = summary([step("build", build) for _ in range(runs)], [])
            report("no-op")
        for label, (changes, command, args) in shapes_of(ws, env, spec).items():
            if only and label not in only:
                continue
            if changes is None:
                phases[label] = dict(skipped="the workspace has no such edit")
                report(label)
                continue
            made_, back, began, kept = [], [], time.time(), touched(ws, changes)
            try:
                if command == "test":
                    # The first test is cargo's, and has the daemon load the tests.
                    first = step(command, args)
                    first["warm_s"] = round(wait_warm(ours, ws)[0] + quiet(), 3)
                    daemon.said()
                    phases[label + " first"] = first
                for turn in range(runs):
                    places = write_edit(ws, changes, False)
                    made_.append(step(command, args))
                    write_edit(ws, changes, True)
                    back.append(step(command, args))
                    if time.time() - began > SHAPE_BUDGET and turn >= 2:
                        break
                phases[label] = dict(summary(made_, back), places=places, files=len(kept))
            except Failed as why:
                for path, text in kept.items():
                    open(path, "w").write(text)
                phases[label] = dict(error=str(why), samples=made_, back_samples=back)
                wait_warm(ours, ws)
            report(label)
    finally:
        _, status = ours.cargo("status")
        ours.cargo("stop")
    return dict(phases=phases, made=made, tag=spec.get("tag"), status=status.stdout[-4000:], disk_end_mb={t: disk(t) for t in ("target", "target-ref")})


def session(name, spec, backend, mode, edits, thinks, work, labels=None):
    """A clean build and then `edits` edits one after another, in fresh checkouts: under
    cargo alone, and under rustd once for each number of seconds in `thinks` left between
    the builds. The result is the time of every build."""
    found = {}

    def one(tag, rustd, think):
        ws, env, ours, stock, _, _ = sides(name, spec, backend, mode, work, tag)
        side, build = (ours if rustd else stock), spec.get("build", [])
        shapes = shapes_of(ws, env, spec)
        order = [label for label in labels or SESSION if shapes[label][0]]
        order = [(label, back) for back in (False, True) for label in order]
        took, first = side.cargo("build", *build)
        if first.returncode != 0:
            raise Failed(f"session: the first build failed: {first.stderr[-1500:]}")
        # "ready": the edits begin when the daemon has loaded, and the wait is counted.
        waited = wait_warm(ours, ws)[0] + quiet() if think == "ready" else 0.0
        think = 0.0 if think == "ready" else think
        builds = []
        for at in range(edits):
            label, back = order[at % len(order)]
            time.sleep(think)
            write_edit(ws, shapes[label][0], back)
            spent, ran = side.cargo("build", *build)
            if ran.returncode != 0:
                raise Failed(f"session: {label} failed: {ran.stderr[-1500:]}")
            builds.append(dict(shape=label, back=back, s=round(spent, 3), built=ours.how_built(ran.stderr) if rustd else "cargo"))
        target = "target" if rustd else "target-ref"
        ran = subprocess.run([os.path.join(ws, target, "debug", spec["bin"]), *spec["run"]], cwd=ws, env=env, capture_output=True, text=True, errors="replace", timeout=900)
        if rustd:
            ours.cargo("stop")
        quiet()
        shutil.rmtree(ws, ignore_errors=True)
        return dict(first_s=round(took, 3), wait_s=round(waited, 3), builds=builds, edits_s=round(sum(b["s"] for b in builds), 3), total_s=round(took + waited + sum(b["s"] for b in builds), 3),
                    by_daemon=sum(b["built"] == "daemon" for b in builds)), (ran.returncode, ran.stdout)

    found["cargo"], expected = one("-session-cargo", False, 0)
    print(f"  session    cargo: first {found['cargo']['first_s']}s, {edits} edits {found['cargo']['edits_s']}s", flush=True)
    for think in thinks:
        row, said = one(f"-session-{think}", True, think)
        found[f"rustd {think}"] = row
        row["wrong"] = None if said == expected else "the program at the end says something else than cargo's"
        print(f"  session    rustd, {think} between: first {row['first_s']}s + {row['wait_s']}s, {edits} edits {row['edits_s']}s, {row['by_daemon']} by the daemon  {row['wrong'] or 'same'}", flush=True)
    return found


def machine():
    """What this was run on."""
    cpu = re.search(r"model name\s*: (.*)", open("/proc/cpuinfo").read())
    memory = re.search(r"MemTotal:\s+(\d+) kB", open("/proc/meminfo").read())
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
    version = lambda argv: (subprocess.run(argv, capture_output=True, text=True).stdout.splitlines() or [""])[0]
    return dict(cpu=cpu and cpu[1], cores=os.cpu_count(), memory_gb=memory and round(int(memory[1]) / 1e6, 1), kernel=platform.release(),
                commit=commit, rustc=version(["rustc", f"+{TOOLCHAIN}", "--version"]), mold=MOLD and version([MOLD, "--version"]), cc=version(["cc", "--version"]))


def shapes_main(argv):
    backends, runs = options(argv, "--backend") or ["cranelift"], int(options(argv, "--runs", "5")[0])
    mode = options(argv, "--mode", "rustd")[0]
    only = options(argv, "--only")
    edits = int(options(argv, "--session", "0")[0])
    thinks = [t if t == "ready" else float(t) for t in options(argv, "--think")] or [0.0, "ready"]
    session_shapes = options(argv, "--session-shapes")
    out = options(argv, "--out", os.path.join(HERE, "bench-large.json"))[0]
    work = (options(argv, "--work") or [None])[0] or tempfile.mkdtemp(prefix="rustd-bench-")
    keep = "--keep" in argv
    names = [a for a in argv if not a.startswith("--")] or list(LARGE)
    os.makedirs(work, exist_ok=True)
    result = json.load(open(out)) if os.path.exists(out) else dict(results={})
    result.update(toolchain=TOOLCHAIN, machine=machine(), runs=runs)
    for name in names:
        for backend in backends:
            key = f"{name}/{backend}/{mode}"
            print(f"{key} ({rustd_of(backend)}; work {work})", flush=True)
            measured = result["results"].get(key, {})
            try:
                if not only or set(only) != {"session"}:
                    fresh = large(name, LARGE[name], backend, mode, runs, work, [o for o in only if o != "session"])
                    fresh["phases"] = dict(measured.get("phases", {}), **fresh["phases"]) if only else fresh["phases"]
                    measured.update(fresh)
                if edits:
                    measured["session"] = session(name, LARGE[name], backend, mode, edits, thinks, work, session_shapes)
            except Failed as why:
                measured["failed"] = str(why)
                print(f"  FAILED {why}", flush=True)
            result["results"][key] = measured
            json.dump(result, open(out, "w"), indent=1)
            if not keep:
                for entry in os.listdir(work):
                    if f"{name}-{backend}-{mode}" in entry:
                        shutil.rmtree(os.path.join(work, entry), ignore_errors=True)
    wrong = [f"{key} {label}: {w}" for key, m in result["results"].items() for label, p in m.get("phases", {}).items() for w in (p.get("wrong") or []) if isinstance(p.get("wrong"), list)]
    print(f"wrote {out}; wrong: {len(wrong)}")
    for line in wrong:
        print(f"  WRONG {line}")


def spread_s(found):
    """`median (least to most)` of a spread, in seconds."""
    return f"{found['median']:.2f} ({found['min']:.2f}–{found['max']:.2f})" if found else "–"


def ratio(cargo, ours):
    """How many times cargo's median is ours."""
    return f"{cargo['median'] / max(ours['median'], 0.001):.1f}×" if cargo and ours else "–"


def who(counts):
    """Who built the runs of a shape, the reason of cargo's shortened."""
    return "; ".join(f"{re.sub(r'/[^ ]*/', '', key)[:90]} ×{n}" for key, n in counts.items())


def tables(result):
    """The Markdown tables of a result of `--shapes`, a project and backend at a time."""
    lines, groups = [], {}
    for key, measured in result["results"].items():
        name, backend, mode = key.split("/")
        groups.setdefault((name, backend), {})[mode] = measured
    for (name, backend), modes in groups.items():
        first = modes.get("rustd") or modes["plain"]
        labels = [l for l in first.get("phases", {}) if l != "cold" and not l.endswith(" first")]
        lines += [f"### {name}, {backend}", ""]
        for mode, measured in modes.items():
            cold = measured.get("phases", {}).get("cold")
            if cold:
                lines.append(f"First build, {MODES[mode]}: {cold['rustd_s']:.1f}s, then {cold['warm_s']:.1f}s until the daemon said `{'; '.join(cold['states'])[:160]}`; "
                             f"cargo alone {cold['cargo_s']:.1f}s" + (f", with mold {cold['mold_s']:.1f}s" if cold.get("mold_s") else "") +
                             (f". The daemon compiled {cold['fns'][0]} of {cold['fns'][1]} functions (+{cold['fns'][2]} instances)" if cold.get("fns") else "") +
                             f"; peak {cold['rss_mb']} MB.")
            if measured.get("failed"):
                lines.append(f"Failed, {MODES[mode]}: {measured['failed'][:400]}")
        if not labels and "cold" in first.get("phases", {}):
            lines += ["", "No edit was measured: the daemon did not load the workspace."]
        lines += [] if not labels else ["", "| edit | cargo alone, s | `cargo rustd`, s | cargo ÷ rustd | cargo with mold, s | cargo alone in the `rustd enable` run, s | plain cargo after `rustd enable`, s | cargo ÷ rustd | crates cargo recompiled |", "|---|---|---|---|---|---|---|---|---|"]
        for label in labels:
            cells = []
            for mode in ("rustd", "plain"):
                phase = modes.get(mode, {}).get("phases", {}).get(label)
                cells.append(phase if phase and "rustd" in phase else None)
            wrapped, plain = cells
            if not wrapped and not plain:
                phase = first["phases"][label]
                lines.append(f"| {NAMES.get(label, label)} | {phase.get('skipped') or 'failed: ' + phase.get('error', '')[:200]} | | | | | | | |")
                continue
            base = wrapped or plain
            lines.append(f"| {NAMES.get(label, label)} | {spread_s(wrapped and wrapped['cargo'])} | {spread_s(wrapped and wrapped['rustd'])} | {ratio(wrapped and wrapped['cargo'], wrapped and wrapped['rustd'])} | "
                         f"{spread_s(wrapped and wrapped.get('mold'))} | {spread_s(plain and plain['cargo'])} | {spread_s(plain and plain['rustd'])} | {ratio(plain and plain['cargo'], plain and plain['rustd'])} | {base['cargo_crates']['median']:.0f} |")
        lines += [] if not labels else ["", "| edit | way | runs | built by | link | functions compiled | daemon peak, MB | taken back: cargo, s | taken back: rustd, s | same as cargo's |", "|---|---|---|---|---|---|---|---|---|---|"]
        for label in labels:
            for mode in ("rustd", "plain"):
                phase = modes.get(mode, {}).get("phases", {}).get(label)
                if not phase or "rustd" not in phase:
                    continue
                back = phase.get("back") or {}
                lines.append(f"| {NAMES.get(label, label)} | {MODES[mode]} | {phase['rustd']['n']} | {who(phase['built'])} | {who(phase['link'])} | {spread_s(phase['compiled']).replace('.00', '')} | {phase['rss_mb']} | "
                             f"{spread_s(back.get('cargo'))} | {spread_s(back.get('rustd'))} | {'NO: ' + str(phase['wrong'][0])[:120] if phase['wrong'] else 'yes'} |")
        for mode, measured in modes.items():
            if "session" not in measured:
                continue
            lines += ["", f"Session, {MODES[mode]}: a clean build, then {len(measured['session']['cargo']['builds'])} edits one after another ({'; '.join(dict.fromkeys(NAMES.get(b['shape'], b['shape']) for b in measured['session']['cargo']['builds']))}, each made and then taken back).", "",
                      "| | first build, s | wait for the daemon, s | the edits, s | total, s | edits the daemon built |", "|---|---|---|---|---|---|"]
            for way, row in measured["session"].items():
                lines.append(f"| {dict(cargo='cargo alone').get(way, 'rustd, edits begin when the daemon is ready' if way.endswith('ready') else 'rustd, edits begin at once')} | {row['first_s']:.1f} | {row.get('wait_s', 0):.1f} | {row['edits_s']:.1f} | {row['total_s']:.1f} | {row['by_daemon']} of {len(row['builds'])} |")
        lines.append("")
    return "\n".join(lines)


NAMES = dict(
    body="body, in the program's crate", leaf="body, in a library deep in the graph", leaf2="body, in a second deep library",
    scatter3="bodies in 3 crates", scatter10="bodies in 10 crates", scatter30="bodies in 30 crates",
    signature="signature, callers updated across crates", generic="generic function used at many types",
    new_fn="new function", new_type="new type", field="changed struct field",
    test_body="`cargo test -p` the program, after a body edit", test_leaf="`cargo test -p` a library, after a body edit",
    test_leaf_lib="`cargo test -p` a library `--lib`, after a body edit",
)
def losses(result):
    """The rows where rustd's median is over cargo's or cargo did some of the builds, as a table."""
    lines = []
    for key, measured in result["results"].items():
        name, backend, mode = key.split("/")
        for label, phase in measured.get("phases", {}).items():
            if "rustd" not in phase or not isinstance(phase["rustd"], dict):
                continue
            by_cargo = sum(n for built, n in phase["built"].items() if built.startswith("cargo"))
            if phase["rustd"]["median"] <= phase["cargo"]["median"] and not by_cargo:
                continue
            reason = next((re.sub(r"/[^ ]*/", "", built)[:150] for built in phase["built"] if built.startswith("cargo")), "")
            lines.append(f"| {name} | {backend} | {MODES[mode]} | {NAMES.get(label, label)} | {spread_s(phase['cargo'])} | {spread_s(phase['rustd'])} | {ratio(phase['cargo'], phase['rustd'])} | "
                         f"{phase['rustd']['n']} | {by_cargo} | {reason} |")
    head = ["| workspace | backend | way | edit | cargo alone, s | rustd, s | cargo ÷ rustd | runs | built by cargo | why cargo built it |", "|---|---|---|---|---|---|---|---|---|---|"]
    return "\n".join(head + lines) if lines else ""


MODES = dict(rustd="`cargo rustd`", plain="plain cargo after `rustd enable`")


def report_main(argv):
    for path in argv:
        result = json.load(open(path))
        m = result["machine"]
        print(f"<!-- {path}: rustd {m['commit']}, {m['rustc']}, {m['cores']} × {m['cpu']}, {m['memory_gb']} GB, {m['mold']} -->\n")
        print(losses(result) if "--losses" in sys.argv else tables(result))


def compare(baseline, result, threshold):
    """What in `result` is worse than in `baseline` by more than `threshold` percent."""
    worse = []
    for key, was in baseline["results"].items():
        now = result["results"].get(key)
        if now is None:
            worse.append(f"{key}: not measured")
            continue
        worse += [f"{key}: {w}" for w in now.get("wrong", [])]
        for label, old in was["phases"].items():
            new = now["phases"].get(label)
            if new is None:
                worse.append(f"{key} {label}: not measured")
                continue
            for field, floor, unit in (("wall_s", FLOOR_SECS, "s"), ("rss_mb", 50, " MB"), ("compiled", 10, " functions")):
                before, after = old.get(field), new.get(field)
                if before is not None and after is not None and after > before * (1 + threshold / 100) and after - before > floor:
                    worse.append(f"{key} {label}: {field} {before}{unit} -> {after}{unit} (+{100 * (after - before) / max(before, 1e-9):.0f}%)")
            if old["built"] == "daemon" and new["built"] != "daemon":
                worse.append(f"{key} {label}: built by {new['built']}, was the daemon")
            if old.get("link") == "spliced" and new.get("link") != "spliced":
                worse.append(f"{key} {label}: {new.get('link')}, was spliced")
    return worse


def options(argv, name, default=None):
    """The values given to every `--name VALUE` in `argv`, which loses them."""
    found = []
    while name in argv:
        at = argv.index(name)
        found.append(argv[at + 1])
        del argv[at:at + 2]
    return found or ([default] if default is not None else [])


def main():
    argv = sys.argv[1:]
    if "--tables" in argv:
        argv.remove("--tables")
        return report_main([a for a in argv if a != "--losses"])
    if "--shapes" in argv:
        argv.remove("--shapes")
        return shapes_main(argv)
    threshold = float(options(argv, "--threshold", "25")[0])
    if "--compare" in argv:
        argv.remove("--compare")
        baseline, result = (json.load(open(path)) for path in argv)
        worse = compare(baseline, result, threshold)
        print("\n".join(worse) or f"nothing over {threshold:.0f}% worse than the baseline")
        sys.exit(1 if worse else 0)
    backends, runs = options(argv, "--backend") or list(BACKENDS), int(options(argv, "--runs", "3")[0])
    out = options(argv, "--out", os.path.join(HERE, "bench.json"))[0]
    work = (options(argv, "--work") or [None])[0] or tempfile.mkdtemp(prefix="rustd-bench-")
    reference_dir = (options(argv, "--reference") or [None])[0]
    keep = "--keep" in argv
    names = [a for a in argv if not a.startswith("--")] or list(PROJECTS)
    os.makedirs(work, exist_ok=True)
    result = dict(toolchain=TOOLCHAIN, machine=platform.machine(), cores=os.cpu_count(), runs=runs, results={})
    for name in names:
        for backend in backends:
            print(f"{name} {backend} ({rustd_of(backend)}; work {work})", flush=True)
            result["results"][f"{name}/{backend}"] = measured = measure(name, PROJECTS[name], backend, runs, work, reference_dir)
            for line in measured["wrong"]:
                print(f"  WRONG {line}", flush=True)
            json.dump(result, open(out, "w"), indent=1)
            if not keep:
                for at in filter(None, (work, reference_dir)):
                    for entry in os.listdir(at):
                        if entry.endswith(f"{name}-{backend}"):
                            shutil.rmtree(os.path.join(at, entry), ignore_errors=True)
    wrong = [f"{key}: {line}" for key, measured in result["results"].items() for line in measured["wrong"]]
    print(f"wrote {out}; wrong: {len(wrong)}")
    sys.exit(1 if wrong else 0)


if __name__ == "__main__":
    main()
