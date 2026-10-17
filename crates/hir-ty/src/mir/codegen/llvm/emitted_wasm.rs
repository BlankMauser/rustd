//! The WebAssembly object LLVM wrote for one function, read back as a [`Code`] of the kind
//! [`wasm::lower`](crate::mir::codegen::wasm) makes: a [`WasmFunction`] and relocations.
//!
//! LLVM writes an object for `wasm-ld`, which differs from what the linker of the page
//! takes in three ways, all settled here:
//!
//! - a call names its callee by function index. The page calls through the table, so each
//!   `call` is rewritten to the slot and a `call_indirect`. Nothing in a function body is
//!   an offset into it, so bytes can be put in;
//! - types are indices into the object's own type section. They become [`WasmSignature`]s;
//! - constants LLVM made (a table for a `match`, say) are data segments. Each becomes a
//!   data object of its own.

use std::ops::Range;

use cranelift_codegen::{
    binemit::Reloc,
    ir::{self, ExternalName},
};
use cranelift_module::{DataId, FuncId, ModuleReloc, ModuleRelocTarget};
use rustc_hash::FxHashMap;
use wasmparser::{
    KnownCustom, Linking, Parser, Payload, RelocationEntry, RelocationType, SymbolInfo, TypeRef,
    ValType,
};

use super::{OWN, Symbol, Unhandled};
use crate::mir::codegen::{
    ConstBlob,
    fragment::{Code, Recorder},
    wasm::{ADDRESS_LEN, STACK_POINTER, TYPE_INDEX_LEN, WasmFunction, WasmSignature, WasmType},
};

const STACK_POINTER_NAME: &str = "__stack_pointer";
/// `call`, `call_indirect` and `i32.const`, as bytes.
const CALL: u8 = 0x10;
const CALL_INDIRECT: u8 = 0x11;
const I32_CONST: u8 = 0x41;
/// A LEB128 of zero, padded to five bytes: where the linker writes a slot or a type.
const PLACE: [u8; 5] = [0x80, 0x80, 0x80, 0x80, 0x00];

/// What a symbol of the object's symbol table is.
#[derive(Clone, Copy)]
enum Named<'data> {
    /// A function, by its index among the imported and then the defined ones.
    Func {
        index: u32,
        name: Option<&'data str>,
    },
    Undefined(&'data str),
    /// Bytes of a data segment, from `offset` on.
    Constant {
        segment: u32,
        offset: u32,
    },
    StackPointer,
    Other,
}

struct Segment<'data> {
    bytes: &'data [u8],
    /// Where `bytes` are in the object.
    range: Range<usize>,
    align: u64,
}

#[derive(Default)]
pub(super) struct EmittedWasm<'data> {
    types: Vec<WasmSignature>,
    /// The functions the object imports: their names and types, by function index.
    imports: Vec<(&'data str, u32)>,
    /// The types of the functions the object defines, after the imported ones.
    defined: Vec<u32>,
    /// Where the bodies are in the object, without their sizes.
    bodies: Vec<Range<usize>>,
    /// Where the contents of the code and data sections start: their relocations count
    /// from there.
    code_start: usize,
    data_start: usize,
    segments: Vec<Segment<'data>>,
    symbols: Vec<Named<'data>>,
    code_relocs: Vec<RelocationEntry>,
    data_relocs: Vec<RelocationEntry>,
}

impl<'data> EmittedWasm<'data> {
    pub(super) fn code(
        object: &'data [u8],
        func: &ir::Function,
        recorder: &mut Recorder<'_>,
    ) -> Result<Code, Unhandled> {
        // The buffer inkwell gives ends in a nul that is no part of the object.
        let object = object.strip_suffix(&[0]).unwrap_or(object);
        let emitted = Self::parse(object)?;
        let (own, body) = emitted.own()?;
        let expected = WasmSignature::of(&func.signature).map_err(Unhandled::Llvm)?;
        if own != expected {
            return Err(Unhandled::Llvm(format!(
                "a function LLVM passes {own:?} where {expected:?} is taken"
            )));
        }
        let callees = Self::callees(func)?;
        let blobs = emitted.blobs(recorder)?;
        let mut function = WasmFunction {
            signature: own,
            types: Vec::new(),
            type_fixups: Vec::new(),
            body: Vec::new(),
        };
        let mut relocs = Vec::new();
        let source = &object[body.clone()];
        let mut copied = 0;
        for reloc in &emitted.code_relocs {
            let Some(at) = (emitted.code_start + reloc.offset as usize)
                .checked_sub(body.start)
                .filter(|at| at + PLACE.len() <= source.len())
            else {
                return Err(Unhandled::Shape("a relocation outside the function"));
            };
            let address = |name, addend| ModuleReloc { offset: 0, kind: Reloc::Abs4, name, addend };
            let symbol =
                || emitted.symbols.get(reloc.index as usize).copied().unwrap_or(Named::Other);
            match reloc.ty {
                RelocationType::FunctionIndexLeb => {
                    let Some(opcode) = at.checked_sub(1).filter(|&it| source[it] == CALL) else {
                        return Err(Unhandled::Shape("a function index outside a call"));
                    };
                    let (index, ty) = emitted.imported(symbol())?;
                    let signature = &emitted.types[ty as usize];
                    if let Some(expected) = callees.get(&index).filter(|it| *it != signature) {
                        return Err(Unhandled::Llvm(format!(
                            "a callee LLVM passes {signature:?} where {expected:?} is taken"
                        )));
                    }
                    function.body.extend_from_slice(&source[copied..opcode]);
                    function.body.push(I32_CONST);
                    relocs.push(ModuleReloc {
                        offset: function.body.len() as u32,
                        ..address(ModuleRelocTarget::user(0, index), 0)
                    });
                    function.body.extend(PLACE);
                    function.body.push(CALL_INDIRECT);
                    let ty = function.ty(signature);
                    function.type_fixups.push((function.body.len() as u32, ty));
                    function.body.extend(PLACE);
                    function.body.push(0);
                    copied = at + PLACE.len();
                    continue;
                }
                RelocationType::TypeIndexLeb => {
                    let signature = emitted
                        .types
                        .get(reloc.index as usize)
                        .ok_or(Unhandled::Shape("a type the object does not have"))?;
                    function.body.extend_from_slice(&source[copied..at]);
                    let ty = function.ty(signature);
                    function.type_fixups.push((function.body.len() as u32, ty));
                }
                RelocationType::TableIndexSleb => {
                    let (index, _) = emitted.imported(symbol())?;
                    function.body.extend_from_slice(&source[copied..at]);
                    relocs.push(ModuleReloc {
                        offset: function.body.len() as u32,
                        ..address(ModuleRelocTarget::user(0, index), 0)
                    });
                }
                // The linker writes every address as a signed LEB128. For an offset of a
                // load or store, which is unsigned, that is the same five bytes.
                RelocationType::MemoryAddrSleb | RelocationType::MemoryAddrLeb => {
                    let (target, addend) = match symbol() {
                        Named::Undefined(name) => match Symbol::parse(name) {
                            Some(Symbol::Data(index)) => (index, reloc.addend),
                            _ => return Err(Unhandled::Shape("data that was not asked for")),
                        },
                        Named::Constant { segment, offset } => {
                            (blobs[segment as usize].as_u32(), reloc.addend + i64::from(offset))
                        }
                        _ => return Err(Unhandled::Shape("an address of no data")),
                    };
                    function.body.extend_from_slice(&source[copied..at]);
                    relocs.push(ModuleReloc {
                        offset: function.body.len() as u32,
                        ..address(ModuleRelocTarget::user(1, target), addend)
                    });
                }
                RelocationType::GlobalIndexLeb => {
                    let Named::StackPointer = symbol() else {
                        return Err(Unhandled::Shape("a global that is not the stack pointer"));
                    };
                    function.body.extend_from_slice(&source[copied..at]);
                    let mut place = PLACE;
                    place[0] |= STACK_POINTER as u8;
                    function.body.extend(place);
                    copied = at + PLACE.len();
                    continue;
                }
                // There is one table.
                RelocationType::TableNumberLeb => continue,
                other => return Err(Unhandled::Relocation(other as u32)),
            }
            function.body.extend(PLACE);
            copied = at + PLACE.len();
        }
        function.body.extend_from_slice(&source[copied..]);

        let (bytes, start) = function.encode().map_err(Unhandled::Llvm)?;
        for reloc in &mut relocs {
            reloc.offset += start;
        }
        Ok(Code { signature: func.signature.clone(), bytes, align: 1, relocs, unwind: None })
    }

    fn parse(object: &'data [u8]) -> Result<Self, Unhandled> {
        let mut emitted = EmittedWasm::default();
        let mut globals = Vec::new();
        for payload in Parser::new(0).parse_all(object) {
            match payload? {
                Payload::TypeSection(types) => {
                    for ty in types.into_iter_err_on_gc_types() {
                        let ty = ty?;
                        emitted.types.push(WasmSignature {
                            params: Self::value_types(ty.params())?,
                            results: Self::value_types(ty.results())?,
                        });
                    }
                }
                Payload::ImportSection(imports) => {
                    for import in imports.into_imports() {
                        let import = import?;
                        match import.ty {
                            TypeRef::Func(ty) => emitted.imports.push((import.name, ty)),
                            TypeRef::Global(_) => globals.push(import.name),
                            _ => {}
                        }
                    }
                }
                Payload::FunctionSection(functions) => {
                    emitted.defined = functions.into_iter().collect::<Result<_, _>>()?;
                }
                Payload::CodeSectionStart { range, .. } => emitted.code_start = range.start,
                Payload::CodeSectionEntry(body) => emitted.bodies.push(body.range()),
                Payload::DataSection(segments) => {
                    emitted.data_start = segments.range().start;
                    for segment in segments {
                        let segment = segment?;
                        let end = segment.range.end;
                        emitted.segments.push(Segment {
                            bytes: segment.data,
                            range: end - segment.data.len()..end,
                            align: 1,
                        });
                    }
                }
                Payload::CustomSection(section) => match section.as_known() {
                    KnownCustom::Linking(linking) => {
                        for subsection in linking.subsections() {
                            match subsection? {
                                Linking::SymbolTable(symbols) => {
                                    for symbol in symbols {
                                        let symbol = Self::named(symbol?, &globals);
                                        emitted.symbols.push(symbol);
                                    }
                                }
                                Linking::SegmentInfo(segments) => {
                                    for (segment, info) in
                                        std::iter::zip(&mut emitted.segments, segments)
                                    {
                                        segment.align = 1 << info?.alignment;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    KnownCustom::Reloc(relocs) => {
                        let relocs = relocs.entries().into_iter().collect::<Result<_, _>>()?;
                        match section.name() {
                            "reloc.CODE" => emitted.code_relocs = relocs,
                            "reloc.DATA" => emitted.data_relocs = relocs,
                            _ => return Err(Unhandled::Shape("relocations of an unknown section")),
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        Ok(emitted)
    }

    fn value_types(types: &[ValType]) -> Result<Vec<WasmType>, Unhandled> {
        types
            .iter()
            .map(|ty| match ty {
                ValType::I32 => Ok(WasmType::I32),
                ValType::I64 => Ok(WasmType::I64),
                ValType::F32 => Ok(WasmType::F32),
                ValType::F64 => Ok(WasmType::F64),
                _ => Err(Unhandled::Shape("a value that is not a number")),
            })
            .collect()
    }

    fn named(symbol: SymbolInfo<'data>, globals: &[&'data str]) -> Named<'data> {
        match symbol {
            SymbolInfo::Func { index, name, .. } => Named::Func { index, name },
            SymbolInfo::Data { name, symbol: None, .. } => Named::Undefined(name),
            SymbolInfo::Data { symbol: Some(place), .. } => {
                Named::Constant { segment: place.index, offset: place.offset }
            }
            SymbolInfo::Global { index, name, .. }
                if name.or(globals.get(index as usize).copied()) == Some(STACK_POINTER_NAME) =>
            {
                Named::StackPointer
            }
            _ => Named::Other,
        }
    }

    /// The signature and the body of the one function the object defines.
    fn own(&self) -> Result<(WasmSignature, Range<usize>), Unhandled> {
        let own = self.symbols.iter().find_map(|symbol| match *symbol {
            Named::Func { index, name: Some(OWN) } => index.checked_sub(self.imports.len() as u32),
            _ => None,
        });
        match (own, self.defined.as_slice(), self.bodies.as_slice()) {
            (Some(0), &[ty], [body]) => Ok((self.types[ty as usize].clone(), body.clone())),
            _ => Err(Unhandled::Shape("an object of more or less than the function")),
        }
    }

    /// The request an imported function stands for, and its type in the object.
    fn imported(&self, symbol: Named<'data>) -> Result<(u32, u32), Unhandled> {
        let Named::Func { index, name } = symbol else {
            return Err(Unhandled::Shape("a call of what is not a function"));
        };
        let &(import, ty) = self
            .imports
            .get(index as usize)
            .ok_or(Unhandled::Shape("code that refers to itself"))?;
        // A function LLVM calls of its own accord, such as wide multiplication, which
        // nothing in the page defines.
        let name = name.unwrap_or(import);
        match Symbol::parse(name) {
            Some(Symbol::Func(index)) => Ok((index, ty)),
            _ => Err(Unhandled::Llvm(format!("a call of `{name}`, which nothing defines"))),
        }
    }

    /// The signatures the IR calls the functions it asked for with, by request.
    fn callees(func: &ir::Function) -> Result<FxHashMap<u32, WasmSignature>, Unhandled> {
        let mut callees = FxHashMap::default();
        for callee in func.dfg.ext_funcs.values() {
            let ExternalName::User(name) = callee.name else { continue };
            let name = &func.params.user_named_funcs()[name];
            if name.namespace != 0 {
                continue;
            }
            let signature = WasmSignature::of(&func.dfg.signatures[callee.signature])
                .map_err(Unhandled::Llvm)?;
            callees.insert(name.index, signature);
        }
        Ok(callees)
    }

    /// A data object for every segment, with the addresses it holds.
    fn blobs(&self, recorder: &mut Recorder<'_>) -> Result<Vec<DataId>, Unhandled> {
        let mut blobs: Vec<ConstBlob> = self
            .segments
            .iter()
            .map(|segment| ConstBlob {
                bytes: segment.bytes.to_vec(),
                relocs: Vec::new(),
                fn_relocs: Vec::new(),
                align: segment.align,
            })
            .collect();
        for reloc in &self.data_relocs {
            let at = self.data_start + reloc.offset as usize;
            let Some(segment) = self.segments.iter().position(|it| it.range.contains(&at)) else {
                return Err(Unhandled::Shape("a relocation outside the constants"));
            };
            let offset = (at - self.segments[segment].range.start) as u32;
            let symbol = self.symbols.get(reloc.index as usize).copied().unwrap_or(Named::Other);
            let blob = &mut blobs[segment];
            match (reloc.ty, symbol) {
                (RelocationType::TableIndexI32, symbol) => {
                    let (index, _) = self.imported(symbol)?;
                    blob.fn_relocs.push((offset, FuncId::from_u32(index)));
                }
                (RelocationType::MemoryAddrI32, Named::Undefined(name)) => {
                    let Some(Symbol::Data(index)) = Symbol::parse(name) else {
                        return Err(Unhandled::Shape("data that was not asked for"));
                    };
                    blob.relocs.push((offset, DataId::from_u32(index), reloc.addend));
                }
                _ => return Err(Unhandled::Shape("constants that hold an address of each other")),
            }
        }
        blobs
            .into_iter()
            .map(|blob| {
                recorder
                    .define_const(blob)
                    .map_err(|_| Unhandled::Shape("constants that cannot be defined"))
            })
            .collect()
    }
}

impl WasmFunction {
    /// The index of `signature` among the types of the indirect calls.
    fn ty(&mut self, signature: &WasmSignature) -> u32 {
        let known = self.types.iter().position(|it| it == signature);
        let index = known.unwrap_or_else(|| {
            self.types.push(signature.clone());
            self.types.len() - 1
        });
        index as u32
    }
}

impl From<wasmparser::BinaryReaderError> for Unhandled {
    fn from(e: wasmparser::BinaryReaderError) -> Self {
        Unhandled::Llvm(e.to_string())
    }
}
