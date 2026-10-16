//! Answering an edit without linking again: the executable of the last full link stays as it
//! is, and the functions that changed are added to its end.
//!
//! A full link of dx writes half a gigabyte to replace a few kilobytes of code. The splice
//! writes those kilobytes: it lays the patch object out behind everything the linker loaded,
//! resolves what it refers to against the executable's symbols, and writes a jump to the new
//! code over the entry of each function the patch replaces. Whatever referred to the old
//! function, a call, a vtable or a function pointer, still arrives there and goes on to the
//! new one.
//!
//! What is appended, in three segments of its own:
//!
//! 1. Read-only: the program headers with the new segments among them, the search table of
//!    the unwinder with the patch's frames added, the patch's constants and unwind tables,
//!    and the load-time relocations with the patch's added.
//! 2. Code: the patch's functions, and a stub for each import they call directly.
//! 3. Data: the patch's writable sections, and an address slot for each symbol the code
//!    reaches through one.
//!
//! The patch holds everything that differs from the object the executable was linked from, so
//! each splice starts from the executable as the linker left it: the last one is undone
//! first. Anything the splice has no answer for is an error, and the caller links in full.

use std::{
    fs::{File, OpenOptions},
    mem::offset_of,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    thread::JoinHandle,
    time::SystemTime,
};

use anyhow::{Context, bail, ensure};
use gimli::UnwindSection;
use object::{
    Endianness, Object, ObjectSection, ObjectSymbol, ObjectSymbolTable, RelocationFlags,
    RelocationTarget, SectionIndex, SymbolIndex, SymbolKind, elf,
    read::elf::{ElfFile64, FileHeader, ProgramHeader},
};
use rustc_hash::{FxHashMap, FxHashSet};

/// The executable of the last full link, which is read on a thread of its own while the
/// daemon waits for the next edit.
pub(crate) struct Linked(JoinHandle<anyhow::Result<Baseline>>);

impl Linked {
    /// Starts reading the executable the linker just wrote at `binary` from `patch` and the
    /// rest of the link line. The patch is read here and now: the next edit writes another.
    pub(crate) fn read(binary: PathBuf, patch: &Path) -> Linked {
        let patch = std::fs::read(patch);
        Linked(std::thread::spawn(move || Baseline::read(binary, &patch?)))
    }

    pub(crate) fn baseline(self) -> anyhow::Result<Baseline> {
        self.0.join().map_err(|_| anyhow::anyhow!("reading the executable panicked"))?
    }
}

/// An executable as the linker wrote it, and what the last splice changed of it.
pub(crate) struct Baseline {
    path: PathBuf,
    image: Image,
    /// The functions of the patch the executable was linked with. A patch without one of
    /// them means the function is as it first was again, which the executable no longer has.
    baked: Vec<Box<[u8]>>,
    /// What the last splice wrote over, to put back before the next.
    undo: Vec<(u64, Vec<u8>)>,
    /// What the file was when we last left it: someone else may have written it since.
    stamp: Stamp,
}

/// What a splice did, for the log.
pub(crate) struct Spliced {
    pub(crate) redirected: usize,
    pub(crate) appended: usize,
}

impl Baseline {
    fn read(path: PathBuf, patch: &[u8]) -> anyhow::Result<Baseline> {
        let file = File::open(&path)?;
        let stamp = Stamp::of(&file)?;
        // SAFETY: the daemon is the one that has the link write the file, and it does not
        // start another while this reads.
        let bytes = unsafe { memmap2::Mmap::map(&file)? };
        let image = Image::read(&bytes)?;
        let patch = ElfFile64::<Endianness>::parse(patch)?;
        let baked = patch
            .symbols()
            .filter(|it| it.is_definition() && !it.is_local())
            .filter_map(|it| it.name_bytes().ok().map(Box::from))
            .collect();
        Ok(Baseline { path, image, baked, undo: Vec::new(), stamp })
    }

    pub(crate) fn is_of(&self, binary: &Path) -> bool {
        self.path == binary
    }

    /// Brings the executable from what the linker wrote to that with `patch` in place of the
    /// functions it defines. On an error the file may be neither: it has to be linked again.
    pub(crate) fn splice(&mut self, patch: &[u8]) -> anyhow::Result<Spliced> {
        let patch = Patch::read(patch, &self.image)?;
        let defined: FxHashSet<&[u8]> = patch.replaced.iter().map(|it| it.name).collect();
        ensure!(
            self.baked.iter().all(|it| defined.contains(&**it)),
            "a function the executable was linked with a patch of is as it first was"
        );
        let addition = patch.place(&self.image)?;

        let file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        ensure!(Stamp::of(&file)? == self.stamp, "the executable was written by someone else");
        for (offset, bytes) in self.undo.drain(..).rev() {
            file.write_all_at(&bytes, offset)?;
        }
        file.set_len(self.image.len)?;
        file.write_all_at(&addition.appended, addition.offset)?;
        for (offset, bytes) in &addition.overwritten {
            let mut old = vec![0; bytes.len()];
            file.read_exact_at(&mut old, *offset)?;
            self.undo.push((*offset, old));
            file.write_all_at(bytes, *offset)?;
        }
        self.stamp = Stamp::of(&file)?;
        Ok(Spliced { redirected: addition.redirected, appended: addition.appended.len() })
    }
}

#[derive(PartialEq)]
struct Stamp {
    len: u64,
    modified: SystemTime,
}

impl Stamp {
    fn of(file: &File) -> anyhow::Result<Stamp> {
        let meta = file.metadata()?;
        Ok(Stamp { len: meta.len(), modified: meta.modified()? })
    }
}

/// What a splice has to know of the executable. Nothing of the file is read again.
struct Image {
    len: u64,
    header: elf::FileHeader64<Endianness>,
    segments: Vec<elf::ProgramHeader64<Endianness>>,
    /// Addresses are relative to where the program is loaded, and the loader adds that to
    /// every address stored in data.
    pie: bool,
    symbols: FxHashMap<Box<[u8]>, Named>,
    /// The address slots the loader fills with what other libraries define.
    imports: FxHashMap<Box<[u8]>, u64>,
    unwind: UnwindTable,
    /// `None` when the loader has no addresses to fix up.
    relocations: Option<LoadRelocations>,
}

/// What a name stands for in the executable.
enum Named {
    One(Defined),
    /// Symbols private to their objects that share the name: which one is meant is lost.
    Many,
}

#[derive(Clone, Copy)]
struct Defined {
    address: u64,
    size: u64,
    kind: SymbolKind,
    /// Where the file has its entry of the symbol table.
    entry: u64,
}

impl Defined {
    fn is(&self, other: &Defined) -> bool {
        (self.address, self.size, self.kind) == (other.address, other.size, other.kind)
    }
}

/// `.eh_frame_hdr`: the frames of the program sorted by the code they describe, which the
/// unwinder finds through a program header.
struct UnwindTable {
    segment: usize,
    address: u64,
    /// Where `.eh_frame` starts.
    frames: u64,
    /// Pairs of the start of a function and its frame, both relative to `address`.
    entries: Vec<u8>,
}

/// `.rela.dyn`, and where `.dynamic` says where it is and how long.
struct LoadRelocations {
    table: Vec<u8>,
    address_at: u64,
    size_at: u64,
}

impl Image {
    fn read(bytes: &[u8]) -> anyhow::Result<Image> {
        let file = ElfFile64::<Endianness>::parse(bytes)?;
        let endian = file.endian();
        let header = *file.elf_header();
        ensure!(
            header.e_machine(endian) == elf::EM_X86_64 && endian == Endianness::Little,
            "not an x86-64 executable"
        );
        let pie = match header.e_type(endian) {
            elf::ET_DYN => true,
            elf::ET_EXEC => false,
            other => bail!("not an executable (type {other})"),
        };
        let segments = file.elf_program_headers().to_vec();

        let mut symbols = FxHashMap::<Box<[u8]>, Named>::default();
        let (table, _) = (file.section_by_name(".symtab").and_then(|it| it.file_range()))
            .context("the executable has no symbol table")?;
        for symbol in file.symbols() {
            if !symbol.is_definition() || symbol.section_index().is_none() {
                continue;
            }
            let name = symbol.name_bytes()?;
            let defined = Defined {
                address: symbol.address(),
                size: symbol.size(),
                kind: symbol.kind(),
                entry: table + (symbol.index().0 * size_of::<elf::Sym64<Endianness>>()) as u64,
            };
            match symbols.get_mut(name) {
                None => _ = symbols.insert(name.into(), Named::One(defined)),
                Some(Named::One(known)) if known.is(&defined) => {}
                Some(known) => *known = Named::Many,
            }
        }

        let mut imports = FxHashMap::default();
        if let (Some(relocations), Some(dynamic)) =
            (file.dynamic_relocations(), file.dynamic_symbol_table())
        {
            for (slot, relocation) in relocations {
                let RelocationFlags::Elf { r_type } = relocation.flags() else { continue };
                let RelocationTarget::Symbol(symbol) = relocation.target() else { continue };
                if matches!(r_type, elf::R_X86_64_GLOB_DAT | elf::R_X86_64_JUMP_SLOT) {
                    let name = dynamic.symbol_by_index(symbol)?.name_bytes()?;
                    imports.entry(name.into()).or_insert(slot);
                }
            }
        }

        let mut image = Image {
            len: bytes.len() as u64,
            header,
            segments,
            pie,
            symbols,
            imports,
            unwind: UnwindTable { segment: 0, address: 0, frames: 0, entries: Vec::new() },
            relocations: None,
        };
        image.unwind = UnwindTable::read(&image, bytes)?;
        image.relocations = LoadRelocations::read(&image, bytes)?;
        Ok(image)
    }

    fn segments_of(&self, kind: u32) -> impl Iterator<Item = &elf::ProgramHeader64<Endianness>> {
        self.segments.iter().filter(move |it| it.p_type(Endianness::Little) == kind)
    }

    /// Where in the file the linker put what is loaded at `address`.
    fn offset_of(&self, address: u64) -> anyhow::Result<u64> {
        let e = Endianness::Little;
        self.segments_of(elf::PT_LOAD)
            .find(|it| (it.p_vaddr(e)..it.p_vaddr(e) + it.p_filesz(e)).contains(&address))
            .map(|it| it.p_offset(e) + (address - it.p_vaddr(e)))
            .with_context(|| format!("nothing of the file is loaded at {address:#x}"))
    }

    fn bytes_at<'a>(&self, bytes: &'a [u8], address: u64, len: u64) -> anyhow::Result<&'a [u8]> {
        let start = self.offset_of(address)? as usize;
        bytes.get(start..start + len as usize).context("the file ends early")
    }

    /// The first address past everything the program loads.
    fn end(&self) -> u64 {
        let e = Endianness::Little;
        self.segments_of(elf::PT_LOAD).map(|it| it.p_vaddr(e) + it.p_memsz(e)).max().unwrap_or(0)
    }

    fn page(&self) -> u64 {
        let e = Endianness::Little;
        self.segments_of(elf::PT_LOAD).map(|it| it.p_align(e)).max().unwrap_or(1).max(1)
    }
}

impl UnwindTable {
    /// The encodings every linker writes the table in: a 4-byte count, and pairs of 4-byte
    /// offsets from the table's start.
    const ENCODINGS: [u8; 4] = [
        1,
        gimli::DW_EH_PE_pcrel.0 | gimli::DW_EH_PE_sdata4.0,
        gimli::DW_EH_PE_udata4.0,
        gimli::DW_EH_PE_datarel.0 | gimli::DW_EH_PE_sdata4.0,
    ];
    const HEADER: usize = 12;
    const ENTRY: usize = 8;

    fn read(image: &Image, bytes: &[u8]) -> anyhow::Result<UnwindTable> {
        let e = Endianness::Little;
        let (segment, header) = image
            .segments
            .iter()
            .enumerate()
            .find(|it| it.1.p_type(e) == elf::PT_GNU_EH_FRAME)
            .context("the executable has no unwind table")?;
        let address = header.p_vaddr(e);
        let table = image.bytes_at(bytes, address, header.p_filesz(e))?;
        ensure!(
            table.len() >= Self::HEADER && table[..4] == Self::ENCODINGS,
            "an unwind table in encodings we don't write"
        );
        let frames = (address + 4).wrapping_add_signed(i64::from(read_i32(&table[4..])));
        let count = u32::from_le_bytes(table[8..12].try_into()?) as usize;
        let entries = table
            .get(Self::HEADER..Self::HEADER + count * Self::ENTRY)
            .context("the unwind table ends early")?
            .to_vec();
        Ok(UnwindTable { segment, address, frames, entries })
    }

    fn len_with(&self, added: usize) -> usize {
        Self::HEADER + self.entries.len() + added * Self::ENTRY
    }

    /// The table as it is at `address`, with `added` frames given by the address of their
    /// code and their own. All of them are of code behind the program's.
    fn moved(&self, address: u64, mut added: Vec<(u64, u64)>) -> anyhow::Result<Vec<u8>> {
        added.sort_unstable();
        let mut table = Vec::with_capacity(self.len_with(added.len()));
        table.extend_from_slice(&Self::ENCODINGS);
        table.extend_from_slice(&relative(self.frames, address + 4)?.to_le_bytes());
        let count = self.entries.len() / Self::ENTRY + added.len();
        table.extend_from_slice(&u32::try_from(count)?.to_le_bytes());
        let shift = relative(self.address, address)?;
        for &field in self.entries.as_chunks().0 {
            let field =
                i32::from_le_bytes(field).checked_add(shift).context("the program is too large")?;
            table.extend_from_slice(&field.to_le_bytes());
        }
        for (code, frame) in added {
            table.extend_from_slice(&relative(code, address)?.to_le_bytes());
            table.extend_from_slice(&relative(frame, address)?.to_le_bytes());
        }
        Ok(table)
    }
}

impl LoadRelocations {
    const ENTRY: usize = size_of::<elf::Rela64<Endianness>>();

    fn read(image: &Image, bytes: &[u8]) -> anyhow::Result<Option<LoadRelocations>> {
        let e = Endianness::Little;
        let Some(dynamic) = image.segments_of(elf::PT_DYNAMIC).next() else { return Ok(None) };
        let start = dynamic.p_offset(e);
        let entries = bytes
            .get(start as usize..(start + dynamic.p_filesz(e)) as usize)
            .context("the file ends early")?;
        let (entries, _) = object::slice_from_bytes::<elf::Dyn64<Endianness>>(
            entries,
            entries.len() / size_of::<elf::Dyn64<Endianness>>(),
        )
        .map_err(|()| anyhow::anyhow!("a misaligned dynamic section"))?;
        let entry = |tag: u32| {
            let at = entries.iter().position(|it| it.d_tag.get(e) == u64::from(tag))?;
            let value_at = start + (at * size_of::<elf::Dyn64<Endianness>>()) as u64 + 8;
            Some((entries[at].d_val.get(e), value_at))
        };
        let (Some((address, address_at)), Some((size, size_at))) =
            (entry(elf::DT_RELA), entry(elf::DT_RELASZ))
        else {
            return Ok(None);
        };
        let table = image.bytes_at(bytes, address, size)?.to_vec();
        Ok(Some(LoadRelocations { table, address_at, size_at }))
    }

    fn len_with(&self, added: usize) -> usize {
        self.table.len() + added * Self::ENTRY
    }
}

/// A patch object, and what of the executable it needs.
struct Patch<'a> {
    file: ElfFile64<'a, Endianness>,
    /// The sections that are loaded, by the segment they go to.
    sections: [Vec<SectionIndex>; 3],
    /// What each symbol a relocation refers to stands for.
    targets: FxHashMap<SymbolIndex, Target>,
    /// The symbols the code reaches through an address slot of ours, in the order of the
    /// slots.
    slots: Vec<SymbolIndex>,
    /// The imports the code calls directly, in the order of their stubs.
    stubs: Vec<SymbolIndex>,
    /// How many addresses the data stores, each of which the loader has to fix up.
    stored: usize,
    frames: usize,
    /// The functions of the executable it has another body for.
    replaced: Vec<Replaced<'a>>,
}

#[derive(Clone, Copy)]
enum Segment {
    Read = 0,
    Code = 1,
    Data = 2,
}

impl Segment {
    const ALL: [Segment; 3] = [Segment::Read, Segment::Code, Segment::Data];

    fn flags(self) -> u32 {
        match self {
            Segment::Read => elf::PF_R,
            Segment::Code => elf::PF_R | elf::PF_X,
            Segment::Data => elf::PF_R | elf::PF_W,
        }
    }
}

#[derive(Clone, Copy)]
enum Target {
    /// At an offset in a section of the patch.
    Ours(SectionIndex, u64),
    /// At an address of the executable.
    Theirs(u64),
    /// In another library: the loader writes its address to the slot.
    Import(u64),
}

struct Replaced<'a> {
    name: &'a [u8],
    section: SectionIndex,
    offset: u64,
    size: u64,
    /// `None` for a function the executable does not have.
    old: Option<Defined>,
}

/// How a relocation of the patch gets its value.
enum Fixup {
    /// The address itself, in 8 bytes.
    Absolute,
    /// The distance to it, in 4 bytes.
    Relative,
    /// The distance to a slot that holds the address, in 4 bytes.
    ThroughSlot,
}

impl Fixup {
    fn of(relocation: &object::Relocation) -> anyhow::Result<Fixup> {
        let RelocationFlags::Elf { r_type } = relocation.flags() else {
            bail!("a relocation that is not ELF's")
        };
        Ok(match r_type {
            elf::R_X86_64_64 => Fixup::Absolute,
            elf::R_X86_64_PC32 | elf::R_X86_64_PLT32 => Fixup::Relative,
            elf::R_X86_64_GOTPCREL | elf::R_X86_64_GOTPCRELX | elf::R_X86_64_REX_GOTPCRELX => {
                Fixup::ThroughSlot
            }
            other => bail!("a relocation of type {other}, which only the linker handles"),
        })
    }
}

impl<'a> Patch<'a> {
    const JUMP: usize = 5;
    /// `jmp *slot(%rip)`.
    const STUB: usize = 6;
    const SLOT: usize = 8;

    fn read(bytes: &'a [u8], image: &Image) -> anyhow::Result<Patch<'a>> {
        let file = ElfFile64::<Endianness>::parse(bytes)?;
        let mut patch = Patch {
            file,
            sections: Default::default(),
            targets: FxHashMap::default(),
            slots: Vec::new(),
            stubs: Vec::new(),
            stored: 0,
            frames: 0,
            replaced: Vec::new(),
        };
        for section in patch.file.sections() {
            let object::SectionFlags::Elf { sh_flags } = section.flags() else { continue };
            if sh_flags & u64::from(elf::SHF_ALLOC) == 0 {
                continue;
            }
            ensure!(sh_flags & u64::from(elf::SHF_TLS) == 0, "the patch has thread-local data");
            let segment = if sh_flags & u64::from(elf::SHF_EXECINSTR) != 0 {
                Segment::Code
            } else if sh_flags & u64::from(elf::SHF_WRITE) != 0 {
                Segment::Data
            } else {
                Segment::Read
            };
            patch.sections[segment as usize].push(section.index());
            if section.name_bytes()? == b".eh_frame" {
                patch.frames += Self::frames_in(section.data()?, 0)?.len();
            }
        }
        let loaded: FxHashSet<SectionIndex> = patch.sections.iter().flatten().copied().collect();
        let code: FxHashSet<SectionIndex> =
            patch.sections[Segment::Code as usize].iter().copied().collect();

        let (mut slots, mut stubs) = (FxHashSet::default(), FxHashSet::default());
        for section in patch.file.sections().filter(|it| loaded.contains(&it.index())) {
            for (_, relocation) in section.relocations() {
                let RelocationTarget::Symbol(index) = relocation.target() else {
                    bail!("a relocation against no symbol")
                };
                let target = match patch.targets.get(&index) {
                    Some(target) => *target,
                    None => {
                        let target = patch.target(index, image, &loaded)?;
                        patch.targets.insert(index, target);
                        target
                    }
                };
                match (Fixup::of(&relocation)?, target) {
                    (Fixup::Absolute, Target::Import(_)) => {
                        bail!("data that stores the address of an import")
                    }
                    (Fixup::Absolute, _) => patch.stored += 1,
                    (Fixup::Relative, Target::Import(_)) => {
                        if stubs.insert(index) {
                            patch.stubs.push(index);
                        }
                    }
                    (Fixup::ThroughSlot, Target::Ours(..) | Target::Theirs(_)) => {
                        if slots.insert(index) {
                            patch.slots.push(index);
                        }
                    }
                    (Fixup::Relative, _) | (Fixup::ThroughSlot, Target::Import(_)) => {}
                }
            }
        }

        for symbol in patch.file.symbols() {
            if !symbol.is_definition() || symbol.is_local() {
                continue;
            }
            let name = symbol.name_bytes()?;
            let section = symbol.section_index().context("a symbol in no section")?;
            let old = match image.symbols.get(name) {
                None => None,
                Some(Named::One(old))
                    if old.kind == SymbolKind::Text && code.contains(&section) =>
                {
                    ensure!(
                        old.size >= Self::JUMP as u64,
                        "{} is too short to jump from",
                        String::from_utf8_lossy(name)
                    );
                    Some(*old)
                }
                Some(_) => bail!(
                    "the patch replaces {}, which is not one function",
                    String::from_utf8_lossy(name)
                ),
            };
            patch.replaced.push(Replaced {
                name,
                section,
                offset: symbol.address(),
                size: symbol.size(),
                old,
            });
        }
        Ok(patch)
    }

    fn target(
        &self,
        index: SymbolIndex,
        image: &Image,
        loaded: &FxHashSet<SectionIndex>,
    ) -> anyhow::Result<Target> {
        let symbol = self.file.symbol_by_index(index)?;
        if let Some(section) = symbol.section_index() {
            ensure!(loaded.contains(&section), "a reference to a section that is not loaded");
            return Ok(Target::Ours(section, symbol.address()));
        }
        let name = symbol.name_bytes()?;
        let shown = || String::from_utf8_lossy(name);
        ensure!(symbol.is_undefined(), "{} is neither defined nor undefined", shown());
        match image.symbols.get(name) {
            Some(Named::One(Defined { kind: SymbolKind::Tls, .. })) => {
                bail!("{} is thread-local", shown())
            }
            Some(Named::One(defined)) => Ok(Target::Theirs(defined.address)),
            Some(Named::Many) => bail!("{} names several things in the executable", shown()),
            None => match image.imports.get(name) {
                Some(slot) => Ok(Target::Import(*slot)),
                None => bail!("{} is not in the executable", shown()),
            },
        }
    }

    /// The frames in an `.eh_frame` section loaded at `address`: where the code of each
    /// starts, and where the frame is.
    fn frames_in(section: &[u8], address: u64) -> anyhow::Result<Vec<(u64, u64)>> {
        let frames = gimli::EhFrame::new(section, gimli::LittleEndian);
        let bases = gimli::BaseAddresses::default().set_eh_frame(address);
        let mut entries = frames.entries(&bases);
        let mut found = Vec::new();
        while let Some(entry) = entries.next()? {
            if let gimli::CieOrFde::Fde(frame) = entry {
                let frame = frame.parse(gimli::EhFrame::cie_from_offset)?;
                found.push((frame.initial_address(), address + frame.offset() as u64));
            }
        }
        Ok(found)
    }

    /// Lays the patch out behind the executable and gives everything in it its address.
    fn place(&self, image: &Image) -> anyhow::Result<Addition> {
        let e = Endianness::Little;
        let page = image.page();
        let offset = image.len.next_multiple_of(page);
        let base = image.end().next_multiple_of(page);
        let relocations = image.relocations.as_ref().filter(|_| image.pie);
        ensure!(!image.pie || relocations.is_some(), "an executable the loader fixes nothing in");
        let fixed_up = if image.pie { self.stored + self.slots.len() } else { 0 };

        // Where everything goes, as offsets from `base`.
        let mut end = 0u64;
        let mut reserve = |len: usize, align: u64| {
            let start = end.next_multiple_of(align);
            end = start + len as u64;
            start
        };
        let mut placed = FxHashMap::default();
        let mut extents = [(0, 0); 3];
        let headers_len = (image.segments.len() + Segment::ALL.len())
            * size_of::<elf::ProgramHeader64<Endianness>>();
        let (mut headers_at, mut unwind_at, mut relocations_at) = (0, 0, 0);
        let (mut stubs_at, mut slots_at) = (0, 0);
        for segment in Segment::ALL {
            let start = reserve(0, page);
            if let Segment::Read = segment {
                headers_at = reserve(headers_len, 8);
                unwind_at = reserve(image.unwind.len_with(self.frames), 4);
            }
            for &index in &self.sections[segment as usize] {
                let section = self.file.section_by_index(index)?;
                placed.insert(index, reserve(section.size() as usize, section.align().max(1)));
            }
            match segment {
                Segment::Read => {
                    relocations_at = reserve(relocations.map_or(0, |it| it.len_with(fixed_up)), 8);
                }
                Segment::Code => stubs_at = reserve(self.stubs.len() * Self::STUB, 1),
                Segment::Data => slots_at = reserve(self.slots.len() * Self::SLOT, 8),
            }
            extents[segment as usize] = (start, reserve(0, 1));
        }
        let mut appended = vec![0u8; end as usize];

        let address = |target: Target| match target {
            Target::Ours(section, at) => base + placed[&section] + at,
            Target::Theirs(address) => address,
            Target::Import(slot) => slot,
        };
        let slot_of: FxHashMap<SymbolIndex, u64> = (self.slots.iter().copied())
            .zip((0..).map(|at| base + slots_at + at * Self::SLOT as u64))
            .collect();
        let stub_of: FxHashMap<SymbolIndex, u64> = (self.stubs.iter().copied())
            .zip((0..).map(|at| base + stubs_at + at * Self::STUB as u64))
            .collect();

        // The addresses stored in data, which the loader moves with the program.
        let mut fixed = Vec::with_capacity(fixed_up);
        let mut store = |appended: &mut [u8], at: u64, value: u64| {
            appended[(at - base) as usize..][..8].copy_from_slice(&value.to_le_bytes());
            if image.pie {
                fixed.push(elf::Rela64 {
                    r_offset: object::U64::new(e, at),
                    r_info: object::U64::new(e, u64::from(elf::R_X86_64_RELATIVE)),
                    r_addend: object::I64::new(e, value as i64),
                });
            }
        };

        let mut frames = Vec::with_capacity(self.frames);
        for section in self.file.sections() {
            let Some(&start) = placed.get(&section.index()) else { continue };
            let data = section.data()?;
            appended[start as usize..][..data.len()].copy_from_slice(data);
            for (at, relocation) in section.relocations() {
                let RelocationTarget::Symbol(index) = relocation.target() else { continue };
                let target = self.targets[&index];
                let place = base + start + at;
                let to = match (Fixup::of(&relocation)?, target) {
                    (Fixup::Absolute, _) => {
                        let value = address(target).wrapping_add_signed(relocation.addend());
                        store(&mut appended, place, value);
                        continue;
                    }
                    (Fixup::Relative, Target::Import(_)) => stub_of[&index],
                    (Fixup::Relative, _) | (Fixup::ThroughSlot, Target::Import(_)) => {
                        address(target)
                    }
                    (Fixup::ThroughSlot, _) => slot_of[&index],
                };
                let value = relative(to.wrapping_add_signed(relocation.addend()), place)?;
                appended[(place - base) as usize..][..4].copy_from_slice(&value.to_le_bytes());
            }
            if section.name_bytes()? == b".eh_frame" {
                let relocated = &appended[start as usize..][..data.len()];
                frames.extend(Self::frames_in(relocated, base + start)?);
            }
        }
        for (&index, &slot) in &slot_of {
            store(&mut appended, slot, address(self.targets[&index]));
        }
        for (&index, &stub) in &stub_of {
            let Target::Import(slot) = self.targets[&index] else { continue };
            let code = &mut appended[(stub - base) as usize..][..Self::STUB];
            code[..2].copy_from_slice(&[0xff, 0x25]);
            code[2..].copy_from_slice(&relative(slot, stub + Self::STUB as u64)?.to_le_bytes());
        }

        let mut overwritten = Vec::new();
        let mut redirected = 0;
        for replaced in &self.replaced {
            let Some(old) = replaced.old else { continue };
            let new = base + placed[&replaced.section] + replaced.offset;
            let mut jump = vec![0xe9];
            jump.extend_from_slice(&relative(new, old.address + Self::JUMP as u64)?.to_le_bytes());
            overwritten.push((image.offset_of(old.address)?, jump));
            // Its symbol names the new code, for whoever reads a backtrace.
            let named = [new.to_le_bytes(), replaced.size.to_le_bytes()].concat();
            overwritten
                .push((old.entry + offset_of!(elf::Sym64<Endianness>, st_value) as u64, named));
            redirected += 1;
        }

        let unwind = image.unwind.moved(base + unwind_at, frames)?;
        appended[unwind_at as usize..][..unwind.len()].copy_from_slice(&unwind);
        if let Some(relocations) = relocations {
            let table = &mut appended[relocations_at as usize..];
            let old = relocations.table.len();
            table[..old].copy_from_slice(&relocations.table);
            let added = object::bytes_of_slice(&fixed);
            table[old..][..added.len()].copy_from_slice(added);
            let len = (old + added.len()) as u64;
            overwritten
                .push((relocations.address_at, (base + relocations_at).to_le_bytes().to_vec()));
            overwritten.push((relocations.size_at, len.to_le_bytes().to_vec()));
        }

        // The program headers, with the ones that moved pointing here and the new segments.
        let mut segments = image.segments.clone();
        let point = |header: &mut elf::ProgramHeader64<Endianness>, at: u64, len: u64| {
            header.p_offset.set(e, offset + at);
            header.p_vaddr.set(e, base + at);
            header.p_paddr.set(e, base + at);
            header.p_filesz.set(e, len);
            header.p_memsz.set(e, len);
        };
        point(&mut segments[image.unwind.segment], unwind_at, unwind.len() as u64);
        if let Some(header) = segments.iter_mut().find(|it| it.p_type(e) == elf::PT_PHDR) {
            point(header, headers_at, headers_len as u64);
        }
        for segment in Segment::ALL {
            let (start, end) = extents[segment as usize];
            let mut header = elf::ProgramHeader64::<Endianness> {
                p_type: object::U32::new(e, elf::PT_LOAD),
                p_flags: object::U32::new(e, segment.flags()),
                p_offset: object::U64::new(e, 0),
                p_vaddr: object::U64::new(e, 0),
                p_paddr: object::U64::new(e, 0),
                p_filesz: object::U64::new(e, 0),
                p_memsz: object::U64::new(e, 0),
                p_align: object::U64::new(e, page),
            };
            point(&mut header, start, end - start);
            segments.push(header);
        }
        let headers = object::bytes_of_slice(&segments);
        appended[headers_at as usize..][..headers.len()].copy_from_slice(headers);
        let mut header = image.header;
        header.e_phoff.set(e, offset + headers_at);
        header.e_phnum.set(e, u16::try_from(segments.len())?);
        overwritten.push((0, object::bytes_of(&header).to_vec()));

        Ok(Addition { offset, appended, overwritten, redirected })
    }
}

/// What a splice writes.
struct Addition {
    /// Where in the file `appended` goes: behind what the linker wrote.
    offset: u64,
    appended: Vec<u8>,
    /// What changes of the file the linker wrote, by where it is.
    overwritten: Vec<(u64, Vec<u8>)>,
    redirected: usize,
}

fn read_i32(bytes: &[u8]) -> i32 {
    i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// The distance from `from` to `to`, where the code has four bytes for it.
fn relative(to: u64, from: u64) -> anyhow::Result<i32> {
    i32::try_from(to.wrapping_sub(from) as i64).context("the program is too large")
}
