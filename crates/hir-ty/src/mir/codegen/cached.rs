//! Compiled functions kept between runs of the daemon by their stable names.
//!
//! An [`Entry`] is a function of a crate that no edit reaches, instantiated with types of such
//! crates, as it was stitched into an object: its code, and what the code refers to by names
//! that mean the same in every run. A build that finds an entry for an instance, and one for
//! everything the entry calls, puts them in the object without asking the database anything
//! about the instance.
//!
//! What an entry imports from rustc is a symbol of the program it was compiled for. Another
//! program may link against a build of the crates without it, so a lookup checks that the
//! symbols are there.

use std::sync::{Arc, Mutex, OnceLock};

use cranelift_codegen::{
    binemit::{CodeOffset, Reloc},
    ir::Signature,
};
use cranelift_module::{ModuleReloc, ModuleRelocTarget};
use rustc_hash::{FxHashMap, FxHashSet};
use serde_derive::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    ConstBlob, DataIdentity,
    debuginfo::{FunctionDebug, LineRow},
    fragment::Code,
    store::{KEY, Store},
    unwind::FunctionUnwind,
};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub(super) struct Entry {
    /// What the symbol of the function is made from.
    pub(super) label: String,
    pub(super) funcs: Vec<CachedFunc>,
    /// A blob indexes `funcs` and the data before it, as the blobs of `Requests` do.
    pub(super) datas: Vec<CachedData>,
    pub(super) code: CachedCode,
    /// What a debugger is told about the function, when the build that kept it told one.
    pub(super) debug: Option<FunctionDebug>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub(super) struct CachedFunc {
    pub(super) target: CachedTarget,
    pub(super) sig: Signature,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub(super) enum CachedTarget {
    /// `checked` when the symbol was one of the program's rustc build, which another
    /// program's may lack.
    Import { symbol: String, checked: bool },
    /// The entry of this name, or whatever else the build has for the instance.
    Named(String),
    /// A function of the checkout that is compiled whenever it is called, by its stable name.
    Root(String),
    /// The `TypeId::of` that returns this.
    TypeId(u128),
    /// The function an `asm!` block is assembled as, by its assembly.
    Asm(String),
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub(super) enum CachedData {
    Blob {
        blob: ConstBlob,
        identity: DataIdentity,
    },
    Vtable {
        key: String,
        blob: ConstBlob,
    },
    /// A static of rustc's by its symbol.
    Static {
        symbol: String,
        thread_local: bool,
    },
    /// A static by its stable name, where its symbol is not what tells whose it is: one of
    /// the checkout is rustc's or ours as the build has it, and rustc exports no symbol for
    /// one it kept to its crate.
    OwnStatic(String),
    /// The file, line and column of a `core::panic::Location`.
    Location(Option<(String, u32, u32)>),
    FilePath(String),
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedReloc {
    offset: CodeOffset,
    kind: Reloc,
    name: ModuleRelocTarget,
    addend: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct CachedCode {
    signature: Signature,
    bytes: Vec<u8>,
    align: u64,
    relocs: Vec<CachedReloc>,
    unwind: Option<FunctionUnwind>,
    lines: Vec<LineRow>,
}

impl PartialEq for CachedCode {
    fn eq(&self, other: &CachedCode) -> bool {
        // `ModuleRelocTarget` is not `PartialEq`: compare what is written out.
        postcard::to_allocvec(self).ok() == postcard::to_allocvec(other).ok()
    }
}

impl From<&Code> for CachedCode {
    fn from(code: &Code) -> CachedCode {
        CachedCode {
            signature: code.signature.clone(),
            bytes: code.bytes.clone(),
            align: code.align,
            relocs: code
                .relocs
                .iter()
                .map(|it| CachedReloc {
                    offset: it.offset,
                    kind: it.kind,
                    name: it.name.clone(),
                    addend: it.addend,
                })
                .collect(),
            unwind: code.unwind.clone(),
            lines: code.lines.clone(),
        }
    }
}

impl CachedCode {
    /// Whether every relocation is inside the code and against one of the first
    /// `count(namespace)` things of its namespace, which code read from the store is not
    /// known to be.
    fn sound(&self, count: impl Fn(u32) -> usize) -> bool {
        (self.align == 0 || self.align.is_power_of_two())
            && self.relocs.iter().all(|it| {
                let known = match it.name {
                    ModuleRelocTarget::User { namespace, index } => {
                        (index as usize) < count(namespace)
                    }
                    _ => true,
                };
                known && (it.offset as usize) < self.bytes.len()
            })
    }

    pub(super) fn signature(&self) -> &Signature {
        &self.signature
    }

    pub(super) fn code(&self) -> Code {
        Code {
            signature: self.signature.clone(),
            bytes: self.bytes.clone(),
            align: self.align,
            relocs: self
                .relocs
                .iter()
                .map(|it| ModuleReloc {
                    offset: it.offset,
                    kind: it.kind,
                    name: it.name.clone(),
                    addend: it.addend,
                })
                .collect(),
            unwind: self.unwind.clone(),
            lines: self.lines.clone(),
        }
    }
}

/// What LLVM made of a function, kept by the IR it was made from. Cranelift keeps its own
/// code the same way, through its incremental cache.
///
/// LLVM asks for things the IR did not name: library functions it calls of its own accord,
/// and tables of addresses. Those are kept here, and relocations against them say which one
/// by its place among them, in a namespace of their own.
#[cfg(feature = "llvm")]
#[derive(Serialize, Deserialize)]
pub(super) struct StoredCode {
    code: CachedCode,
    imports: Vec<String>,
    blobs: Vec<super::ConstBlob>,
}

#[cfg(feature = "llvm")]
impl StoredCode {
    const IMPORTS: u32 = 2;
    const BLOBS: u32 = 3;

    /// The key for `func` compiled at `optimization`. Cranelift's key of the IR leaves out
    /// which requests the IR's names stand for, which LLVM's code has in it.
    pub(super) fn key(
        optimization: super::backend::Optimization,
        func: &cranelift_codegen::ir::Function,
    ) -> Option<[u8; KEY]> {
        let isa = super::target().ok()?;
        let ir = cranelift_codegen::incremental_cache::compute_cache_key(&**isa, func);
        let mut hasher = Sha256::new();
        hasher.update(b"rustd llvm 2");
        hasher.update(super::Backend::Llvm(optimization).to_string());
        hasher.update(ir.to_string());
        // Nor does it tell which statement an instruction is of, which the code's lines do.
        for block in func.layout.blocks() {
            for inst in func.layout.block_insts(block) {
                hasher.update(func.srcloc(inst).bits().to_le_bytes());
            }
        }
        for (_, name) in func.params.user_named_funcs().iter() {
            hasher.update(name.namespace.to_le_bytes());
            hasher.update(name.index.to_le_bytes());
        }
        Some(hasher.finalize().into())
    }

    /// `code` as it can be kept, if everything asked for since `mark` is of a kind that is.
    pub(super) fn of(
        code: &Code,
        recorder: &super::fragment::Recorder<'_>,
        mark: super::fragment::Mark,
    ) -> Option<StoredCode> {
        use super::fragment::{DataRequest, FuncTarget};
        let (funcs, datas) = recorder.since(mark);
        let imports = funcs.iter().map(|it| match &it.target {
            FuncTarget::Symbol(symbol) => Some(symbol.clone()),
            _ => None,
        });
        let blobs = datas.iter().map(|it| match it {
            DataRequest::Blob { blob, identity: DataIdentity::Shared } => Some(blob.clone()),
            _ => None,
        });
        let mut stored = StoredCode {
            code: CachedCode::from(code),
            imports: imports.collect::<Option<_>>()?,
            blobs: blobs.collect::<Option<_>>()?,
        };
        for reloc in &mut stored.code.relocs {
            reloc.name = match reloc.name {
                ModuleRelocTarget::User { namespace: 0, index } if index as usize >= mark.funcs => {
                    ModuleRelocTarget::user(Self::IMPORTS, index - mark.funcs as u32)
                }
                ModuleRelocTarget::User { namespace: 1, index } if index as usize >= mark.datas => {
                    ModuleRelocTarget::user(Self::BLOBS, index - mark.datas as u32)
                }
                ref other => other.clone(),
            };
        }
        Some(stored)
    }

    /// The code, with what LLVM asked for asked of `recorder` again.
    pub(super) fn restore(self, recorder: &mut super::fragment::Recorder<'_>) -> Option<Code> {
        if !self.sound(recorder.mark()) {
            return None;
        }
        let sig = recorder.make_signature();
        let imports: Vec<_> =
            self.imports.iter().map(|it| recorder.import(it, &sig).ok()).collect::<Option<_>>()?;
        let blobs: Vec<_> = self
            .blobs
            .into_iter()
            .map(|it| recorder.define_const(it).ok())
            .collect::<Option<_>>()?;
        let mut code = self.code.code();
        for reloc in &mut code.relocs {
            reloc.name = match reloc.name {
                ModuleRelocTarget::User { namespace: Self::IMPORTS, index } => {
                    ModuleRelocTarget::user(0, imports.get(index as usize)?.as_u32())
                }
                ModuleRelocTarget::User { namespace: Self::BLOBS, index } => {
                    ModuleRelocTarget::user(1, blobs.get(index as usize)?.as_u32())
                }
                ref other => other.clone(),
            };
        }
        Some(code)
    }

    pub(super) fn encode(&self) -> Option<Vec<u8>> {
        postcard::to_allocvec(self).ok()
    }

    pub(super) fn decode(bytes: &[u8]) -> Option<StoredCode> {
        postcard::from_bytes(bytes).ok()
    }

    /// Whether the code and the blobs refer only to what is kept with them and to what a
    /// function that had asked for `asked` before it was compiled could name.
    fn sound(&self, asked: super::fragment::Mark) -> bool {
        let code = self.code.sound(|namespace| match namespace {
            0 => asked.funcs,
            1 => asked.datas,
            Self::IMPORTS => self.imports.len(),
            Self::BLOBS => self.blobs.len(),
            _ => 0,
        });
        code && self.blobs.iter().all(|it| it.sound(asked.funcs, asked.datas))
    }
}

impl Entry {
    /// The key of the entry for `name`. The store tells the entries of this build of the
    /// compiler from another's.
    ///
    /// An entry kept with what a debugger is told is not the one kept without.
    ///
    /// `checks_overflow` is whether the crates cargo builds have `-C overflow-checks`, when
    /// they agree: code of the standard library that takes the setting of its callers has the
    /// same name in a build with the checks and in one without.
    pub(super) fn key(name: &str, debug: bool, checks_overflow: Option<bool>) -> Option<[u8; KEY]> {
        let mut hasher = Sha256::new();
        // Each with its length, so that no two lists of them are the same bytes.
        let mut field = |bytes: &[u8]| {
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        };
        field(b"rustd entry 6");
        // Code of one producer is not what another would have given.
        field(super::Backend::from_env().ok()?.to_string().as_bytes());
        // Nor is code for one target what another runs.
        field(super::Target::selected().ok()?.triple().as_bytes());
        field(&[debug as u8, checks_overflow.map_or(2, u8::from)]);
        field(name.as_bytes());
        Some(hasher.finalize().into())
    }

    pub(super) fn encode(&self) -> Option<Vec<u8>> {
        postcard::to_allocvec(self).ok()
    }

    /// The entry the bytes are of, when they are of one that can be stitched: everything
    /// it refers to by its place is in it.
    pub(super) fn decode(bytes: &[u8]) -> Option<Entry> {
        let entry: Entry = postcard::from_bytes(bytes).ok()?;
        entry.sound().then_some(entry)
    }

    fn sound(&self) -> bool {
        let funcs = self.funcs.len();
        // A data object points only to the ones before it.
        let datas = self.datas.iter().enumerate().all(|(before, data)| match data {
            CachedData::Blob { blob, .. } | CachedData::Vtable { blob, .. } => {
                blob.sound(funcs, before)
            }
            CachedData::Static { .. }
            | CachedData::OwnStatic(_)
            | CachedData::Location(_)
            | CachedData::FilePath(_) => true,
        });
        datas && self.code.sound(|namespace| if namespace == 0 { funcs } else { self.datas.len() })
    }

    /// Whether the program has everything of rustc's that the entry was linked against.
    pub(super) fn links(&self, defines: &dyn Fn(&str) -> bool) -> bool {
        let funcs = self.funcs.iter().all(|it| match &it.target {
            CachedTarget::Import { symbol, checked } => !checked || defines(symbol),
            CachedTarget::Named(_)
            | CachedTarget::Root(_)
            | CachedTarget::TypeId(_)
            | CachedTarget::Asm(_) => true,
        });
        let datas = self.datas.iter().all(|it| match it {
            CachedData::Static { symbol, .. } => defines(symbol),
            _ => true,
        });
        funcs && datas
    }

    fn named(&self) -> impl Iterator<Item = &str> {
        self.funcs.iter().filter_map(|it| match &it.target {
            CachedTarget::Named(name) => Some(name.as_str()),
            CachedTarget::Import { .. }
            | CachedTarget::Root(_)
            | CachedTarget::TypeId(_)
            | CachedTarget::Asm(_) => None,
        })
    }
}

/// Whether `bytes` have the name of the directory `place` in them.
pub(super) fn holds(bytes: &[u8], place: &str) -> bool {
    !place.is_empty() && bytes.windows(place.len()).any(|it| it == place.as_bytes())
}

impl Entry {
    /// The names of the files the entry tells of: in its panic locations, in what `file!()`
    /// is, and in what a debugger is told.
    fn files(&mut self) -> impl Iterator<Item = &mut String> {
        let named = self.datas.iter_mut().filter_map(|it| match it {
            CachedData::Location(Some((file, ..))) | CachedData::FilePath(file) => Some(file),
            _ => None,
        });
        named.chain(self.debug.iter_mut().flat_map(|it| &mut it.files))
    }

    fn of_cargo(&mut self) -> bool {
        self.files().any(|it| super::moved(it, super::CARGO_HOME, "").is_some())
    }

    /// Names the files under `cargo_home` as every machine does, on the way to the store.
    /// Not to be kept when a file is already named as one of those and is not: it would be
    /// read back as one.
    pub(super) fn leave(&mut self, cargo_home: Option<&str>) -> bool {
        if cargo_home == Some(super::CARGO_HOME) {
            return true;
        }
        if self.of_cargo() {
            return false;
        }
        let Some(home) = cargo_home else { return true };
        for file in self.files() {
            if let Some(there) = super::moved(file, home, super::CARGO_HOME) {
                *file = there;
            }
        }
        true
    }

    /// Names the files of cargo's directory as this machine does, on the way from the store.
    /// `None` when the entry has such files and this machine has no such directory.
    pub(super) fn arrived(mut self, cargo_home: Option<&str>) -> Option<Entry> {
        if cargo_home == Some(super::CARGO_HOME) || !self.of_cargo() {
            return Some(self);
        }
        let home = cargo_home?;
        for file in self.files() {
            if let Some(here) = super::moved(file, super::CARGO_HOME, home) {
                *file = here;
            }
        }
        Some(self)
    }
}

/// The entries of the store that one program was asked about.
pub(super) struct Entries {
    store: &'static Store,
    /// Whether the entries asked for are the ones with what a debugger is told.
    debug: bool,
    /// Whether the build checks arithmetic for overflow: see [`Entry::key`].
    checks_overflow: Option<bool>,
    /// Where cargo keeps the sources it fetched on this machine.
    cargo_home: Option<String>,
    known: Mutex<Known>,
}

#[derive(Default)]
struct Known {
    entries: FxHashMap<Arc<str>, Option<Arc<Entry>>>,
    /// Whether the program can be built from an entry and everything it calls.
    usable: FxHashMap<Arc<str>, bool>,
}

/// An entry on the way down from the one asked about.
struct Visit {
    name: Arc<str>,
    entry: Arc<Entry>,
    /// How many of the functions the entry refers to have been looked at.
    seen: usize,
    /// The highest visit that this one or one below it calls back into: until that one is
    /// settled, neither is this.
    leans_on: usize,
    /// Where in the list of unsettled entries those below this one start.
    below: usize,
}

impl Entries {
    pub(super) fn new(
        store: &'static Store,
        debug: bool,
        checks_overflow: Option<bool>,
        cargo_home: Option<String>,
    ) -> Entries {
        Entries { store, debug, checks_overflow, cargo_home, known: Mutex::default() }
    }

    /// Never use the entry again in this process: it did not fit the object it was read for.
    pub(super) fn ban(name: &str) {
        Self::banned().lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_owned());
    }

    fn banned() -> &'static Mutex<FxHashSet<String>> {
        static BANNED: OnceLock<Mutex<FxHashSet<String>>> = OnceLock::new();
        BANNED.get_or_init(Mutex::default)
    }

    fn known(&self) -> std::sync::MutexGuard<'_, Known> {
        self.known.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn get(&self, name: &str) -> Option<Arc<Entry>> {
        if let Some(known) = self.known().entries.get(name) {
            return known.clone();
        }
        let banned = Self::banned().lock().unwrap_or_else(|e| e.into_inner()).contains(name);
        let entry = (!banned)
            .then(|| {
                let key = Entry::key(name, self.debug, self.checks_overflow)?;
                Entry::decode(&self.store.get(&key)?)?.arrived(self.cargo_home.as_deref())
            })
            .flatten()
            .map(Arc::new);
        self.known().entries.insert(Arc::from(name), entry.clone());
        entry
    }

    fn settle(&self, name: Arc<str>, usable: bool) {
        self.known().usable.insert(name, usable);
    }

    /// Whether there is an entry for `name` and for everything it calls, and what they import
    /// is in the program, as `defines` tells.
    pub(super) fn usable(&self, name: &str, defines: &dyn Fn(&str) -> bool) -> bool {
        let settled = |name: &str| self.known().usable.get(name).copied();
        if let Some(usable) = settled(name) {
            return usable;
        }
        let name: Arc<str> = Arc::from(name);
        let Some(entry) = self.get(&name).filter(|it| it.links(defines)) else {
            self.settle(name, false);
            return false;
        };
        // Entries call each other in circles, so one is only known to be usable along with
        // the ones it leans on.
        let mut unsettled = vec![name.clone()];
        let mut leaning = FxHashMap::<Arc<str>, usize>::default();
        leaning.insert(name.clone(), 0);
        let mut path = vec![Visit { name, entry, seen: 0, leans_on: 0, below: 0 }];
        while !path.is_empty() {
            let depth = path.len() - 1;
            let visit = &mut path[depth];
            let entry = visit.entry.clone();
            let Some(callee) = entry.named().nth(visit.seen) else {
                let Some(done) = path.pop() else { break };
                if done.leans_on >= depth {
                    for name in unsettled.drain(done.below..) {
                        leaning.remove(&name);
                        self.settle(name, true);
                    }
                } else {
                    leaning.insert(done.name, done.leans_on);
                    let above = &mut path[depth - 1];
                    above.leans_on = above.leans_on.min(done.leans_on);
                }
                continue;
            };
            visit.seen += 1;
            if let Some(&on) = leaning.get(callee) {
                visit.leans_on = visit.leans_on.min(on);
                continue;
            }
            let found = match settled(callee) {
                Some(true) => continue,
                Some(false) => None,
                None => self.get(callee).filter(|it| it.links(defines)),
            };
            let callee: Arc<str> = Arc::from(callee);
            let Some(entry) = found else {
                self.settle(callee, false);
                for visit in path {
                    self.settle(visit.name, false);
                }
                return false;
            };
            leaning.insert(callee.clone(), depth + 1);
            unsettled.push(callee.clone());
            path.push(Visit {
                name: callee,
                entry,
                seen: 0,
                leans_on: depth + 1,
                below: unsettled.len() - 1,
            });
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use cranelift_codegen::{
        ir::{AbiParam, types},
        isa::CallConv,
    };
    use cranelift_module::{DataId, FuncId};

    use super::{
        super::{
            Resolved,
            debuginfo::{DebugType, Field, Place, VariableDebug},
            fuzz,
        },
        *,
    };

    fn signature() -> Signature {
        let mut signature = Signature::new(CallConv::SystemV);
        signature.params.push(AbiParam::new(types::I64));
        signature.returns.push(AbiParam::new(types::I32));
        signature
    }

    fn reloc(offset: u32, namespace: u32, index: u32) -> CachedReloc {
        let name = ModuleRelocTarget::user(namespace, index);
        CachedReloc { offset, kind: Reloc::X86CallPCRel4, name, addend: -4 }
    }

    fn code(relocs: Vec<CachedReloc>) -> CachedCode {
        let lines = vec![LineRow { offset: 0, span: 0 }, LineRow { offset: 8, span: 3 }];
        CachedCode {
            signature: signature(),
            bytes: vec![0x90; 32],
            align: 16,
            relocs,
            unwind: None,
            lines,
        }
    }

    fn blob(relocs: Vec<(u32, DataId, i64)>, fn_relocs: Vec<(u32, FuncId)>) -> ConstBlob {
        ConstBlob { bytes: vec![0; 24], relocs, fn_relocs, align: 8 }
    }

    /// A type `depth` deep, by way of each kind of type that has one inside.
    fn nested(depth: u32) -> DebugType {
        let inner = |depth| Box::new(nested(depth));
        let name = format!("T{depth}");
        match depth {
            0 | 1 => DebugType::Base { name, size: 4, encoding: 7 },
            _ if depth % 3 == 0 => DebugType::Pointer { name, to: Some(inner(depth - 1)) },
            _ if depth % 3 == 1 => DebugType::Array { of: inner(depth - 1), len: 2 },
            _ => {
                let field = Field { name: "inside".to_owned(), offset: 0, ty: nested(depth - 1) };
                DebugType::Struct { name, size: 8, fields: vec![field] }
            }
        }
    }

    fn debug(ty: DebugType) -> FunctionDebug {
        let mut debug =
            FunctionDebug::new("run".to_owned(), "_ZN4case3run17h0123456789abcdefE".to_owned());
        debug.files.push("src/lib.rs".to_owned());
        debug.declared = Some(Place { file: 0, line: 3, column: 1 });
        debug.rows.push((4, Place { file: 0, line: 4, column: 5 }));
        debug.variables.push(VariableDebug { name: "it".to_owned(), param: true, ty, offset: -16 });
        debug
    }

    /// An entry with one of everything an entry has.
    fn entry() -> Entry {
        let import = CachedTarget::Import { symbol: "memcpy".to_owned(), checked: true };
        let targets = [
            import,
            CachedTarget::Named("c:1f:case::helper".to_owned()),
            CachedTarget::Root("case::edited".to_owned()),
            CachedTarget::TypeId(7 << 100),
            CachedTarget::Asm("nop".to_owned()),
        ];
        let datas = vec![
            CachedData::Static { symbol: "ENVIRON".to_owned(), thread_local: false },
            CachedData::Blob { blob: blob(Vec::new(), Vec::new()), identity: DataIdentity::Shared },
            CachedData::Vtable {
                key: "dyn Run for Case".to_owned(),
                blob: blob(vec![(8, DataId::from_u32(1), 4)], vec![(16, FuncId::from_u32(4))]),
            },
            CachedData::OwnStatic("case::COUNT".to_owned()),
            CachedData::Location(Some(("src/lib.rs".to_owned(), 3, 9))),
            CachedData::FilePath("src/lib.rs".to_owned()),
        ];
        Entry {
            label: "case::run".to_owned(),
            funcs: targets
                .into_iter()
                .map(|target| CachedFunc { target, sig: signature() })
                .collect(),
            datas,
            code: code(vec![reloc(4, 0, 4), reloc(12, 1, 5)]),
            debug: Some(debug(nested(6))),
        }
    }

    /// An entry of a crate cargo fetched to `home`, with each kind of file name it can have.
    fn fetched_to(home: &str) -> Entry {
        let file =
            format!("{home}/registry/src/index.crates.io-1949cf8c6b5b557f/dep-0.1.0/src/lib.rs");
        let mut entry = entry();
        entry.datas.push(CachedData::Location(Some((file.clone(), 3, 9))));
        entry.datas.push(CachedData::FilePath(file.clone()));
        entry.debug.as_mut().unwrap().files.push(file);
        entry
    }

    #[test]
    fn a_fetched_file_is_stored_under_the_same_name_from_every_cargo_home() {
        let (one, other) = ("/home/one/.cargo", "/srv/another user/cargo");
        let mut stored = fetched_to(one);
        assert!(stored.leave(Some(one)));
        let mut theirs = fetched_to(other);
        assert!(theirs.leave(Some(other)));
        let bytes = stored.encode().unwrap();
        assert_eq!(bytes, theirs.encode().unwrap());
        assert!(!holds(&bytes, one) && !holds(&bytes, other));
        assert!(holds(&bytes, "/cargo/registry/src/"));
        // The files of the checkout are named as they were.
        assert!(stored.files().any(|it| it == "src/lib.rs"));
        let here = Entry::decode(&bytes).unwrap().arrived(Some(other)).unwrap();
        assert_eq!(here.encode().unwrap(), fetched_to(other).encode().unwrap());
    }

    #[test]
    fn a_fetched_file_is_not_taken_where_cargo_has_no_directory() {
        let mut stored = fetched_to("/home/one/.cargo");
        assert!(stored.leave(Some("/home/one/.cargo")));
        assert!(Entry::decode(&stored.encode().unwrap()).unwrap().arrived(None).is_none());
        // An entry without one is taken anywhere.
        assert!(entry().arrived(None).is_some());
    }

    #[test]
    fn a_directory_named_as_cargos_stands_in_is_not_mistaken_for_it() {
        // A checkout that really is under `/cargo` is not stored as if cargo fetched it.
        assert!(!fetched_to("/cargo").leave(Some("/home/one/.cargo")));
        assert!(!fetched_to("/cargo").leave(None));
        // A cargo home that is `/cargo` already names its files as every machine does.
        let mut same = fetched_to("/cargo");
        assert!(same.leave(Some("/cargo")));
        assert_eq!(same.encode().unwrap(), fetched_to("/cargo").encode().unwrap());
        // Only whole components are the home.
        let mut beside = fetched_to("/home/one/.cargo-old");
        assert!(beside.leave(Some("/home/one/.cargo")));
        assert!(beside.files().any(|it| it.starts_with("/home/one/.cargo-old/")));
    }

    /// Does with an entry what stitching it does with the places it names.
    fn follow(entry: &Entry) {
        let mut ids = Resolved { funcs: Vec::new(), datas: Vec::new() };
        ids.funcs.extend((0..entry.funcs.len() as u32).map(FuncId::from_u32));
        for (at, data) in entry.datas.iter().enumerate() {
            if let CachedData::Blob { blob, .. } | CachedData::Vtable { blob, .. } = data {
                ids.blob(blob);
            }
            ids.datas.push(DataId::from_u32(at as u32));
        }
        for reloc in &entry.code.code().relocs {
            ids.reloc(reloc, FuncId::from_u32(0));
        }
    }

    #[test]
    fn a_crate_is_named_by_all_of_its_hash_and_a_key_by_all_of_a_name() {
        let (mut one, mut other) = ([0x5a; 32], [0x5a; 32]);
        // The same in the 64 bits an id was once cut to.
        (one[31], other[31]) = (1, 2);
        let (one, other) = (super::super::ContentId(one), super::super::ContentId(other));
        assert_eq!(one.to_string().len(), 64);
        assert_ne!(one.to_string(), other.to_string());
        let key = |name: &str, debug, checked| Entry::key(name, debug, checked).unwrap();
        let named = |id: &super::super::ContentId| key(&format!("~{id}:run"), false, None);
        assert_ne!(named(&one), named(&other));
        let name = format!("~{one}:run");
        let keys = [
            key(&name, false, None),
            key(&name, true, None),
            key(&name, false, Some(false)),
            key(&name, false, Some(true)),
            key(&format!("{name}\0"), false, None),
        ];
        let distinct: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(distinct.len(), keys.len());
    }

    #[test]
    fn an_entry_is_read_back_as_it_was_written() {
        let written = entry();
        let read = Entry::decode(&written.encode().unwrap()).unwrap();
        assert_eq!(read, written);
        follow(&read);
    }

    #[test]
    fn an_entry_that_names_what_it_lacks_is_no_entry() {
        let broken: [fn(&mut Entry); 7] = [
            |it| it.code.relocs.push(reloc(0, 0, 5)),
            |it| it.code.relocs.push(reloc(0, 1, 6)),
            |it| it.code.relocs.push(reloc(32, 0, 0)),
            |it| it.code.align = 3,
            // A blob points to what is before it.
            |it| {
                it.datas[1] = CachedData::Blob {
                    blob: blob(vec![(0, DataId::from_u32(1), 0)], Vec::new()),
                    identity: DataIdentity::Own,
                }
            },
            |it| {
                it.datas[1] = CachedData::Blob {
                    blob: blob(Vec::new(), vec![(0, FuncId::from_u32(5))]),
                    identity: DataIdentity::Own,
                }
            },
            |it| {
                it.datas[1] = CachedData::Blob {
                    blob: blob(Vec::new(), vec![(24, FuncId::from_u32(0))]),
                    identity: DataIdentity::Own,
                }
            },
        ];
        for (which, change) in broken.iter().enumerate() {
            let mut entry = entry();
            change(&mut entry);
            assert!(Entry::decode(&entry.encode().unwrap()).is_none(), "{which}");
        }
    }

    #[test]
    fn a_type_inside_too_many_others_is_not_read_and_not_kept() {
        let deepest = DebugType::DEEPEST;
        let kept = Entry { debug: Some(debug(nested(deepest))), ..entry() };
        assert_eq!(Entry::decode(&kept.encode().unwrap()).unwrap(), kept);
        assert!(nested(deepest).kept().is_some());
        assert!(nested(deepest + 1).kept().is_none());
        // Deep enough to take all of a thread's stack, were it read.
        let thread = std::thread::Builder::new().stack_size(64 << 20);
        let deep =
            thread.spawn(|| Entry { debug: Some(debug(nested(40_000))), ..entry() }.encode());
        let deep = deep.unwrap().join().unwrap().unwrap();
        assert!(Entry::decode(&deep).is_none());
        // What was refused does not count against the next.
        assert!(Entry::decode(&kept.encode().unwrap()).is_some());
    }

    #[test]
    fn no_bytes_break_the_reading_of_an_entry() {
        let seeds = vec![
            entry().encode().unwrap(),
            Entry { debug: None, datas: Vec::new(), code: code(Vec::new()), ..entry() }
                .encode()
                .unwrap(),
        ];
        let (mut read, mut refused) = (0, 0);
        for bytes in fuzz::inputs("entry", seeds) {
            let (largest, entry) = fuzz::largest_allocation(|| Entry::decode(&bytes));
            assert!(largest <= fuzz::allowed(bytes.len()), "{largest} bytes for {}", bytes.len());
            match entry {
                Some(entry) => {
                    follow(&entry);
                    read += 1;
                }
                None => refused += 1,
            }
        }
        eprintln!("entry: {read} read, {refused} refused");
        assert!(read > 2 && refused > 0);
    }

    #[cfg(feature = "llvm")]
    fn stored(relocs: Vec<CachedReloc>) -> StoredCode {
        let table = blob(vec![(0, DataId::from_u32(1), 0)], vec![(8, FuncId::from_u32(2))]);
        StoredCode { code: code(relocs), imports: vec!["memset".to_owned()], blobs: vec![table] }
    }

    #[cfg(feature = "llvm")]
    #[test]
    fn code_of_llvm_that_names_what_was_not_asked_for_is_not_restored() {
        use super::super::fragment::Mark;
        let asked = Mark { funcs: 3, datas: 2 };
        let fits = [reloc(0, 0, 2), reloc(4, 1, 1), reloc(8, 2, 0), reloc(12, 3, 0)];
        assert!(stored(fits.into()).sound(asked));
        for outside in [
            reloc(0, 0, 3),
            reloc(0, 1, 2),
            reloc(0, 2, 1),
            reloc(0, 3, 1),
            reloc(0, 4, 0),
            reloc(32, 0, 0),
        ] {
            assert!(!stored(vec![outside]).sound(asked));
        }
        assert!(!stored(Vec::new()).sound(Mark { funcs: 2, datas: 2 }));
        assert!(!stored(Vec::new()).sound(Mark { funcs: 3, datas: 1 }));
    }

    #[cfg(feature = "llvm")]
    #[test]
    fn no_bytes_break_the_reading_of_code_of_llvm() {
        use super::super::fragment::Mark;
        let seeds = vec![stored(vec![reloc(0, 0, 2), reloc(8, 2, 0)]).encode().unwrap()];
        let (mut read, mut refused) = (0, 0);
        for bytes in fuzz::inputs("stored-code", seeds) {
            let (largest, stored) = fuzz::largest_allocation(|| StoredCode::decode(&bytes));
            assert!(largest <= fuzz::allowed(bytes.len()), "{largest} bytes for {}", bytes.len());
            match stored {
                Some(stored) => {
                    stored.sound(Mark { funcs: 3, datas: 2 });
                    read += 1;
                }
                None => refused += 1,
            }
        }
        eprintln!("stored code: {read} read, {refused} refused");
        assert!(read > 1 && refused > 0);
    }
}
