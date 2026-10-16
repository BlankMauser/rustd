//! What the process compiles for. A daemon builds for one target, named once before any
//! function is compiled, and everything that differs between targets asks here: how wide an
//! address is, what writes a function's code, and what an object file is.

use std::sync::OnceLock;

use cranelift_codegen::ir::{Type, types};

/// A target rustd has a code producer for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// x86-64 ELF, the System V calling convention.
    X86_64Linux,
    /// WebAssembly with 32-bit addresses, as relocatable wasm objects.
    Wasm32(Calls),
}

/// How wasm code calls a function it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Calls {
    /// `call`, as rustc's code does.
    Direct,
    /// `call_indirect` through the slot the linker gives the function in the function
    /// table, so that a module loaded later replaces the function by writing its slot.
    Table,
}

static TARGET: OnceLock<Target> = OnceLock::new();

impl Target {
    /// The target of a triple like `wasm32-unknown-unknown`, when there is a producer for it.
    pub fn from_triple(triple: &str) -> Option<Target> {
        let (arch, rest) = triple.split_once('-')?;
        match arch {
            "x86_64" if rest.split('-').any(|it| it == "linux") => Some(Target::X86_64Linux),
            "wasm32" => Some(Target::Wasm32(Calls::Direct)),
            _ => None,
        }
    }

    /// Names the target of the process. Fails when another one was named or used already.
    pub fn select(target: Target) -> Result<(), Target> {
        match TARGET.set(target) {
            Ok(()) => Ok(()),
            Err(_) if Target::current() == target => Ok(()),
            Err(_) => Err(Target::current()),
        }
    }

    pub fn current() -> Target {
        *TARGET.get_or_init(|| Target::X86_64Linux)
    }

    pub fn is_wasm(self) -> bool {
        matches!(self, Target::Wasm32(_))
    }

    /// The integer an address is.
    pub(super) fn pointer(self) -> Type {
        match self {
            Target::X86_64Linux => types::I64,
            Target::Wasm32(_) => types::I32,
        }
    }

    /// How many bytes an address takes.
    pub(crate) fn word(self) -> usize {
        self.pointer().bytes() as usize
    }
}
