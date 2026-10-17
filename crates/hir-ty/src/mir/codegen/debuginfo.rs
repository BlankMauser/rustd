//! DWARF for the functions of an object: which line every instruction is of (`.debug_line`)
//! and what every function is called and where it is declared (`.debug_info`). A backtrace
//! and a debugger read the same tables rustc writes for its own code, so a frame of ours
//! reads like one of rustc's.
//!
//! A compiled function knows its statements by their place in the body ([`LineRow`]), which
//! an edit above the function leaves alone. The lines are looked up when the function is
//! stitched into an object ([`FunctionDebug`]), and all functions of the object share one
//! compilation unit ([`DebugContext`]).
//!
//! A variable is told of when it is kept in one slot of the frame for as long as the function
//! runs ([`VariableDebug`]), by where the slot is from the frame pointer.

mod types;

pub(super) use types::Types;

use cranelift_codegen::{
    binemit::CodeOffset,
    gimli::{
        self, DW_AT_artificial, DW_AT_byte_size, DW_AT_comp_dir, DW_AT_const_value, DW_AT_count,
        DW_AT_data_member_location, DW_AT_decl_file, DW_AT_decl_line, DW_AT_discr,
        DW_AT_discr_value, DW_AT_encoding, DW_AT_enum_class, DW_AT_frame_base, DW_AT_high_pc,
        DW_AT_language, DW_AT_linkage_name, DW_AT_location, DW_AT_low_pc, DW_AT_name,
        DW_AT_producer, DW_AT_ranges, DW_AT_stmt_list, DW_AT_type, DW_TAG_array_type,
        DW_TAG_base_type, DW_TAG_enumeration_type, DW_TAG_enumerator, DW_TAG_formal_parameter,
        DW_TAG_lexical_block, DW_TAG_member, DW_TAG_pointer_type, DW_TAG_structure_type,
        DW_TAG_subprogram, DW_TAG_subrange_type, DW_TAG_template_type_parameter, DW_TAG_union_type,
        DW_TAG_variable, DW_TAG_variant, DW_TAG_variant_part, DwAte, Encoding, Format,
        LineEncoding, RunTimeEndian, SectionId,
        write::{
            Address, AttributeValue, DwarfUnit, EndianVec, Expression, FileId, LineProgram,
            LineString, Location, LocationList, Range, RangeList, Result as WriteResult, Sections,
            UnitEntryId, Writer,
        },
    },
    ir,
    isa::TargetIsa,
};
use cranelift_module::FuncId;
use cranelift_object::{
    ObjectProduct,
    object::{
        BinaryFormat, RelocationEncoding, RelocationFlags, RelocationKind, SectionKind,
        write::{Relocation, SymbolId},
    },
};
use rustc_hash::FxHashMap;
use serde_derive::{Deserialize, Serialize};

/// The statement the code of a function is of from `offset` on, as an index into the
/// statements the function was built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct LineRow {
    pub(super) offset: CodeOffset,
    pub(super) span: u32,
}

impl LineRow {
    /// The first source location that stands for a statement: 0 is that of none.
    const FIRST: u32 = 1;

    /// The source location of instructions built for the statement of index `span`.
    pub(super) fn source_loc(span: u32) -> ir::SourceLoc {
        ir::SourceLoc::new(span + Self::FIRST)
    }

    fn span_of(loc: ir::SourceLoc) -> Option<u32> {
        super::unwind::seh::unguarded(loc).bits().checked_sub(Self::FIRST)
    }

    /// The rows of a function from the source location its code has from each offset on, in
    /// the order of the offsets. Code of no statement goes with the statement before it.
    pub(super) fn of_ranges(
        ranges: impl IntoIterator<Item = (CodeOffset, ir::SourceLoc)>,
    ) -> Vec<LineRow> {
        let mut rows: Vec<LineRow> = Vec::new();
        for (offset, loc) in ranges {
            let Some(span) = Self::span_of(loc) else { continue };
            if rows.last().is_none_or(|last| last.span != span) {
                rows.push(LineRow { offset, span });
            }
        }
        rows
    }
}

/// A line of a source file, which is an index into the files of a [`FunctionDebug`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Place {
    pub(super) file: u32,
    pub(super) line: u32,
    pub(super) column: u32,
}

/// A type among the types of a [`FunctionDebug`], by its place in them.
pub(super) type TypeIndex = u32;

/// What a debugger is told of a type: its name, its size, and what it is made of as far as
/// that is told. A type names the types it is made of by their place in the same list, so
/// one that points to itself can be told of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum DebugType {
    /// A number, a `bool` or a `char`, whose bytes are read as the `DW_ATE_*` of `encoding`.
    Base {
        name: String,
        size: u64,
        encoding: u8,
    },
    /// A pointer of one word. `to` is `None` where what it points to is not told.
    Pointer {
        name: String,
        to: Option<TypeIndex>,
    },
    Array {
        of: TypeIndex,
        len: u64,
    },
    /// A struct, a tuple, a closure or a pointer of two words. One without fields may have
    /// some that are not told.
    Struct(Aggregate),
    Union(Aggregate),
    /// An enum none of whose variants has a field: a number with a name for each value.
    Names {
        name: String,
        size: u64,
        names: Vec<(String, u64)>,
    },
    /// An enum with fields. `tag` is `None` where there is one variant and nothing says which.
    Enum {
        of: Aggregate,
        tag: Option<Tag>,
        variants: Vec<Variant>,
    },
}

impl DebugType {
    /// Every type this one is made of, as it names them.
    fn parts_mut(&mut self) -> Vec<&mut TypeIndex> {
        match self {
            DebugType::Base { .. } | DebugType::Names { .. } => Vec::new(),
            DebugType::Pointer { to, .. } => to.iter_mut().collect(),
            DebugType::Array { of, .. } => vec![of],
            DebugType::Struct(of) | DebugType::Union(of) => of.parts_mut().collect(),
            DebugType::Enum { of, variants, .. } => {
                let variants = variants.iter_mut().flat_map(|it| &mut it.fields);
                of.parts_mut().chain(variants.map(|it| &mut it.ty)).collect()
            }
        }
    }
}

/// Adds `types`, which name each other by their places among themselves, to the end of
/// `all`, and returns where the first of them is then.
pub(super) fn add_types(all: &mut Vec<DebugType>, types: &[DebugType]) -> TypeIndex {
    let first = all.len() as TypeIndex;
    all.extend(types.iter().cloned().map(|mut ty| {
        ty.parts_mut().into_iter().for_each(|part| *part += first);
        ty
    }));
    first
}

/// A name, a size, the fields and the types the generic parameters stand for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Aggregate {
    pub(super) name: String,
    pub(super) size: u64,
    pub(super) fields: Vec<Field>,
    pub(super) parameters: Vec<(String, TypeIndex)>,
}

impl Aggregate {
    fn parts_mut(&mut self) -> impl Iterator<Item = &mut TypeIndex> {
        let parameters = self.parameters.iter_mut().map(|(_, ty)| ty);
        self.fields.iter_mut().map(|it| &mut it.ty).chain(parameters)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Field {
    pub(super) name: String,
    pub(super) offset: u64,
    pub(super) ty: TypeIndex,
}

/// Where an enum keeps which variant a value is of: an unsigned number of `size` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Tag {
    pub(super) offset: u64,
    pub(super) size: u64,
}

/// A variant of an enum. `tag` is the value of the tag that says a value is of this variant;
/// the variant without one is what every other value of the tag means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Variant {
    pub(super) name: String,
    pub(super) tag: Option<u64>,
    pub(super) fields: Vec<Field>,
}

/// Where the value of a variable is while the function runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum VariablePlace {
    /// In the frame, this far from what the frame is found by, from the first instruction
    /// to the last.
    Frame(i64),
    /// What a DWARF expression says, for the code from the first offset up to the second.
    /// Code of no range has the value nowhere a debugger can read it.
    Ranges(Vec<(CodeOffset, CodeOffset, Vec<u8>)>),
}

/// Where the stack slots of a function are while it runs, for the ones that hold a local
/// written with a name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct SlotPlaces {
    /// The DWARF expression of what [`VariablePlace::Frame`] and `DW_OP_fbreg` count from.
    pub(super) frame_base: Vec<u8>,
    /// The number of a slot and where its bytes are.
    pub(super) slots: Vec<(u32, VariablePlace)>,
}

impl SlotPlaces {
    /// The places in a frame Cranelift laid out, which it finds by the frame pointer.
    pub(super) fn of_frame(
        isa: &dyn TargetIsa,
        frame: &cranelift_codegen::MachBufferFrameLayout,
        slots: impl Iterator<Item = ir::StackSlot>,
    ) -> SlotPlaces {
        // FIXME: variables are told of on x86-64 alone, by `rbp`.
        if isa.triple().architecture != target_lexicon::Architecture::X86_64 {
            return SlotPlaces::default();
        }
        let to_fp = i64::from(frame.frame_to_fp_offset);
        let slots = slots
            .filter_map(|slot| {
                let offset = i64::from(frame.stackslots.get(slot)?.offset) - to_fp;
                Some((slot.as_u32(), VariablePlace::Frame(offset)))
            })
            .collect();
        SlotPlaces { frame_base: vec![gimli::DW_OP_reg0.0 + gimli::X86_64::RBP.0 as u8], slots }
    }
}

impl VariablePlace {
    /// Where the value is that this place holds the address of. Code a debugger cannot
    /// read the address in has no place for the value.
    pub(super) fn pointee(&self) -> VariablePlace {
        let ranges = match self {
            VariablePlace::Frame(offset) => {
                let mut held = vec![gimli::DW_OP_fbreg.0];
                // An expression of one operation writes nowhere it can fail.
                let _ = gimli::leb128::write::signed(&mut held, *offset);
                vec![(0, CodeOffset::MAX, held)]
            }
            VariablePlace::Ranges(ranges) => ranges.clone(),
        };
        let ranges = ranges
            .into_iter()
            .filter_map(|(from, to, held)| Some((from, to, pointee_expression(&held)?)))
            .collect();
        VariablePlace::Ranges(ranges)
    }
}

/// The expression of the memory an address is of, from the one of where the address is
/// held: in a register, or in memory a single operation finds.
fn pointee_expression(held: &[u8]) -> Option<Vec<u8>> {
    let (&operation, operands) = held.split_first()?;
    let register = gimli::DW_OP_reg0.0..=gimli::DW_OP_reg31.0;
    let based = gimli::DW_OP_breg0.0..=gimli::DW_OP_breg31.0;
    if register.contains(&operation) && operands.is_empty() {
        return Some(vec![operation - gimli::DW_OP_reg0.0 + gimli::DW_OP_breg0.0, 0]);
    }
    if operation != gimli::DW_OP_fbreg.0 && !based.contains(&operation) {
        return None;
    }
    // The one operand is a LEB128 number, which ends at the first byte without the high bit.
    let end = operands.iter().position(|byte| byte & 0x80 == 0)?;
    (end + 1 == operands.len()).then(|| [held, &[gimli::DW_OP_deref.0]].concat())
}

impl SlotPlaces {
    pub(super) fn of(&self, slot: u32) -> Option<&VariablePlace> {
        self.slots.iter().find(|it| it.0 == slot).map(|it| &it.1)
    }
}

/// A part of a function its names are looked up in: the body, a block, what follows a `let`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Scope {
    /// The scope this one is inside of, by its place among the scopes. It comes before.
    pub(super) parent: Option<u32>,
    /// The code of the scope and of the scopes inside it, in the order of the offsets.
    pub(super) ranges: Vec<(CodeOffset, CodeOffset)>,
}

/// A parameter or a `let` of a function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct VariableDebug {
    pub(super) name: String,
    pub(super) param: bool,
    pub(super) ty: TypeIndex,
    pub(super) place: VariablePlace,
    /// The scope the name is known in, by its place among the scopes: `None` is all of the
    /// function.
    pub(super) scope: Option<u32>,
}

/// What a debugger is told about one function, with nothing in it that depends on where the
/// function ends up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FunctionDebug {
    /// The name as it is written.
    pub(super) name: String,
    /// What a backtrace calls the function: its symbol when that is one rustc mangled, and
    /// otherwise its path the way such a symbol demangles.
    pub(super) linkage_name: String,
    /// The files as rustc names them, relative to the checkout when they are of it.
    pub(super) files: Vec<String>,
    pub(super) declared: Option<Place>,
    /// Where the code is from, from each offset on, in the order of the offsets.
    pub(super) rows: Vec<(CodeOffset, Place)>,
    /// The types of the variables and what those are made of.
    pub(super) types: Vec<DebugType>,
    pub(super) scopes: Vec<Scope>,
    pub(super) variables: Vec<VariableDebug>,
    /// The DWARF expression of what [`VariablePlace::Frame`] and `DW_OP_fbreg` count from.
    pub(super) frame_base: Vec<u8>,
}

impl FunctionDebug {
    pub(super) fn new(name: String, linkage_name: String) -> Self {
        FunctionDebug {
            name,
            linkage_name,
            files: Vec::new(),
            declared: None,
            rows: Vec::new(),
            types: Vec::new(),
            scopes: Vec::new(),
            variables: Vec::new(),
            frame_base: Vec::new(),
        }
    }

    /// The place of `line` and `column` in the file rustc calls `file`.
    pub(super) fn place(&mut self, file: String, line: u32, column: u32) -> Place {
        let known = self.files.iter().position(|it| *it == file);
        let file = known.unwrap_or_else(|| {
            self.files.push(file);
            self.files.len() - 1
        });
        Place { file: file as u32, line, column }
    }

    /// Adds that the code from `offset` on is of `place`.
    pub(super) fn row(&mut self, offset: CodeOffset, place: Place) {
        if self.rows.last().is_none_or(|last| last.1 != place) {
            self.rows.push((offset, place));
        }
    }
}

/// How many functions a compilation unit describes before the next one is begun. What reads
/// a unit reads all of it to find one function, so a unit of every function of a large
/// object takes a debugger or `addr2line` a second to answer from.
const FUNCTIONS_OF_A_UNIT: usize = 256;

/// The compilation units of an object, which every function of the object is described in.
pub(super) struct DebugContext {
    /// The unit functions are being added to.
    dwarf: DwarfUnit,
    /// The units that have all their functions.
    full: Vec<gimli::write::Unit>,
    encoding: Encoding,
    directory: Vec<u8>,
    files: FxHashMap<String, FileId>,
    ranges: Vec<Range>,
    types: FxHashMap<SharedType, UnitEntryId>,
    /// Whether the variables of the functions are told of, and not their lines alone.
    variables: bool,
}

/// What tells one type of the unit from another: every function with a variable of the
/// type shares its entry.
#[derive(PartialEq, Eq, Hash)]
enum SharedType {
    Named(gimli::DwTag, String, u64),
    Array(UnitEntryId, u64),
}

/// The entries of one function's types, as far as they have been asked for.
struct FunctionTypes<'a> {
    types: &'a [DebugType],
    entries: Vec<Option<UnitEntryId>>,
}

impl DebugContext {
    /// The unit of an object for `isa` whose files are named from `directory`, or `None`
    /// where the tables are not written yet: only ELF has them. The variables of the
    /// functions are left out unless `variables` asks for them.
    pub(super) fn new(isa: &dyn TargetIsa, directory: &str, variables: bool) -> Option<Self> {
        if isa.triple().binary_format != target_lexicon::BinaryFormat::Elf
            || super::Target::current().is_wasm()
        {
            return None;
        }
        let encoding =
            Encoding { format: Format::Dwarf32, version: 4, address_size: isa.pointer_bytes() };
        let mut context = DebugContext {
            dwarf: DwarfUnit::new(encoding),
            full: Vec::new(),
            encoding,
            directory: directory.as_bytes().to_vec(),
            files: FxHashMap::default(),
            ranges: Vec::new(),
            types: FxHashMap::default(),
            variables,
        };
        context.begin_unit();
        Some(context)
    }

    /// Sets up the unit functions are added to, which is a new one.
    fn begin_unit(&mut self) {
        let dwarf = &mut self.dwarf;
        dwarf.unit.line_program = LineProgram::new(
            self.encoding,
            LineEncoding::default(),
            LineString::String(self.directory.clone()),
            None,
            LineString::String(b"<rustd>".to_vec()),
            None,
        );
        let producer = dwarf.strings.add(concat!("rustd ", env!("CARGO_PKG_VERSION")));
        let name = dwarf.strings.add("<rustd>");
        let directory = dwarf.strings.add(self.directory.clone());
        let root = dwarf.unit.get_mut(dwarf.unit.root());
        root.set(DW_AT_producer, AttributeValue::StringRef(producer));
        root.set(DW_AT_language, AttributeValue::Language(gimli::DW_LANG_Rust));
        root.set(DW_AT_name, AttributeValue::StringRef(name));
        root.set(DW_AT_comp_dir, AttributeValue::StringRef(directory));
        root.set(DW_AT_stmt_list, AttributeValue::LineProgramRef);
        root.set(DW_AT_low_pc, AttributeValue::Address(Address::Constant(0)));
    }

    /// Ends the unit functions were added to, which is left out when it has none, and
    /// begins the next.
    fn end_unit(&mut self) {
        let next = gimli::write::Unit::new(self.encoding, LineProgram::none());
        let mut unit = std::mem::replace(&mut self.dwarf.unit, next);
        let ranges = std::mem::take(&mut self.ranges);
        self.files.clear();
        self.types.clear();
        self.begin_unit();
        if ranges.is_empty() {
            return;
        }
        let ranges = unit.ranges.add(RangeList(ranges));
        let root = unit.root();
        unit.get_mut(root).set(DW_AT_ranges, AttributeValue::RangeListRef(ranges));
        self.full.push(unit);
    }

    fn file(&mut self, name: &str) -> FileId {
        if let Some(&id) = self.files.get(name) {
            return id;
        }
        let program = &mut self.dwarf.unit.line_program;
        let directory = program.default_directory();
        let id = program.add_file(LineString::String(name.as_bytes().to_vec()), directory, None);
        self.files.insert(name.to_owned(), id);
        id
    }

    /// The entry of the type at `index` of `of`.
    fn ty(&mut self, of: &mut FunctionTypes<'_>, index: TypeIndex) -> Option<UnitEntryId> {
        if let Some(entry) = of.entries.get(index as usize).copied()? {
            return Some(entry);
        }
        let ty = of.types.get(index as usize)?;
        let shared = match ty {
            DebugType::Base { name, size, .. } => {
                SharedType::Named(DW_TAG_base_type, name.clone(), *size)
            }
            DebugType::Pointer { name, .. } => {
                SharedType::Named(DW_TAG_pointer_type, name.clone(), 0)
            }
            DebugType::Array { of: element, len } => {
                SharedType::Array(self.ty(of, *element)?, *len)
            }
            DebugType::Struct(it) | DebugType::Enum { of: it, .. } => {
                SharedType::Named(DW_TAG_structure_type, it.name.clone(), it.size)
            }
            DebugType::Union(it) => SharedType::Named(DW_TAG_union_type, it.name.clone(), it.size),
            DebugType::Names { name, size, .. } => {
                SharedType::Named(DW_TAG_enumeration_type, name.clone(), *size)
            }
        };
        if let Some(&entry) = self.types.get(&shared) {
            of.entries[index as usize] = Some(entry);
            // A function that followed fewer pointers to the type left it without what it
            // points to.
            if let DebugType::Pointer { to: Some(to), .. } = ty
                && self.dwarf.unit.get(entry).get(DW_AT_type).is_none()
                && let Some(to) = self.ty(of, *to)
            {
                self.dwarf.unit.get_mut(entry).set(DW_AT_type, AttributeValue::UnitRef(to));
            }
            return Some(entry);
        }
        let root = self.dwarf.unit.root();
        let entry = match &shared {
            SharedType::Named(tag, name, _) => self.named(root, *tag, name),
            SharedType::Array(..) => self.dwarf.unit.add(root, DW_TAG_array_type),
        };
        // A type that points to itself finds its entry here.
        of.entries[index as usize] = Some(entry);
        self.types.insert(shared, entry);
        self.fill(of, entry, ty);
        Some(entry)
    }

    /// Writes what `ty` is made of into its `entry`.
    fn fill(&mut self, of: &mut FunctionTypes<'_>, entry: UnitEntryId, ty: &DebugType) {
        let size = |unit: &mut gimli::write::Unit, size: u64| {
            unit.get_mut(entry).set(DW_AT_byte_size, AttributeValue::Udata(size));
        };
        match ty {
            DebugType::Base { size: bytes, encoding, .. } => {
                size(&mut self.dwarf.unit, *bytes);
                let encoding = AttributeValue::Encoding(DwAte(*encoding));
                self.dwarf.unit.get_mut(entry).set(DW_AT_encoding, encoding);
            }
            DebugType::Pointer { to, .. } => {
                if let Some(to) = to.and_then(|to| self.ty(of, to)) {
                    self.dwarf.unit.get_mut(entry).set(DW_AT_type, AttributeValue::UnitRef(to));
                }
            }
            DebugType::Array { of: element, len } => {
                let Some(element) = self.ty(of, *element) else { return };
                let unit = &mut self.dwarf.unit;
                unit.get_mut(entry).set(DW_AT_type, AttributeValue::UnitRef(element));
                let range = unit.add(entry, DW_TAG_subrange_type);
                unit.get_mut(range).set(DW_AT_count, AttributeValue::Udata(*len));
            }
            DebugType::Struct(it) | DebugType::Union(it) => {
                size(&mut self.dwarf.unit, it.size);
                self.members(of, entry, &it.fields);
                self.parameters(of, entry, &it.parameters);
            }
            DebugType::Names { size: bytes, names, .. } => {
                size(&mut self.dwarf.unit, *bytes);
                let number = self.unsigned(*bytes);
                let unit = self.dwarf.unit.get_mut(entry);
                unit.set(DW_AT_type, AttributeValue::UnitRef(number));
                unit.set(DW_AT_enum_class, AttributeValue::Flag(true));
                for (name, value) in names {
                    let name = self.named(entry, DW_TAG_enumerator, name);
                    let value = AttributeValue::Udata(*value);
                    self.dwarf.unit.get_mut(name).set(DW_AT_const_value, value);
                }
            }
            DebugType::Enum { of: it, tag, variants } => {
                size(&mut self.dwarf.unit, it.size);
                self.variants(of, entry, it.size, *tag, variants);
                self.parameters(of, entry, &it.parameters);
            }
        }
    }

    /// The variants of the enum at `entry` the way rustc tells of them: a part of the enum
    /// that is one of several members by the value of another, each member a struct of its
    /// own with the fields of a variant.
    fn variants(
        &mut self,
        of: &mut FunctionTypes<'_>,
        entry: UnitEntryId,
        size: u64,
        tag: Option<Tag>,
        variants: &[Variant],
    ) {
        let part = self.dwarf.unit.add(entry, DW_TAG_variant_part);
        if let Some(tag) = tag {
            let number = self.unsigned(tag.size);
            let unit = &mut self.dwarf.unit;
            let member = unit.add(part, DW_TAG_member);
            let entry = unit.get_mut(member);
            entry.set(DW_AT_type, AttributeValue::UnitRef(number));
            entry.set(DW_AT_artificial, AttributeValue::Flag(true));
            entry.set(DW_AT_data_member_location, AttributeValue::Udata(tag.offset));
            unit.get_mut(part).set(DW_AT_discr, AttributeValue::UnitRef(member));
        }
        for variant in variants {
            let fields = self.named(entry, DW_TAG_structure_type, &variant.name);
            self.dwarf.unit.get_mut(fields).set(DW_AT_byte_size, AttributeValue::Udata(size));
            self.members(of, fields, &variant.fields);
            let choice = self.dwarf.unit.add(part, DW_TAG_variant);
            if let Some(value) = variant.tag {
                let value = AttributeValue::Udata(value);
                self.dwarf.unit.get_mut(choice).set(DW_AT_discr_value, value);
            }
            let member = self.named(choice, DW_TAG_member, &variant.name);
            let member = self.dwarf.unit.get_mut(member);
            member.set(DW_AT_type, AttributeValue::UnitRef(fields));
            member.set(DW_AT_data_member_location, AttributeValue::Udata(0));
        }
    }

    fn members(&mut self, of: &mut FunctionTypes<'_>, entry: UnitEntryId, fields: &[Field]) {
        for field in fields {
            let Some(ty) = self.ty(of, field.ty) else { continue };
            let member = self.named(entry, DW_TAG_member, &field.name);
            let member = self.dwarf.unit.get_mut(member);
            member.set(DW_AT_type, AttributeValue::UnitRef(ty));
            member.set(DW_AT_data_member_location, AttributeValue::Udata(field.offset));
        }
    }

    /// The types the generic parameters of the type at `entry` stand for, which a
    /// pretty-printer finds what a `Vec` holds by.
    fn parameters(
        &mut self,
        of: &mut FunctionTypes<'_>,
        entry: UnitEntryId,
        parameters: &[(String, TypeIndex)],
    ) {
        for (name, ty) in parameters {
            let Some(ty) = self.ty(of, *ty) else { continue };
            let parameter = self.named(entry, DW_TAG_template_type_parameter, name);
            self.dwarf.unit.get_mut(parameter).set(DW_AT_type, AttributeValue::UnitRef(ty));
        }
    }

    /// The unsigned number of `size` bytes.
    fn unsigned(&mut self, size: u64) -> UnitEntryId {
        let name = format!("u{}", size * 8);
        let shared = SharedType::Named(DW_TAG_base_type, name.clone(), size);
        if let Some(&entry) = self.types.get(&shared) {
            return entry;
        }
        let entry = self.named(self.dwarf.unit.root(), DW_TAG_base_type, &name);
        let unit = self.dwarf.unit.get_mut(entry);
        unit.set(DW_AT_byte_size, AttributeValue::Udata(size));
        unit.set(DW_AT_encoding, AttributeValue::Encoding(gimli::DW_ATE_unsigned));
        self.types.insert(shared, entry);
        entry
    }

    fn named(&mut self, parent: UnitEntryId, tag: gimli::DwTag, name: &str) -> UnitEntryId {
        let name = self.dwarf.strings.add(name.as_bytes());
        let entry = self.dwarf.unit.add(parent, tag);
        self.dwarf.unit.get_mut(entry).set(DW_AT_name, AttributeValue::StringRef(name));
        entry
    }

    /// Tells of the variables of the function at `function`, which starts at `start` and is
    /// `len` bytes of code.
    fn variables(
        &mut self,
        function: UnitEntryId,
        start: Address,
        len: u32,
        debug: &FunctionDebug,
    ) {
        if !self.variables || debug.variables.is_empty() || debug.frame_base.is_empty() {
            return;
        }
        let base = Expression::raw(debug.frame_base.clone());
        self.dwarf.unit.get_mut(function).set(DW_AT_frame_base, AttributeValue::Exprloc(base));
        let mut types =
            FunctionTypes { types: &debug.types, entries: vec![None; debug.types.len()] };
        let mut blocks = vec![None; debug.scopes.len()];
        for variable in &debug.variables {
            let Some(ty) = self.ty(&mut types, variable.ty) else { continue };
            let parent = match variable.scope {
                None => Some(function),
                Some(scope) => self.block(function, start, debug, &mut blocks, scope),
            };
            // A name of a scope that no code is of is not looked up.
            let Some(parent) = parent else { continue };
            let tag = if variable.param { DW_TAG_formal_parameter } else { DW_TAG_variable };
            let entry = self.named(parent, tag, &variable.name);
            self.dwarf.unit.get_mut(entry).set(DW_AT_type, AttributeValue::UnitRef(ty));
            if let Some(place) = self.place(start, len, &variable.place) {
                self.dwarf.unit.get_mut(entry).set(DW_AT_location, place);
            }
        }
    }

    /// The entry of the scope `scope` of a function, inside the entry of the scope it is in.
    fn block(
        &mut self,
        function: UnitEntryId,
        start: Address,
        debug: &FunctionDebug,
        blocks: &mut [Option<UnitEntryId>],
        scope: u32,
    ) -> Option<UnitEntryId> {
        if let Some(block) = blocks.get(scope as usize).copied()? {
            return Some(block);
        }
        let Scope { parent, ranges } = debug.scopes.get(scope as usize)?;
        if ranges.is_empty() {
            return None;
        }
        let parent = match parent {
            Some(parent) => self.block(function, start, debug, blocks, *parent)?,
            None => function,
        };
        let ranges = ranges
            .iter()
            .map(|&(from, to)| Range::StartLength {
                begin: past(start, from),
                length: (to - from).into(),
            })
            .collect();
        let ranges = self.dwarf.unit.ranges.add(RangeList(ranges));
        let block = self.dwarf.unit.add(parent, DW_TAG_lexical_block);
        self.dwarf.unit.get_mut(block).set(DW_AT_ranges, AttributeValue::RangeListRef(ranges));
        blocks[scope as usize] = Some(block);
        Some(block)
    }

    /// Where a variable of a function is, or `None` where it is nowhere to be read.
    fn place(&mut self, start: Address, len: u32, place: &VariablePlace) -> Option<AttributeValue> {
        let ranges = match place {
            VariablePlace::Frame(offset) => {
                let mut place = Expression::new();
                place.op_fbreg(*offset);
                return Some(AttributeValue::Exprloc(place));
            }
            VariablePlace::Ranges(ranges) => ranges,
        };
        if let [(0, to, expression)] = ranges.as_slice()
            && *to >= len
        {
            return Some(AttributeValue::Exprloc(Expression::raw(expression.clone())));
        }
        let places: Vec<Location> = ranges
            .iter()
            .filter(|(from, to, _)| from < to && *from < len)
            .map(|(from, to, expression)| Location::StartLength {
                begin: past(start, *from),
                length: (to.min(&len) - from).into(),
                data: Expression::raw(expression.clone()),
            })
            .collect();
        if places.is_empty() {
            return None;
        }
        let places = self.dwarf.unit.locations.add(LocationList(places));
        Some(AttributeValue::LocationListRef(places))
    }

    /// Describes the function `func` of the module, which is `len` bytes of code.
    pub(super) fn add_function(&mut self, func: FuncId, len: u32, debug: &FunctionDebug) {
        if len == 0 {
            return;
        }
        if self.ranges.len() == FUNCTIONS_OF_A_UNIT {
            self.end_unit();
        }
        let start = Address::Symbol { symbol: func.as_u32() as usize, addend: 0 };
        let files: Vec<FileId> = debug.files.iter().map(|it| self.file(it)).collect();
        let file = |place: &Place| files.get(place.file as usize).copied();
        self.ranges.push(Range::StartLength { begin: start, length: len.into() });

        let name = self.dwarf.strings.add(debug.name.as_bytes());
        let linkage_name = self.dwarf.strings.add(debug.linkage_name.as_bytes());
        let unit = &mut self.dwarf.unit;
        let function = unit.add(unit.root(), DW_TAG_subprogram);
        let entry = unit.get_mut(function);
        entry.set(DW_AT_name, AttributeValue::StringRef(name));
        entry.set(DW_AT_linkage_name, AttributeValue::StringRef(linkage_name));
        entry.set(DW_AT_low_pc, AttributeValue::Address(start));
        entry.set(DW_AT_high_pc, AttributeValue::Udata(len.into()));
        if let Some(declared) = &debug.declared
            && let Some(file) = file(declared)
        {
            entry.set(DW_AT_decl_file, AttributeValue::FileIndex(Some(file)));
            entry.set(DW_AT_decl_line, AttributeValue::Udata(declared.line.into()));
        }
        self.variables(function, start, len, debug);

        // The code ahead of the first statement sets the frame up, which is of the line the
        // function is declared on, or of the first statement when that line is not known.
        let first = debug.rows.first().map(|it| it.1);
        let entry_place = debug.declared.or(first).filter(|_| first.is_some());
        let lead = entry_place.filter(|_| debug.rows.first().is_some_and(|it| it.0 != 0));
        let rows = lead.map(|place| (0, place)).into_iter().chain(debug.rows.iter().copied());
        let program = &mut self.dwarf.unit.line_program;
        let mut any = false;
        for (offset, place) in rows {
            let Some(file) = file(&place) else { continue };
            if !any {
                program.begin_sequence(Some(start));
                any = true;
            }
            let row = program.row();
            row.address_offset = offset.into();
            row.file = file;
            row.line = place.line.into();
            row.column = place.column.into();
            program.generate_row();
        }
        if any {
            program.end_sequence(len.into());
        }
    }

    /// Writes the sections into the object, when it has a function to describe.
    pub(super) fn emit(mut self, product: &mut ObjectProduct) -> Result<(), String> {
        self.end_unit();
        if self.full.is_empty() || product.object.format() != BinaryFormat::Elf {
            return Ok(());
        }
        let mut dwarf = gimli::write::Dwarf::new();
        (dwarf.strings, dwarf.line_strings) = (self.dwarf.strings, self.dwarf.line_strings);
        for unit in self.full {
            dwarf.units.add(unit);
        }
        let mut sections = Sections::new(SectionWriter::default());
        dwarf.write(&mut sections).map_err(|e| e.to_string())?;

        let mut placed = FxHashMap::default();
        sections.for_each(|id, section: &SectionWriter| {
            if !section.bytes.slice().is_empty() {
                let kind = match id {
                    SectionId::DebugStr | SectionId::DebugLineStr => SectionKind::DebugString,
                    _ => SectionKind::Debug,
                };
                let name = id.name().as_bytes().to_vec();
                let placed_at = product.object.add_section(Vec::new(), name, kind);
                product.object.section_mut(placed_at).set_data(section.bytes.slice().to_vec(), 1);
                placed.insert(id, placed_at);
            }
            Ok::<_, String>(())
        })?;
        sections.for_each(|id, section: &SectionWriter| {
            let Some(&at) = placed.get(&id) else { return Ok(()) };
            for reloc in &section.relocs {
                let (symbol, addend) = match reloc.target {
                    Target::Function(func) => function_place(product, func),
                    Target::Section(section) => {
                        let Some(&section) = placed.get(&section) else {
                            return Err(format!("{} points to a section left out", id.name()));
                        };
                        (product.object.section_symbol(section), 0)
                    }
                };
                let flags = RelocationFlags::Generic {
                    kind: RelocationKind::Absolute,
                    encoding: RelocationEncoding::Generic,
                    size: reloc.size * 8,
                };
                product
                    .object
                    .add_relocation(
                        at,
                        Relocation {
                            offset: reloc.offset,
                            symbol,
                            flags,
                            addend: addend + reloc.addend,
                        },
                    )
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })
    }
}

/// The address `by` bytes past `start`.
fn past(start: Address, by: CodeOffset) -> Address {
    match start {
        Address::Constant(start) => Address::Constant(start + u64::from(by)),
        Address::Symbol { symbol, addend } => {
            Address::Symbol { symbol, addend: addend + i64::from(by) }
        }
    }
}

/// The symbol to name a function by and how far past it the function starts. A local
/// function is named through its section, as `.eh_frame` names it.
fn function_place(product: &mut ObjectProduct, func: FuncId) -> (SymbolId, i64) {
    let symbol = product.function_symbol(func);
    match product.object.symbol_section_and_offset(symbol) {
        Some((section, offset)) => (section, offset as i64),
        None => (symbol, 0),
    }
}

/// What a relocation of a debug section points into.
#[derive(Clone, Copy)]
enum Target {
    Function(FuncId),
    Section(SectionId),
}

#[derive(Clone, Copy)]
struct Reloc {
    offset: u64,
    size: u8,
    target: Target,
    addend: i64,
}

/// A [`Writer`] of one debug section that collects the relocations its output needs: the
/// addresses of functions, and the offsets into other debug sections, which a linker moves.
#[derive(Clone)]
struct SectionWriter {
    relocs: Vec<Reloc>,
    bytes: EndianVec<RunTimeEndian>,
}

impl Default for SectionWriter {
    fn default() -> Self {
        SectionWriter { relocs: Vec::new(), bytes: EndianVec::new(RunTimeEndian::Little) }
    }
}

impl Writer for SectionWriter {
    type Endian = RunTimeEndian;

    fn endian(&self) -> Self::Endian {
        self.bytes.endian()
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn write(&mut self, bytes: &[u8]) -> WriteResult<()> {
        self.bytes.write(bytes)
    }

    fn write_at(&mut self, offset: usize, bytes: &[u8]) -> WriteResult<()> {
        self.bytes.write_at(offset, bytes)
    }

    fn write_address(&mut self, address: Address, size: u8) -> WriteResult<()> {
        match address {
            Address::Constant(val) => self.write_udata(val, size),
            Address::Symbol { symbol, addend } => {
                self.relocs.push(Reloc {
                    offset: self.len() as u64,
                    size,
                    target: Target::Function(FuncId::from_u32(symbol as u32)),
                    addend,
                });
                self.write_udata(0, size)
            }
        }
    }

    fn write_offset(&mut self, val: usize, section: SectionId, size: u8) -> WriteResult<()> {
        self.relocs.push(Reloc {
            offset: self.len() as u64,
            size,
            target: Target::Section(section),
            addend: val as i64,
        });
        self.write_udata(0, size)
    }

    fn write_offset_at(
        &mut self,
        offset: usize,
        val: usize,
        section: SectionId,
        size: u8,
    ) -> WriteResult<()> {
        self.relocs.push(Reloc {
            offset: offset as u64,
            size,
            target: Target::Section(section),
            addend: val as i64,
        });
        self.write_udata_at(offset, 0, size)
    }
}
