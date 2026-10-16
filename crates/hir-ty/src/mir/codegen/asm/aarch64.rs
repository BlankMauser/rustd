//! The function for a block on AArch64. `x19` holds the slot while the block runs: `asm!`
//! takes it as no operand, and a call keeps it.

use std::fmt::Write;

use hir_def::hir::{AsmOptions, AsmPiece, InlineAsmRegOrRegClass};

use super::{Operand, SYMBOL, Wrapper};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reg {
    /// `x` and `w` of one number are one register.
    Gpr(u32),
    /// `v`, `q`, `d`, `s`, `h` and `b` of one number are one register.
    Vector(u32),
    /// One that holds no value here and that the function leaves alone or restores.
    Scratch,
}

impl Reg {
    /// Whether a function has to leave the register as it found it: `x19` to `x28`, and the
    /// low halves of `v8` to `v15`.
    fn kept(self) -> bool {
        matches!(self, Reg::Gpr(20..=28) | Reg::Vector(8..=15))
    }

    fn named(name: &str) -> Result<Reg, String> {
        let name = name.trim_matches('"');
        let numbered = |prefixes: &[&str]| {
            prefixes.iter().find_map(|prefix| name.strip_prefix(prefix)?.parse::<u32>().ok())
        };
        match (numbered(&["x", "w"]), numbered(&["v", "q", "d", "s", "h", "b"])) {
            (Some(number @ (0..=17 | 20..=28)), _) => Ok(Reg::Gpr(number)),
            (_, Some(number @ 0..=31)) => Ok(Reg::Vector(number)),
            // The link register is restored on the way out, and the rest holds predicates.
            _ if name == "x30" || name == "lr" || name == "ffr" => Ok(Reg::Scratch),
            _ if numbered(&["p", "z"]).is_some() => Ok(Reg::Scratch),
            _ => Err(format!("inline assembly that uses `{name}`")),
        }
    }

    /// The register as an instruction that moves `size` bytes names it.
    fn sized(self, size: u32) -> Result<String, String> {
        match (self, size) {
            (Reg::Gpr(number), 1 | 2 | 4) => Ok(format!("w{number}")),
            (Reg::Gpr(number), 8) => Ok(format!("x{number}")),
            (Reg::Vector(number), 1 | 2 | 4 | 8 | 16) => {
                Ok(format!("{}{number}", ["b", "h", "s", "d", "q"][size.trailing_zeros() as usize]))
            }
            _ => Err(format!("an inline assembly operand of {size} bytes in such a register")),
        }
    }

    /// The instructions that load and store `size` bytes of the register.
    fn transfer(self, size: u32) -> (&'static str, &'static str) {
        match (self, size) {
            (Reg::Gpr(_), 1) => ("ldrb", "strb"),
            (Reg::Gpr(_), 2) => ("ldrh", "strh"),
            _ => ("ldr", "str"),
        }
    }
}

pub(super) fn wrapper(
    template: &[AsmPiece],
    options: AsmOptions,
    operands: &[Operand<'_>],
) -> Result<Wrapper, String> {
    if options.contains(AsmOptions::MAY_UNWIND) {
        return Err("inline assembly that unwinds".to_owned());
    }
    // The registers the operands name come first: a class takes one of what is left.
    let mut regs = vec![None; operands.len()];
    for (index, operand) in operands.iter().enumerate() {
        if let Operand::Reg { reg: InlineAsmRegOrRegClass::Reg(name), .. } = operand {
            regs[index] = Some(Reg::named(name.as_str())?);
        }
    }
    for (index, operand) in operands.iter().enumerate() {
        let Operand::Reg { reg: InlineAsmRegOrRegClass::RegClass(class), .. } = operand else {
            continue;
        };
        let class = class.as_str();
        let free = |reg: &Reg| !regs.contains(&Some(*reg));
        let found = match class {
            "reg" => (0..=17).chain(20..=28).map(Reg::Gpr).find(free),
            // The ones a call does not keep come first.
            "vreg" => (0..8).chain(16..32).chain(8..16).map(Reg::Vector).find(free),
            "vreg_low16" => (0..16).map(Reg::Vector).find(free),
            _ => return Err(format!("the register class `{class}`")),
        };
        let Some(found) = found else {
            return Err(format!("inline assembly with more `{class}` operands than registers"));
        };
        regs[index] = Some(found);
    }

    let mut kept: Vec<Reg> = regs.iter().flatten().copied().filter(|reg| reg.kept()).collect();
    kept.dedup();
    let mut slot_size = kept.len() as u32 * 8;
    let mut offsets = vec![None; operands.len()];
    for (index, operand) in operands.iter().enumerate() {
        let Operand::Reg { size: Some(size), .. } = *operand else { continue };
        if !size.is_power_of_two() {
            return Err(format!("an inline assembly operand of {size} bytes"));
        }
        slot_size = slot_size.next_multiple_of(size.min(16));
        offsets[index] = Some(slot_size);
        slot_size += size;
    }
    let values = || {
        operands.iter().enumerate().filter_map(|(index, operand)| match *operand {
            Operand::Reg { size: Some(size), input, output, .. } => {
                Some((regs[index]?, size, offsets[index]?, input, output))
            }
            _ => None,
        })
    };

    let mut text = format!(
        ".globl {SYMBOL}\n{SYMBOL}:\nstp x29, x30, [sp, #-32]!\nstr x19, [sp, #16]\nmov x19, x0\n"
    );
    for (at, reg) in kept.iter().enumerate() {
        _ = writeln!(text, "str {}, [x19, #{}]", reg.sized(8)?, at * 8);
    }
    for (reg, size, offset, input, _) in values() {
        if input {
            _ = writeln!(text, "{} {}, [x19, #{offset}]", reg.transfer(size).0, reg.sized(size)?);
        }
    }
    for piece in template {
        let (index, modifier) = match piece {
            AsmPiece::Lit(lit) => {
                text.push_str(lit);
                continue;
            }
            AsmPiece::Operand { index, modifier } => (*index, *modifier),
        };
        match (operands.get(index), regs.get(index).copied().flatten(), modifier) {
            (Some(Operand::Const(value)), ..) => _ = write!(text, "{value}"),
            (_, Some(Reg::Gpr(number)), None | Some('x')) => _ = write!(text, "x{number}"),
            (_, Some(Reg::Gpr(number)), Some('w')) => _ = write!(text, "w{number}"),
            (_, Some(Reg::Vector(number)), None) => _ = write!(text, "v{number}"),
            (_, Some(Reg::Vector(number)), Some(part @ ('v' | 'q' | 'd' | 's' | 'h' | 'b'))) => {
                _ = write!(text, "{part}{number}")
            }
            (_, Some(_), Some(other)) => return Err(format!("the operand modifier `{other}`")),
            _ => return Err("an inline assembly template that names no operand".to_owned()),
        }
    }
    text.push('\n');
    if options.contains(AsmOptions::NORETURN) {
        text.push_str("brk #1\n");
    } else {
        for (reg, size, offset, _, output) in values() {
            if output {
                _ = writeln!(
                    text,
                    "{} {}, [x19, #{offset}]",
                    reg.transfer(size).1,
                    reg.sized(size)?
                );
            }
        }
        for (at, reg) in kept.iter().enumerate() {
            _ = writeln!(text, "ldr {}, [x19, #{}]", reg.sized(8)?, at * 8);
        }
        text.push_str("ldr x19, [sp, #16]\nldp x29, x30, [sp], #32\nret\n");
    }
    Ok(Wrapper { text, slot_size: slot_size.next_multiple_of(16).max(16), offsets })
}
