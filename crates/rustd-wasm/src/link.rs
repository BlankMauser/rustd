//! Makes WebAssembly modules out of the objects codegen writes, one per build.
//!
//! The first module and every one after it share one memory, one function table and one
//! stack pointer, which they import. Each function has a slot in the table for as long as
//! the page lives, and all calls go through the table. So a build after an edit is a module
//! with only the functions whose code changed, and instantiating it writes their slots: what
//! called the old code calls the new code, and the program's memory stays what it was.

use std::{
    borrow::Cow,
    hash::{Hash, Hasher},
};

use hir_ty::mir::codegen::wasm::{
    ADDRESS_LEN, STACK_POINTER, TYPE_INDEX_LEN, WasmFunction, WasmSignature,
};
use object::{Object, ObjectSection, ObjectSymbol, RelocationTarget, SectionKind, SymbolKind};
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};
use wasm_encoder::{
    CodeSection, ConstExpr, DataSection, ElementSection, Elements, EntityType, FunctionSection,
    GlobalType, ImportSection, MemoryType, Module, RefType, TableType, TypeSection, ValType,
};

/// The module the memory, the table and the stack pointer are imported from.
const ENV: &str = "env";
/// The module the functions the program declares in `extern` blocks are imported from.
const HOST: &str = "host";

/// Nothing is put below this, so that null and what is near it stay unmapped data.
const STACK_START: u64 = 1 << 16;
const STACK_SIZE: u64 = 1 << 20;
const DATA_ALIGN: u64 = 16;

/// Where everything linked so far is.
pub struct Linker {
    /// The table slot of each function, with what was last put there.
    slots: FxHashMap<Key, Slot>,
    data: FxHashMap<Key, Placed>,
    table_len: u32,
    memory_len: u64,
}

/// What a function or a piece of data is from one build to the next: the one of its name,
/// when another object could name it, and otherwise whatever has its content. The symbols
/// of the rest are numbered anew in every build.
#[derive(PartialEq, Eq, Hash, Clone)]
enum Key {
    Named(String),
    Content(u64),
}

struct Slot {
    index: u32,
    /// The content of the function there, `None` until something defines or imports it.
    content: Option<u64>,
}

struct Placed {
    address: u64,
    content: u64,
}

/// A module to instantiate, and what it needs to be there first.
pub struct Patch {
    pub module: Vec<u8>,
    /// How many slots the table has to have, and how many bytes the memory.
    pub table_len: u32,
    pub memory_len: u64,
    /// The stack pointer to start with, for the first module.
    pub stack_top: u64,
    /// The slots of the functions of the program that can be called by name.
    pub exports: Vec<(String, u32)>,
    /// The symbols of the functions in the module, and the bytes of data in it.
    pub functions: Vec<String>,
    pub data_len: usize,
}

impl Default for Linker {
    fn default() -> Self {
        Linker {
            slots: FxHashMap::default(),
            data: FxHashMap::default(),
            // Slot 0 stays empty: a null function pointer traps.
            table_len: 1,
            memory_len: STACK_START + STACK_SIZE,
        }
    }
}

/// A symbol the object defines in a section with content.
struct Defined<'a> {
    name: String,
    bytes: Cow<'a, [u8]>,
    relocs: Vec<Reloc>,
    is_code: bool,
    writable: bool,
    /// Whether another object could name it.
    global: bool,
}

/// A place that holds the address of a symbol, relative to the start of its own symbol.
struct Reloc {
    offset: usize,
    target: object::SymbolIndex,
    addend: i64,
}

enum Target {
    Defined(usize),
    /// A function by name, which the page has to provide.
    Undefined(String),
}

/// The symbols of one object.
struct Symbols<'a> {
    defined: Vec<Defined<'a>>,
    targets: FxHashMap<object::SymbolIndex, Target>,
    keys: Vec<Key>,
    /// What each symbol is made of, which is its key unless it has a name.
    contents: Vec<u64>,
}

impl<'a> Symbols<'a> {
    fn read(file: &object::File<'a>) -> Result<Symbols<'a>, String> {
        let mut defined = Vec::new();
        let mut targets = FxHashMap::default();
        for section in file.sections() {
            let is_code = match section.kind() {
                SectionKind::Text => true,
                SectionKind::Data
                | SectionKind::ReadOnlyData
                | SectionKind::ReadOnlyDataWithRel
                | SectionKind::ReadOnlyString
                | SectionKind::UninitializedData => false,
                // Unwind tables and the like describe machine code that is not there.
                _ => continue,
            };
            let content = section.data().map_err(|e| format!("object: {e}"))?;
            let mut relocs: Vec<_> = section.relocations().collect();
            relocs.sort_unstable_by_key(|it| it.0);
            for symbol in file.symbols().filter(|it| it.section_index() == Some(section.index())) {
                if symbol.kind() == SymbolKind::Section {
                    continue;
                }
                let start = (symbol.address() - section.address()) as usize;
                let end = start + symbol.size() as usize;
                let bytes = match content.get(start..end) {
                    Some(bytes) => Cow::Borrowed(bytes),
                    // `.bss` has no bytes in the file.
                    None => Cow::Owned(vec![0; end - start]),
                };
                let from = relocs.partition_point(|it| (it.0 as usize) < start);
                let to = relocs.partition_point(|it| (it.0 as usize) < end);
                let relocs = relocs[from..to]
                    .iter()
                    .map(|(offset, reloc)| {
                        let RelocationTarget::Symbol(target) = reloc.target() else {
                            return Err("object: relocation without a symbol".to_owned());
                        };
                        // Data is written for a 64-bit target, which the code builder is set up as;
                        // an address is four bytes all the same.
                        if !matches!(reloc.size(), 32 | 64) {
                            return Err(format!("object: {}-bit relocation", reloc.size()));
                        }
                        let offset = *offset as usize - start;
                        Ok(Reloc { offset, target, addend: reloc.addend() })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let writable =
                    matches!(section.kind(), SectionKind::Data | SectionKind::UninitializedData);
                let name = symbol.name().map_err(|e| format!("object: {e}"))?.to_owned();
                let global = symbol.is_global();
                targets.insert(symbol.index(), Target::Defined(defined.len()));
                defined.push(Defined { name, bytes, relocs, is_code, writable, global });
            }
        }
        for symbol in file.symbols().filter(|it| it.is_undefined()) {
            let name = symbol.name().map_err(|e| format!("object: {e}"))?;
            if !name.is_empty() {
                targets.insert(symbol.index(), Target::Undefined(name.to_owned()));
            }
        }
        let mut symbols = Symbols { defined, targets, keys: Vec::new(), contents: Vec::new() };
        symbols.contents = symbols.contents()?;
        symbols.keys = (symbols.defined.iter().zip(&symbols.contents))
            .map(|(it, &content)| match it.named() {
                true => Key::Named(it.name.clone()),
                false => Key::Content(content),
            })
            .collect();
        Ok(symbols)
    }

    /// A hash for each symbol of everything it is: its bytes, and what they refer to, where
    /// a symbol without a name of its own is what it is made of in turn.
    fn contents(&self) -> Result<Vec<u64>, String> {
        let hash = |it: &dyn Fn(&mut FxHasher)| {
            let mut hasher = FxHasher::default();
            it(&mut hasher);
            hasher.finish()
        };
        let mut contents: Vec<u64> = (self.defined.iter())
            .map(|it| hash(&|hasher| (it.is_code, it.writable, &it.bytes).hash(hasher)))
            .collect();
        // Two symbols that differ only in what they reach after n steps are told apart in
        // round n, so there is nothing more to tell apart once a round finds nothing.
        let mut distinct = 0;
        loop {
            let mut next = Vec::with_capacity(contents.len());
            for (symbol, &own) in self.defined.iter().zip(&contents) {
                let mut hasher = FxHasher::default();
                own.hash(&mut hasher);
                for reloc in &symbol.relocs {
                    (reloc.offset, reloc.addend).hash(&mut hasher);
                    match self.targets.get(&reloc.target) {
                        Some(Target::Undefined(name)) => name.hash(&mut hasher),
                        Some(&Target::Defined(at)) if self.defined[at].named() => {
                            self.defined[at].name.hash(&mut hasher)
                        }
                        Some(&Target::Defined(at)) => contents[at].hash(&mut hasher),
                        None => return Err(format!("{}: relocation to nothing", symbol.name)),
                    }
                }
                next.push(hasher.finish());
            }
            let now = next.iter().collect::<FxHashSet<_>>().len();
            contents = next;
            if now <= distinct {
                return Ok(contents);
            }
            distinct = now;
        }
    }
}

impl Defined<'_> {
    fn named(&self) -> bool {
        self.global
    }
}

impl Linker {
    /// Links objects against everything linked before. Of two definitions of a name the
    /// one in the earlier object counts, so a patch goes before the object it amends.
    /// `exports` are the symbols of the functions to list in the patch.
    pub fn link<'a>(
        &mut self,
        objects: &[&[u8]],
        exports: impl Iterator<Item = &'a str>,
    ) -> Result<Patch, String> {
        let mut out = Output::default();
        let mut seen = FxHashSet::default();
        let files = (objects.iter())
            .map(|object| object::File::parse(*object).map_err(|e| format!("object: {e}")))
            .collect::<Result<Vec<_>, _>>()?;
        let symbols = files.iter().map(Symbols::read).collect::<Result<Vec<_>, _>>()?;
        // Everything gets its place before anything is written: code and data hold the
        // places of each other, in whichever object those are.
        let new: Vec<_> = symbols.iter().map(|it| self.place_new(it, &mut seen)).collect();
        for (symbols, new) in symbols.iter().zip(new) {
            self.write(symbols, new, &mut out)?;
        }
        // What is left without code comes from the page.
        let mut imported: Vec<_> = std::mem::take(&mut out.called).into_iter().collect();
        imported.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        for (name, signature) in imported {
            let slot = self.slots.get_mut(&Key::Named(name.clone())).expect("placed by the call");
            if slot.content.is_none() {
                slot.content = Some(0);
                let ty = out.ty(&signature);
                out.imported.push((slot.index, ty, name));
            }
        }

        let exports = exports
            .filter_map(|name| {
                Some((name.to_owned(), self.slots.get(&Key::Named(name.to_owned()))?.index))
            })
            .collect();
        Ok(Patch {
            data_len: out.data.iter().map(|it| it.1.len()).sum(),
            functions: out.defined.iter().map(|it| it.3.clone()).collect(),
            module: out.module(),
            table_len: self.table_len,
            memory_len: self.memory_len,
            stack_top: STACK_START + STACK_SIZE,
            exports,
        })
    }

    /// Gives what is new in one object its place, and returns which of its symbols those
    /// are. `seen` has the names that objects before this one defined.
    fn place_new(&mut self, symbols: &Symbols<'_>, seen: &mut FxHashSet<String>) -> Vec<usize> {
        let mut new = Vec::new();
        for (at, symbol) in symbols.defined.iter().enumerate() {
            let (key, content) = (&symbols.keys[at], symbols.contents[at]);
            if symbol.global && !seen.insert(symbol.name.clone()) {
                continue;
            }
            if symbol.is_code {
                self.slot(key);
                let slot = self.slots.get_mut(key).expect("just given a slot");
                if slot.content != Some(content) {
                    slot.content = Some(content);
                    new.push(at);
                }
                continue;
            }
            // What the program wrote to a static since stays, unless the static changed.
            if self.data.get(key).is_none_or(|it| it.content != content) {
                let address = self.memory_len.next_multiple_of(DATA_ALIGN);
                self.memory_len = address + symbol.bytes.len() as u64;
                self.data.insert(key.clone(), Placed { address, content });
                new.push(at);
            }
        }
        new
    }

    /// Puts the symbols of an object that [`Self::place_new`] found new into the module.
    fn write(
        &mut self,
        symbols: &Symbols<'_>,
        new: Vec<usize>,
        out: &mut Output,
    ) -> Result<(), String> {
        for at in new {
            let symbol = &symbols.defined[at];
            if !symbol.is_code {
                let mut bytes = symbol.bytes.to_vec();
                for reloc in &symbol.relocs {
                    let value = self.place(symbols, reloc.target).wrapping_add(reloc.addend as u64);
                    let place = bytes.get_mut(reloc.offset..reloc.offset + 4);
                    place
                        .ok_or("relocation outside")?
                        .copy_from_slice(&(value as u32).to_le_bytes());
                }
                if bytes.iter().any(|&it| it != 0) {
                    out.data.push((self.data[&symbols.keys[at]].address, bytes));
                }
                continue;
            }
            let (mut function, start) = WasmFunction::decode(&symbol.bytes)?;
            for reloc in &symbol.relocs {
                let at = reloc.offset.checked_sub(start).ok_or("relocation outside")?;
                // A call is the address and `call_indirect` with its type.
                // The signature of a function nothing defines is that of a call to it.
                let type_at = (at + ADDRESS_LEN + 1) as u32;
                if let Some(Target::Undefined(name)) = symbols.targets.get(&reloc.target)
                    && let Some(&(_, ty)) = function.type_fixups.iter().find(|it| it.0 == type_at)
                    && !out.called.contains_key(name)
                {
                    out.called.insert(name.clone(), function.types[ty as usize].clone());
                }
                let value = (self.place(symbols, reloc.target) as i64).wrapping_add(reloc.addend);
                let place = function.body.get_mut(at..at + ADDRESS_LEN);
                write_sleb(place.ok_or("relocation outside")?, value);
            }
            for &(at, ty) in &function.type_fixups {
                let ty = out.ty(&function.types[ty as usize]);
                let place = &mut function.body[at as usize..at as usize + TYPE_INDEX_LEN];
                write_uleb(place, ty);
            }
            let ty = out.ty(&function.signature);
            let slot = self.slots[&symbols.keys[at]].index;
            out.defined.push((slot, ty, function.body, symbol.name.clone()));
        }
        Ok(())
    }

    /// Whether a build before this one had the code of the function.
    pub fn defines(&self, symbol: &str) -> bool {
        let slot = self.slots.get(&Key::Named(symbol.to_owned()));
        slot.is_some_and(|it| it.content.is_some())
    }

    /// The symbols an object refers to and does not define.
    pub fn undefined(object: &[u8]) -> Result<Vec<String>, String> {
        let file = object::File::parse(object).map_err(|e| format!("object: {e}"))?;
        let undefined = file.symbols().filter(|it| it.is_undefined());
        Ok(undefined.filter_map(|it| Some(it.name().ok()?.to_owned())).collect())
    }

    /// Where a symbol of the object is: the slot of a function, the address of data.
    fn place(&mut self, symbols: &Symbols<'_>, target: object::SymbolIndex) -> u64 {
        match symbols.targets.get(&target) {
            // Defined by an object of another build, or by the page if by nothing.
            Some(Target::Undefined(name)) => {
                let key = Key::Named(name.clone());
                match self.data.get(&key) {
                    Some(data) => data.address,
                    None => self.slot(&key) as u64,
                }
            }
            Some(&Target::Defined(at)) if symbols.defined[at].is_code => {
                self.slots[&symbols.keys[at]].index as u64
            }
            Some(&Target::Defined(at)) => self.data[&symbols.keys[at]].address,
            None => unreachable!("`Symbols::contents` saw every relocation"),
        }
    }

    fn slot(&mut self, key: &Key) -> u32 {
        if let Some(slot) = self.slots.get(key) {
            return slot.index;
        }
        let index = self.table_len;
        self.table_len += 1;
        self.slots.insert(key.clone(), Slot { index, content: None });
        index
    }
}

/// What goes in the module of one build.
#[derive(Default)]
struct Output {
    types: Vec<WasmSignature>,
    /// Slot, type, body and symbol of each function.
    defined: Vec<(u32, u32, Vec<u8>, String)>,
    /// The functions called that nothing defined, with the signature of a call.
    called: FxHashMap<String, WasmSignature>,
    /// Slot, type and name of each function the page provides.
    imported: Vec<(u32, u32, String)>,
    data: Vec<(u64, Vec<u8>)>,
}

impl Output {
    fn ty(&mut self, signature: &WasmSignature) -> u32 {
        let index = self.types.iter().position(|it| it == signature).unwrap_or_else(|| {
            self.types.push(signature.clone());
            self.types.len() - 1
        });
        index as u32
    }

    fn module(self) -> Vec<u8> {
        let mut types = TypeSection::new();
        for ty in &self.types {
            let params = ty.params.iter().map(|&it| ValType::from(it));
            let results = ty.results.iter().map(|&it| ValType::from(it));
            types.ty().function(params, results);
        }
        let mut imports = ImportSection::new();
        let memory = MemoryType {
            minimum: 0,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        };
        imports.import(ENV, "memory", EntityType::Memory(memory));
        let table = TableType {
            element_type: RefType::FUNCREF,
            table64: false,
            minimum: 0,
            maximum: None,
            shared: false,
        };
        imports.import(ENV, "table", EntityType::Table(table));
        debug_assert_eq!(STACK_POINTER, 0);
        let stack_pointer = GlobalType { val_type: ValType::I32, mutable: true, shared: false };
        imports.import(ENV, "stack_pointer", EntityType::Global(stack_pointer));
        for (_, ty, name) in &self.imported {
            imports.import(HOST, name, EntityType::Function(*ty));
        }
        let mut functions = FunctionSection::new();
        let mut code = CodeSection::new();
        let mut elements = ElementSection::new();
        let mut put = |slot: u32, function: usize| {
            let slot = ConstExpr::i32_const(slot as i32);
            elements.active(None, &slot, Elements::Functions([function as u32][..].into()));
        };
        for (at, (slot, _, _)) in self.imported.iter().enumerate() {
            put(*slot, at);
        }
        for (at, (slot, ty, body, _)) in self.defined.iter().enumerate() {
            functions.function(*ty);
            code.raw(body);
            put(*slot, self.imported.len() + at);
        }
        let mut data = DataSection::new();
        for (address, bytes) in &self.data {
            data.active(0, &ConstExpr::i32_const(*address as i32), bytes.iter().copied());
        }
        let mut module = Module::new();
        module.section(&types).section(&imports).section(&functions).section(&elements);
        module.section(&code).section(&data);
        module.finish()
    }
}

/// Writes a signed LEB128 padded to the length of `place`.
fn write_sleb(place: &mut [u8], value: i64) {
    let last = place.len() - 1;
    for (at, byte) in place.iter_mut().enumerate() {
        let bits = (value >> (7 * at).min(63)) as u8 & 0x7f;
        *byte = if at == last { bits } else { bits | 0x80 };
    }
}

/// Writes an unsigned LEB128 padded to the length of `place`.
fn write_uleb(place: &mut [u8], value: u32) {
    let last = place.len() - 1;
    for (at, byte) in place.iter_mut().enumerate() {
        let bits = (value >> (7 * at)) as u8 & 0x7f;
        *byte = if at == last { bits } else { bits | 0x80 };
    }
}
