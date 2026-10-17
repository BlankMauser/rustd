#!/usr/bin/env python3
"""cargo_rustd.py: `cargo rustd` against plain cargo on real projects, unmodified.

    cargo_rustd.py [PROJECT...] [--small] [--only-projects] [--only-checks] [--keep]

Three parts, all of which have to pass.

Projects. Each one below is cloned at its tag, which has to be the commit it is pinned to, into `RUSTD_PROJECTS` (~/rustd-projects) when
it is not there, and copied; the clone is never written to. In the copy, with plain cargo
building into a target directory of its own as the reference:
  cold      `cargo rustd build` from nothing, with no daemon; the program is run
  warm      once the daemon has loaded the workspace, `cargo rustd build` again; the program,
            which is now the one the daemon linked, is run
  test      `cargo rustd test`, cold and again warm
  edit      the source is edited; `cargo rustd build`, the program is run, `cargo rustd test`
  broken    a type error is added; both have to fail with the same status and the same error
After every step the program's output and exit status and the result of every test have to be
what plain cargo gives for the same source. A build cargo did in place of the daemon passes
(that is the contract) and is in the table with the reason.

Flags. A small workspace made here is built and run with each flag `cargo rustd` passes
through, cold and after an edit, and compared with cargo the same way.

Daemon. Started on demand, reused, shared by a git worktree, alive after its client is
killed, replaced when the client is another build of rustd, gone when idle, and its workers
gone with it.

Writes CARGO-RUSTD.md and cargo_rustd.json next to this file, or CARGO-RUSTD-LLVM.md and
cargo_rustd_llvm.json under `RUSTD_BACKEND=llvm`. A run of some projects replaces their rows
and keeps the others. `--small` is the three projects of the CI job.
"""
import collections, json, os, re, shutil, signal, subprocess, sys, tempfile, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
RUSTD = os.environ.get("RUSTD_BIN") or os.path.join(ROOT, "target/release/rustd")
TOOLCHAIN = open(os.path.join(HERE, "toolchain")).read().strip()
SOURCES = os.environ.get("RUSTD_PROJECTS") or os.path.expanduser("~/rustd-projects")
WARMUP_SECS = 1800

PROJECTS = {
    "ripgrep": dict(
        repo="BurntSushi/ripgrep", tag="15.1.0", rev="af60c2de9d85e7f3d81c78601669468cf02dabab",
        run=["-n", "--no-heading", "--sort", "path", "fn main", "crates/core/main.rs", "build.rs"],
        edits=[("crates/core/main.rs", "fn main() -> ExitCode {", 'fn main() -> ExitCode {\n    eprintln!("edited main");'),
               ("crates/printer/src/standard.rs", "pub fn new() -> StandardBuilder {", 'pub fn new() -> StandardBuilder {\n        eprintln!("edited printer");')],
    ),
    "fd": dict(
        repo="sharkdp/fd", tag="v10.3.0", rev="d38148f0aabdd073b4080cde770f679f3197b920",
        run=["--extension", "rs", "main", "src"],
        edits=[("src/main.rs", "fn main() {", 'fn main() {\n    eprintln!("edited main");')],
    ),
    "bat": dict(
        repo="sharkdp/bat", tag="v0.26.0", rev="c734087e1d15fb730d7653389c841fd9e3016556",
        run=["--plain", "--color=never", "rustfmt.toml"],
        edits=[("src/bin/bat/main.rs", "fn main() {", 'fn main() {\n    eprintln!("edited main");')],
    ),
    "hyperfine": dict(
        repo="sharkdp/hyperfine", tag="v1.19.0", rev="12fec42098642a19855ead34c8cb1e0be28c8ead",
        run=["--help"],
        edits=[("src/main.rs", "fn main() {", 'fn main() {\n    eprintln!("edited main");')],
    ),
    "just": dict(
        repo="casey/just", tag="1.43.0", rev="b7e7c46495b34e9407982ffbbb25eb2808ec40b7",
        run=["--list", "--unsorted"],
        edits=[("src/main.rs", "fn main() {", 'fn main() {\n  eprintln!("edited main");')],
    ),
    "axum": dict(
        repo="tokio-rs/axum", tag="axum-v0.8.6", rev="c1bb9c3e90625fb8de4f9c9750f669cb4edac3df",
        build=["-p", "example-hello-world"], serves="http://127.0.0.1:3000/",
        test=["-p", "example-testing"],
        edits=[("examples/hello-world/src/main.rs", "<h1>Hello, World!</h1>", "<h1>Hello, edited world!</h1>"),
               ("examples/testing/src/main.rs", "fn app() -> Router {", 'fn app() -> Router {\n    eprintln!("edited app");')],
        broken="examples/hello-world/src/main.rs",
    ),
    "nushell": dict(
        repo="nushell/nushell", tag="0.108.0", rev="da141be11b3acaa55b39321e5f9651d02aa2ed6f",
        run=["-n", "-c", "[3 1 2] | sort | str join ','"],
        test=["-p", "nu-path"],
        edits=[("src/main.rs", "fn main() -> Result<()> {", 'fn main() -> Result<()> {\n    eprintln!("edited main");')],
    ),
    "cargo": dict(
        repo="rust-lang/cargo", tag="0.92.0", rev="ea2d97820c16195b0ca3fadb4319fe512c199a43",
        run=["metadata", "--format-version", "1", "--no-deps", "--offline", "--manifest-path", "crates/cargo-platform/Cargo.toml"],
        test=["-p", "cargo-platform"],
        edits=[("src/bin/cargo/main.rs", "fn main() {", 'fn main() {\n    eprintln!("edited main");')],
    ),
    "rust-analyzer": dict(
        repo="rust-lang/rust-analyzer", tag="2026-10-12", rev="2ad81e72b7e978cb800dffe1bcce5c9f98032acf",
        build=["-p", "rust-analyzer"], run=["--version"], test=["-p", "stdx"],
        edits=[("crates/rust-analyzer/src/bin/main.rs", "fn main() -> anyhow::Result<ExitCode> {", 'fn main() -> anyhow::Result<ExitCode> {\n    eprintln!("edited main");')],
    ),
    "tokei": dict(
        repo="XAMPPRocky/tokei", tag="v15.0.0", rev="c14f744716272fadeb27a74443cdffa0af35f82f",
        run=["src", "--sort", "code"],
        edits=[("src/main.rs", "fn main() -> Result<(), Box<dyn Error>> {", 'fn main() -> Result<(), Box<dyn Error>> {\n    eprintln!("edited main");')],
    ),
    "eza": dict(
        repo="eza-community/eza", tag="v0.23.5", rev="98442ab17c2c3738701b62a7e060b1431ae2d6ea",
        run=["--oneline", "--color=never", "src"],
        edits=[("src/main.rs", "fn main() {", 'fn main() {\n    eprintln!("edited main");')],
    ),
    "starship": dict(
        repo="starship/starship", tag="v1.27.0", rev="f8e4e2155542cc84f49b342122d371f74e0ad253",
        run=["module", "character"], test=["--lib", "modules::character"],
        edits=[("src/main.rs", "fn main() {", 'fn main() {\n    eprintln!("edited main");')],
    ),
    "delta": dict(
        repo="dandavison/delta", tag="0.20.1", rev="b88d2290468449d75950bfa150ea7c1291fe2977",
        run=["--list-languages"],
        edits=[("src/main.rs", "fn main() -> std::io::Result<()> {", 'fn main() -> std::io::Result<()> {\n    eprintln!("edited main");')],
    ),
    "helix": dict(
        repo="helix-editor/helix", tag="25.07.1", rev="a05c151bb6e8e9c65ec390b0ae2afe7a5efd619b",
        env=dict(HELIX_DISABLE_AUTO_GRAMMAR_BUILD="1"),
        run=["--version"], test=["-p", "helix-stdx"],
        edits=[("helix-term/src/main.rs", "fn main() -> Result<()> {", 'fn main() -> Result<()> {\n    eprintln!("edited main");')],
    ),
    "tonic": dict(
        repo="hyperium/tonic", tag="v0.14.5", rev="3f7caf3171393734ef19e12d010bd9c945c9e242",
        build=["-p", "examples", "--bin", "helloworld-server"], client="helloworld-client",
        test=["-p", "tonic-types"],
        edits=[("examples/src/helloworld/server.rs", '"Hello {}!"', '"Hello edited {}!"')],
    ),
    "wasmtime": dict(
        repo="bytecodealliance/wasmtime", tag="v49.0.3", rev="f96eabaa11a06086e4fb779ed1c1688481067ab9",
        run=["wast", "tests/misc_testsuite/add.wast"], test=["-p", "wasmtime-environ"],
        edits=[("src/bin/wasmtime.rs", "fn main() -> Result<()> {", 'fn main() -> Result<()> {\n    eprintln!("edited main");')],
    ),
}

# The projects of the CI job: small, and a build script, a workspace and a test suite among them.
SMALL = ["ripgrep", "fd", "hyperfine"]

# What cargo and `cargo rustd` print about the build, which is not the program's output.
STATUS = re.compile(r"^\s*((Compiling|Finished|Running|Downloading|Downloaded|Blocking|Locking|Updating|Adding|Fresh|Dirty|Doc-tests)\b|rustd:|warning:)")


def base_env(home):
    env = dict(os.environ, RUSTUP_TOOLCHAIN=TOOLCHAIN, RUSTD_HOME=home, CARGO_TERM_COLOR="never")
    env["PATH"] = os.path.dirname(RUSTD) + os.pathsep + env["PATH"]
    for name in ("RUSTFLAGS", "CARGO_TARGET_DIR", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_TARGET"):
        env.pop(name, None)
    return env


def timed(argv, cwd, env, timeout=7200):
    began = time.time()
    ran = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL)
    return time.time() - began, ran


class Side:
    """One of the two builds of a checkout: `cargo rustd`, or cargo into `target-ref`."""

    def __init__(self, ws, env, rustd):
        self.ws, self.rustd = ws, rustd
        self.env = env if rustd else dict(env, CARGO_TARGET_DIR=os.path.join(ws, "target-ref"))

    def cargo(self, command, *args, env=None, timeout=7200):
        argv = ["cargo", *(["rustd"] if self.rustd else []), command, *args]
        return timed(argv, self.ws, dict(self.env, **(env or {})), timeout)


def how_built(stderr):
    """Whether the daemon built it, or why cargo did."""
    reason = re.search(r"^rustd: (.*); (building with cargo|this is `cargo \w+`)$", stderr, re.M)
    if reason:
        return "cargo: " + reason[1]
    kept = re.search(r"^rustd: (.*); nothing has changed since cargo built$", stderr, re.M)
    if kept:
        return "unchanged: " + kept[1]
    return "daemon" if "[rustd]" in stderr else "cargo"


def program_output(ran):
    """What the program said, without what cargo said, and with the two target directories
    named alike."""
    def clean(text):
        # What rustc warned of, which cargo shows again with every build, comes before
        # cargo says what it runs.
        lines = text.splitlines()
        running = [at for at, l in enumerate(lines) if re.match(r"^\s*Running `", l)]
        lines = [l for l in lines[running[-1] + 1 if running else 0:] if not STATUS.match(l)]
        text = "\n".join(lines).replace("target-ref", "target")
        # A panic message has the id of the thread, and a logger the time.
        text = re.sub(r"' \(\d+\) panicked", "' panicked", text)
        return re.sub(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z?", "<time>", text)
    return ran.returncode, clean(ran.stdout), clean(ran.stderr)


def test_results(ran):
    """The outcome of each test and of each test executable, and the exit status."""
    tests = collections.Counter(re.findall(r"^test (.+?) \.\.\. (ok|FAILED|ignored)", ran.stdout, re.M))
    totals = sorted(re.findall(r"^test result: (\w+)\. (\d+ passed; \d+ failed; \d+ ignored; \d+ measured; \d+ filtered out)", ran.stdout, re.M))
    return ran.returncode, tests, totals


def difference(ours, theirs):
    """`None` when the two are the same, or the first thing that is not."""
    if ours == theirs:
        return None
    if not isinstance(ours, tuple):
        return f"{ours!r} for {theirs!r}"
    for mine, other in zip(ours, theirs):
        if mine != other:
            if isinstance(mine, collections.Counter):
                only = list((mine - other).items())[:3], list((other - mine).items())[:3]
                return f"tests differ: rustd only {only[0]}, cargo only {only[1]}"
            if isinstance(mine, str):
                pairs = [(a, b) for a, b in zip(mine.splitlines(), other.splitlines()) if a != b][:2]
                return f"output differs: {pairs or (mine[-200:], other[-200:])}"
            return f"{mine!r} for {other!r}"
    return "differ"


def asked_by(client):
    """What the server says to the client program `client`, which cargo built: the message
    of its answer."""
    def ask(timeout):
        ran = subprocess.run([client], capture_output=True, text=True, timeout=timeout + 5)
        if ran.returncode != 0:
            raise OSError(ran.stderr[-200:])
        return "; ".join(re.findall(r'message: "[^"]*"', ran.stdout))
    return ask


def fetched(url):
    return lambda timeout: urllib.request.urlopen(url, timeout=timeout).read().decode()


def served(side, command, ask):
    """What the server `cargo run` starts on `side` answers when `ask` asks it."""
    argv = ["cargo", *(["rustd"] if side.rustd else []), "run", *command]
    server = subprocess.Popen(argv, cwd=side.ws, env=side.env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, start_new_session=True)
    try:
        for _ in range(3000):
            if server.poll() is not None:
                return f"exited {server.returncode}: {server.stderr.read()[-300:]}"
            try:
                return ask(5)
            except OSError:
                time.sleep(0.1)
        return "no answer"
    finally:
        os.killpg(server.pid, signal.SIGKILL)
        server.wait()
        # The port is free again before the other side's server asks for it.
        for _ in range(100):
            try:
                ask(1)
                time.sleep(0.05)
            except OSError:
                break


def wait_warm(side, root):
    """Waits until the daemon has every build of `root` loaded or has given up on one. The
    result is the seconds it took and how each ended."""
    began = time.time()
    while time.time() - began < WARMUP_SECS:
        _, status = side.cargo("status")
        states = re.findall(rf"^    {re.escape(root)}: (.*)$", status.stdout, re.M)
        if states and not any(s.startswith("loading") for s in states):
            return time.time() - began, states
        time.sleep(0.25)
    return time.time() - began, ["not loaded in time"]


def rewrite(path, old, new):
    """Replaces the first `old` in the file by `new`. The result is the text it had."""
    text = open(path).read()
    assert old in text, f"{old!r} is not in {path}"
    open(path, "w").write(text.replace(old, new, 1))
    return text


def fetch(name, spec):
    """The checkout of the project, cloned at its tag when it is not there. The tag has to
    be the commit the project is pinned to."""
    source = os.path.join(SOURCES, name)
    if not os.path.isdir(source):
        os.makedirs(SOURCES, exist_ok=True)
        subprocess.run(["git", "clone", "-q", "--depth", "1", "--branch", spec["tag"], f"https://github.com/{spec['repo']}", source], check=True)
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=source, capture_output=True, text=True).stdout.strip()
    assert head == spec["rev"], f"{name} is at {head}, not at the {spec['rev']} it is pinned to"
    return source


def project(name, spec, work):
    """Runs the steps of one project. The result is the row of the table: times, who built
    each step, and every difference from cargo."""
    source = fetch(name, spec)
    ws = os.path.realpath(os.path.join(work, name))
    shutil.copytree(source, ws, symlinks=True, ignore=shutil.ignore_patterns("target", "target-ref"))
    env = dict(base_env(os.path.join(work, "home")), **spec.get("env", {}))
    ours, theirs = Side(ws, env, True), Side(ws, env, False)
    build, run, test = spec.get("build", []), spec.get("run", []), spec.get("test", [])
    row = dict(project=name, tag=spec["tag"], wrong=[], times={}, built={})
    subprocess.run(["cargo", "fetch"], cwd=ws, env=env, capture_output=True)
    ours.cargo("stop")

    def step(label, command, args, compare):
        """Runs `command` on both sides and compares what `compare` reads from each."""
        (took, mine), (reference, other) = ours.cargo(command, *args), theirs.cargo(command, *args)
        row["times"][label] = (round(took, 2), round(reference, 2))
        row["built"][label] = how_built(mine.stderr)
        wrong = difference(compare(mine), compare(other))
        if wrong:
            row["wrong"].append(f"{label}: {wrong}")
            open(os.path.join(work, f"{name}-{label}.rustd.txt"), "w").write(mine.stdout + mine.stderr)
            open(os.path.join(work, f"{name}-{label}.cargo.txt"), "w").write(other.stdout + other.stderr)
        print(f"  {label:12} rustd {took:7.2f}s  cargo {reference:7.2f}s  {row['built'][label]:8.80}  {wrong or 'same'}", flush=True)

    def runs(label):
        if ask:
            mine, other = served(ours, build, ask), served(theirs, build, ask)
            if mine != other:
                row["wrong"].append(f"{label}: served {mine[:80]!r} for {other[:80]!r}")
            print(f"  {label:12} served {mine[:50]!r}  {'same' if mine == other else 'WRONG'}", flush=True)
        else:
            step(label, "run", [*build, "--", *run], program_output)

    def warm(label):
        took, states = wait_warm(ours, ws)
        row["times"][label] = (round(took, 2), None)
        row["built"][label] = "; ".join(states)
        print(f"  {label:12} {took:7.2f}s  {'; '.join(states)[:150]}", flush=True)

    status = lambda ran: ran.returncode
    step("cold build", "build", build, status)
    ask = fetched(spec["serves"]) if "serves" in spec else None
    if "client" in spec:
        theirs.cargo("build", "-p", build[1], "--bin", spec["client"])
        ask = asked_by(os.path.join(ws, "target-ref/debug", spec["client"]))
    runs("cold run")
    warm("warm-up")
    step("warm build", "build", build, status)
    runs("warm run")
    step("cold test", "test", test, test_results)
    warm("test warm-up")
    step("warm test", "test", test, test_results)
    for file, old, new in spec["edits"]:
        rewrite(os.path.join(ws, file), old, new)
    step("edit build", "build", build, status)
    runs("edit run")
    step("edit test", "test", test, test_results)
    path = os.path.join(ws, spec.get("broken") or spec["edits"][0][0])
    edited = open(path).read()
    open(path, "w").write(edited + '\nfn rustd_type_error() -> u32 {\n    "not a number"\n}\n')
    errors = lambda ran: (ran.returncode, sorted(set(re.findall(r"^error(?:\[E\d+\])?: .*$", ran.stderr, re.M))))
    step("broken", "build", build, errors)
    open(path, "w").write(edited)
    step("mended", "build", build, status)
    runs("mended run")
    _, counted = ours.cargo("status")
    row["status"] = counted.stdout
    ours.cargo("stop")
    return row


FLAG_WORKSPACE = {
    "Cargo.toml": '[workspace]\nmembers = ["app", "util"]\nresolver = "2"\n\n[profile.quick]\ninherits = "dev"\n\n[profile.quick.package.util]\nopt-level = 3\noverflow-checks = false\n',
    "util/Cargo.toml": '[package]\nname = "util"\nversion = "0.1.0"\nedition = "2021"\n\n[features]\nloud = []\n',
    "util/src/lib.rs": 'pub fn greeting(name: &str) -> String {\n    let text = format!("hello {name}");\n    if cfg!(feature = "loud") { text.to_uppercase() } else { text }\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn greets() {\n        assert!(super::greeting("a").to_lowercase().starts_with("hello"));\n    }\n}\n',
    "app/Cargo.toml": '[package]\nname = "app"\nversion = "0.1.0"\nedition = "2021"\ndefault-run = "app"\n\n[features]\nloud = ["util/loud"]\n\n[dependencies]\nutil = { path = "../util" }\n',
    "app/src/main.rs": 'fn main() {\n    let name = std::env::args().nth(1).unwrap_or("world".to_string());\n    println!("{}", util::greeting(&name));\n    if name == "fail" {\n        std::process::exit(3);\n    }\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn in_the_program() {\n        assert_eq!(util::greeting("x").len(), 7);\n    }\n}\n',
    "app/src/bin/other.rs": 'fn main() {\n    println!("other: {}", util::greeting("other"));\n}\n',
    "app/examples/demo.rs": 'fn main() {\n    println!("demo: {}", util::greeting("demo"));\n}\n',
    "app/tests/it.rs": '#[test]\nfn greets_it() {\n    assert_eq!(util::greeting("it").to_lowercase(), "hello it");\n}\n\n#[test]\nfn fails_when_asked() {\n    assert!(std::env::var("FLAGS_FAIL").is_err());\n}\n\n// A cargo run by a test has the units of the build: the program stays the file it was.\n#[test]\nfn a_cargo_inside_leaves_the_program() {\n    use std::os::unix::fs::MetadataExt;\n    let program = env!("CARGO_BIN_EXE_app");\n    let before = std::fs::metadata(program).unwrap().ino();\n    let cargo = std::env::var("CARGO").unwrap_or("cargo".to_string());\n    let built = std::process::Command::new(cargo).args(["build", "-q", "-p", "app"]).status().unwrap();\n    assert!(built.success());\n    assert_eq!(before, std::fs::metadata(program).unwrap().ino());\n}\n\n// What cargo tells a test of the places of the build is what the daemon tells it.\n#[test]\nfn the_places_of_the_build_are_cargos() {\n    assert!(std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).is_dir());\n    let ran = std::process::Command::new(env!("CARGO_BIN_EXE_other")).output().unwrap();\n    assert_eq!(String::from_utf8_lossy(&ran.stdout).lines().count(), 1);\n}\n',
}

# Each: the subcommand, its arguments, the variables it runs with.
FLAG_CASES = [
    ("run", [], {}),
    ("run", ["--", "fail"], {}),
    ("run", ["--release"], {}),
    ("run", ["--target", "x86_64-unknown-linux-gnu"], {}),
    ("run", ["-p", "app"], {}),
    ("run", ["--features", "loud", "-p", "app"], {}),
    ("run", ["--bin", "other"], {}),
    ("run", ["--example", "demo"], {}),
    ("run", ["--manifest-path", "app/Cargo.toml"], {}),
    ("run", ["--profile", "quick"], {}),
    ("run", ["-q", "name"], {}),
    ("run", [], {"RUSTFLAGS": "--cfg extra"}),
    ("build", ["--workspace"], {}),
    ("build", ["--all-features"], {}),
    ("check", [], {}),
    ("test", [], {}),
    ("test", ["--test", "it"], {}),
    ("test", ["-p", "util", "--lib"], {}),
    ("test", ["greets"], {}),
    ("test", ["--no-fail-fast"], {"FLAGS_FAIL": "1"}),
    ("test", [], {"FLAGS_FAIL": "1"}),
    ("test", ["--no-run"], {}),
]


def flags(work):
    """Every flag case on both sides, cold, warm and after an edit. The result is the rows
    and what differed."""
    ws = os.path.realpath(os.path.join(work, "flags"))
    for file, text in FLAG_WORKSPACE.items():
        os.makedirs(os.path.dirname(os.path.join(ws, file)), exist_ok=True)
        open(os.path.join(ws, file), "w").write(text)
    env = base_env(os.path.join(work, "home-flags"))
    ours, theirs = Side(ws, env, True), Side(ws, env, False)
    rows, wrong = [], []

    def round_of(label):
        for command, args, extra in FLAG_CASES:
            (_, mine), (_, other) = ours.cargo(command, *args, env=extra), theirs.cargo(command, *args, env=extra)
            read = test_results if command == "test" else program_output
            differs = difference(read(mine), read(other))
            shown = " ".join([*(f"{k}={v}" for k, v in extra.items()), "cargo rustd", command, *args])
            rows.append((label, shown, how_built(mine.stderr), mine.returncode, differs or "same"))
            if differs:
                wrong.append(f"{label}: {shown}: {differs}")
            print(f"  {label:6} {shown:50} exit {mine.returncode:3}  {how_built(mine.stderr):8.60}  {differs or 'same'}", flush=True)

    round_of("cold")
    wait_warm(ours, ws)
    round_of("warm")
    rewrite(os.path.join(ws, "util/src/lib.rs"), 'format!("hello {name}")', 'format!("hello {name}!")')
    rewrite(os.path.join(ws, "app/src/main.rs"), ".len(), 7)", ".len(), 8)")
    rewrite(os.path.join(ws, "app/tests/it.rs"), '"hello it"', '"hello it!"')
    round_of("edit")
    _, counted = ours.cargo("status")
    ours.cargo("stop")
    return rows, wrong, counted.stdout


def daemon(work):
    """The life of the daemon. The result is each check with whether it held."""
    ws = os.path.realpath(os.path.join(work, "life"))
    for file, text in FLAG_WORKSPACE.items():
        os.makedirs(os.path.dirname(os.path.join(ws, file)), exist_ok=True)
        open(os.path.join(ws, file), "w").write(text)
    with open(os.path.join(ws, "app/src/bin/nested.rs"), "w") as nested:
        nested.write('fn main() {\n    let cargo = std::process::Command::new("cargo").arg("build").status();\n    std::process::exit(cargo.ok().and_then(|it| it.code()).unwrap_or(1));\n}\n')
    git = lambda *args, cwd=ws: subprocess.run(["git", "-c", "user.name=rustd", "-c", "user.email=rustd@localhost", *args], cwd=cwd, check=True, capture_output=True)
    open(os.path.join(ws, ".gitignore"), "w").write("target\ntarget-ref\n")
    git("init", "-q"), git("add", "."), git("commit", "-qm", "first")
    tree = os.path.realpath(os.path.join(work, "life-worktree"))
    git("worktree", "add", "-q", tree)
    home = os.path.join(work, "home-life")
    env = base_env(home)
    here, there = Side(ws, env, True), Side(tree, env, True)
    checks = []

    def check(name, held, detail=""):
        checks.append((name, bool(held), detail))
        print(f"  {'ok ' if held else 'NOT'} {name}  {detail}", flush=True)

    def status(side=None):
        text = (side or here).cargo("status")[1].stdout
        pid = re.search(r"^daemon: pid (\d+)", text, re.M)
        return (int(pid[1]) if pid else None), [int(p) for p in re.findall(r"worker: pid (\d+)", text)], text

    alive = lambda pid: os.path.exists(f"/proc/{pid}") and "Z" not in open(f"/proc/{pid}/stat").read().split(")")[-1].split()[0]
    edit = lambda root, text: open(os.path.join(root, "app/src/main.rs"), "a").write(f"\n// {text}\n")

    check("no daemon before the first build", status()[0] is None)
    _, first = here.cargo("build")
    pid, workers, _ = status()
    check("the first build starts one", first.returncode == 0 and pid is not None, f"pid {pid}")
    wait_warm(here, ws)
    # A program that runs plain `cargo build` on its own checkout, as a test suite may, after
    # the daemon built it: cargo's own artifacts are behind the sources, so cargo builds
    # them again and its executable takes the place of the daemon's. Both are right.
    edit(ws, "nested")
    _, installed = here.cargo("build")
    _, nested = here.cargo("run", "--bin", "nested")
    _, after = here.cargo("run", "--bin", "app")
    _, reference = timed(["cargo", "run", "--bin", "app"], ws, dict(env, CARGO_TARGET_DIR=os.path.join(ws, "target-ref")))
    check("a program that runs cargo on its checkout builds, and the next run is right", how_built(installed.stderr) == "daemon" and nested.returncode == 0 and after.returncode == reference.returncode and after.stdout == reference.stdout, f"{how_built(installed.stderr)}, nested {nested.returncode}, then {how_built(after.stderr)}")
    edit(ws, "one")
    _, second = here.cargo("build")
    check("a later build is the daemon's, by the same daemon", how_built(second.stderr) == "daemon" and status()[0] == pid, how_built(second.stderr))

    there.cargo("build")
    wait_warm(there, tree)
    _, workers_now, text = status()
    shared = re.search(rf"worker: pid \d+,[^\n]*\n(    [^\n]*\n)*    {re.escape(tree)}: ready", text)
    check("a worktree is loaded by the same daemon and the same worker", status()[0] == pid and len(workers_now) == 1 and shared, f"workers {workers_now}")
    edit(tree, "two")
    _, in_tree = there.cargo("run")
    check("a build in the worktree is the daemon's", how_built(in_tree.stderr) == "daemon" and in_tree.returncode == 0, how_built(in_tree.stderr))

    edit(ws, "three")
    client = subprocess.Popen(["cargo", "rustd", "build"], cwd=ws, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    time.sleep(0.01)
    os.killpg(client.pid, signal.SIGKILL)
    client.wait()
    edit(ws, "four")
    _, after_kill = here.cargo("run")
    check("the daemon outlives a client that is killed", status()[0] == pid and after_kill.returncode == 0 and "hello world" in after_kill.stdout, how_built(after_kill.stderr))

    other = os.path.join(work, "other-build")
    os.makedirs(other, exist_ok=True)
    for name in ("rustd", "cargo-rustd"):
        shutil.copy(os.path.join(os.path.dirname(RUSTD), name), os.path.join(other, name))
    newer = Side(ws, dict(env, PATH=other + os.pathsep + env["PATH"]), True)
    edit(ws, "five")
    _, by_newer = newer.cargo("run")
    replaced = status(newer)[0]
    ran = by_newer.returncode == 0 and "hello world" in by_newer.stdout
    check("a client of another build replaces the daemon", ran and replaced not in (None, pid) and not alive(pid), f"pid {pid} then {replaced}")
    check("the workers of the daemon it replaced are gone", not any(alive(p) for p in workers_now), f"{workers_now}")
    newer.cargo("stop")

    quick = Side(ws, dict(env, RUSTD_IDLE_SECS="3"), True)
    quick.cargo("build")
    pid, _, _ = status()
    wait_warm(quick, ws)
    _, workers, _ = status()
    time.sleep(6)
    gone = not alive(pid) and not any(alive(p) for p in workers)
    check("idle for RUSTD_IDLE_SECS, the daemon and its workers exit", pid is not None and workers and gone, f"daemon {pid}, workers {workers}")

    here.cargo("build")
    wait_warm(here, ws)
    pid, workers, _ = status()
    os.kill(pid, signal.SIGKILL)
    time.sleep(1)
    check("workers exit when the daemon is killed", workers and not any(alive(p) for p in workers), f"workers {workers}")
    edit(ws, "six")
    _, again = here.cargo("run")
    check("the build after that starts a daemon again and succeeds", again.returncode == 0 and "hello world" in again.stdout and status()[0] not in (None, pid))
    here.cargo("stop")
    abuse(work, ws, tree, env, check, status, alive)
    return checks


def abuse(work, ws, tree, env, check, status, alive):
    """The daemon under what it is not asked for politely: many clients at once, a client
    and a daemon killed while a build is asked for, and a client that is another user."""
    here = Side(ws, env, True)
    git = lambda *args: subprocess.run(["git", *args], cwd=ws, check=True, capture_output=True)
    trees = [ws, tree]
    for n in (2, 3):
        trees.append(os.path.realpath(os.path.join(work, f"life-worktree{n}")))
        git("worktree", "add", "-q", trees[-1])
    sides = [Side(root, env, True) for root in trees]
    for side in sides:
        side.cargo("build")
    for side in sides:
        wait_warm(side, side.ws)
    pid, workers, _ = status()
    for n, root in enumerate(trees):
        rewrite(os.path.join(root, "util/src/lib.rs"), 'format!("hello {name}")', f'format!("tree{n} {{name}}")')
    spawn = lambda root, *args: subprocess.Popen(["cargo", "rustd", *args], cwd=root, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, stdin=subprocess.DEVNULL, start_new_session=True)
    clients = [(n, spawn(root, "run", f"c{i}")) for i in range(4) for n, root in enumerate(trees)]
    ended = [(n, i // 4, client.communicate(timeout=600), client.returncode) for i, (n, client) in enumerate(clients)]
    right = [code == 0 and out[0] == f"tree{n} c{i}\n" for n, i, out, code in ended]
    by_daemon = sum(how_built(out[1]) == "daemon" for _, _, out, _ in ended)
    check("16 clients at once on 4 worktrees each run their own tree's program", all(right) and status()[0] == pid and status()[1] == workers, f"{sum(right)} of 16 right, {by_daemon} built by the daemon, {len(workers)} worker")

    # A worker that is stopped holds the build a client asks for until it is continued.
    edit = lambda text: open(os.path.join(ws, "app/src/main.rs"), "a").write(f"\n// {text}\n")
    held = lambda: [os.kill(p, signal.SIGSTOP) for p in workers]
    freed = lambda: [os.kill(p, signal.SIGCONT) for p in workers if alive(p)]
    edit("held one")
    held()
    client = spawn(ws, "build")
    time.sleep(1)
    waits = client.poll() is None
    os.killpg(client.pid, signal.SIGKILL)
    client.wait()
    freed()
    _, after = here.cargo("run", "again")
    check("a client killed while its build is held leaves the daemon serving", waits and after.returncode == 0 and after.stdout == "tree0 again\n" and status()[0] == pid and how_built(after.stderr) == "daemon", how_built(after.stderr))

    edit("held two")
    held()
    client = spawn(ws, "run", "killed")
    time.sleep(1)
    waits = client.poll() is None
    os.kill(pid, signal.SIGKILL)
    freed()
    out, err = client.communicate(timeout=600)
    said = how_built(err)
    check("the build whose daemon is killed is cargo's, is right, and says so", waits and client.returncode == 0 and out == "tree0 killed\n" and "the daemon ended before it answered" in said, said)
    time.sleep(1)
    check("the workers of the killed daemon are gone", not any(alive(p) for p in workers), f"{workers}")
    wait_warm(here, ws)
    edit("after")
    _, after = here.cargo("run", "after")
    check("the build after that is a new daemon's", after.returncode == 0 and after.stdout == "tree0 after\n" and status()[0] not in (None, pid) and how_built(after.stderr) == "daemon", how_built(after.stderr))

    other = os.path.join(work, "other-build")
    older = Side(ws, dict(env, PATH=other + os.pathsep + env["PATH"]), True)
    pid = status()[0]
    seen = older.cargo("status")[1].stdout.splitlines()[0]
    check("status by another build says the daemon is another build, and leaves it", "another build of rustd" in seen and status()[0] == pid, seen)
    edit("skew")
    _, skewed = older.cargo("run", "skew")
    said = how_built(skewed.stderr)
    check("a build by another build of rustd is cargo's, says why, and replaces the daemon", skewed.returncode == 0 and skewed.stdout == "tree0 skew\n" and "another build of rustd" in said and status(older)[0] not in (None, pid) and not alive(pid), said)
    wait_warm(older, ws)
    edit("back")
    _, back = here.cargo("run", "back")
    check("and the first build of rustd replaces that one the same way", back.returncode == 0 and back.stdout == "tree0 back\n" and "another build of rustd" in how_built(back.stderr) and status()[0] is not None, how_built(back.stderr))
    wait_warm(here, ws)

    stranger(env["RUSTD_HOME"], check)
    here.cargo("stop")


CONNECT = "import socket, sys\ns = socket.socket(socket.AF_UNIX)\ns.settimeout(5)\ntry:\n    s.connect(sys.argv[1])\nexcept OSError as e:\n    print('connect:', e.strerror)\n    sys.exit()\ns.sendall(sys.argv[2].encode() + b'\\n')\ntry:\n    print('answer:', s.recv(65536).decode())\nexcept OSError as e:\n    print('answer:', e)\n"

LISTEN = "import os, socket, sys\ns = socket.socket(socket.AF_UNIX)\ns.bind(sys.argv[1])\nos.chmod(sys.argv[1], 0o666)\ns.listen()\nprint('listening', flush=True)\ns.settimeout(20)\ntry:\n    c, _ = s.accept()\n    c.settimeout(5)\n    print('heard:', len(c.recv(65536)))\nexcept OSError as e:\n    print('heard: nothing,', e)\n"


def stranger(home, check):
    """A second user at the daemon's socket: kept out by the directory and the socket's
    modes, and turned away by the daemon when those are opened to it."""
    name = "rustd-stranger"
    sudo = lambda *argv: subprocess.run(["sudo", "-n", *argv], capture_output=True, text=True)
    if sudo("true").returncode != 0:
        check("a second user is turned away (needs sudo, which is not here)", os.environ.get("RUSTD_NO_SUDO") == "1", "not run")
        return
    sudo("userdel", name)
    made = sudo("useradd", "--system", "--no-create-home", "--shell", "/usr/sbin/nologin", name)
    sock = os.path.join(home, "daemon.sock")
    version = re.search(r"version (\S+),", subprocess.run(["cargo", "rustd", "status"], env=dict(os.environ, RUSTD_HOME=home, PATH=os.path.dirname(RUSTD) + os.pathsep + os.environ["PATH"]), capture_output=True, text=True).stdout)[1]
    request = json.dumps({"version": version, "replaces": False, "request": "Status"})
    knock = lambda user: subprocess.run([*(["sudo", "-n", "-u", user] if user else []), sys.executable, "-c", CONNECT, sock, request], capture_output=True, text=True, cwd="/").stdout.strip()
    try:
        mine = knock(None)
        check("the user's own raw request is answered", made.returncode == 0 and '"Status"' in mine, mine[:60])
        modes = (os.stat(home).st_mode & 0o777, os.stat(sock).st_mode & 0o777)
        closed = knock(name)
        check("a second user can't reach the socket", modes == (0o700, 0o600) and closed == "connect: Permission denied", f"modes {modes[0]:o} {modes[1]:o}, {closed}")
        # Every directory above has to let the second user through for the daemon to be the one that refuses.
        opened, at = [], home
        while at != "/" and os.stat(at).st_uid == os.getuid():
            opened.append((at, os.stat(at).st_mode & 0o7777))
            os.chmod(at, opened[-1][1] | 0o011)
            at = os.path.dirname(at)
        os.chmod(sock, 0o666)
        try:
            refused = knock(name)
        finally:
            os.chmod(sock, 0o600)
            for at, mode in opened:
                os.chmod(at, mode)
        log = open(os.path.join(home, "daemon.log")).read()
        check("with the modes opened, the daemon turns the second user away by its uid", refused.startswith("answer:") and "Status" not in refused and "the peer is user" in log, f"{refused!r}")
        mine = knock(None)
        check("and still answers its own user", '"Status"' in mine, mine[:60])
        # The other way round: the second user listens where a command line points the
        # user's client, which sends its request and its variables to whoever serves it.
        trap = os.path.join(tempfile.mkdtemp(prefix="rustd-trap-", dir="/tmp"), "sock")
        os.chmod(os.path.dirname(trap), 0o777)
        listener = subprocess.Popen(["sudo", "-n", "-u", name, sys.executable, "-c", LISTEN, trap], stdout=subprocess.PIPE, text=True, cwd="/")
        try:
            listener.stdout.readline()
            asked = subprocess.run([RUSTD, "sync", trap], capture_output=True, text=True, timeout=60)
            heard = listener.communicate(timeout=30)[0].strip()
        finally:
            listener.kill()
            sudo("rm", "-rf", os.path.dirname(trap))
        check("the client says nothing to a socket a second user serves", asked.returncode != 0 and "the peer is user" in asked.stderr and heard == "heard: 0", f"status {asked.returncode}, {heard}")
    finally:
        sudo("userdel", name)


def table(rows):
    """The table of the report: for every step the seconds of `cargo rustd` and of cargo."""
    steps = ["cold build", "warm-up", "warm build", "cold test", "test warm-up", "warm test", "edit build", "edit test", "broken", "mended"]
    out = ["| Project | " + " | ".join(steps) + " | Differences |", "|---|" + "---:|" * len(steps) + "---|"]
    for row in rows:
        cells = []
        for step in steps:
            ours, theirs = row["times"].get(step, (None, None))
            who = row["built"].get(step, "")
            mark = "" if who in ("daemon", "cargo") or "warm-up" in step else " (c)" if who.startswith("cargo") else ""
            if "warm-up" in step:
                mark = "" if who and all(s == "ready" for s in who.split("; ")) else " (f)"
            cells.append("-" if ours is None else f"{ours:.2f}{mark}" if theirs is None else f"{'**' if who == 'daemon' else ''}{ours:.2f}{'**' if who == 'daemon' else ''}{mark} / {theirs:.2f}")
        out.append(f"| {row['project']} {row['tag']} | " + " | ".join(cells) + f" | {len(row['wrong'])} |")
    return "\n".join(out)


def main():
    argv = sys.argv[1:]
    names = [a for a in argv if not a.startswith("--")] or (SMALL if "--small" in argv else list(PROJECTS))
    work = tempfile.mkdtemp(prefix="rustd-cargo-")
    print(f"work {work}; rustd {RUSTD}", flush=True)
    result = dict(rustd=RUSTD, toolchain=TOOLCHAIN, projects=[], flags=[], daemon=[], wrong=[])
    if "--only-checks" not in argv:
        for name in names:
            print(f"{name}:", flush=True)
            row = project(name, PROJECTS[name], work)
            result["projects"].append(row)
            result["wrong"] += [f"{name}: {w}" for w in row["wrong"]]
            if "--keep" not in argv:
                shutil.rmtree(os.path.join(work, name), ignore_errors=True)
    if "--only-projects" not in argv:
        print("flags:", flush=True)
        rows, wrong, counted = flags(work)
        result["flags"], result["flags_status"] = rows, counted
        result["wrong"] += wrong
        print("daemon:", flush=True)
        result["daemon"] = daemon(work)
        result["wrong"] += [f"daemon: {name} {detail}" for name, held, detail in result["daemon"] if not held]
    # A run of some of it leaves the rows of the rest as the run before wrote them.
    llvm = os.environ.get("RUSTD_BACKEND") == "llvm"
    stored = os.path.join(HERE, "cargo_rustd_llvm.json" if llvm else "cargo_rustd.json")
    before = json.load(open(stored)) if os.path.exists(stored) else {}
    rows = {row["project"]: row for row in before.get("projects", []) + result["projects"]}
    result["projects"] = [rows[name] for name in PROJECTS if name in rows]
    if "--only-projects" in argv:
        result.update({part: before.get(part, []) for part in ("flags", "flags_status", "daemon")})
    json.dump(result, open(stored, "w"), indent=1)
    with open(os.path.join(HERE, "CARGO-RUSTD-LLVM.md" if llvm else "CARGO-RUSTD.md"), "w") as md:
        md.write(f"# cargo rustd against cargo, {'LLVM at -O2' if llvm else 'Cranelift'}\n\nGenerated by `harness/cargo_rustd.py`. Seconds, `cargo rustd` / plain cargo for the same step. "
                 "Bold is a build the daemon did; `(c)` one cargo did in its place, with the reason below; `(f)` a warm-up that failed. "
                 "Differences counts the steps whose program output, exit status or test results were not cargo's.\n\n")
        md.write(table(result["projects"]) + "\n\n")
        for row in result["projects"]:
            reasons = [f"{step}: {who}" for step, who in row["built"].items() if who.startswith(("cargo: ", "unchanged: ")) or "failed" in who]
            if reasons or row["wrong"]:
                md.write(f"**{row['project']}**\n\n" + "".join(f"- {r}\n" for r in reasons + row["wrong"]) + "\n")
        if result["flags"]:
            md.write("## Flags\n\n| Round | Command | Built by | Exit | Against cargo |\n|---|---|---|---:|---|\n")
            md.writelines(f"| {label} | `{shown}` | {who} | {code} | {same} |\n" for label, shown, who, code, same in result["flags"])
        if result["daemon"]:
            md.write("\n## Daemon\n\n" + "".join(f"- {'ok' if held else 'NOT'}: {name} {detail}\n" for name, held, detail in result["daemon"]))
    if "--keep" not in argv:
        shutil.rmtree(work, ignore_errors=True)
    print("PASS" if not result["wrong"] else "FAIL:\n" + "\n".join(result["wrong"]))
    sys.exit(1 if result["wrong"] else 0)


if __name__ == "__main__":
    main()
