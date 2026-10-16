//! The object file LLVM wrote for one function, read back as a [`Code`].
//!
//! The code is the function's section. Constants LLVM put in sections of their own (jump
//! tables, float literals) are laid out after it in the same bytes, and what the code says
//! about where they are is filled in here, so that the rest of rustd sees one piece of code
//! that refers to nothing but what the function asked for.

use cranelift_codegen::{
    binemit::Reloc,
    gimli::{self, BaseAddresses, CieOrFde, EhFrame, LittleEndian, UnwindSection},
    ir::{LibCall, Signature},
    isa::CallConv,
};
use cranelift_module::{FuncId, ModuleReloc, ModuleRelocTarget, default_libcall_names};
use object::{
    Object, ObjectSection, ObjectSymbol, RelocationFlags, RelocationTarget, SectionIndex,
    SectionKind, SymbolSection, elf,
};
use rustc_hash::FxHashMap;

use super::{OWN, Symbol, Unhandled};
use crate::mir::codegen::{
    fragment::{Code, Recorder},
    unwind::{FrameRows, FrameRule, FunctionUnwind},
};

pub(super) struct Emitted<'data> {
    file: object::File<'data>,
    /// Where each section that is part of the code starts in `bytes`.
    placed: FxHashMap<SectionIndex, u64>,
    order: Vec<SectionIndex>,
    bytes: Vec<u8>,
    align: u64,
    relocs: Vec<ModuleReloc>,
}

impl<'data> Emitted<'data> {
    pub(super) fn code(
        object: &'data [u8],
        signature: Signature,
        recorder: &mut Recorder<'_>,
    ) -> Result<Code, Unhandled> {
        let file = object::File::parse(object)?;
        let own = file
            .symbols()
            .find(|symbol| symbol.name() == Ok(OWN) && symbol.is_definition())
            .ok_or(Unhandled::Shape("an object without the function"))?;
        let SymbolSection::Section(text) = own.section() else {
            return Err(Unhandled::Shape("an object without the function"));
        };
        if own.address() != 0 {
            return Err(Unhandled::Shape("code ahead of the function"));
        }
        let mut emitted = Emitted {
            file,
            placed: FxHashMap::default(),
            order: Vec::new(),
            bytes: Vec::new(),
            align: 1,
            relocs: Vec::new(),
        };
        emitted.place(text)?;
        emitted.relocate(recorder)?;
        let unwind = emitted.unwind()?;
        let Emitted { bytes, align, relocs, .. } = emitted;
        Ok(Code { signature, bytes, align, relocs, unwind })
    }

    /// Lays out `first` and every section its relocations lead to.
    fn place(&mut self, first: SectionIndex) -> Result<(), Unhandled> {
        let mut pending = vec![first];
        while let Some(index) = pending.pop() {
            if self.placed.contains_key(&index) {
                continue;
            }
            let section = self.file.section_by_index(index)?;
            match section.kind() {
                SectionKind::Text
                | SectionKind::ReadOnlyData
                | SectionKind::ReadOnlyString
                | SectionKind::ReadOnlyDataWithRel => {}
                _ => return Err(Unhandled::Shape("code that refers to a writable section")),
            }
            let align = section.align().max(1);
            self.align = self.align.max(align);
            let start = (self.bytes.len() as u64).next_multiple_of(align);
            self.bytes.resize(start as usize, 0);
            self.bytes.extend_from_slice(section.data()?);
            self.placed.insert(index, start);
            self.order.push(index);
            for (_, relocation) in section.relocations() {
                if let SymbolSection::Section(target) = self.symbol(&relocation)?.section() {
                    pending.push(target);
                }
            }
        }
        Ok(())
    }

    fn symbol(
        &self,
        relocation: &object::Relocation,
    ) -> Result<object::Symbol<'data, '_>, Unhandled> {
        let RelocationTarget::Symbol(symbol) = relocation.target() else {
            return Err(Unhandled::Shape("a relocation against no symbol"));
        };
        Ok(self.file.symbol_by_index(symbol)?)
    }

    /// Fills in what points from one placed section into another, and turns the rest into
    /// relocations against requests.
    fn relocate(&mut self, recorder: &mut Recorder<'_>) -> Result<(), Unhandled> {
        for &index in &self.order {
            let section = self.file.section_by_index(index)?;
            let base = self.placed[&index];
            for (offset, relocation) in section.relocations() {
                let RelocationFlags::Elf { r_type } = relocation.flags() else {
                    return Err(Unhandled::Shape("an object that is not ELF"));
                };
                let at = base + offset;
                let symbol = self.symbol(&relocation)?;
                if let SymbolSection::Section(target) = symbol.section() {
                    // Both ends are in `bytes`, whose pieces do not move against each other.
                    let (elf::R_X86_64_PC32 | elf::R_X86_64_PLT32) = r_type else {
                        return Err(Unhandled::Shape("constants that hold an address"));
                    };
                    let target = self.placed[&target] + symbol.address();
                    let value = target.wrapping_add_signed(relocation.addend()).wrapping_sub(at);
                    let field = self
                        .bytes
                        .get_mut(at as usize..at as usize + 4)
                        .ok_or(Unhandled::Shape("a relocation outside its section"))?;
                    field.copy_from_slice(&(value as u32).to_le_bytes());
                    continue;
                }
                let kind = match r_type {
                    elf::R_X86_64_PLT32 => Reloc::X86CallPLTRel4,
                    elf::R_X86_64_PC32 => Reloc::X86PCRel4,
                    // The relaxable forms are loads from the GOT like any other.
                    elf::R_X86_64_GOTPCREL
                    | elf::R_X86_64_GOTPCRELX
                    | elf::R_X86_64_REX_GOTPCRELX => Reloc::X86GOTPCRel4,
                    elf::R_X86_64_TLSGD => Reloc::ElfX86_64TlsGd,
                    other => return Err(Unhandled::Relocation(other)),
                };
                let name = Self::target(symbol.name()?, recorder)?;
                self.relocs.push(ModuleReloc {
                    offset: at as u32,
                    kind,
                    name,
                    addend: relocation.addend(),
                });
            }
        }
        Ok(())
    }

    /// What an undefined symbol of the object stands for.
    fn target(name: &str, recorder: &mut Recorder<'_>) -> Result<ModuleRelocTarget, Unhandled> {
        if let Some(symbol) = Symbol::parse(name) {
            return Ok(match symbol {
                Symbol::Func(index) => ModuleRelocTarget::user(0, index),
                Symbol::Data(index) => ModuleRelocTarget::user(1, index),
            });
        }
        let names = default_libcall_names();
        if let Some(libcall) = LibCall::all_libcalls().iter().find(|&&it| names(it) == name) {
            return Ok(ModuleRelocTarget::LibCall(*libcall));
        }
        // A function LLVM calls of its own accord that Cranelift has no name for: a libm
        // rounding function, wide division. Nothing is checked against the signature of an
        // import.
        let id: FuncId = recorder
            .import(name, &Signature::new(CallConv::SystemV))
            .map_err(|_| Unhandled::Shape("a library call that cannot be imported"))?;
        Ok(ModuleRelocTarget::user(0, id.as_u32()))
    }

    /// The function's entry of `.eh_frame` as rules, and its entry of `.gcc_except_table`
    /// as it is: the offsets in it are from the start of the function.
    fn unwind(&self) -> Result<Option<FunctionUnwind>, Unhandled> {
        let Some(section) = self.file.section_by_name(".eh_frame") else { return Ok(None) };
        let eh_frame = EhFrame::new(section.data()?, LittleEndian);
        let bases = BaseAddresses::default().set_eh_frame(0).set_text(0);
        let malformed = |_| Unhandled::Shape("an unwind table that does not parse");
        let mut entries = eh_frame.entries(&bases);
        let mut rows = None;
        while let Some(entry) = entries.next().map_err(malformed)? {
            let CieOrFde::Fde(partial) = entry else { continue };
            let fde = partial
                .parse(|_, bases, offset| eh_frame.cie_from_offset(bases, offset))
                .map_err(malformed)?;
            if rows.is_some() {
                return Err(Unhandled::Shape("more than one unwind table entry"));
            }
            let cie = fde.cie();
            let entry_rules = Self::rules(cie.instructions(&eh_frame, &bases), cie)?;
            if entry_rules != FrameRule::ON_ENTRY.map(|rule| (0, rule)) {
                return Err(Unhandled::Shape("an unwind table that starts from other rules"));
            }
            rows = Some(FrameRows {
                len: fde.len() as u32,
                rows: Self::rules(fde.instructions(&eh_frame, &bases), cie)?,
            });
        }
        let Some(rows) = rows else { return Ok(None) };
        let landing_pads = match self.file.section_by_name(".gcc_except_table") {
            Some(table) if table.relocations().next().is_some() => {
                return Err(Unhandled::Shape("landing pads that name a type"));
            }
            Some(table) => Some(table.data()?.to_vec()),
            None => None,
        };
        Ok(Some(FunctionUnwind::from_rows(rows, landing_pads)))
    }

    fn rules<R: gimli::Reader>(
        mut instructions: gimli::CallFrameInstructionIter<'_, R>,
        cie: &gimli::CommonInformationEntry<R>,
    ) -> Result<Vec<(u32, FrameRule)>, Unhandled> {
        use gimli::CallFrameInstruction as Cfi;
        let scale = cie.data_alignment_factor();
        let factored =
            |offset: i64| offset.checked_mul(scale).and_then(|it| i32::try_from(it).ok());
        let unknown = Unhandled::Shape("an unwind rule there is no answer for");
        let mut at = 0u32;
        let mut rules = Vec::new();
        while let Some(instruction) = instructions.next().map_err(|_| unknown_rule())? {
            let rule = match instruction {
                Cfi::AdvanceLoc { delta } => {
                    at += delta * cie.code_alignment_factor() as u32;
                    continue;
                }
                Cfi::Nop => continue,
                Cfi::DefCfa { register, offset } => FrameRule::Cfa(register.0, offset as i32),
                Cfi::DefCfaSf { register, factored_offset } => {
                    FrameRule::Cfa(register.0, factored(factored_offset).ok_or(unknown_rule())?)
                }
                Cfi::DefCfaRegister { register } => FrameRule::CfaRegister(register.0),
                Cfi::DefCfaOffset { offset } => FrameRule::CfaOffset(offset as i32),
                Cfi::DefCfaOffsetSf { factored_offset } => {
                    FrameRule::CfaOffset(factored(factored_offset).ok_or(unknown_rule())?)
                }
                Cfi::Offset { register, factored_offset } => FrameRule::Offset(
                    register.0,
                    factored(factored_offset as i64).ok_or(unknown_rule())?,
                ),
                Cfi::OffsetExtendedSf { register, factored_offset } => {
                    FrameRule::Offset(register.0, factored(factored_offset).ok_or(unknown_rule())?)
                }
                Cfi::Restore { register } => FrameRule::Restore(register.0),
                Cfi::SameValue { register } => FrameRule::SameValue(register.0),
                Cfi::Undefined { register } => FrameRule::Undefined(register.0),
                Cfi::RememberState => FrameRule::RememberState,
                Cfi::RestoreState => FrameRule::RestoreState,
                Cfi::ArgsSize { size } => FrameRule::ArgsSize(size as u32),
                _ => return Err(unknown),
            };
            rules.push((at, rule));
        }
        Ok(rules)
    }
}

fn unknown_rule() -> Unhandled {
    Unhandled::Shape("an unwind rule there is no answer for")
}

impl From<object::Error> for Unhandled {
    fn from(e: object::Error) -> Self {
        Unhandled::Llvm(format!("object: {e}"))
    }
}
