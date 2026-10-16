#!/usr/bin/env python3
"""asmedits.py: a daemon follows edits to `asm!` blocks and to the code around them.

The instructions are the machine's, x86-64 or AArch64. Each step is compared with what `cargo build` of the same source prints.
"""
import platform

from includes import follow

# What the blocks say on each architecture: three times and five times a number, a sum of two
# 32-bit registers, the same with a constant, with a named register, and a complement.
X86 = {
    "triple": "lea {out}, [{x} + {x} * 2]", "three": "{x} * 2]", "five": "{x} * 4]",
    "add": '"add {0:e}, {1:e}", inout(reg) a, in(reg) b',
    "add_const": '"add {0:e}, {1:e}", "add {0:e}, {2}", inout(reg) a, in(reg) b, const 9',
    "add_named": '"add eax, {0:e}", "add eax, {1}", in(reg) b, const 9, inout("eax") a',
    "not": '"not {0:e}", inout(reg) a',
}
AARCH64 = {
    "triple": "add {out}, {x}, {x}, lsl #1", "three": "lsl #1", "five": "lsl #2",
    "add": '"add {0:w}, {0:w}, {1:w}", inout(reg) a, in(reg) b',
    "add_const": '"add {0:w}, {0:w}, {1:w}", "add {0:w}, {0:w}, #{2}", inout(reg) a, in(reg) b, const 9',
    "add_named": '"add w9, w9, {0:w}", "add w9, w9, #{1}", in(reg) b, const 9, inout("x9") a',
    "not": '"mvn {0:w}, {0:w}", inout(reg) a',
}
ASM = AARCH64 if platform.machine() in ("arm64", "aarch64") else X86

MAIN = "src/main.rs"
MAIN_TEXT = """use std::arch::asm;

fn scale(x: u64) -> u64 {
    let out: u64;
    unsafe { asm!("%s", x = in(reg) x, out = lateout(reg) out) };
    out
}

fn sum(a: u32, b: u32) -> u32 {
    let mut a = a;
    unsafe { asm!(%s) };
    a + 1
}

fn main() {
    println!("{} {}", scale(7), sum(3, 4));
}
""" % (ASM["triple"], ASM["add"])
NOT = "    unsafe { asm!(%s) };\n" % ASM["not"]
# (what the step is, [(file, old, new)], whether cargo rejects it)
STEPS = [
    ("nothing edited", [], False),
    ("an instruction", [(MAIN, ASM["three"], ASM["five"])], False),
    ("the code around a block", [(MAIN, "a + 1", "a + 2")], False),
    ("an operand added", [(MAIN, ASM["add"], ASM["add_const"])], False),
    ("a block made the same as another", [(MAIN, ASM["five"], ASM["three"])], False),
    ("a block that does not type check", [(MAIN, "in(reg) x,", "in(reg) missing,")], True),
    ("mended", [(MAIN, "in(reg) missing,", "in(reg) x,")], False),
    ("a named register", [(MAIN, ASM["add_const"], ASM["add_named"])], False),
    ("a block added", [(MAIN, "    a + 2", NOT + "    a + 2")], False),
    ("a block removed", [(MAIN, NOT, "")], False),
]

if __name__ == "__main__":
    follow([(MAIN, MAIN_TEXT)], STEPS)
