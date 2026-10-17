//! What rustd emits code for. The target is an input of the process, named by its triple
//! (`rustd --target <triple>`), and the host when nothing names one. Everything in the
//! backend that differs between platforms asks the [`Target`] for the property it depends
//! on (architecture, object format, calling convention, unwind tables) and never asks what
//! the daemon itself was built for.

use std::sync::OnceLock;

use cranelift_codegen::{
    isa::{self, CallConv, OwnedTargetIsa},
    settings::{self, Configurable},
};

static SELECTED: OnceLock<Target> = OnceLock::new();

#[derive(Debug)]
pub struct Target {
    triple: String,
    pub arch: Arch,
    pub object_format: ObjectFormat,
    /// The convention of `extern "C"`, which the Rust conventions are laid over.
    pub call_conv: CallConv,
    pub unwind: UnwindFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

impl Arch {
    /// How long a function is at least, and the byte one that is shorter is filled up with:
    /// the length of a jump from its entry to anywhere a replacement of it may be. Nothing
    /// when every function is that long as it is.
    pub fn shortest_function(self) -> Option<(usize, u8)> {
        match self {
            // `jmp rel32`, filled up with `int3`.
            Arch::X86_64 => Some((5, 0xcc)),
            // An instruction is as long as `b`.
            Arch::Aarch64 => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFormat {
    Elf,
    MachO,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnwindFormat {
    /// DWARF call frame information in `.eh_frame`, landing pads in `.gcc_except_table`.
    EhFrame,
}

impl Target {
    /// Names the target of this process. It is said once, before anything is compiled.
    pub fn select(triple: &str) -> Result<&'static Target, String> {
        let target = Target::parse(triple)?;
        let selected = SELECTED.get_or_init(|| target);
        if selected.triple == triple {
            Ok(selected)
        } else {
            Err(format!("the target is {} already, not {triple}", selected.triple))
        }
    }

    /// The target that was named, or the host.
    pub fn selected() -> Result<&'static Target, String> {
        match SELECTED.get() {
            Some(target) => Ok(target),
            None => Target::select(Target::host()),
        }
    }

    /// The triple of what the daemon itself runs on.
    pub fn host() -> &'static str {
        // The wasm backend writes its own code from Cranelift's IR and only borrows the
        // settings of an ISA Cranelift has, whatever the compiler itself runs on.
        if cfg!(feature = "wasm") {
            return "x86_64-unknown-linux-gnu";
        }
        // FIXME: the triples of the hosts that have no code emitted for them yet are missing,
        // and `parse` turns those down by name.
        match (std::env::consts::ARCH, std::env::consts::OS) {
            ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
            ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
            ("aarch64", "macos") => "aarch64-apple-darwin",
            ("x86_64", "macos") => "x86_64-apple-darwin",
            ("x86_64", "windows") => "x86_64-pc-windows-msvc",
            ("aarch64", "windows") => "aarch64-pc-windows-msvc",
            _ => "unknown",
        }
    }

    fn parse(triple: &str) -> Result<Target, String> {
        let mut parts = triple.split('-');
        let arch = match parts.next() {
            Some("x86_64") => Arch::X86_64,
            Some("aarch64") => Arch::Aarch64,
            _ => return Err(format!("rustd emits no code for the architecture of {triple}")),
        };
        let system = parts.find(|part| matches!(*part, "linux" | "darwin"));
        // Cranelift's names for the C convention: `SystemV` is the psABI's on x86-64 and
        // AAPCS64 on AArch64, which Apple's differs from in what goes on the stack.
        let (object_format, unwind, call_conv) = match (system, arch) {
            (Some("linux"), _) => (ObjectFormat::Elf, UnwindFormat::EhFrame, CallConv::SystemV),
            (Some("darwin"), Arch::Aarch64) => {
                (ObjectFormat::MachO, UnwindFormat::EhFrame, CallConv::AppleAarch64)
            }
            // FIXME: x86-64 Mach-O has no splice and no thread locals written for it.
            _ => return Err(format!("rustd emits no code for the system of {triple}")),
        };
        Ok(Target { triple: triple.to_owned(), arch, object_format, call_conv, unwind })
    }

    pub fn triple(&self) -> &str {
        &self.triple
    }

    /// The processor code is made for: the baseline of the target, as rustc compiles for.
    pub fn cpu(&self) -> &'static str {
        match (self.arch, self.object_format) {
            (Arch::X86_64, _) => "x86-64",
            (Arch::Aarch64, ObjectFormat::MachO) => "apple-m1",
            (Arch::Aarch64, ObjectFormat::Elf) => "generic",
        }
    }

    /// The code generator for the target. `regalloc` names Cranelift's register allocator.
    pub(super) fn isa(&self, regalloc: &str) -> Result<OwnedTargetIsa, String> {
        let mut flags = settings::builder();
        flags.set("is_pic", "true").map_err(|e| e.to_string())?;
        let tls_model = match self.object_format {
            ObjectFormat::Elf => "elf_gd",
            ObjectFormat::MachO => "macho",
        };
        flags.set("tls_model", tls_model).map_err(|e| e.to_string())?;
        flags.set("opt_level", "none").map_err(|e| e.to_string())?;
        flags.set("enable_llvm_abi_extensions", "true").map_err(|e| e.to_string())?;
        flags.set("regalloc_algorithm", regalloc).map_err(|e| e.to_string())?;
        isa::lookup_by_name(&self.triple)
            .map_err(|e| e.to_string())?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())
    }
}
