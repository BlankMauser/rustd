#!/usr/bin/env python3
"""debuginfo.py: what a debugger and a backtrace are told of rustd's code, against rustc's.

Four programs are built by cargo and again with their crates from rustd: the corpus runner, the
stress workspace and rung2 with a panic put into a copy of each, and a small program of its own
that a daemon then follows through edits, each added to the linked binary where that can be done.
Every one is run with RUST_BACKTRACE=1 and has to print, for each panic, the frames of the
workspace's own files that the cargo build prints: the same functions at the same file and line.
`llvm-dwarfdump --verify` has to find nothing wrong with any of the binaries, and gdb has to stop
at a line of a function rustd compiled, say where it is, and step to other lines of it.

Then the cost: the time and the size of the corpus's object with and without debug info, and
whether rustd leaves it out when the profile says `debug = false`.

RUSTD_BACKEND=llvm   the same of LLVM's code. Its optimizer turns calls in tail position into
                     jumps, so a frame the cargo build prints may be missing: those are counted.
--only NAME          one of: corpus stress rung2 edits callers locals types scopes rich profile cost
"""
import bisect, glob, os, re, shutil, struct, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
os.environ.setdefault("CARGO_PROFILE_DEV_DEBUG", "true")
import linkline
from difftest import Fixture, RUSTD, sysroot_rlibs, built

LLVM = os.environ.get("RUSTD_BACKEND") == "llvm"
DWARFDUMP = shutil.which("llvm-dwarfdump") or shutil.which("llvm-dwarfdump-20") or "/usr/lib/llvm-20/bin/llvm-dwarfdump"
PANIC = 'if std::env::var_os("DEBUGINFO_PANIC").is_some() { drop(std::panic::take_hook()); panic!("deliberate"); }'
# What rustc calls the code that drops a coroutine, which rustd has the coroutine itself do.
DROP_GLUE = re.compile(r"^core::ptr::drop_glue::<(.*)>$")

EDITS_TOML = """[package]
name = "sline"
version = "0.1.0"
edition = "2021"
"""
MAIN = "src/main.rs"
EDITS_MAIN = """use std::fmt::Display;

struct Guard(&'static str);

impl Drop for Guard {
    fn drop(&mut self) {
        if self.0.is_empty() {
            println!("never");
        }
    }
}

#[track_caller]
fn must(value: Option<u32>) -> u32 {
    match value {
        Some(it) => it,
        None => panic!("nothing there"),
    }
}

fn pick<T: Display>(items: &[T], at: usize) -> String {
    let _guard = Guard("pick");
    let found = items.get(at).map(|it| it.to_string());
    let length = found.map(|it| it.len() as u32);
    must(length).to_string()
}

fn walk(depth: u32) -> String {
    if depth > 0 {
        return walk(depth - 1) + "!";
    }
    let run = |at: usize| pick(&[1u8, 2, 3], at);
    run(7) + "?"
}

fn main() {
    println!("{}", walk(2));
}
"""
LOCALS_MAIN = """struct Point { x: i32, y: i64 }
struct Pair(u8, f64);

fn area(width: u32, scale: f32, name: &str) -> u32 {
    let point = Point { x: 3, y: -4 };
    let pair = Pair(7, 1.5);
    let list = [1u16, 2, 3];
    let tuple = (true, 'c', 9u64);
    let sum = width + point.x as u32 + pair.0 as u32 + list[1] as u32 + tuple.2 as u32;
    let borrowed = &point;
    println!("{} {} {} {}", sum, scale, name, borrowed.y);
    sum
}

fn main() {
    println!("{}", area(10, 2.5, "abc"));
}
"""
# What gdb is to print of each, stopped at the `println!` of `area`.
LOCALS = [("width", "10"), ("scale", "2.5"), ("name", '"abc"'), ("point", "sline::Point {x: 3, y: -4}"),
          ("pair", "sline::Pair (7, 1.5)"), ("list", "[1, 2, 3]"), ("tuple", "(true, 99 'c', 9)"), ("sum", "31"),
          ("*borrowed", "sline::Point {x: 3, y: -4}")]
OPTIMIZED_OUT = "<optimized out>"

TYPES_MAIN = """#[derive(Clone, Copy)]
struct Point { x: i32, y: i64 }
struct Pair(u8, f64);
#[derive(Clone, Copy)]
enum Shape { Circle(u32), Rect { w: u16, h: u16 }, Dot }
#[derive(Clone, Copy)]
enum Level { Low = 1, Mid = 5, High = 9 }
union Bits { whole: u32, half: u16 }

fn show(width: u32, name: &str, big: [u64; 5]) -> u32 {
    let point = Point { x: 3, y: -4 };
    let pair = Pair(7, 1.5);
    let tuple = (true, 'c', 9u64);
    let shape = Shape::Rect { w: 4, h: 5 };
    let circle = Shape::Circle(11);
    let dot = Shape::Dot;
    let level = Level::Mid;
    let some = Some(42u32);
    let none: Option<u32> = None;
    let boxed: Option<&Point> = Some(&point);
    let missing: Option<&Point> = None;
    let ok: Result<u8, String> = Ok(3);
    let list = vec![1i32, 2, 3];
    let text = String::from("hello");
    let slice = &list[1..];
    let bits = Bits { whole: 0x10002 };
    let borrowed = &point;
    let sum = width + point.x as u32 + pair.0 as u32 + tuple.2 as u32 + big[4] as u32;
    println!("{} {} {} {}", sum, name, borrowed.y, slice.len());
    std::hint::black_box((&shape, &circle, &dot, &level, &some, &none, &boxed, &missing, &ok, &text, &bits, &tuple));
    sum
}

fn main() {
    println!("{}", show(10, "abc", [1, 2, 3, 4, 5]));
}
"""

# Names bound more than once and names of an inner block. Each `// stop` is a line a debugger
# stops at, where the names of SCOPED are asked for: the answer depends on the block.
SCOPES_MAIN = """use std::hint::black_box;

fn scoped(seed: u32) -> u64 {
    let value = seed + 1;
    black_box(value); // stop
    let value = u64::from(value) * 3;
    let total = {
        let inner = value + 5;
        let value = "text";
        black_box(inner) + black_box(value.len() as u64) // stop
    };
    black_box(total); // stop
    let mut sum = 0;
    for step in 0..2u64 {
        let value = [step, total];
        sum += black_box(value[1]); // stop
    }
    match black_box(Some(sum)) {
        Some(value) if value > 1 => black_box(value + total), // stop
        _ => 0,
    }
}

fn main() {
    println!("{}", scoped(black_box(4)));
}
"""
# Closures, what an `async fn` keeps, pointers to `dyn` and enums with fields or without a tag
# of their own: the types a debugger's printers for Rust look into the shape of.
RICH_MAIN = """use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

trait Named { fn name(&self) -> u32; }
struct Dog { legs: u32 }
impl Named for Dog { fn name(&self) -> u32 { self.legs } }
enum Tree { Leaf, Node(Box<Tree>, u32) }
enum Wide { A(u64, u8), B { text: &'static str }, C }

struct Once(bool);
impl Future for Once {
    type Output = u32;
    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<u32> {
        if self.0 { Poll::Ready(7) } else { self.0 = true; Poll::Pending }
    }
}

async fn steps(start: u32) -> u32 {
    let first = Once(false).await + start;
    let kept = [first, 2];
    let second = Once(false).await;
    kept[0] + second
}

fn show(seed: u32) -> u32 {
    let offset = seed + 1;
    let mut count = 0u64;
    let by_ref = |x: u32| x + offset;
    let by_move = move |x: u32| x * offset;
    let mut by_mut = || { count += 1; count };
    let dog = Dog { legs: 4 };
    let object: &dyn Named = &dog;
    let boxed: Box<dyn Named> = Box::new(Dog { legs: 3 });
    let leaf = Tree::Leaf;
    let node = Tree::Node(Box::new(Tree::Leaf), 5);
    let wide = Wide::A(9, 1);
    let wide_b = Wide::B { text: "hi" };
    let wide_c = Wide::C;
    let nested: Option<Option<bool>> = Some(None);
    let ordering = std::cmp::Ordering::Less;
    let mut future = Box::pin(steps(seed));
    let fresh = steps(seed);
    let mut cx = Context::from_waker(Waker::noop());
    let _ = future.as_mut().poll(&mut cx);
    let once = &*future;
    let _ = std::hint::black_box(&once);
    let sum = by_ref(1) + by_move(2) + by_mut() as u32 + object.name() + boxed.name();
    println!("{}", sum);
    std::hint::black_box((&leaf, &node, &wide, &wide_b, &wide_c, &nested, &ordering, &fresh));
    let _ = future.as_mut().poll(&mut cx);
    let _ = future.as_mut().poll(&mut cx);
    sum
}

fn main() {
    println!("{}", show(std::hint::black_box(10)));
}
"""
RICH = ["by_ref", "by_move", "by_mut", "object", "boxed", "leaf", "node", "wide", "wide_b", "wide_c", "nested",
        "ordering", "fresh", "*once", "future"]
SCOPED = ["seed", "value", "inner", "total", "sum", "step"]
TYPES = ["width", "name", "big", "point", "pair", "tuple", "shape", "circle", "dot", "level", "some", "none", "boxed",
         "missing", "ok", "list", "text", "slice", "bits", "*borrowed", "sum"]

# Panics behind functions rustc inlines even in a dev build, and behind `#[track_caller]` ones.
CALLERS_MAIN = """use std::panic::{catch_unwind, Location};

#[inline(always)]
fn always(items: &[u32], at: usize) -> u32 {
    items[at]
}

#[inline]
fn hinted(items: &[u32], at: usize) -> u32 {
    always(items, at) + 1
}

fn indexes() -> u32 {
    hinted(&[1, 2, 3], std::env::args().count() + 8)
}

#[track_caller]
fn inner(value: Option<u32>) -> u32 {
    value.expect("inner")
}

#[track_caller]
fn outer(value: Option<u32>) -> u32 {
    inner(value) + 1
}

fn tracked() -> u32 {
    outer(None) + 1
}

#[track_caller]
fn here() -> u32 {
    panic!("called from line {}", Location::caller().line())
}

fn located() -> u32 {
    here() + 1
}

trait Named {
    #[track_caller]
    fn must(&self) -> u32;
}

impl Named for u8 {
    fn must(&self) -> u32 {
        panic!("through a trait")
    }
}

fn dynamic() -> u32 {
    let it: &dyn Named = &3u8;
    it.must() + 1
}

fn closure() -> u32 {
    let take = |value: Option<u32>| value.unwrap() + 1;
    take(None) + 1
}

fn bound() -> u32 {
    let items = [1u32, 2, 3];
    let item = items[std::env::args().count() + 8];
    item + 1
}

fn main() {
    let cases: [fn() -> u32; 6] = [indexes, tracked, located, dynamic, closure, bound];
    for case in cases {
        let _ = catch_unwind(case);
    }
}
"""

# (what the step is, [(old, new)])
EDITS = [
    ("nothing edited", []),
    ("a line added above everything", [("use std::fmt::Display;", "// One line further down.\nuse std::fmt::Display;")]),
    ("the function that panics made longer", [("        None => panic!", "        Some(0) => 1,\n        None => panic!")]),
    ("its caller, with the call on another line", [("    must(length).to_string()", '    let found = must(length);\n    found.to_string()')]),
    ("a function added between them", [
        ("fn walk(", "fn between(at: usize) -> String {\n    pick(&[1u8, 2, 3], at) + \".\"\n}\n\nfn walk("),
        ("let run = |at: usize| pick(&[1u8, 2, 3], at);", "let run = |at: usize| between(at);"),
    ]),
    ("lines taken away again", [("// One line further down.\n", "")]),
]


def panics(stderr):
    """The panics a run printed: where each says it was and what it said, and its frames as
    [name, place]."""
    found, lines = [], stderr.splitlines()
    for at, line in enumerate(lines):
        if "panicked at" in line:
            said = lines[at + 1].strip() if at + 1 < len(lines) else ""
            found.append((line.split("panicked at")[1].strip() + " " + said, []))
        elif found and (frame := re.match(r"\s+\d+: (.*)", line)):
            found[-1][1].append([frame.group(1).strip(), None])
        elif found and found[-1][1] and (place := re.match(r"\s+at (.*)", line)):
            found[-1][1][-1][1] = place.group(1).strip()
    return found


def own(frames):
    """The frames of the workspace's own files, by function and file:line."""
    kept = []
    for name, place in frames:
        if place and place.startswith("./"):
            glue = DROP_GLUE.match(name)
            kept.append((glue.group(1) if glue else name, place.rsplit(":", 1)[0]))
    return kept


def within(ours, theirs):
    """Whether `ours` is `theirs` with frames left out."""
    rest = iter(theirs)
    return all(frame in rest for frame in ours)


def backtraces(binary, cwd, env=None):
    ran = subprocess.run([binary], capture_output=True, text=True, errors="replace", cwd=cwd, timeout=900,
                         env={**os.environ, "RUST_BACKTRACE": "1", **(env or {})})
    return panics(ran.stderr)


class Tally:
    def __init__(self):
        self.wrong, self.lines = 0, []

    def say(self, what, verdict, more=""):
        self.wrong += verdict.isupper()
        print(f"{what:58} {verdict:14} {more}", flush=True)


def jumps_away(binary, name):
    """Whether the function rustd compiled under that name leaves by jumping to another one
    rather than calling it, so that it has no frame while the other runs. Its range is what the
    debug info says, and the jump is read from the disassembly; None where that cannot be read."""
    said = subprocess.run([DWARFDUMP, f"--name={name}", binary], capture_output=True, text=True).stdout
    entries = [it for it in said.split("\n\n") if "DW_TAG_subprogram" in it]
    if not entries and re.fullmatch(r"\w+(::\w+)*", name):
        # A function with a symbol of its own is named by its last part and its mangled path.
        parts = name.split("::")
        said = subprocess.run([DWARFDUMP, f"--name={parts[-1]}", binary], capture_output=True, text=True).stdout
        mangled = "".join(f"{len(part)}{part}" for part in parts)
        entries = [it for it in said.split("\n\n") if "DW_TAG_subprogram" in it
                   and re.search(rf'DW_AT_linkage_name\s+\("_R\w*?{mangled}"\)', it)]
    ranges = [(low.group(1), high.group(1)) for it in entries
              if (low := re.search(r"DW_AT_low_pc\s+\((0x[0-9a-f]+)\)", it))
              and (high := re.search(r"DW_AT_high_pc\s+\((0x[0-9a-f]+)\)", it))]
    if not ranges or not shutil.which("objdump"):
        return None
    for low, high in ((int(low, 16), int(high, 16)) for low, high in ranges):
        listed = subprocess.run(["objdump", "-d", "--no-show-raw-insn", f"--start-address={low:#x}",
                                 f"--stop-address={high:#x}", binary], capture_output=True, text=True).stdout
        targets = [int(it, 16) for it in re.findall(r"^\s*[0-9a-f]+:\s+(?:jmp|b)\s+([0-9a-f]+)\b", listed, re.M)]
        if any(not low <= target < high for target in targets):
            return True
    return False


def compare(tally, what, theirs, ours, binary=None):
    """Holds the frames of each panic of rustd's binary against the cargo build's."""
    differ, fewer, frames, shown, absent = 0, 0, 0, "", set()
    for (their_at, their_frames), (our_at, our_frames) in zip(theirs, ours):
        want, got = own(their_frames), own(our_frames)
        frames += len(got)
        if their_at == our_at and want == got:
            continue
        if LLVM and their_at == our_at and got and within(got, want):
            fewer += 1
            absent.update(frame[0] for frame in want if frame not in got)
            continue
        differ += 1
        shown = shown or f"at {our_at}: {[g for g in got if g not in want][:2]} for {[w for w in want if w not in got][:2]}"
    if not theirs or len(theirs) != len(ours):
        return tally.say(what, "NOT THE PANICS", f"{len(ours)} for {len(theirs)}")
    if not frames:
        return tally.say(what, "NO FRAMES")
    more = f"{len(theirs)} panics, {frames} frames"
    if fewer:
        # A frame may only be missing because its function left by a jump: anything else is a
        # frame the debug info lost.
        left = {name: jumps_away(binary, name) if binary else None for name in sorted(absent)}
        kept = [name for name, jumps in left.items() if jumps is False]
        if kept:
            return tally.say(what, "FRAMES LOST", f"{more}; {kept[0]} calls and has no frame")
        seen = sum(1 for jumps in left.values() if jumps)
        more += (f", {fewer} with frames the optimizer left out: {seen} of {len(left)} functions seen to leave"
                 f" by a jump, first {next(iter(left))[:60]}")
    tally.say(what, "DIFFERENT" if differ else "same", more + (f"; {differ} differ, first {shown}" if differ else ""))


def verify(tally, what, binary):
    read = subprocess.run([DWARFDUMP, "--verify", binary], capture_output=True, text=True)
    units = subprocess.run([DWARFDUMP, "--debug-info", "--show-children=0", binary], capture_output=True, text=True).stdout
    ours = len(re.findall(r'DW_AT_producer\s+\("rustd ', units))
    wrong = read.returncode != 0 or "No errors." not in read.stdout
    said = next((l for l in read.stdout.splitlines() if "error" in l.lower()), "")
    verdict = "INVALID" if wrong else "sound" if ours else "NO UNIT"
    tally.say(f"{what}: llvm-dwarfdump --verify", verdict, said[:80] or f"{ours} units of rustd's")


def frames_of(binary):
    """Where the code of each frame an unwinder finds starts and ends: the ones in the table
    the `PT_GNU_EH_FRAME` program header leads to. A splice adds frames to that table that
    no section of the file is said to hold."""
    data = open(binary, "rb").read()
    phoff, = struct.unpack_from("<Q", data, 0x20)
    phentsize, phnum = struct.unpack_from("<HH", data, 0x36)
    headers = [struct.unpack_from("<IIQQQQQQ", data, phoff + i * phentsize) for i in range(phnum)]
    loads = [(vaddr, offset, filesz) for kind, _, offset, vaddr, _, filesz, _, _ in headers if kind == 1]

    def at(address):
        return next(offset + address - vaddr for vaddr, offset, filesz in loads if vaddr <= address < vaddr + filesz)

    def pointer(encoding, address):
        """The address written at `address` as `encoding` says, and how many bytes it takes."""
        size, form = {0x00: (8, "<Q"), 0x03: (4, "<I"), 0x04: (8, "<Q"), 0x0b: (4, "<i"), 0x0c: (8, "<q")}[encoding & 0x0f]
        value, = struct.unpack_from(form, data, at(address))
        return value + (address if encoding & 0x70 == 0x10 else 0), size

    def code_encoding(common):
        """How the frames of the common entry at `common` write where their code starts."""
        rest = at(common) + 9
        end = data.index(b"\0", rest)
        letters, rest = data[rest:end], end + 1
        for _ in range(4):
            while data[rest] & 0x80:
                rest += 1
            rest += 1
        for letter in letters[1:]:
            if letter == ord("R"):
                return data[rest]
            if letter == ord("L"):
                rest += 1
            elif letter == ord("P"):
                rest += 1 + pointer(data[rest], 0)[1]
        return 0

    table = next((vaddr for kind, _, _, vaddr, _, _, _, _ in headers if kind == 0x6474e550), None)
    if table is None:
        return {}
    _, frame_encoding, count_encoding, table_encoding = data[at(table):at(table) + 4]
    _, size = pointer(frame_encoding, table + 4)
    count, counted = pointer(count_encoding, table + 4 + size)
    assert table_encoding == 0x3b, f"a search table written as {table_encoding:#x}"
    ends = {}
    for i in range(count):
        _, frame = struct.unpack_from("<ii", data, at(table + 4 + size + counted + i * 8))
        frame += table
        encoding = code_encoding(frame + 4 - struct.unpack_from("<I", data, at(frame + 4))[0])
        start, size_of = pointer(encoding, frame + 8)
        length, _ = pointer(encoding & 0x0f, frame + 8 + size_of)
        ends[start] = start + length
    return ends


def own_ranges(binary):
    """Where the code of each function of rustd's is, as its units say."""
    units = subprocess.run([DWARFDUMP, "--debug-info", "--show-children=0", binary], capture_output=True, text=True).stdout
    ours = set()
    for unit in units.split("DW_TAG_compile_unit")[1:]:
        if re.search(r'DW_AT_producer\s+\("rustd ', unit):
            # The unit's own entry ends at the first empty line; what follows is of its functions.
            ours |= {(int(a, 16), int(b, 16)) for a, b in re.findall(r"\[0x([0-9a-f]+), 0x([0-9a-f]+)\)", unit.split("\n\n")[0])}
    return ours


def unlined(binary):
    """The runs of code in rustd's functions that no row of the line table gives a line.

    A row is of the code from its address to the next row's in its sequence, and a row of line 0
    gives none. A function is looked at as far as the unwinder has it: past that it is filled, and
    nothing runs there. One the unwinder does not have and with no row at all is assembly alone,
    which `unwinding` counts."""
    rows = subprocess.run([DWARFDUMP, "--debug-line", binary], capture_output=True, text=True).stdout
    lined, last = [], None
    for address, line, flags in re.findall(r"^0x([0-9a-f]{16}) +(\d+)(?: +\d+)+(.*)$", rows, re.M):
        address = int(address, 16)
        if last and last[1] and address > last[0]:
            lined.append((last[0], address))
        last = None if "end_sequence" in flags else (address, int(line))
    lined.sort()
    starts, ends, gaps = [a for a, _ in lined], frames_of(binary), []
    for a, b in sorted(own_ranges(binary)):
        b, at = min(b, ends.get(a, b)), a
        i, any_row, holes = max(bisect.bisect_right(starts, a) - 1, 0), False, []
        while i < len(lined) and lined[i][0] < b:
            x, y = lined[i]
            if y > at:
                any_row = True
                if x > at:
                    holes.append((at, x))
                at = y
            i += 1
        if at < b:
            holes.append((at, b))
        if any_row or a in ends:
            gaps += holes
    return gaps


def unwinding(tally, what, binary):
    """Whether the functions a debugger is told of are the ones an unwinder is told of.

    Every run of code in a unit of rustd's has to begin where a frame of the unwinder's begins and
    end no earlier than it. It may end later: a function shorter than a jump to another place is
    filled up to the length of one, for an edit to put that jump there, and the fill is not code
    to unwind from. A `#[naked]` function is its assembly, which says nothing of unwinding and has
    no lines either: one with no entry and no line is that."""
    ours = own_ranges(binary)
    ends = frames_of(binary)
    bare = sorted((a, b) for a, b in ours if a not in ends)
    rows = subprocess.run([DWARFDUMP, "--debug-line", binary], capture_output=True, text=True).stdout if bare else ""
    # The row a sequence ends with is of the address past its code, where another function may begin.
    lined = sorted({int(a, 16) for a in re.findall(r"^0x([0-9a-f]{16}) +[1-9](?!.*end_sequence)", rows, re.M)})
    without = sum(any(a <= row < b for row in lined) for a, b in bare)
    beyond = sum(a in ends and ends[a] > b for a, b in ours)
    filled = sum(a in ends and ends[a] < b for a, b in ours)
    verdict = "NO FUNCTIONS" if not ours else "DISAGREE" if without or beyond else "agree"
    tally.say(f"{what}: .eh_frame and the ranges", verdict,
              f"{len(ours)} functions, {without} without an entry, {beyond} that end before theirs, {filled} filled past theirs, {len(bare) - without} of assembly alone")
    gaps = unlined(binary)
    worst = ", ".join(f"{a:#x}+{b - a}" for a, b in sorted(gaps, key=lambda gap: gap[0] - gap[1])[:3])
    tally.say(f"{what}: a line for every instruction", "NO LINE" if gaps or not ours else "all",
              f"{len(gaps)} runs of {sum(b - a for a, b in gaps)} bytes without one in {len(ours)} functions" + (f": {worst}" if gaps else ""))


def profiler(tally, what, binary, cwd, crates):
    """Whether perf, unwinding by `.eh_frame`, names the functions of `crates` and their lines."""
    perf = shutil.which("perf")
    data = os.path.join(tempfile.mkdtemp(prefix="rustd-debuginfo-perf-"), "perf.data")
    record = perf and subprocess.run([perf, "record", "-o", data, "-F", "5000", "--call-graph", "dwarf", binary],
                                     cwd=cwd, capture_output=True, text=True)
    if not perf or record.returncode != 0 or not os.path.exists(data):
        # Not a verdict on the binary: the machine does not let this user profile.
        tally.say(f"{what}: perf", "not run", "no perf here, or it may not sample: see kernel.perf_event_paranoid")
        return
    script = subprocess.run([perf, "script", "-i", data, "-F", "ip,sym,srcline"], capture_output=True, text=True).stdout
    frames = re.findall(r"^\s+[0-9a-f]+ (\S+)\n\s+(\S+)$", script, re.M)
    mine = [(name, line) for name, line in frames if any(f"{len(c)}{c}" in name or f"{c}::" in name for c in crates)]
    lost = [name for name, line in mine if not re.search(r"\.rs:\d+$", line)]
    stacks = [stack for stack in script.split("\n\n") if stack.strip()]
    through = sum(bool(re.search(r"4main\b|::main\b", stack)) for stack in stacks)
    verdict = "NO SAMPLES" if not mine else "NO LINE" if lost else "NOT UNWOUND" if not through else "named"
    tally.say(f"{what}: perf, unwinding by .eh_frame", verdict,
              f"{len(mine)} frames of rustd's functions in {len(stacks)} samples, {len(lost)} without a line, {through} samples back to main"
              + (f": {lost[0][:60]}" if lost else ""))


def unit_of(binary, address):
    found = subprocess.run([DWARFDUMP, f"--lookup={address:#x}", binary], capture_output=True, text=True).stdout
    producer = re.search(r'DW_AT_producer\s+\("([^"]*)"\)', found)
    return producer.group(1) if producer else "nothing"


def debugger(tally, what, binary, cwd, place, env=None):
    """Has gdb stop at `place` (file:line), say where that is, and step on from there."""
    commands = ["set pagination off", "set confirm off", "set print demangle off", "set print asm-demangle off",
                "set startup-with-shell off", f"break {place}", "run", "echo <where>\\n", "bt 3", "echo <mapped>\\n",
                "info proc mappings", "print/x $pc", "echo <line>\\n", "info line *$pc",
                *[x for _ in range(4) for x in ("echo <step>\\n", "step", "frame")], "kill"]
    for name, value in (env or {}).items():
        commands.insert(5, f"set environment {name} {value}")
    args = [x for c in commands for x in ("-ex", c)]
    try:
        ran = subprocess.run(["gdb", "-q", "-nx", "-batch", *args, binary], capture_output=True, text=True,
                             errors="replace", cwd=cwd, timeout=300)
    except subprocess.TimeoutExpired:
        return tally.say(f"{what}: gdb at {place}", "NO ANSWER")
    out = ran.stdout
    file, line = place.rsplit(":", 1)
    said = f"{what}: gdb at {os.path.basename(file)}:{line}"
    # A breakpoint at a line with code in more than one place is numbered 1.1, 1.2 by gdb 13 on.
    stop = re.search(r"Breakpoint 1(?:\.\d+)?, .*? at (\S+):(\d+)", out, re.S)
    if not stop or stop.group(2) != line or not file.endswith(os.path.basename(stop.group(1))):
        return tally.say(said, "NOT STOPPED", (out.strip().splitlines() or ["nothing printed"])[-1][:80])
    value = re.search(r"\$1 = (0x[0-9a-f]+)", out)
    base = re.search(r"^\s*(0x[0-9a-f]+)\s+0x[0-9a-f]+\s+0x[0-9a-f]+\s+0x0\s", out.split("<mapped>")[1], re.M)
    if not value or not base:
        return tally.say(said, "NO ADDRESS")
    # The file's address of the code, whether or not the program is loaded where it says.
    pc, base = int(value.group(1), 16), int(base.group(1), 16)
    producers = [unit_of(binary, address) for address in (pc - base, pc)]
    if not any(it.startswith("rustd") for it in producers):
        return tally.say(said, "NOT RUSTD'S", f"the code is of {producers[0]}")
    name = re.search(r"^#0\s+(.*?) \(", out.split("<where>")[1], re.M)
    if f"Line {line} of" not in out.split("<line>")[1]:
        return tally.say(said, "NO LINE", out.split("<line>")[1].strip()[:80])
    stepped = {m for part in out.split("<step>")[1:] for m in re.findall(r"^#0 .*? at (\S+:\d+)", part, re.M)}
    stepped = {s for s in stepped if not s.endswith(f":{line}")}
    if not stepped:
        return tally.say(said, "NOT STEPPED")
    frames = len(re.findall(r"^#\d+ ", out.split("<where>")[1].split("<mapped>")[0], re.M))
    tally.say(said, "stopped", f"in {name.group(1)[:40] if name else '?'}, {frames} frames up, stepped to {', '.join(sorted(stepped))[:60]}")


def call_site(ours):
    """Where the function that panics first is called from, for a debugger to stop at and step
    into it from: the panic itself is the last line that runs of the function it is in."""
    for _, frames in ours:
        if places := own(frames):
            return places[min(1, len(places) - 1)][1]


def program(tally, what, workspace, crates, package, inject=None):
    ws = workspace
    if inject:
        ws = tempfile.mkdtemp(prefix=f"rustd-debuginfo-{what}-")
        shutil.copytree(workspace, ws, dirs_exist_ok=True, ignore=shutil.ignore_patterns("target"))
        path, old, new = inject
        text = open(os.path.join(ws, path)).read()
        assert old in text, old
        open(os.path.join(ws, path), "w").write(text.replace(old, new, 1))
    env = {"DEBUGINFO_PANIC": "1"} if inject else {}
    fx = Fixture(ws, crates, ["-p", package])
    fx.rustc_build()
    fx.rustd_build()
    theirs, ours = backtraces(fx.reference, fx.ws, env), backtraces(fx.binary, fx.ws, env)
    compare(tally, f"{what}: frames of each panic", theirs, ours, fx.binary)
    verify(tally, what, fx.binary)
    unwinding(tally, what, fx.binary)
    if place := call_site(ours):
        debugger(tally, what, fx.binary, fx.ws, place[2:], env)
    else:
        tally.say(f"{what}: gdb", "NO PLACE")
    functions, count = described(fx.binary)
    tally.say(f"{what}: what a debugger is told of", "counted", f"{functions} functions, {count} parameters and locals in them")
    if not inject:
        profiler(tally, what, fx.binary, fx.ws, crates + [package])
    return fx


def edits(tally):
    """The program of its own, a step at a time under a daemon."""
    ws = tempfile.mkdtemp(prefix="rustd-debuginfo-edits-")
    os.makedirs(os.path.join(ws, "src"))
    for path, text in (("Cargo.toml", EDITS_TOML), (MAIN, EDITS_MAIN)):
        open(os.path.join(ws, path), "w").write(text)
    work = tempfile.mkdtemp(prefix="rustd-debuginfo-out-")
    socket, log_path = os.path.join(work, "rustd.sock"), os.path.join(work, "daemon.log")
    reference = os.path.join(work, "reference")
    fx = Fixture(ws, ["sline"], ["-p", "sline"])
    fx.rustc_build()
    obj, patch, binary = (os.path.join(work, f"sline.{n}") for n in ("o", "patch.o", "bin"))
    link = linkline.with_output(linkline.ahead(list(fx.link_args), [patch, obj], RUSTD), binary)
    symbols = [x for rlib in fx.rlibs + sysroot_rlibs() for x in ("--symbols", rlib)]
    ask = ["--crate", "sline", "--out", obj, "--patch", patch, "--link-cmd", linkline.join(link), "--binary", binary, *symbols]
    out = open(log_path, "a")
    daemon = subprocess.Popen([RUSTD, ws, "--socket", socket, *ask], stderr=out, stdout=out)
    while "watching" not in open(log_path).read():
        if daemon.poll() is not None:
            raise SystemExit(f"the daemon did not start: {log_path}")
        time.sleep(0.05)
    spliced = 0
    for what, changes in EDITS:
        seen = len(open(log_path).read())
        text = open(os.path.join(ws, MAIN)).read()
        for old, new in changes:
            assert old in text, old
            text = text.replace(old, new, 1)
        open(os.path.join(ws, MAIN), "w").write(text)
        time.sleep(0.02)
        reply = subprocess.run([RUSTD, "sync", socket, *ask], capture_output=True, text=True, timeout=120)
        cargo = subprocess.run(["cargo", "build", "-q"], cwd=ws, capture_output=True, text=True,
                               env={**os.environ, "CARGO_TARGET_DIR": reference})
        assert cargo.returncode == 0, cargo.stderr[-2000:]
        if daemon.poll() is not None:
            raise SystemExit(f"the daemon died at {what!r}: {log_path}")
        links = re.findall(r"^link: \S+ \((.*)\)$", open(log_path).read()[seen:], re.M)
        how = "not linked" if not links else "spliced" if links[-1].startswith("spliced") else "linked in full"
        spliced += how == "spliced"
        if not (reply.stdout + reply.stderr).strip().startswith("ok"):
            tally.say(f"edits: {what}", "REFUSED", (reply.stdout + reply.stderr).strip()[:60])
            continue
        compare(tally, f"edits: {what} ({how})", backtraces(built(reference, "sline"), ws), backtraces(binary, ws))
        verify(tally, f"edits: {what}", binary)
        unwinding(tally, f"edits: {what}", binary)
    ours = backtraces(binary, ws)
    if place := call_site(ours):
        debugger(tally, "edits: the last binary", binary, ws, place[2:])
    daemon.kill()
    if os.environ.get("RUSTD_SPLICE") != "0":
        tally.say("edits: added to the linked binary", "spliced" if spliced else "NONE SPLICED", f"{spliced} of {len(EDITS)} steps")


def described(binary):
    """How many functions rustd's units tell a debugger of, and how many variables in them."""
    dumped = subprocess.run([DWARFDUMP, "--debug-info", binary], capture_output=True, text=True).stdout
    functions = variables = 0
    for unit in dumped.split("DW_TAG_compile_unit")[1:]:
        if re.search(r'DW_AT_producer\s+\("rustd', unit):
            functions += unit.count("DW_TAG_subprogram")
            variables += unit.count("DW_TAG_variable") + unit.count("DW_TAG_formal_parameter")
    return functions, variables


def variables(tally):
    """Whether gdb prints the parameters and locals of a function of rustd's as they are.

    A value the optimizer did away with is to be said to be so, and is not counted as wrong."""
    ws = tempfile.mkdtemp(prefix="rustd-debuginfo-locals-")
    os.makedirs(os.path.join(ws, "src"))
    for path, text in (("Cargo.toml", EDITS_TOML), (MAIN, LOCALS_MAIN)):
        open(os.path.join(ws, path), "w").write(text)
    fx = Fixture(ws, ["sline"], ["-p", "sline"])
    fx.rustc_build()
    fx.rustd_build()
    verify(tally, "locals", fx.binary)
    line = LOCALS_MAIN[:LOCALS_MAIN.index("println!")].count("\n") + 1
    commands = ["set pagination off", "set startup-with-shell off", f"break {MAIN}:{line}", "run",
                *[x for name, _ in LOCALS for x in (f"echo <{name}>\\n", f"print {name}")], "kill"]
    ran = subprocess.run(["gdb", "-q", "-nx", "-batch", *[x for c in commands for x in ("-ex", c)], fx.binary],
                         capture_output=True, text=True, errors="replace", cwd=ws, timeout=300)
    printed = {name: re.search(rf"<{re.escape(name)}>\n\$\d+ = (.*)", ran.stdout) for name, _ in LOCALS}
    right = [name for name, value in LOCALS if printed[name] and printed[name].group(1) == value]
    functions, count = described(fx.binary)
    gone = [name for name, _ in LOCALS if printed[name] and OPTIMIZED_OUT in printed[name].group(1)]
    wrong = [f"{name} = {printed[name].group(1) if printed[name] else 'unknown'}" for name, _ in LOCALS
             if name not in right and name not in gone]
    tally.say("locals: gdb prints them", "printed" if not wrong else "NOT AS THEY ARE",
              f"{len(right)} of {len(LOCALS)}, {len(gone)} optimized out, {count} described in {functions} functions"
              + (f", but {wrong[0][:50]}" if wrong else ""))


def callers(tally):
    """Whether panics behind inlined and `#[track_caller]` functions say where they were, and
    have the frames a cargo build's have."""
    ws = tempfile.mkdtemp(prefix="rustd-debuginfo-callers-")
    os.makedirs(os.path.join(ws, "src"))
    for path, text in (("Cargo.toml", EDITS_TOML), (MAIN, CALLERS_MAIN)):
        open(os.path.join(ws, path), "w").write(text)
    fx = Fixture(ws, ["sline"], ["-p", "sline"])
    fx.rustc_build()
    fx.rustd_build()
    compare(tally, "callers: frames of each panic", backtraces(fx.reference, ws), backtraces(fx.binary, ws), fx.binary)
    verify(tally, "callers", fx.binary)


def printed_by(debugger, binary, cwd, place, names):
    """What a debugger prints for each of the names when stopped at a place, addresses aside."""
    commands = ["set pagination off", "set startup-with-shell off", f"break {place}", "run",
                *[x for name in names for x in (f"echo <{name}>\\n", f"print {name}")], "kill"]
    ran = subprocess.run([debugger, "-q", "-batch", *[x for c in commands for x in ("-ex", c)], binary],
                         capture_output=True, text=True, errors="replace", cwd=cwd, timeout=300)
    found = {name: re.search(rf"<{re.escape(name)}>\n\$\d+ = (.*)", ran.stdout) for name in names}
    return {name: re.sub(r"0x[0-9a-f]+", "0x..", it.group(1)) if it else None for name, it in found.items()}


def lldb_path():
    """The search path `rust-lldb` finds an `lldb` on, or None: distributions name theirs by version."""
    if shutil.which("lldb"):
        return os.environ["PATH"]
    for found in sorted(glob.glob("/usr/lib/llvm-*/bin/lldb"), reverse=True):
        return os.path.dirname(found) + os.pathsep + os.environ["PATH"]
    return None


def shown_by_lldb(binary, cwd, file, line, names):
    """What `rust-lldb` shows for each of the names when stopped at a line, addresses aside."""
    commands = [f"breakpoint set --file {file} --line {line}", "run", *[f"frame variable {name}" for name in names], "kill"]
    ran = subprocess.run(["rust-lldb", "--batch", *[x for c in commands for x in ("-o", c)], binary],
                         capture_output=True, text=True, errors="replace", cwd=cwd, timeout=300,
                         env={**os.environ, "PATH": lldb_path()})
    shown = {}
    for name in names:
        found = re.search(rf"^\(lldb\) frame variable {re.escape(name)}\n(.*?)(?=^\(lldb\) )", ran.stdout, re.S | re.M)
        # An address of nothing stays what it is: lldb shows bytes it has no place for as zeros.
        shown[name] = re.sub(r"0x0*[1-9a-f][0-9a-f]*", "0x..", found.group(1).strip()) if found and found.group(1).strip() else None
    return shown


def agreement(names, theirs, ours, lost=()):
    """How many of the names a debugger shows as it does for a rustc build, which it has no
    value for or only part of one, and which it shows otherwise. The names are returned with
    what is said. `lost` are names known to have no whole value, for a debugger that does not
    say so itself."""
    partly = (OPTIMIZED_OUT, "<synthetic pointer>", "not available")
    gone = [name for name in names if ours[name] != theirs[name]
            and (name in lost or ours[name] and any(it in ours[name] for it in partly))]
    wrong = [f"{name} = {ours[name]}, not {theirs[name]}" for name in names if ours[name] != theirs[name] and name not in gone]
    unknown = [name for name in names if theirs[name] is None]
    return (not wrong and not unknown, gone,
            f"{len(names) - len(wrong) - len(gone)} of {len(names)}, {len(gone)} optimized out"
            + (f", but {wrong[0][:160]}" if wrong else "") + (f", rustc's {unknown[0]} unknown" if unknown else ""))


def rich(tally):
    """Whether closures, the states of an `async fn`, pointers to `dyn` and enums are shown as
    those of a rustc build are, by the toolchain's gdb and by its lldb."""
    ws = tempfile.mkdtemp(prefix="rustd-debuginfo-rich-")
    os.makedirs(os.path.join(ws, "src"))
    for path, text in (("Cargo.toml", EDITS_TOML), (MAIN, RICH_MAIN)):
        open(os.path.join(ws, path), "w").write(text)
    fx = Fixture(ws, ["sline"], ["-p", "sline"])
    fx.rustc_build()
    fx.rustd_build()
    verify(tally, "rich", fx.binary)
    reference = os.path.join(ws, "target", "debug", "sline")
    line = RICH_MAIN[:RICH_MAIN.index('println!("{}", sum)')].count("\n") + 1
    lost = []
    if shutil.which("rust-gdb"):
        theirs = printed_by("rust-gdb", reference, ws, f"{MAIN}:{line}", RICH)
        # A name of rustd's own for the table a `dyn` pointer has is no part of what is shown.
        ours = {name: it and re.sub(r" <\.L[^>]*>", "", it)
                for name, it in printed_by("rust-gdb", fx.binary, ws, f"{MAIN}:{line}", RICH).items()}
        same, lost, said = agreement(RICH, theirs, ours)
        tally.say("rich: rust-gdb shows them", "as rustc's" if same else "NOT AS RUSTC'S", said)
    else:
        tally.say("rich: rust-gdb shows them", "not looked at", "no rust-gdb to ask")
    if shutil.which("rust-lldb") and lldb_path():
        theirs = shown_by_lldb(reference, ws, MAIN, line, RICH)
        # lldb shows zeros where a value is there in part, so what gdb found to be so is not held against it.
        same, _, said = agreement(RICH, theirs, shown_by_lldb(fx.binary, ws, MAIN, line, RICH), lost)
        tally.say("rich: rust-lldb shows them", "as rustc's" if same else "NOT AS RUSTC'S", said)
    else:
        tally.say("rich: rust-lldb shows them", "not looked at", "no rust-lldb or no lldb to ask")


def shapes(tally):
    """Whether the debugger shows values of each kind of type as it shows those of a rustc build.

    The one the toolchain wraps with its pretty-printers is asked, so a `Vec` or a `String` is
    shown by what it holds only when the description has the shape the printers look for."""
    debugger = shutil.which("rust-gdb")
    if not debugger:
        return tally.say("types: the debugger shows them", "not looked at", "no rust-gdb to ask")
    ws = tempfile.mkdtemp(prefix="rustd-debuginfo-types-")
    os.makedirs(os.path.join(ws, "src"))
    for path, text in (("Cargo.toml", EDITS_TOML), (MAIN, TYPES_MAIN)):
        open(os.path.join(ws, path), "w").write(text)
    fx = Fixture(ws, ["sline"], ["-p", "sline"])
    fx.rustc_build()
    fx.rustd_build()
    verify(tally, "types", fx.binary)
    place = f"{MAIN}:{TYPES_MAIN[:TYPES_MAIN.index('println!')].count(chr(10)) + 1}"
    theirs = printed_by(debugger, os.path.join(ws, "target", "debug", "sline"), ws, place, TYPES)
    ours = printed_by(debugger, fx.binary, ws, place, TYPES)
    unknown = [name for name in TYPES if theirs[name] is None]
    gone = [name for name in TYPES if ours[name] and OPTIMIZED_OUT in ours[name]]
    wrong = [f"{name} = {ours[name]}, not {theirs[name]}" for name in TYPES if ours[name] != theirs[name] and name not in gone]
    tally.say("types: the debugger shows them", "as rustc's" if not wrong and not unknown else "NOT AS RUSTC'S",
              f"{len(TYPES) - len(wrong) - len(gone)} of {len(TYPES)}, {len(gone)} optimized out"
              + (f", but {wrong[0][:90]}" if wrong else "") + (f", rustc's {unknown[0]} unknown" if unknown else ""))


def scopes(tally):
    """Whether a name is known where its block says and means the binding of that block: at
    several lines the debugger is asked for the same names, and has to answer as it does for a
    rustc build, also where the answer is that there is no such name."""
    debugger = shutil.which("rust-gdb")
    if not debugger:
        return tally.say("scopes: names by their block", "not looked at", "no rust-gdb to ask")
    ws = tempfile.mkdtemp(prefix="rustd-debuginfo-scopes-")
    os.makedirs(os.path.join(ws, "src"))
    for path, text in (("Cargo.toml", EDITS_TOML), (MAIN, SCOPES_MAIN)):
        open(os.path.join(ws, path), "w").write(text)
    fx = Fixture(ws, ["sline"], ["-p", "sline"])
    fx.rustc_build()
    fx.rustd_build()
    verify(tally, "scopes", fx.binary)
    stops = [at + 1 for at, line in enumerate(SCOPES_MAIN.split("\n")) if "// stop" in line]
    asked = right = gone = unknown = 0
    wrong = []
    for line in stops:
        theirs = printed_by(debugger, os.path.join(ws, "target", "debug", "sline"), ws, f"{MAIN}:{line}", SCOPED)
        ours = printed_by(debugger, fx.binary, ws, f"{MAIN}:{line}", SCOPED)
        for name in SCOPED:
            asked += 1
            if ours[name] == theirs[name]:
                right += 1
                unknown += theirs[name] is None
            elif ours[name] and OPTIMIZED_OUT in ours[name]:
                gone += 1
            else:
                wrong.append(f"{name} at line {line} = {ours[name]}, not {theirs[name]}")
    tally.say("scopes: names by their block", "as rustc's" if not wrong else "NOT AS RUSTC'S",
              f"{right} of {asked} at {len(stops)} lines ({unknown} of them not a name there), {gone} optimized out"
              + (f", but {'; '.join(wrong[:3])[:200]}" if wrong else ""))


def settings(tally):
    """Whether the profile's settings for what a debugger reads are followed, or refused with the reason."""
    ws = tempfile.mkdtemp(prefix="rustd-debuginfo-profile-")
    os.makedirs(os.path.join(ws, "src"))
    for path, text in (("Cargo.toml", EDITS_TOML), (MAIN, LOCALS_MAIN)):
        open(os.path.join(ws, path), "w").write(text)
    fx = Fixture(ws, ["sline"], ["-p", "sline"])
    fx.rustc_build()

    def built(**profile):
        """The object rustd writes under the settings: whether it has anything for a debugger, and how many variables."""
        names = {f"CARGO_PROFILE_DEV_{name.upper()}": value for name, value in profile.items()}
        os.environ.update(names)
        try:
            fx.rustd_build()
            return sections(fx.objects[0])[1] > 0, described(fx.objects[0])[1], None
        except SystemExit as refused:
            return None, None, str(refused)
        finally:
            for name in names:
                del os.environ[name]

    for what, profile, lines, locals_ in (
            ("debug = true", {}, True, True),
            ("debug = \"line-tables-only\"", {"debug": "line-tables-only"}, True, False),
            ("debug = 1", {"debug": "1"}, True, False),
            ("strip = \"debuginfo\"", {"strip": "debuginfo"}, False, False),
            ("strip = true", {"strip": "true"}, False, False),
            ("split-debuginfo = \"off\"", {"split_debuginfo": "off"}, True, True),
            ("split-debuginfo = \"packed\"", {"split_debuginfo": "packed"}, None, None),
            ("split-debuginfo = \"unpacked\"", {"split_debuginfo": "unpacked"}, None, None),
            ("split-debuginfo = \"packed\" with debug = false", {"split_debuginfo": "packed", "debug": "false"}, False, False)):
        has, count, refused = built(**profile)
        if lines is None:
            reason = re.search(r"`split-debuginfo.*", refused or "", re.S)
            tally.say(f"profile: {what}", "refused" if reason else "NOT REFUSED",
                      " ".join(reason.group(0).split())[:110] if reason else (refused or "it built")[-110:])
        else:
            followed = has == lines and bool(count) == locals_
            tally.say(f"profile: {what}", "followed" if followed else "NOT FOLLOWED",
                      refused[-110:] if refused else f"{'lines' if has else 'nothing'}, {count} variables")


def sections(path):
    """The size of an object, and of the sections of it a debugger reads."""
    read = subprocess.run(["size", "-A", path], capture_output=True, text=True).stdout
    debug = sum(int(l.split()[1]) for l in read.splitlines() if l.startswith((".debug_", ".rela.debug_")))
    return os.path.getsize(path), debug


def cost(tally):
    """The corpus's object with and without debug info, and what the profile's setting does."""
    fx = Fixture(os.path.join(HERE, "corpus"), ["cases"], ["-p", "runner"])
    fx.rustc_build()
    measured = {}
    for setting in ("0", "1"):
        os.environ["RUSTD_DEBUGINFO"] = setting
        times = []
        for _ in range(3):
            began = time.time()
            fx.rustd_build()
            times.append(time.time() - began)
        measured[setting] = (min(times), *sections(fx.objects[0]), os.path.getsize(fx.binary))
    del os.environ["RUSTD_DEBUGINFO"]
    (t0, s0, d0, b0), (t1, s1, d1, b1) = measured["0"], measured["1"]
    told = f"{d1} of sections a debugger reads, {s1 - s0 - d1} of their relocations"
    tally.say("cost: the corpus's object without debug info", "measured", f"{t0:.2f} s to compile and link, object {s0} bytes, binary {b0} bytes")
    tally.say("cost: with it", "measured" if d1 and not d0 else "NO DIFFERENCE",
              f"{t1:.2f} s ({(t1 / t0 - 1) * 100:+.1f}%), object {s1} bytes ({(s1 / s0 - 1) * 100:+.1f}%, {told}), binary {b1} bytes")
    for setting, expected in (("false", False), ("true", True), ("line-tables-only", True)):
        os.environ["CARGO_PROFILE_DEV_DEBUG"] = setting
        fx.rustd_build()
        has = sections(fx.objects[0])[1] > 0
        tally.say(f"cost: the profile says debug = {setting}", ("described" if has else "left out") if has == expected else "NOT FOLLOWED")
    os.environ["CARGO_PROFILE_DEV_DEBUG"] = "true"


def main():
    only = sys.argv[sys.argv.index("--only") + 1] if "--only" in sys.argv else None
    tally = Tally()
    parts = {
        "corpus": lambda: program(tally, "corpus", os.path.join(HERE, "corpus"), ["cases"], "runner"),
        "stress": lambda: program(tally, "stress", os.path.join(HERE, "stress"), ["sapp", "sbase", "smid"], "sapp",
                                  ("base/src/shapes.rs", "items.iter().map(|it| it.area()).sum()",
                                   "items.iter().map(|it| {\n        " + PANIC + "\n        it.area()\n    }).sum()")),
        "rung2": lambda: program(tally, "rung2", os.path.join(HERE, "rung2"), ["rung2"], "rung2",
                                 ("src/main.rs", ".map_ok(|it| it + 1)", ".map_ok(|it| {\n        " + PANIC + "\n        it + 1\n    })")),
        "edits": lambda: edits(tally),
        "callers": lambda: callers(tally),
        "locals": lambda: variables(tally),
        "types": lambda: shapes(tally),
        "scopes": lambda: scopes(tally),
        "rich": lambda: rich(tally),
        "profile": lambda: settings(tally),
        "cost": lambda: cost(tally),
    }
    for name, part in parts.items():
        if only in (None, name):
            part()
    print(f"{'wrong: ' + str(tally.wrong) if tally.wrong else 'all right'} ({'llvm' if LLVM else 'cranelift'})")
    sys.exit(1 if tally.wrong else 0)


if __name__ == "__main__":
    main()
