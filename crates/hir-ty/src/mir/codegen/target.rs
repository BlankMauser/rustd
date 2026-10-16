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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFormat {
    Elf,
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
        let (object_format, unwind) = if parts.any(|part| part == "linux") {
            (ObjectFormat::Elf, UnwindFormat::EhFrame)
        } else {
            return Err(format!("rustd emits no code for the system of {triple}"));
        };
        // Cranelift's name for the C convention of the architecture on a System V system:
        // the psABI's on x86-64, AAPCS64 on AArch64.
        let call_conv = CallConv::SystemV;
        Ok(Target { triple: triple.to_owned(), arch, object_format, call_conv, unwind })
    }

    pub fn triple(&self) -> &str {
        &self.triple
    }

    /// The code generator for the target. `regalloc` names Cranelift's register allocator.
    pub(super) fn isa(&self, regalloc: &str) -> Result<OwnedTargetIsa, String> {
        let mut flags = settings::builder();
        flags.set("is_pic", "true").map_err(|e| e.to_string())?;
        flags.set("tls_model", "elf_gd").map_err(|e| e.to_string())?;
        flags.set("opt_level", "none").map_err(|e| e.to_string())?;
        flags.set("enable_llvm_abi_extensions", "true").map_err(|e| e.to_string())?;
        flags.set("regalloc_algorithm", regalloc).map_err(|e| e.to_string())?;
        isa::lookup_by_name(&self.triple)
            .map_err(|e| e.to_string())?
            .finish(settings::Flags::new(flags))
            .map_err(|e| e.to_string())
    }
}
