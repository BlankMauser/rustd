#!/usr/bin/env python3
"""asmedits.py: a daemon follows edits to `asm!` blocks and to the code around them.

x86-64 only. Each step is compared with what `cargo build` of the same source prints.
"""
from includes import follow

MAIN = "src/main.rs"
MAIN_TEXT = """use std::arch::asm;

fn scale(x: u64) -> u64 {
    let out: u64;
    unsafe { asm!("lea {out}, [{x} + {x} * 2]", x = in(reg) x, out = lateout(reg) out) };
    out
}

fn sum(a: u32, b: u32) -> u32 {
    let mut a = a;
    unsafe { asm!("add {0:e}, {1:e}", inout(reg) a, in(reg) b) };
    a + 1
}

fn main() {
    println!("{} {}", scale(7), sum(3, 4));
}
"""
# (what the step is, [(file, old, new)], whether cargo rejects it)
STEPS = [
    ("nothing edited", [], False),
    ("an instruction", [(MAIN, "{x} * 2]", "{x} * 4]")], False),
    ("the code around a block", [(MAIN, "a + 1", "a + 2")], False),
    ("an operand added", [
        (MAIN, '"add {0:e}, {1:e}", inout(reg) a, in(reg) b',
         '"add {0:e}, {1:e}", "add {0:e}, {2}", inout(reg) a, in(reg) b, const 9'),
    ], False),
    ("a block made the same as another", [(MAIN, "{x} * 4]", "{x} * 2]")], False),
    ("a block that does not type check", [(MAIN, "in(reg) x,", "in(reg) missing,")], True),
    ("mended", [(MAIN, "in(reg) missing,", "in(reg) x,")], False),
    ("a named register", [
        (MAIN, '"add {0:e}, {1:e}", "add {0:e}, {2}", inout(reg) a, in(reg) b, const 9',
         '"add eax, {0:e}", "add eax, {1}", in(reg) b, const 9, inout("eax") a'),
    ], False),
    ("a block added", [
        (MAIN, "    a + 2", '    unsafe { asm!("not {0:e}", inout(reg) a) };\n    a + 2'),
    ], False),
    ("a block removed", [(MAIN, '    unsafe { asm!("not {0:e}", inout(reg) a) };\n', "")], False),
]

if __name__ == "__main__":
    follow([(MAIN, MAIN_TEXT)], STEPS)
