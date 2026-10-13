//! rustd spike: lower RA MIR to native code with Cranelift.
//!
//! Every local lives in memory. Calls follow rustc's (unstable) Rust ABI so the
//! output can be linked against rustc-compiled objects.

use std::fmt;

use base_db::Crate;
use cranelift_codegen::{
    ir::{
        self, AbiParam, ArgumentPurpose, AtomicRmwOp, InstBuilder, MemFlagsData, Signature,
        StackSlotData, StackSlotKind, TrapCode, Type, Value, condcodes::{FloatCC, IntCC}, types,
    },
    isa::{self, CallConv},
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, ModuleReloc, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use hir_def::{
    AdtId, DefWithBodyId, FunctionId, HasModule, ItemContainerId, Lookup, StaticId, TraitId,
    VariantId,
    attrs::AttrFlags,
    expr_store::Body,
    layout::{BackendRepr, Primitive, Scalar, TagEncoding, Variants},
    signatures::{FunctionSignature, StaticFlags, StaticSignature, StructFlags, StructSignature},
};
use la_arena::ArenaMap;
use rustc_abi::{Integer, Size};
use rustc_ast_ir::Mutability;
use rustc_hash::FxHashMap;
use rustc_index::Idx;
use rustc_type_ir::inherent::{
    AdtDef as _, BoundExistentialPredicates as _, GenericArgs as _, IntoKind, SliceLike, Ty as _,
};
use salsa::plumbing::{AsId, FromId};
use triomphe::Arc;

use crate::{
    CallableDefId, InferBodyId, InferenceResult, MemoryMap, ParamEnvAndCrate,
    consteval::try_const_usize,
    db::HirDatabase,
    db::{GeneralConstId, InternedClosureId},
    drop::{DropGlue, has_drop_glue},
    infer::PointerCast,
    layout::{Layout, RustcEnumVariantIdx},
    method_resolution::{is_dyn_method, lookup_impl_const},
    next_solver::{
        Allocation, AllocationData, ConstKind, DbInterner, GenericArgs, Region, StoredGenericArgs,
        StoredTy, TraitRef, Ty, TyKind, TypingMode, UnevaluatedConst, ValTree,
        infer::{DbInternerInferExt, InferCtxt},
    },
    utils::detect_variant_from_bytes,
};

use self::vtable::{Slot, Upcast, VtableLayout};
use super::{
    AggregateKind, BasicBlockId, BinOp, CastKind, LocalId, MirBody, Operand, OperandKind, PlaceTy,
    ProjectionElem, Rvalue, StatementKind, StoredPlace, TerminatorKind, UnOp, return_slot,
};

mod vtable;

#[derive(Debug)]
pub enum CodegenError {
    /// The function uses something this backend can't compile yet; fall back to rustc.
    Unsupported(String),
    /// MIR lowering or layout failed in RA.
    Frontend(String),
    Backend(String),
    /// rustc exports nothing for the function, so there is nothing to replace: only code in
    /// its own crate can call it.
    NoSymbol,
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodegenError::Unsupported(it) => write!(f, "unsupported: {it}"),
            CodegenError::Frontend(it) => write!(f, "frontend: {it}"),
            CodegenError::Backend(it) => write!(f, "backend: {it}"),
            CodegenError::NoSymbol => write!(f, "unsupported: no rustc symbol for function"),
        }
    }
}

type Result<T> = std::result::Result<T, CodegenError>;

macro_rules! unsupported {
    ($($arg:tt)*) => {
        return Err(CodegenError::Unsupported(format!($($arg)*)))
    };
}

/// Symbol names come from the rustc-built artifacts we link against.
pub trait SymbolResolver {
    fn function(&self, func: FunctionId) -> Option<String>;
    fn static_(&self, id: StaticId) -> Option<String>;
    /// A non-generic function by path, e.g. `core::panicking::panic_bounds_check`.
    fn path(&self, path: &str) -> Option<String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum OverflowOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Neg,
    Shl,
    Shr,
}

/// How a value crosses a call boundary in rustc's Rust ABI.
#[derive(Debug, Clone, Copy)]
enum PassMode {
    Ignore,
    Direct(AbiScalar),
    Pair(AbiScalar, AbiScalar, Size),
    /// Small aggregate passed as one integer register.
    Cast(Type),
    Indirect,
}

#[derive(Debug, Clone, Copy)]
struct AbiScalar {
    ty: Type,
    signed: bool,
}

impl AbiScalar {
    fn param(self) -> AbiParam {
        let param = AbiParam::new(self.ty);
        match (self.ty.is_int() && self.ty.bits() < 32, self.signed) {
            (false, _) => param,
            (true, true) => param.sext(),
            (true, false) => param.uext(),
        }
    }
}

/// The result of compiling a set of root functions into one object file.
pub struct CodegenUnit {
    pub object: Vec<u8>,
    /// Roots that were compiled, with their exported symbol.
    pub compiled: Vec<(FunctionId, String)>,
    /// Roots that have to come from rustc.
    pub skipped: Vec<(FunctionId, CodegenError)>,
    /// Callees without a rustc symbol (generic or inlined) that were compiled as local functions.
    pub instances: usize,
}

impl CodegenUnit {
    /// Compiles every root it can, together with the instances those roots call.
    ///
    /// A callee that can't be compiled poisons its callers, so codegen restarts with that callee
    /// rejected up front until a round finishes without a new rejection.
    pub fn compile(
        db: &dyn HirDatabase,
        symbols: &dyn SymbolResolver,
        roots: &[FunctionId],
        known: &mut Rejections,
    ) -> Result<CodegenUnit> {
        // FIXME: a rejection outlives an edit that would lift it. Dropping them here costs one
        // full pass per rejected call chain, and they go away with the rustc fallback.
        let Rejections { rejected, fingerprints, code, stats } = known;
        (code.hits, code.misses) = (0, 0);
        let track = std::env::var_os("RUSTD_STATS").is_some();
        let mut own_statics = roots.first().map(|root| root.krate(db));
        loop {
            let mut cg = ObjectCodegen::new(db, symbols, rejected, code, own_statics)?;
            cg.track = track;
            let mut compiled = Vec::new();
            let mut skipped = Vec::new();
            for &root in roots {
                match cg.define_function(root) {
                    Ok(symbol) => compiled.push((root, symbol)),
                    Err(e) => skipped.push((root, e)),
                }
            }
            let failed = cg.define_instances();
            let from_rustc = skipped.iter().any(|(_, e)| !matches!(e, CodegenError::NoSymbol));
            if !cg.statics.is_empty() && from_rustc {
                // Functions left to rustc keep using rustc's copy of the crate's private
                // statics, so we can't bring our own.
                own_statics = None;
                rejected.extend(failed);
                continue;
            }
            if failed.is_empty() {
                let instances = cg.instances.len();
                *stats = BuildStats {
                    mir: cg.mir_time,
                    bodies: cg.seen.len(),
                    changed: 0,
                    reused: cg.code.hits,
                    compiled: cg.code.misses,
                };
                for (key, print) in cg.seen.drain(..) {
                    if fingerprints.insert(key, print) != Some(print) {
                        stats.changed += 1;
                    }
                }
                return Ok(CodegenUnit { object: cg.finish()?, compiled, skipped, instances });
            }
            rejected.extend(failed);
        }
    }
}

/// Instances already found impossible to compile, kept between builds so a warm build does
/// not rediscover them one restart at a time.
#[derive(Default)]
pub struct Rejections {
    rejected: FxHashMap<Instance, String>,
    /// A hash of each body's MIR in the last build, to count how many an edit changed.
    fingerprints: FxHashMap<Instance, u64>,
    code: CodeCache,
    pub stats: BuildStats,
}

/// Machine code by a hash of the Cranelift IR it was compiled from. The IR holds no source
/// positions and names its callees indirectly, so moving or renaming code does not miss.
#[derive(Default)]
struct CodeCache {
    entries: FxHashMap<Box<[u8]>, Vec<u8>>,
    hits: usize,
    misses: usize,
}

impl cranelift_codegen::incremental_cache::CacheKvStore for CodeCache {
    fn get(&self, key: &[u8]) -> Option<std::borrow::Cow<'_, [u8]>> {
        self.entries.get(key).map(|blob| blob.as_slice().into())
    }

    fn insert(&mut self, key: &[u8], blob: Vec<u8>) {
        self.entries.insert(key.into(), blob);
    }
}

/// Measurements of the last build, collected when `RUSTD_STATS` is set.
#[derive(Default, Clone, Copy)]
pub struct BuildStats {
    /// Time spent in the queries that return MIR.
    pub mir: std::time::Duration,
    pub bodies: usize,
    /// Bodies whose MIR differs from the build before.
    pub changed: usize,
    /// Functions whose machine code was reused, and functions that went through Cranelift.
    pub reused: usize,
    pub compiled: usize,
}

/// A function with concrete generic arguments that we compile ourselves.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Instance {
    def: InstanceDef,
    args: StoredGenericArgs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum InstanceDef {
    Fn(FunctionId),
    /// An `InternedClosureId`, stored without its database lifetime.
    Closure(salsa::Id),
    /// `drop_in_place` for the type that is the instance's only generic argument.
    DropGlue,
    /// `FnOnce::call_once` in the vtable of the closure type that is the instance's only
    /// generic argument: calls the closure through the pointer it is handed, then drops it.
    CallOnceShim,
}

/// What a call transfers control to.
enum Callee<'db> {
    Instance(InstanceDef, GenericArgs<'db>),
    Pointer(Value),
}

struct ObjectCodegen<'a, 'db> {
    db: &'db dyn HirDatabase,
    symbols: &'a dyn SymbolResolver,
    module: ObjectModule,
    imports: FxHashMap<String, FuncId>,
    instances: FxHashMap<Instance, FuncId>,
    pending: Vec<(Instance, ParamEnvAndCrate<'db>, FuncId)>,
    /// Instances that failed in an earlier round, with the reason.
    rejected: &'a FxHashMap<Instance, String>,
    code: &'a mut CodeCache,
    track: bool,
    mir_time: std::time::Duration,
    seen: Vec<(Instance, u64)>,
    vtables: FxHashMap<(StoredTy, Option<(TraitId, StoredGenericArgs)>), DataId>,
    /// The crate whose writable statics we define ourselves when rustc exports no symbol for
    /// them. Only sound when every function of that crate is ours.
    own_statics: Option<Crate>,
    statics: FxHashMap<StaticId, DataId>,
    builder_ctx: FunctionBuilderContext,
    location_counter: u32,
}

impl<'a, 'db> ObjectCodegen<'a, 'db> {
    fn new(
        db: &'db dyn HirDatabase,
        symbols: &'a dyn SymbolResolver,
        rejected: &'a FxHashMap<Instance, String>,
        code: &'a mut CodeCache,
        own_statics: Option<Crate>,
    ) -> Result<Self> {
        let backend = |e: &dyn fmt::Display| CodegenError::Backend(e.to_string());
        let mut flags = settings::builder();
        flags.set("is_pic", "true").map_err(|e| backend(&e))?;
        flags.set("opt_level", "none").map_err(|e| backend(&e))?;
        flags.set("enable_llvm_abi_extensions", "true").map_err(|e| backend(&e))?;
        let isa = isa::lookup_by_name("x86_64-unknown-linux-gnu")
            .map_err(|e| backend(&e))?
            .finish(settings::Flags::new(flags))
            .map_err(|e| backend(&e))?;
        let builder =
            ObjectBuilder::new(isa, "rustd", default_libcall_names()).map_err(|e| backend(&e))?;
        Ok(ObjectCodegen {
            db,
            symbols,
            module: ObjectModule::new(builder),
            imports: FxHashMap::default(),
            instances: FxHashMap::default(),
            pending: Vec::new(),
            rejected,
            code,
            track: false,
            mir_time: Default::default(),
            seen: Vec::new(),
            vtables: FxHashMap::default(),
            own_statics,
            statics: FxHashMap::default(),
            builder_ctx: FunctionBuilderContext::new(),
            location_counter: 0,
        })
    }

    fn finish(self) -> Result<Vec<u8>> {
        self.module.finish().emit().map_err(|e| CodegenError::Backend(e.to_string()))
    }

    /// Compiles a non-generic function and exports it under its rustc symbol name.
    fn define_function(&mut self, func: FunctionId) -> Result<String> {
        let db = self.db;
        let owner: InferBodyId<'db> = func.into();
        let start = std::time::Instant::now();
        let body = db.mir_body(owner);
        self.mir_time += start.elapsed();
        let body = body.map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
        let root = Instance {
            def: InstanceDef::Fn(func),
            args: GenericArgs::new_from_slice(&[]).store(),
        };
        self.fingerprint(root, body);
        let Some(symbol) = self.symbols.function(func) else {
            return Err(CodegenError::NoSymbol);
        };
        let krate = owner.module(db).krate(db);
        let env =
            ParamEnvAndCrate { param_env: db.trait_environment(owner.generic_def(db)), krate };
        if std::env::var("RUSTD_DUMP_MIR")
            .is_ok_and(|want| FunctionSignature::of(db, func).name.as_str() == want)
        {
            eprintln!("{:#?}", body.basic_blocks);
        }
        let mut ctx = self.compile_body(body, env)?;
        let id = self
            .module
            .declare_function(&symbol, Linkage::Export, &ctx.func.signature)
            .map_err(|e| CodegenError::Backend(e.to_string()))?;
        self.emit(id, &mut ctx)?;
        Ok(symbol)
    }

    fn fingerprint(&mut self, instance: Instance, body: &MirBody<'db>) {
        if !self.track {
            return;
        }
        use std::hash::{Hash, Hasher};
        let mut hasher = rustc_hash::FxHasher::default();
        format!("{:?}{:?}", body.basic_blocks, body.locals).hash(&mut hasher);
        self.seen.push((instance, hasher.finish()));
    }

    /// Compiles the instances requested by calls. Returns the ones that failed.
    fn define_instances(&mut self) -> Vec<(Instance, String)> {
        let mut failed = Vec::new();
        while let Some((instance, env, id)) = self.pending.pop() {
            if let Err(e) = self.define_instance(&instance, env, id) {
                failed.push((instance, e.to_string()));
            }
        }
        failed
    }

    fn define_instance(
        &mut self,
        instance: &Instance,
        env: ParamEnvAndCrate<'db>,
        id: FuncId,
    ) -> Result<()> {
        if let InstanceDef::DropGlue = instance.def {
            let ty = instance.args.as_ref().type_at(0);
            let mut ctx = self.build(env, None, |fx| fx.drop_glue(ty))?;
            return self.emit(id, &mut ctx);
        }
        if let InstanceDef::CallOnceShim = instance.def {
            let ty = instance.args.as_ref().type_at(0);
            let mut ctx = self.build(env, None, |fx| fx.call_once_shim(ty))?;
            return self.emit(id, &mut ctx);
        }
        let start = std::time::Instant::now();
        let body = match instance.def {
            InstanceDef::DropGlue | InstanceDef::CallOnceShim => unreachable!(),
            InstanceDef::Fn(func) => {
                self.db.monomorphized_mir_body(func.into(), instance.args.clone(), env.store())
            }
            InstanceDef::Closure(id) => self.db.monomorphized_mir_body_for_closure(
                InternedClosureId::from_id(id),
                instance.args.clone(),
                env.store(),
            ),
        };
        self.mir_time += start.elapsed();
        let body = body.map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
        self.fingerprint(instance.clone(), body);
        if let (Ok(want), InstanceDef::Fn(func)) = (std::env::var("RUSTD_DUMP_MIR"), instance.def)
            && FunctionSignature::of(self.db, func).name.as_str() == want
        {
            eprintln!("{:#?}", body.basic_blocks);
        }
        let mut ctx = self.compile_body(body, env)?;
        // The call site declared the instance with the signature it computed from its operands.
        let declared = &self.module.declarations().get_function_decl(id).signature;
        if *declared != ctx.func.signature {
            unsupported!(
                "call site and body disagree on the signature: {declared} and {}",
                ctx.func.signature
            );
        }
        self.emit(id, &mut ctx)
    }

    /// Runs Cranelift on a finished function. Cranelift panics on IR it has no lowering for
    /// (i128 corners, for one); that must cost us one function, not the process.
    fn emit(&mut self, id: FuncId, ctx: &mut cranelift_codegen::Context) -> Result<()> {
        let (module, code) = (&mut self.module, &mut *self.code);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (_, hit) = ctx
                .compile_with_cache(module.isa(), code, &mut Default::default())
                .map_err(|e| format!("{:?}", e.inner))?;
            let Some(compiled) = ctx.compiled_code() else {
                return Err("no compiled code".to_owned());
            };
            let relocs: Vec<_> = compiled
                .buffer
                .relocs()
                .iter()
                .map(|reloc| ModuleReloc::from_mach_reloc(reloc, &ctx.func, id))
                .collect();
            module
                .define_function_bytes(
                    id,
                    compiled.buffer.alignment.into(),
                    compiled.buffer.data(),
                    &relocs,
                )
                .map_err(|e| format!("{e:?}"))?;
            Ok::<_, String>(hit)
        }));
        match outcome {
            Ok(Ok(hit)) => {
                if hit {
                    self.code.hits += 1;
                } else {
                    self.code.misses += 1;
                }
                Ok(())
            }
            Ok(Err(e)) => Err(CodegenError::Backend(e)),
            Err(panic) => {
                let message = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("unknown");
                Err(CodegenError::Backend(format!("backend panic: {message}")))
            }
        }
    }

    fn compile_body(
        &mut self,
        body: &'db MirBody<'db>,
        env: ParamEnvAndCrate<'db>,
    ) -> Result<cranelift_codegen::Context> {
        self.build(env, Some(body), |fx| fx.codegen())
    }

    fn build(
        &mut self,
        env: ParamEnvAndCrate<'db>,
        body: Option<&'db MirBody<'db>>,
        fill: impl for<'cx> FnOnce(&mut FunctionCx<'cx, 'a, 'db>) -> Result<()>,
    ) -> Result<cranelift_codegen::Context> {
        let interner = DbInterner::new_with(self.db, env.krate);
        let mut ctx = self.module.make_context();
        let mut builder_ctx = std::mem::take(&mut self.builder_ctx);
        let result = {
            let mut fx = FunctionCx {
                cg: self,
                body,
                env,
                infcx: interner.infer_ctxt().build(TypingMode::PostAnalysis),
                interner,
                builder: FunctionBuilder::new(&mut ctx.func, &mut builder_ctx),
                locals: ArenaMap::new(),
                blocks: ArenaMap::new(),
                drop_flags: ArenaMap::new(),
                ret_mode: PassMode::Ignore,
            };
            fill(&mut fx).map(|()| {
                fx.builder.seal_all_blocks();
                let config = fx.cg.module.target_config();
                fx.builder.finalize(config);
            })
        };
        // A failed function leaves the builder context dirty.
        self.builder_ctx = if result.is_ok() { builder_ctx } else { FunctionBuilderContext::new() };
        result?;
        Ok(ctx)
    }

    /// The function to call for `func` instantiated with `args`: the rustc-compiled one when it
    /// has a symbol we can link against, otherwise an instance compiled into this object.
    fn callee(
        &mut self,
        def: InstanceDef,
        args: GenericArgs<'db>,
        env: ParamEnvAndCrate<'db>,
        sig: &Signature,
    ) -> Result<FuncId> {
        if let InstanceDef::Fn(func) = def
            && args.is_empty()
            && let Some(symbol) = self.symbols.function(func)
        {
            return self.import(&symbol, sig);
        }
        let instance = Instance { def, args: args.store() };
        let signature = match def {
            InstanceDef::Fn(func) => Some(FunctionSignature::of(self.db, func)),
            InstanceDef::Closure(_) | InstanceDef::DropGlue | InstanceDef::CallOnceShim => None,
        };
        let name = match def {
            InstanceDef::Fn(_) => signature.map_or("", |it| it.name.as_str()),
            InstanceDef::Closure(_) => "{closure}",
            InstanceDef::DropGlue => "drop_in_place",
            InstanceDef::CallOnceShim => "call_once",
        };
        if let Some(reason) = self.rejected.get(&instance) {
            unsupported!("callee `{name}`: {reason}");
        }
        if let Some(&id) = self.instances.get(&instance) {
            return Ok(id);
        }
        if signature.is_some_and(|it| !it.has_body()) {
            // The allocator entry points are declared in `alloc` and defined at link time,
            // under names in a crate of their own.
            if let Some(symbol) = self.symbols.path(&format!("__rustc::{name}")) {
                return self.import(&symbol, sig);
            }
            // FIXME: this passes arguments the way Rust functions take them, which is only
            // what a foreign function expects for scalars and pointers.
            if let InstanceDef::Fn(func) = def
                && let Some(symbol) = self.link_name(func)
                && !symbol.starts_with("llvm.")
            {
                return self.import(&symbol, sig);
            }
            unsupported!("call to `{name}`, which has no body and no rustc symbol");
        }
        let symbol = format!("rustd::{name}::{}", self.instances.len());
        let id = self
            .module
            .declare_function(&symbol, Linkage::Local, sig)
            .map_err(|e| CodegenError::Backend(e.to_string()))?;
        self.instances.insert(instance.clone(), id);
        self.pending.push((instance, env, id));
        Ok(id)
    }

    /// The symbol a function declared in an `extern` block stands for.
    fn link_name(&self, func: FunctionId) -> Option<String> {
        let ItemContainerId::ExternBlockId(_) = func.lookup(self.db).container else {
            return None;
        };
        let name = AttrFlags::link_name(self.db, func.into())
            .unwrap_or_else(|| FunctionSignature::of(self.db, func).name.symbol().clone());
        Some(name.as_str().to_owned())
    }

    fn define_const(&mut self, blob: ConstBlob) -> Result<DataId> {
        self.define_data(blob, false)
    }

    fn define_data(&mut self, blob: ConstBlob, writable: bool) -> Result<DataId> {
        let backend = |e: cranelift_module::ModuleError| CodegenError::Backend(e.to_string());
        let n = self.location_counter;
        self.location_counter += 1;
        let id = self
            .module
            .declare_data(&format!(".Lrustd.const.{n}"), Linkage::Local, writable, false)
            .map_err(backend)?;
        let mut data = DataDescription::new();
        data.define(blob.bytes.into_boxed_slice());
        data.set_align(blob.align);
        for (offset, target) in blob.relocs {
            let target = self.module.declare_data_in_data(target, &mut data);
            data.write_data_addr(offset, target, 0);
        }
        for (offset, target) in blob.fn_relocs {
            let target = self.module.declare_func_in_data(target, &mut data);
            data.write_function_addr(offset, target);
        }
        self.module.define_data(id, &data).map_err(backend)?;
        Ok(id)
    }

    fn import(&mut self, symbol: &str, sig: &Signature) -> Result<FuncId> {
        if let Some(&id) = self.imports.get(symbol) {
            return Ok(id);
        }
        let id = self
            .module
            .declare_function(symbol, Linkage::Import, sig)
            .map_err(|e| CodegenError::Backend(e.to_string()))?;
        self.imports.insert(symbol.to_owned(), id);
        Ok(id)
    }
}

/// What a call to an intrinsic turned into.
enum Intrinsic {
    /// Lowered inline; continue at the call's target.
    Inline,
    /// Lowered to a trap; the block is already terminated.
    Diverges,
    /// Not known here; call it like a normal function.
    Unknown,
}

/// Read-only data whose pointers refer to other data objects.
struct ConstBlob {
    bytes: Vec<u8>,
    relocs: Vec<(u32, DataId)>,
    fn_relocs: Vec<(u32, FuncId)>,
    align: u64,
}

impl ConstBlob {
    fn usize_at(&self, offset: usize) -> Option<usize> {
        let bytes = self.bytes.get(offset..offset + 8)?;
        Some(u64::from_le_bytes(bytes.try_into().ok()?) as usize)
    }
}

/// A place in memory. `meta` is the slice length (or vtable) when the place came from a fat
/// pointer.
#[derive(Clone, Copy)]
struct PlaceRef<'db> {
    addr: Value,
    meta: Option<Value>,
    ty: PlaceTy<'db>,
}

#[derive(Clone, Copy)]
struct LocalSlot {
    addr: Value,
}

struct FunctionCx<'cx, 'a, 'db> {
    cg: &'cx mut ObjectCodegen<'a, 'db>,
    /// `None` while generating a shim that has no MIR.
    body: Option<&'db MirBody<'db>>,
    env: ParamEnvAndCrate<'db>,
    infcx: InferCtxt<'db>,
    interner: DbInterner<'db>,
    builder: FunctionBuilder<'cx>,
    locals: ArenaMap<LocalId, LocalSlot>,
    blocks: ArenaMap<BasicBlockId, ir::Block>,
    /// One byte per local with drop glue: whether it currently holds a value. RA's MIR has no
    /// drop elaboration, so `Drop` terminators are conditional on these.
    drop_flags: ArenaMap<LocalId, Value>,
    ret_mode: PassMode,
}

fn is_unsized(ty: Ty<'_>) -> bool {
    matches!(ty.kind(), TyKind::Slice(_) | TyKind::Str | TyKind::Dynamic(..))
}

const PTR: Type = types::I64;
/// Field accesses can be unaligned (packed structs, chunked copies).
const MEM: MemFlagsData = MemFlagsData::new().with_notrap();

impl<'cx, 'a, 'db> FunctionCx<'cx, 'a, 'db> {
    fn layout(&self, ty: Ty<'db>) -> Result<Arc<Layout>> {
        self.cg
            .db
            .layout_of_ty(ty.store(), self.env.store())
            .map_err(|e| CodegenError::Frontend(format!("layout of {ty:?}: {e:?}")))
    }

    fn scalar(&self, scalar: Scalar) -> Result<AbiScalar> {
        Ok(match scalar.primitive() {
            Primitive::Int(int, signed) => AbiScalar {
                ty: match int {
                    Integer::I8 => types::I8,
                    Integer::I16 => types::I16,
                    Integer::I32 => types::I32,
                    Integer::I64 => types::I64,
                    Integer::I128 => types::I128,
                },
                signed,
            },
            Primitive::Pointer(_) => AbiScalar { ty: PTR, signed: false },
            Primitive::Float(rustc_abi::Float::F32) => AbiScalar { ty: types::F32, signed: true },
            Primitive::Float(rustc_abi::Float::F64) => AbiScalar { ty: types::F64, signed: true },
            Primitive::Float(_) => unsupported!("f16 and f128"),
        })
    }

    fn scalar_of(&self, ty: Ty<'db>) -> Result<AbiScalar> {
        match self.layout(ty)?.backend_repr {
            BackendRepr::Scalar(scalar) => self.scalar(scalar),
            _ => unsupported!("expected scalar, got {ty:?}"),
        }
    }

    fn pass_mode(&self, layout: &Layout, is_ret: bool) -> Result<PassMode> {
        if layout.is_zst() {
            return Ok(PassMode::Ignore);
        }
        Ok(match layout.backend_repr {
            BackendRepr::Scalar(scalar) => PassMode::Direct(self.scalar(scalar)?),
            BackendRepr::ScalarPair(a, b) => {
                let offset = a.size(&self.dl()).align_to(b.align(&self.dl()).abi);
                let (a, b) = (self.scalar(a)?, self.scalar(b)?);
                if is_ret && (a.ty == types::I128 || b.ty == types::I128) {
                    // More than the two return registers hold, so LLVM returns it in memory.
                    PassMode::Indirect
                } else {
                    PassMode::Pair(a, b, offset)
                }
            }
            BackendRepr::Memory { sized: true } => {
                let max = 8;
                match layout.size.bytes() {
                    size if size > max => PassMode::Indirect,
                    1 => PassMode::Cast(types::I8),
                    2 => PassMode::Cast(types::I16),
                    // An integer of exactly the aggregate's size; the bits above it are undefined.
                    3 | 4 => PassMode::Cast(types::I32),
                    _ => PassMode::Cast(types::I64),
                }
            }
            // rustc passes vectors in memory so that callers and callees built with different
            // target features agree.
            BackendRepr::SimdVector { .. } => PassMode::Indirect,
            _ => unsupported!("abi of {:?}", layout.backend_repr),
        })
    }

    fn dl(&self) -> &'db rustc_abi::TargetDataLayout {
        self.cg.db.target_data_layout_or_default(self.env.krate)
    }

    fn push_abi_params(&self, mode: PassMode, params: &mut Vec<AbiParam>) {
        match mode {
            PassMode::Ignore => (),
            PassMode::Direct(s) => params.push(s.param()),
            PassMode::Pair(a, b, _) => params.extend([a.param(), b.param()]),
            PassMode::Cast(ty) => params.push(AbiParam::new(ty)),
            PassMode::Indirect => params.push(AbiParam::new(PTR)),
        }
    }

    fn signature(&self, args: &[PassMode], ret: PassMode) -> Signature {
        let mut sig = Signature::new(CallConv::SystemV);
        match ret {
            PassMode::Indirect => {
                sig.params.push(AbiParam::special(PTR, ArgumentPurpose::StructReturn))
            }
            mode => self.push_abi_params(mode, &mut sig.returns),
        }
        for &arg in args {
            self.push_abi_params(arg, &mut sig.params);
        }
        sig
    }

    /// Loads the register image of a `PassMode::Cast` aggregate of `size` bytes.
    fn load_cast(&mut self, ty: Type, addr: Value, size: Size) -> Value {
        if u64::from(ty.bytes()) == size.bytes() {
            return self.builder.ins().load(ty, MEM, addr, 0);
        }
        let tmp = self.scratch();
        self.copy(tmp, addr, size);
        self.builder.ins().load(ty, MEM, tmp, 0)
    }

    fn store_cast(&mut self, value: Value, addr: Value, size: Size) {
        let ty = self.builder.func.dfg.value_type(value);
        if u64::from(ty.bytes()) == size.bytes() {
            self.builder.ins().store(MEM, value, addr, 0);
            return;
        }
        let tmp = self.scratch();
        self.builder.ins().store(MEM, value, tmp, 0);
        self.copy(addr, tmp, size);
    }

    /// Eight bytes of stack.
    fn scratch(&mut self) -> Value {
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            8,
            3,
        ));
        self.builder.ins().stack_addr(PTR, slot, 0)
    }

    fn stack_slot(&mut self, layout: &Layout) -> Value {
        let (size, align) = (layout.size.bytes() as u32, layout.align.abi.bytes());
        if align <= 16 {
            let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                size,
                align.trailing_zeros() as u8,
            ));
            return self.builder.ins().stack_addr(PTR, slot, 0);
        }
        // Cranelift keeps the frame 16-byte aligned and no more, so over-allocate and round up.
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size + align as u32,
            4,
        ));
        let base = self.builder.ins().stack_addr(PTR, slot, 0);
        let bumped = self.builder.ins().iadd_imm(base, align as i64 - 1);
        self.builder.ins().band_imm(bumped, -(align as i64))
    }

    /// The signature a MIR body is compiled with.
    fn body_signature(&self, body: &MirBody<'db>) -> Result<Signature> {
        let ret_layout = self.layout(body.locals[return_slot()].ty.as_ref())?;
        let ret_mode = self.pass_mode(&ret_layout, true)?;
        let mut param_modes = Vec::with_capacity(body.param_locals.len());
        for &local in &body.param_locals {
            let layout = self.layout(body.locals[local].ty.as_ref())?;
            param_modes.push(self.pass_mode(&layout, false)?);
        }
        Ok(self.signature(&param_modes, ret_mode))
    }

    fn codegen(&mut self) -> Result<()> {
        let body = self.body.expect("MIR body");
        let ret_layout = self.layout(body.locals[return_slot()].ty.as_ref())?;
        self.ret_mode = self.pass_mode(&ret_layout, true)?;
        let param_layouts = body
            .param_locals
            .iter()
            .map(|&l| self.layout(body.locals[l].ty.as_ref()))
            .collect::<Result<Vec<_>>>()?;
        let param_modes =
            param_layouts.iter().map(|l| self.pass_mode(l, false)).collect::<Result<Vec<_>>>()?;
        self.builder.func.signature = self.body_signature(body)?;

        let entry = self.builder.create_block();
        self.builder.append_block_params_for_function_params(entry);
        self.builder.switch_to_block(entry);
        let mut incoming = self.builder.block_params(entry).to_vec().into_iter();

        if let PassMode::Indirect = self.ret_mode {
            let addr = incoming.next().expect("sret param");
            self.locals.insert(return_slot(), LocalSlot { addr });
        }
        for ((&local, layout), mode) in
            body.param_locals.iter().zip(&param_layouts).zip(param_modes)
        {
            let addr = match mode {
                PassMode::Indirect => incoming.next().expect("indirect param"),
                _ => self.stack_slot(layout),
            };
            match mode {
                PassMode::Ignore | PassMode::Indirect => (),
                PassMode::Direct(_) => {
                    let value = incoming.next().expect("direct param");
                    self.builder.ins().store(MEM, value, addr, 0);
                }
                PassMode::Cast(_) => {
                    let value = incoming.next().expect("cast param");
                    self.store_cast(value, addr, layout.size);
                }
                PassMode::Pair(_, _, offset) => {
                    let a = incoming.next().expect("pair param");
                    let b = incoming.next().expect("pair param");
                    self.builder.ins().store(MEM, a, addr, 0);
                    self.builder.ins().store(MEM, b, addr, offset.bytes() as i32);
                }
            }
            self.locals.insert(local, LocalSlot { addr });
        }
        for (local, data) in body.locals.iter() {
            if self.locals.contains_idx(local) {
                continue;
            }
            let layout = self.layout(data.ty.as_ref())?;
            let addr = self.stack_slot(&layout);
            self.locals.insert(local, LocalSlot { addr });
        }
        for (local, data) in body.locals.iter() {
            let ty = data.ty.as_ref();
            if local == return_slot()
                || has_drop_glue(&self.infcx, ty, self.env.param_env) == DropGlue::None
            {
                continue;
            }
            let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                1,
                0,
            ));
            let flag = self.builder.ins().stack_addr(PTR, slot, 0);
            let live =
                self.builder.ins().iconst(types::I8, body.param_locals.contains(&local) as i64);
            self.builder.ins().store(MEM, live, flag, 0);
            self.drop_flags.insert(local, flag);
        }
        for (id, _) in body.basic_blocks.iter() {
            let block = self.builder.create_block();
            self.blocks.insert(id, block);
        }
        self.builder.ins().jump(self.blocks[body.start_block], &[]);

        // Only blocks that can run are compiled: `if cfg!(miri) { .. }` calls functions that do
        // not exist.
        let mut reachable = rustc_hash::FxHashSet::default();
        let mut worklist = vec![body.start_block];
        while let Some(id) = worklist.pop() {
            if !reachable.insert(id) {
                continue;
            }
            match body.basic_blocks[id].terminator.as_ref().map(|it| &it.kind) {
                Some(TerminatorKind::SwitchInt { discr, targets }) => match self.const_bits(discr) {
                    Some(bits) => worklist.push(targets.target_for_value(bits)),
                    None => worklist.extend_from_slice(targets.all_targets()),
                },
                Some(
                    TerminatorKind::Goto { target }
                    | TerminatorKind::Drop { target, .. }
                    | TerminatorKind::DropAndReplace { target, .. }
                    | TerminatorKind::Assert { target, .. }
                    | TerminatorKind::FalseEdge { real_target: target, .. }
                    | TerminatorKind::FalseUnwind { real_target: target, .. },
                ) => worklist.push(*target),
                Some(TerminatorKind::Call { target, .. }) => worklist.extend(*target),
                _ => (),
            }
        }
        for (id, block) in body.basic_blocks.iter() {
            if !reachable.contains(&id) {
                continue;
            }
            self.builder.switch_to_block(self.blocks[id]);
            for statement in &block.statements {
                match &statement.kind {
                    StatementKind::Assign(place, rvalue) => {
                        self.assign(place, rvalue)?;
                        self.set_drop_flag(place, true);
                    }
                    StatementKind::Deinit(_) => unsupported!("deinit"),
                    StatementKind::StorageLive(_)
                    | StatementKind::StorageDead(_)
                    | StatementKind::FakeRead(_)
                    | StatementKind::Nop => (),
                }
            }
            let Some(terminator) = &block.terminator else {
                unsupported!("block without terminator");
            };
            self.terminator(&terminator.kind)?;
        }
        Ok(())
    }

    /// The value of an integer, `bool` or `char` literal operand.
    fn const_bits(&self, operand: &Operand) -> Option<u128> {
        let OperandKind::Constant { konst, ty } = &operand.kind else { return None };
        let ty = ty.as_ref();
        if !matches!(ty.kind(), TyKind::Bool | TyKind::Char | TyKind::Int(_) | TyKind::Uint(_)) {
            return None;
        }
        let ConstKind::Value(value) = konst.as_ref().kind() else { return None };
        let layout = self.layout(ty).ok()?;
        Some(value.value.inner().to_leaf().to_bits(layout.size))
    }

    fn place(&mut self, place: &StoredPlace) -> Result<(Value, PlaceTy<'db>)> {
        let place = self.place_ref(place)?;
        Ok((place.addr, place.ty))
    }

    fn place_ref(&mut self, place: &StoredPlace) -> Result<PlaceRef<'db>> {
        let mut addr = self.locals[place.local].addr;
        let mut meta = None;
        let mut ty = PlaceTy::from_ty(self.body.expect("MIR body").locals[place.local].ty.as_ref());
        for proj in place.projection.as_slice() {
            let prev = ty;
            ty = ty.projection_ty(&self.infcx, proj, self.env.param_env);
            match proj {
                ProjectionElem::Deref => match self.layout(prev.ty)?.backend_repr {
                    BackendRepr::Scalar(_) => {
                        addr = self.builder.ins().load(PTR, MEM, addr, 0);
                        meta = None;
                    }
                    BackendRepr::ScalarPair(..) => {
                        meta = Some(self.builder.ins().load(PTR, MEM, addr, 8));
                        addr = self.builder.ins().load(PTR, MEM, addr, 0);
                    }
                    _ => unsupported!("deref of {:?}", prev.ty),
                },
                ProjectionElem::Field(field) => {
                    let layout = self.layout(prev.ty)?;
                    let offset = match &layout.variants {
                        Variants::Single { .. } | Variants::Empty => {
                            layout.fields.offset(field.0 as usize)
                        }
                        Variants::Multiple { variants, .. } => {
                            let Some(VariantId::EnumVariantId(variant)) = prev.variant_id else {
                                unsupported!("field of enum without downcast");
                            };
                            variants[RustcEnumVariantIdx(variant.index(self.cg.db))]
                                .fields
                                .offset(field.0 as usize)
                        }
                    };
                    addr = self.builder.ins().iadd_imm(addr, offset.bytes() as i64);
                }
                ProjectionElem::Downcast(_) => (),
                ProjectionElem::Index(local) => {
                    let index = self.builder.ins().load(PTR, MEM, self.locals[*local].addr, 0);
                    addr = self.element(addr, meta, prev.ty, index)?;
                    meta = None;
                }
                ProjectionElem::ConstantIndex { offset, from_end } => {
                    let index = if *from_end {
                        let len = self.len(prev.ty, meta)?;
                        // Unlike rustc's MIR, offset 0 from the end is the last element.
                        self.builder.ins().iadd_imm(len, -(*offset as i64) - 1)
                    } else {
                        self.builder.ins().iconst(PTR, *offset as i64)
                    };
                    addr = self.element(addr, meta, prev.ty, index)?;
                    meta = None;
                }
                ProjectionElem::Subslice { from, to } => {
                    let size = self.element_size(prev.ty)?;
                    // FIXME: rustc checks `from + to <= len` when matching the pattern.
                    addr = self.builder.ins().iadd_imm(addr, (*from * size) as i64);
                    meta = match meta {
                        Some(len) => {
                            Some(self.builder.ins().iadd_imm(len, -((*from + *to) as i64)))
                        }
                        None => None,
                    };
                }
            }
        }
        Ok(PlaceRef { addr, meta, ty })
    }

    fn element_size(&self, container: Ty<'db>) -> Result<u64> {
        match container.kind() {
            TyKind::Array(elem, _) | TyKind::Slice(elem) => Ok(self.layout(elem)?.size.bytes()),
            _ => unsupported!("indexing into {container:?}"),
        }
    }

    /// Number of elements of an array, slice or `str` place.
    fn len(&mut self, container: Ty<'db>, meta: Option<Value>) -> Result<Value> {
        match (container.kind(), meta) {
            (TyKind::Array(_, len), _) => {
                let Some(len) = try_const_usize(self.cg.db, len) else {
                    unsupported!("array with unevaluated length");
                };
                Ok(self.builder.ins().iconst(PTR, len as i64))
            }
            (TyKind::Slice(_) | TyKind::Str, Some(len)) => Ok(len),
            _ => unsupported!("length of {container:?}"),
        }
    }

    /// Address of `container[index]`, after a bounds check.
    fn element(
        &mut self,
        addr: Value,
        meta: Option<Value>,
        container: Ty<'db>,
        index: Value,
    ) -> Result<Value> {
        let size = self.element_size(container)?;
        let len = self.len(container, meta)?;
        let out_of_bounds = self.builder.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
        self.bounds_panic_if(out_of_bounds, index, len)?;
        let offset = self.builder.ins().imul_imm(index, size as i64);
        Ok(self.builder.ins().iadd(addr, offset))
    }

    /// Branches to `core::panicking::panic_bounds_check(index, len, location)` when `cond` is set.
    fn bounds_panic_if(&mut self, cond: Value, index: Value, len: Value) -> Result<()> {
        let Some(symbol) = self.cg.symbols.path("core::panicking::panic_bounds_check") else {
            self.builder.ins().trapnz(cond, TrapCode::HEAP_OUT_OF_BOUNDS);
            return Ok(());
        };
        let panic_block = self.builder.create_block();
        let ok_block = self.builder.create_block();
        self.builder.set_cold_block(panic_block);
        self.builder.ins().brif(cond, panic_block, &[], ok_block, &[]);

        self.builder.switch_to_block(panic_block);
        let location = self.caller_location()?;
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.extend([AbiParam::new(PTR); 3]);
        let callee = self.cg.import(&symbol, &sig)?;
        let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
        self.builder.ins().call(callee, &[index, len, location]);
        self.builder.ins().trap(TrapCode::user(1).expect("non-zero trap code"));

        self.builder.switch_to_block(ok_block);
        Ok(())
    }

    fn static_ty(&self, id: StaticId) -> Ty<'db> {
        let db = self.cg.db;
        let ty = InferenceResult::of(db, DefWithBodyId::from(id))
            .expr_ty(Body::of(db, id.into()).root_expr());
        Ty::new_ref(self.interner, Region::new_erased(self.interner), ty, Mutability::Not)
    }

    fn operand_ty(&mut self, operand: &Operand) -> Result<Ty<'db>> {
        Ok(match &operand.kind {
            OperandKind::Copy(place) | OperandKind::Move(place) => self.place(place)?.1.ty,
            OperandKind::Constant { ty, .. } => ty.as_ref(),
            OperandKind::Allocation { allocation } => allocation.as_ref().ty,
            OperandKind::Static(id) => self.static_ty(*id),
        })
    }

    /// Address of memory holding the operand's value. Constants live in read-only data, so the
    /// memory must not be written through.
    fn operand(&mut self, operand: &Operand) -> Result<(Value, Ty<'db>)> {
        match &operand.kind {
            OperandKind::Copy(place) | OperandKind::Move(place) => {
                let (addr, ty) = self.place(place)?;
                // RA's lowering says `Copy` for moves too; a use of a value that needs dropping
                // can only be a move.
                if self.drop_flags.contains_idx(place.local)
                    && has_drop_glue(&self.infcx, ty.ty, self.env.param_env) != DropGlue::None
                {
                    // FIXME: a partial move forgets the rest of the local, which leaks it.
                    let dead = self.builder.ins().iconst(types::I8, 0);
                    self.builder.ins().store(MEM, dead, self.drop_flags[place.local], 0);
                }
                Ok((addr, ty.ty))
            }
            OperandKind::Constant { konst, ty } => {
                let ty = ty.as_ref();
                let blob = match konst.as_ref().kind() {
                    ConstKind::Value(value) => {
                        let is_int = matches!(
                            ty.kind(),
                            TyKind::Bool | TyKind::Char | TyKind::Int(_) | TyKind::Uint(_)
                        );
                        if is_int {
                            let layout = self.layout(ty)?;
                            let scalar = self.scalar_of(ty)?;
                            let bits = value.value.inner().to_leaf().to_bits(layout.size);
                            let addr = self.stack_slot(&layout);
                            let value = self.iconst(scalar.ty, bits);
                            self.builder.ins().store(MEM, value, addr, 0);
                            return Ok((addr, ty));
                        }
                        self.valtree_blob(ty, value.value)?
                    }
                    ConstKind::Unevaluated(unevaluated) => {
                        let allocation = self.eval_const(unevaluated)?;
                        self.allocation_blob(allocation)?
                    }
                    kind => unsupported!("constant {kind:?}"),
                };
                Ok((self.blob_addr(blob)?, ty))
            }
            OperandKind::Allocation { allocation } => {
                let allocation = allocation.as_ref();
                let ty = allocation.ty;
                let blob = self.allocation_blob(allocation)?;
                Ok((self.blob_addr(blob)?, ty))
            }
            OperandKind::Static(id) => {
                let Some(symbol) = self.cg.symbols.static_(*id) else {
                    return self.private_static(*id);
                };
                let data = self
                    .cg
                    .module
                    .declare_data(&symbol, Linkage::Import, false, false)
                    .map_err(|e| CodegenError::Backend(e.to_string()))?;
                let data = self.cg.module.declare_data_in_func(data, self.builder.func);
                let value = self.builder.ins().symbol_value(PTR, data);
                let ty = self.static_ty(*id);
                let layout = self.layout(ty)?;
                let addr = self.stack_slot(&layout);
                self.builder.ins().store(MEM, value, addr, 0);
                Ok((addr, ty))
            }
        }
    }

    /// A static nothing exports gets a copy of its value. Nobody can tell the copy from the
    /// original as long as it cannot be written to.
    // FIXME: the copy has its own address, which pointer comparisons can observe.
    fn private_static(&mut self, id: StaticId) -> Result<(Value, Ty<'db>)> {
        let db = self.cg.db;
        let ty = self.static_ty(id);
        let flags = StaticSignature::of(db, id).flags;
        if flags.contains(StaticFlags::EXTERN) {
            unsupported!("extern static without a rustc symbol");
        }
        let allocation = db
            .const_eval_static(id)
            .map_err(|e| CodegenError::Frontend(format!("const eval: {e:?}")))?;
        let value = if !flags.contains(StaticFlags::MUTABLE) && self.is_freeze(allocation.ty) {
            let blob = self.allocation_blob(allocation)?;
            self.blob_addr(blob)?
        } else {
            if self.cg.own_statics != Some(id.krate(db)) {
                unsupported!("writable static without a rustc symbol");
            }
            let data = match self.cg.statics.get(&id) {
                Some(&data) => data,
                None => {
                    let blob = self.allocation_blob(allocation)?;
                    let data = self.cg.define_data(blob, true)?;
                    self.cg.statics.insert(id, data);
                    data
                }
            };
            let data = self.cg.module.declare_data_in_func(data, self.builder.func);
            self.builder.ins().symbol_value(PTR, data)
        };
        let addr = self.scratch();
        self.builder.ins().store(MEM, value, addr, 0);
        Ok((addr, ty))
    }

    /// Whether `ty` is known to have no `UnsafeCell` outside of indirections.
    fn is_freeze(&self, ty: Ty<'db>) -> bool {
        let db = self.cg.db;
        match ty.kind() {
            TyKind::Bool
            | TyKind::Char
            | TyKind::Int(_)
            | TyKind::Uint(_)
            | TyKind::Float(_)
            | TyKind::Str
            | TyKind::Never
            | TyKind::Ref(..)
            | TyKind::RawPtr(..)
            | TyKind::FnPtr(..)
            | TyKind::FnDef(..) => true,
            TyKind::Array(element, _) | TyKind::Slice(element) => self.is_freeze(element),
            TyKind::Tuple(fields) => fields.iter().all(|field| self.is_freeze(field)),
            TyKind::Adt(def, args) => {
                let variants: Vec<VariantId> = match def.def_id() {
                    AdtId::StructId(id) => {
                        if Some(id) == self.interner.lang_items().UnsafeCell {
                            return false;
                        }
                        vec![id.into()]
                    }
                    AdtId::EnumId(id) => {
                        id.enum_variants(db).variants.iter().map(|(_, it)| it.0.into()).collect()
                    }
                    AdtId::UnionId(_) => return false,
                };
                variants.into_iter().all(|variant| {
                    db.field_types(variant).iter().all(|(_, field)| {
                        let ty = field.ty().instantiate(self.interner, args).skip_norm_wip();
                        self.is_freeze(ty)
                    })
                })
            }
            _ => false,
        }
    }

    fn eval_const(&mut self, unevaluated: UnevaluatedConst<'db>) -> Result<Allocation<'db>> {
        let db = self.cg.db;
        let UnevaluatedConst { def, args } = unevaluated;
        let frontend = |e| CodegenError::Frontend(format!("const eval: {e:?}"));
        match def.0 {
            GeneralConstId::ConstId(id) => {
                let (id, args) = lookup_impl_const(&self.infcx, self.env.param_env, id, args);
                db.const_eval(id, args, Some(self.env)).map_err(frontend)
            }
            GeneralConstId::StaticId(id) => db.const_eval_static(id).map_err(frontend),
            GeneralConstId::AnonConstId(id) => {
                db.anon_const_eval(id, args, Some(self.env)).map_err(frontend)
            }
        }
    }

    fn blob_addr(&mut self, blob: ConstBlob) -> Result<Value> {
        let data = self.cg.define_const(blob)?;
        let data = self.cg.module.declare_data_in_func(data, self.builder.func);
        Ok(self.builder.ins().symbol_value(PTR, data))
    }

    fn valtree_blob(&mut self, ty: Ty<'db>, valtree: ValTree<'db>) -> Result<ConstBlob> {
        let layout = self.layout(ty)?;
        let align = layout.align.abi.bytes();
        let pointee = match ty.kind() {
            TyKind::Bool
            | TyKind::Char
            | TyKind::Int(_)
            | TyKind::Uint(_)
            | TyKind::Float(_)
            | TyKind::RawPtr(..) => {
                let bits = valtree.inner().to_leaf().to_bits(layout.size);
                let bytes = bits.to_le_bytes()[..layout.size.bytes_usize()].to_vec();
                return Ok(ConstBlob { bytes, relocs: Vec::new(), fn_relocs: Vec::new(), align });
            }
            TyKind::Ref(_, pointee, _) => pointee,
            _ => unsupported!("constant of type {ty:?}"),
        };
        let mut bytes = vec![0; layout.size.bytes_usize()];
        let target = match pointee.kind() {
            TyKind::Str | TyKind::Slice(_) => {
                let mut target = ConstBlob { bytes: Vec::new(), relocs: Vec::new(), fn_relocs: Vec::new(), align: 1 };
                let items = valtree.inner().to_branch();
                for item in items.iter() {
                    let ConstKind::Value(item) = item.kind() else {
                        unsupported!("unevaluated const in a slice constant");
                    };
                    let item = self.valtree_blob(item.ty, item.value)?;
                    if !item.relocs.is_empty() {
                        unsupported!("slice constant of pointers");
                    }
                    target.align = target.align.max(item.align);
                    target.bytes.extend(item.bytes);
                }
                bytes[8..16].copy_from_slice(&(items.len() as u64).to_le_bytes());
                target
            }
            TyKind::Dynamic(..) => unsupported!("`dyn` constant"),
            _ => self.valtree_blob(pointee, valtree)?,
        };
        let target = self.cg.define_const(target)?;
        Ok(ConstBlob { bytes, relocs: vec![(0, target)], fn_relocs: Vec::new(), align })
    }

    fn allocation_blob(&mut self, allocation: Allocation<'db>) -> Result<ConstBlob> {
        let AllocationData { ty, memory, memory_map } = &*allocation;
        let layout = self.layout(*ty)?;
        let size = layout.size.bytes_usize();
        let mut bytes = memory.to_vec();
        match (bytes.len(), size) {
            (len, size) if len == size => (),
            // Const eval stores some scalars as 16 bytes.
            (16, size) if size < 16 => bytes.truncate(size),
            (len, 16) if len < 16 => bytes.resize(16, 0),
            _ => unsupported!("constant whose size differs from its layout"),
        }
        let mut blob = ConstBlob { bytes, relocs: Vec::new(), fn_relocs: Vec::new(), align: layout.align.abi.bytes() };
        self.relocate(&mut blob, 0, *ty, memory_map)?;
        Ok(blob)
    }

    /// Const eval gives pointers as keys into `map`. Emits each target as its own data object
    /// and records a relocation for the pointer, walking `ty` to find them.
    fn relocate(
        &mut self,
        blob: &mut ConstBlob,
        offset: usize,
        ty: Ty<'db>,
        map: &MemoryMap<'db>,
    ) -> Result<()> {
        if let MemoryMap::Empty = map {
            return Ok(());
        }
        let db = self.cg.db;
        match ty.kind() {
            TyKind::Ref(_, pointee, _) => {
                let Some(addr) = blob.usize_at(offset) else {
                    unsupported!("constant pointer out of bounds");
                };
                let target = match map {
                    MemoryMap::Simple(memory) if addr == 0 => Some(&**memory),
                    MemoryMap::Complex(complex) => complex.memory.get(&addr).map(|it| &**it),
                    _ => None,
                };
                let Some(target) = target else { return Ok(()) };
                let mut target =
                    ConstBlob { bytes: target.to_vec(), relocs: Vec::new(), fn_relocs: Vec::new(), align: 16 };
                match pointee.kind() {
                    TyKind::Str => (),
                    TyKind::Slice(elem) => {
                        let Some(len) = blob.usize_at(offset + 8) else {
                            unsupported!("constant slice length out of bounds");
                        };
                        let size = self.layout(elem)?.size.bytes_usize();
                        for i in 0..len {
                            self.relocate(&mut target, i * size, elem, map)?;
                        }
                    }
                    TyKind::Dynamic(..) => unsupported!("`dyn` constant"),
                    _ => self.relocate(&mut target, 0, pointee, map)?,
                }
                let target = self.cg.define_const(target)?;
                blob.bytes[offset..offset + 8].fill(0);
                blob.relocs.push((offset as u32, target));
            }
            // Const eval gives function pointers as ids into its table of fn item types. A
            // function cast to a raw pointer keeps that id, so a raw pointer holding one is
            // taken to be a function. FIXME: const eval should track provenance instead.
            TyKind::FnPtr(..) | TyKind::RawPtr(..) => {
                let Some(id) = blob.usize_at(offset) else {
                    unsupported!("constant pointer out of bounds");
                };
                let fn_ty = match map.vtable_ty(id) {
                    Ok(fn_ty) if matches!(fn_ty.kind(), TyKind::FnDef(..)) => fn_ty,
                    _ if matches!(ty.kind(), TyKind::RawPtr(..)) => return Ok(()),
                    _ if id == 0 => return Ok(()),
                    _ => unsupported!("fn pointer in a constant"),
                };
                let target = self.fn_item(fn_ty)?;
                blob.bytes[offset..offset + 8].fill(0);
                blob.fn_relocs.push((offset as u32, target));
            }
            TyKind::Adt(def, args) => {
                let layout = self.layout(ty)?;
                let (variant, offsets): (VariantId, Vec<usize>) = match def.def_id() {
                    AdtId::StructId(id) => (
                        id.into(),
                        (0..layout.fields.count())
                            .map(|i| layout.fields.offset(i).bytes_usize())
                            .collect(),
                    ),
                    AdtId::UnionId(_) => return Ok(()),
                    AdtId::EnumId(id) => {
                        let Some(bytes) =
                            blob.bytes.get(offset..offset + layout.size.bytes_usize())
                        else {
                            unsupported!("constant enum out of bounds");
                        };
                        let Some((variant, layout)) =
                            detect_variant_from_bytes(&layout, db, self.dl(), bytes, id)
                        else {
                            return Ok(());
                        };
                        (
                            variant.into(),
                            (0..layout.fields.count())
                                .map(|i| layout.fields.offset(i).bytes_usize())
                                .collect(),
                        )
                    }
                };
                for ((_, field), field_offset) in db.field_types(variant).iter().zip(offsets) {
                    let field = field.ty().instantiate(self.interner, args).skip_norm_wip();
                    self.relocate(blob, offset + field_offset, field, map)?;
                }
            }
            TyKind::Tuple(tys) => {
                let layout = self.layout(ty)?;
                for (i, field) in tys.iter().enumerate() {
                    let field_offset = layout.fields.offset(i).bytes_usize();
                    self.relocate(blob, offset + field_offset, field, map)?;
                }
            }
            TyKind::Array(elem, len) => {
                let Some(len) = try_const_usize(db, len) else {
                    unsupported!("array with unevaluated length");
                };
                let size = self.layout(elem)?.size.bytes_usize();
                for i in 0..len as usize {
                    self.relocate(blob, offset + i * size, elem, map)?;
                }
            }
            TyKind::Alias(..) => {
                let normalized =
                    crate::next_solver::obligation_ctxt::ObligationCtxt::new(&self.infcx)
                        .structurally_normalize_ty(
                            &crate::next_solver::infer::traits::ObligationCause::dummy(),
                            self.env.param_env,
                            ty,
                        )
                        .unwrap_or(ty);
                if !matches!(normalized.kind(), TyKind::Alias(..)) {
                    self.relocate(blob, offset, normalized, map)?;
                }
            }
            _ => (),
        }
        Ok(())
    }

    fn iconst(&mut self, ty: Type, bits: u128) -> Value {
        if ty.bits() < 64 {
            // Cranelift wants narrow immediates without stray high bits.
            let bits = bits as u64 & ((1 << ty.bits()) - 1);
            return self.builder.ins().iconst(ty, bits as i64);
        }
        if ty == types::I128 {
            let lo = self.builder.ins().iconst(types::I64, bits as u64 as i64);
            let hi = self.builder.ins().iconst(types::I64, (bits >> 64) as u64 as i64);
            self.builder.ins().iconcat(lo, hi)
        } else {
            self.builder.ins().iconst(ty, bits as u64 as i64)
        }
    }

    fn load_scalar(&mut self, operand: &Operand) -> Result<(Value, AbiScalar)> {
        let (addr, ty) = self.operand(operand)?;
        let scalar = self.scalar_of(ty)?;
        Ok((self.builder.ins().load(scalar.ty, MEM, addr, 0), scalar))
    }

    fn copy(&mut self, dst: Value, src: Value, size: Size) {
        let mut offset = 0i32;
        let mut left = size.bytes() as i32;
        for ty in [types::I64, types::I32, types::I16, types::I8] {
            while left >= ty.bytes() as i32 {
                let value = self.builder.ins().load(ty, MEM, src, offset);
                self.builder.ins().store(MEM, value, dst, offset);
                offset += ty.bytes() as i32;
                left -= ty.bytes() as i32;
            }
        }
    }

    fn assign(&mut self, place: &StoredPlace, rvalue: &Rvalue) -> Result<()> {
        let (dst, dst_ty) = self.place(place)?;
        let store = |this: &mut Self, value: Value| {
            this.builder.ins().store(MEM, value, dst, 0);
        };
        match rvalue {
            Rvalue::Use(operand) => {
                let (src, ty) = self.operand(operand)?;
                let size = self.layout(ty)?.size;
                self.copy(dst, src, size);
            }
            Rvalue::CopyForDeref(place) => {
                let (src, ty) = self.place(place)?;
                let size = self.layout(ty.ty)?.size;
                self.copy(dst, src, size);
            }
            Rvalue::Ref(_, place) => {
                let place = self.place_ref(place)?;
                store(self, place.addr);
                if let BackendRepr::ScalarPair(..) = self.layout(dst_ty.ty)?.backend_repr {
                    let Some(meta) = place.meta else {
                        unsupported!("fat reference to a place without metadata");
                    };
                    self.builder.ins().store(MEM, meta, dst, 8);
                }
            }
            Rvalue::Len(place) => {
                let place = self.place_ref(place)?;
                let len = self.len(place.ty.ty, place.meta)?;
                store(self, len);
            }
            Rvalue::Discriminant(place) => {
                let (addr, ty) = self.place(place)?;
                let to = self.scalar_of(dst_ty.ty)?;
                let value = self.discriminant(addr, ty.ty, to)?;
                store(self, value);
            }
            Rvalue::Repeat(operand, len) => {
                let Some(len) = try_const_usize(self.cg.db, len.as_ref()) else {
                    unsupported!("repeat with unevaluated length");
                };
                let (src, ty) = self.operand(operand)?;
                let size = self.layout(ty)?.size;
                if len <= 16 {
                    for i in 0..len as u64 {
                        let element = self.builder.ins().iadd_imm(dst, (i * size.bytes()) as i64);
                        self.copy(element, src, size);
                    }
                } else {
                    let header = self.builder.create_block();
                    let body = self.builder.create_block();
                    let done = self.builder.create_block();
                    self.builder.append_block_param(header, PTR);
                    let zero = self.builder.ins().iconst(PTR, 0);
                    self.builder.ins().jump(header, &[zero.into()]);
                    self.builder.switch_to_block(header);
                    let index = self.builder.block_params(header)[0];
                    let more =
                        self.builder.ins().icmp_imm(IntCC::UnsignedLessThan, index, len as i64);
                    self.builder.ins().brif(more, body, &[], done, &[]);
                    self.builder.switch_to_block(body);
                    let offset = self.builder.ins().imul_imm(index, size.bytes() as i64);
                    let element = self.builder.ins().iadd(dst, offset);
                    self.copy(element, src, size);
                    let next = self.builder.ins().iadd_imm(index, 1);
                    self.builder.ins().jump(header, &[next.into()]);
                    self.builder.switch_to_block(done);
                }
            }
            Rvalue::CheckedBinaryOp(op, lhs, rhs) => {
                let value = self.binary_op(op, lhs, rhs)?;
                store(self, value);
            }
            Rvalue::UnaryOp(op, operand) => {
                let (value, _) = self.load_scalar(operand)?;
                let is_bool = matches!(self.operand_ty(operand)?.kind(), TyKind::Bool);
                let value = match op {
                    UnOp::Not if is_bool => self.builder.ins().bxor_imm(value, 1),
                    UnOp::Not => self.builder.ins().bnot(value),
                    UnOp::Neg if self.builder.func.dfg.value_type(value).is_float() => {
                        self.builder.ins().fneg(value)
                    }
                    UnOp::Neg => self.builder.ins().ineg(value),
                };
                store(self, value);
            }
            Rvalue::Cast(kind, operand, target) => {
                self.cast(kind, operand, target.as_ref(), dst)?
            }
            Rvalue::Aggregate(kind, operands) => match kind {
                AggregateKind::Tuple(ty) => {
                    let layout = self.layout(ty.as_ref())?;
                    self.write_fields(dst, &layout, operands)?;
                }
                AggregateKind::Adt(variant, args) => {
                    let layout = self.variant_layout(dst, *variant, args.as_ref())?;
                    self.write_fields(dst, &layout, operands)?;
                }
                AggregateKind::Array(elem) => {
                    let size = self.layout(elem.as_ref())?.size;
                    for (i, operand) in operands.iter().enumerate() {
                        let (src, _) = self.operand(operand)?;
                        let element =
                            self.builder.ins().iadd_imm(dst, (i as u64 * size.bytes()) as i64);
                        self.copy(element, src, size);
                    }
                }
                AggregateKind::Union(..) => {
                    let [operand] = &**operands else {
                        unsupported!("union aggregate without exactly one field");
                    };
                    let (src, ty) = self.operand(operand)?;
                    let size = self.layout(ty)?.size;
                    self.copy(dst, src, size);
                }
                AggregateKind::Closure(ty) => {
                    let layout = self.layout(ty.as_ref())?;
                    self.write_fields(dst, &layout, operands)?;
                }
            },
            _ => unsupported!("rvalue {rvalue:?}"),
        }
        Ok(())
    }

    fn cast(
        &mut self,
        kind: &CastKind,
        operand: &Operand,
        to_ty: Ty<'db>,
        dst: Value,
    ) -> Result<()> {
        let (src, from_ty) = self.operand(operand)?;
        let from = self.layout(from_ty)?;
        let to = self.layout(to_ty)?;
        match kind {
            // RA records an unsizing coercion on both a block and its tail expression; the
            // outer one has nothing left to do.
            CastKind::PointerCoercion(PointerCast::Unsize) if from_ty == to_ty => {
                self.copy(dst, src, to.size)
            }
            CastKind::PointerCoercion(PointerCast::Unsize) => {
                let (Some(from_pointee), Some(to_pointee)) =
                    (self.coerced_pointee(from_ty, to_ty), self.coerced_pointee(to_ty, from_ty))
                else {
                    unsupported!("unsizing {from_ty:?} into {to_ty:?}");
                };
                // The pointer comes first and the metadata right after it in everything that
                // can be unsized: references, raw pointers, and the smart pointers around them.
                if to.size.bytes() != 16 || !matches!(from.size.bytes(), 8 | 16) {
                    unsupported!("unsizing {from_ty:?} to {to_ty:?}");
                }
                let old =
                    (from.size.bytes() == 16).then(|| self.builder.ins().load(PTR, MEM, src, 8));
                let meta = self.unsize_metadata(from_pointee, to_pointee, old)?;
                let ptr = self.builder.ins().load(PTR, MEM, src, 0);
                self.builder.ins().store(MEM, ptr, dst, 0);
                self.builder.ins().store(MEM, meta, dst, 8);
            }
            CastKind::PointerCoercion(PointerCast::ReifyFnPointer) => {
                let callee = self.fn_item(from_ty)?;
                let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
                let pointer = self.builder.ins().func_addr(PTR, callee);
                self.builder.ins().store(MEM, pointer, dst, 0);
            }
            CastKind::PointerCoercion(
                PointerCast::MutToConstPointer
                | PointerCast::ArrayToPointer
                | PointerCast::UnsafeFnPointer,
            ) => self.copy(dst, src, to.size),
            CastKind::IntToInt
            | CastKind::PtrToPtr
            | CastKind::PointerExposeAddress
            | CastKind::PointerFromExposedAddress
            | CastKind::FnPtrToPtr => match (from.backend_repr, to.backend_repr) {
                (BackendRepr::Scalar(from), BackendRepr::Scalar(to)) => {
                    let (from, to) = (self.scalar(from)?, self.scalar(to)?);
                    let value = self.builder.ins().load(from.ty, MEM, src, 0);
                    let value = self.int_cast(value, from, to);
                    self.builder.ins().store(MEM, value, dst, 0);
                }
                // Fat to fat keeps the metadata, fat to thin drops it.
                (
                    BackendRepr::ScalarPair(..),
                    BackendRepr::ScalarPair(..) | BackendRepr::Scalar(_),
                ) => self.copy(dst, src, to.size),
                _ => unsupported!("cast from {from_ty:?} to {to_ty:?}"),
            },
            CastKind::IntToFloat | CastKind::FloatToInt | CastKind::FloatToFloat => {
                let (from, to) = (self.scalar_of(from_ty)?, self.scalar_of(to_ty)?);
                let value = self.builder.ins().load(from.ty, MEM, src, 0);
                let value = self.float_cast(value, from, to)?;
                self.builder.ins().store(MEM, value, dst, 0);
            }
            _ => unsupported!("cast {kind:?}"),
        }
        Ok(())
    }

    /// The function a fn item type names, declared so its address can be taken.
    fn fn_item(&mut self, fn_ty: Ty<'db>) -> Result<FuncId> {
        let TyKind::FnDef(def, generic_args) = fn_ty.kind() else {
            unsupported!("fn pointer to {fn_ty:?}");
        };
        let CallableDefId::FunctionId(func) = def.0 else {
            unsupported!("fn pointer to a constructor");
        };
        let db = self.cg.db;
        if FunctionSignature::is_intrinsic(db, func) {
            unsupported!("fn pointer to an intrinsic");
        }
        if is_dyn_method(self.interner, self.env.param_env, func, generic_args).is_some() {
            unsupported!("fn pointer to a `dyn` method");
        }
        let (callee, callee_args) = db.lookup_impl_method(self.env, func, generic_args);
        let either::Either::Left(callee) = callee else {
            unsupported!("fn pointer to a builtin derive method");
        };
        let sig = self.interner.instantiate_bound_regions_with_erased(fn_ty.fn_sig(self.interner));
        let ret_layout = self.layout(sig.output())?;
        let ret_mode = self.pass_mode(&ret_layout, true)?;
        let mut arg_modes = Vec::new();
        for ty in sig.inputs().iter() {
            let layout = self.layout(*ty)?;
            arg_modes.push(self.pass_mode(&layout, false)?);
        }
        let sig = self.signature(&arg_modes, ret_mode);
        self.cg.callee(InstanceDef::Fn(callee), callee_args, self.env, &sig)
    }

    /// The discriminant of the enum at `addr`, as a `to`.
    fn discriminant(&mut self, addr: Value, ty: Ty<'db>, to: AbiScalar) -> Result<Value> {
        let db = self.cg.db;
        let TyKind::Adt(def, _) = ty.kind() else {
            return Ok(self.iconst(to.ty, 0));
        };
        let AdtId::EnumId(id) = def.def_id() else {
            return Ok(self.iconst(to.ty, 0));
        };
        let layout = self.layout(ty)?;
        let enum_variants = id.enum_variants(db);
        let discr_of = |index: usize| {
            db.const_eval_discriminant(enum_variants.variants[index].0)
                .map_err(|e| CodegenError::Frontend(format!("discriminant: {e:?}")))
        };
        match &layout.variants {
            Variants::Empty => unsupported!("discriminant of an uninhabited enum"),
            Variants::Single { index } => {
                let discr = discr_of(index.0)?;
                Ok(self.iconst(to.ty, discr as u128))
            }
            Variants::Multiple { tag, tag_encoding, tag_field, variants } => {
                let tag = self.scalar(*tag)?;
                let offset = layout.fields.offset(tag_field.index()).bytes() as i32;
                let value = self.builder.ins().load(tag.ty, MEM, addr, offset);
                match tag_encoding {
                    TagEncoding::Direct => Ok(self.int_cast(value, tag, to)),
                    TagEncoding::Niche { untagged_variant, niche_variants, niche_start } => {
                        for (index, _) in variants.iter_enumerated() {
                            if discr_of(index.0)? != index.0 as i128 {
                                unsupported!("niche enum with explicit discriminants");
                            }
                        }
                        let first = niche_variants.start().0;
                        let count = niche_variants.end().0 - first;
                        let start = self.iconst(tag.ty, *niche_start);
                        let relative = self.builder.ins().isub(value, start);
                        let is_niche = self.builder.ins().icmp_imm(
                            IntCC::UnsignedLessThanOrEqual,
                            relative,
                            count as i64,
                        );
                        let relative =
                            self.int_cast(relative, AbiScalar { signed: false, ..tag }, to);
                        let niche = self.builder.ins().iadd_imm(relative, first as i64);
                        let untagged = self.iconst(to.ty, untagged_variant.0 as u128);
                        Ok(self.builder.ins().select(is_niche, niche, untagged))
                    }
                }
            }
        }
    }

    /// Layout whose `fields` describe `variant`. Writes the tag for multi-variant enums.
    fn variant_layout(
        &mut self,
        dst: Value,
        variant: VariantId,
        args: GenericArgs<'db>,
    ) -> Result<Arc<Layout>> {
        let db = self.cg.db;
        let adt: AdtId = variant.adt_id(db);
        let layout = self.layout(Ty::new_adt(self.interner, adt, args))?;
        let Variants::Multiple { tag, tag_encoding, tag_field, variants } = &layout.variants else {
            return Ok(layout);
        };
        let VariantId::EnumVariantId(variant) = variant else {
            unsupported!("multi-variant non-enum");
        };
        let index = variant.index(db);
        let tag_ty = self.scalar(*tag)?.ty;
        let offset = layout.fields.offset(tag_field.index()).bytes() as i32;
        let tag_value = match tag_encoding {
            TagEncoding::Direct => {
                let discr = db
                    .const_eval_discriminant(variant)
                    .map_err(|e| CodegenError::Frontend(format!("discriminant: {e:?}")))?;
                Some(discr as u128)
            }
            TagEncoding::Niche { untagged_variant, .. } if untagged_variant.0 == index => None,
            TagEncoding::Niche { niche_variants, niche_start, .. } => {
                Some(((index - niche_variants.start().0) as u128).wrapping_add(*niche_start))
            }
        };
        if let Some(tag_value) = tag_value {
            let value = self.iconst(tag_ty, tag_value);
            self.builder.ins().store(MEM, value, dst, offset);
        }
        Ok(Arc::new(variants[RustcEnumVariantIdx(index)].clone()))
    }

    fn write_fields(&mut self, dst: Value, layout: &Layout, operands: &[Operand]) -> Result<()> {
        if operands.len() > layout.fields.count() {
            unsupported!("constructor with more operands than the layout has fields");
        }
        for (i, operand) in operands.iter().enumerate() {
            let (src, ty) = self.operand(operand)?;
            let size = self.layout(ty)?.size;
            let field = self.builder.ins().iadd_imm(dst, layout.fields.offset(i).bytes() as i64);
            self.copy(field, src, size);
        }
        Ok(())
    }

    fn binary_op(&mut self, op: &BinOp, lhs: &Operand, rhs: &Operand) -> Result<Value> {
        let (mut l_addr, mut ty) = self.operand(lhs)?;
        let (mut r_addr, mut r_ty) = self.operand(rhs)?;
        // RA lowers comparisons of references without the deref.
        // Where the innermost reference is stored, for the length of a `str`.
        let (mut l_ref, mut r_ref) = (None, None);
        while let TyKind::Ref(_, inner, _) = ty.kind() {
            ty = inner;
            l_ref = Some(l_addr);
            l_addr = self.builder.ins().load(PTR, MEM, l_addr, 0);
        }
        while let TyKind::Ref(_, inner, _) = r_ty.kind() {
            r_ty = inner;
            r_ref = Some(r_addr);
            r_addr = self.builder.ins().load(PTR, MEM, r_addr, 0);
        }
        // A constant pattern of array type. Elements without padding or a custom `PartialEq`
        // are equal exactly when their bytes are.
        if let (BinOp::Eq | BinOp::Ne, TyKind::Array(element, _)) = (op, ty.kind())
            && matches!(
                element.kind(),
                TyKind::Int(_) | TyKind::Uint(_) | TyKind::Bool | TyKind::Char
            )
        {
            let size = self.layout(ty)?.size.bytes();
            let len = self.builder.ins().iconst(PTR, size as i64);
            let order = self.memcmp(l_addr, r_addr, len)?;
            let cc = if let BinOp::Eq = op { IntCC::Equal } else { IntCC::NotEqual };
            return Ok(self.builder.ins().icmp_imm(cc, order, 0));
        }
        // A string literal pattern.
        if let (BinOp::Eq | BinOp::Ne, TyKind::Str, Some(l_ref), Some(r_ref)) =
            (op, ty.kind(), l_ref, r_ref)
        {
            let l_len = self.builder.ins().load(PTR, MEM, l_ref, 8);
            let r_len = self.builder.ins().load(PTR, MEM, r_ref, 8);
            let same_len = self.builder.ins().icmp(IntCC::Equal, l_len, r_len);
            let zero = self.builder.ins().iconst(PTR, 0);
            let len = self.builder.ins().select(same_len, l_len, zero);
            let order = self.memcmp(l_addr, r_addr, len)?;
            let same_bytes = self.builder.ins().icmp_imm(IntCC::Equal, order, 0);
            let equal = self.builder.ins().band(same_len, same_bytes);
            return Ok(if let BinOp::Eq = op {
                equal
            } else {
                self.builder.ins().bxor_imm(equal, 1)
            });
        }
        let scalar = self.scalar_of(ty)?;
        let r_scalar = self.scalar_of(r_ty)?;
        let l = self.builder.ins().load(scalar.ty, MEM, l_addr, 0);
        let r = self.builder.ins().load(r_scalar.ty, MEM, r_addr, 0);
        let signed = scalar.signed;
        if scalar.ty.is_float() {
            return self.float_binary_op(op, l, r);
        }
        let cc = match (op, signed) {
            (BinOp::Eq, _) => Some(IntCC::Equal),
            (BinOp::Ne, _) => Some(IntCC::NotEqual),
            (BinOp::Lt, true) => Some(IntCC::SignedLessThan),
            (BinOp::Lt, false) => Some(IntCC::UnsignedLessThan),
            (BinOp::Le, true) => Some(IntCC::SignedLessThanOrEqual),
            (BinOp::Le, false) => Some(IntCC::UnsignedLessThanOrEqual),
            (BinOp::Gt, true) => Some(IntCC::SignedGreaterThan),
            (BinOp::Gt, false) => Some(IntCC::UnsignedGreaterThan),
            (BinOp::Ge, true) => Some(IntCC::SignedGreaterThanOrEqual),
            (BinOp::Ge, false) => Some(IntCC::UnsignedGreaterThanOrEqual),
            _ => None,
        };
        if let Some(cc) = cc {
            return Ok(self.builder.ins().icmp(cc, l, r));
        }
        if let BinOp::Mul = op
            && self.builder.func.dfg.value_type(l) == types::I128
        {
            let (value, overflow) = self.wide_mul_overflow(l, r, signed)?;
            self.panic_if(overflow, OverflowOp::Mul)?;
            return Ok(value);
        }
        let ins = self.builder.ins();
        Ok(match op {
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                unreachable!("comparisons return above")
            }
            BinOp::BitAnd => ins.band(l, r),
            BinOp::BitOr => ins.bor(l, r),
            BinOp::BitXor => ins.bxor(l, r),
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let (value, overflow) = match (op, signed) {
                    (BinOp::Add, true) => ins.sadd_overflow(l, r),
                    (BinOp::Add, false) => ins.uadd_overflow(l, r),
                    (BinOp::Sub, true) => ins.ssub_overflow(l, r),
                    (BinOp::Sub, false) => ins.usub_overflow(l, r),
                    (BinOp::Mul, true) => ins.smul_overflow(l, r),
                    (_, _) => ins.umul_overflow(l, r),
                };
                let op = match op {
                    BinOp::Add => OverflowOp::Add,
                    BinOp::Sub => OverflowOp::Sub,
                    _ => OverflowOp::Mul,
                };
                self.panic_if(overflow, op)?;
                value
            }
            BinOp::Div | BinOp::Rem => {
                // FIXME: rustc panics on a zero divisor and on MIN / -1; this traps.
                match (op, signed) {
                    (BinOp::Div, true) => ins.sdiv(l, r),
                    (BinOp::Div, false) => ins.udiv(l, r),
                    (_, true) => ins.srem(l, r),
                    (_, false) => ins.urem(l, r),
                }
            }
            BinOp::Shl | BinOp::Shr => {
                let bits = self.builder.ins().iconst(r_scalar.ty, i64::from(scalar.ty.bits()));
                let overflow = self.builder.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, r, bits);
                let (shift_op, value) = match (op, signed) {
                    (BinOp::Shl, _) => (OverflowOp::Shl, self.builder.ins().ishl(l, r)),
                    (_, true) => (OverflowOp::Shr, self.builder.ins().sshr(l, r)),
                    (_, false) => (OverflowOp::Shr, self.builder.ins().ushr(l, r)),
                };
                self.panic_if(overflow, shift_op)?;
                value
            }
            BinOp::Offset => unsupported!("pointer offset"),
        })
    }

    fn float_binary_op(&mut self, op: &BinOp, l: Value, r: Value) -> Result<Value> {
        let ins = self.builder.ins();
        Ok(match op {
            BinOp::Eq => ins.fcmp(FloatCC::Equal, l, r),
            BinOp::Ne => ins.fcmp(FloatCC::NotEqual, l, r),
            BinOp::Lt => ins.fcmp(FloatCC::LessThan, l, r),
            BinOp::Le => ins.fcmp(FloatCC::LessThanOrEqual, l, r),
            BinOp::Gt => ins.fcmp(FloatCC::GreaterThan, l, r),
            BinOp::Ge => ins.fcmp(FloatCC::GreaterThanOrEqual, l, r),
            BinOp::Add => ins.fadd(l, r),
            BinOp::Sub => ins.fsub(l, r),
            BinOp::Mul => ins.fmul(l, r),
            BinOp::Div => ins.fdiv(l, r),
            BinOp::Rem => {
                let ty = self.builder.func.dfg.value_type(l);
                let name = if ty == types::F32 { "fmodf" } else { "fmod" };
                self.float_libcall(name, ty, &[l, r])?
            }
            _ => unsupported!("float operator {op:?}"),
        })
    }

    /// Calls a libm function taking and returning `ty`.
    fn float_libcall(&mut self, name: &str, ty: Type, args: &[Value]) -> Result<Value> {
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.extend(args.iter().map(|_| AbiParam::new(ty)));
        sig.returns.push(AbiParam::new(ty));
        let callee = self.cg.import(name, &sig)?;
        let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
        let call = self.builder.ins().call(callee, args);
        Ok(self.builder.inst_results(call)[0])
    }

    /// `as` between a float and an integer or another float.
    fn float_cast(&mut self, value: Value, from: AbiScalar, to: AbiScalar) -> Result<Value> {
        if from.ty == types::I128 || to.ty == types::I128 {
            unsupported!("cast between a float and a 128-bit integer");
        }
        let ins = self.builder.ins();
        Ok(match (from.ty.is_float(), to.ty.is_float()) {
            (true, true) if from.ty == to.ty => value,
            (true, true) if to.ty == types::F64 => ins.fpromote(types::F64, value),
            (true, true) => ins.fdemote(types::F32, value),
            (false, true) => {
                let wide = if from.ty.bits() < 32 {
                    if from.signed {
                        ins.sextend(types::I32, value)
                    } else {
                        ins.uextend(types::I32, value)
                    }
                } else {
                    value
                };
                if from.signed {
                    self.builder.ins().fcvt_from_sint(to.ty, wide)
                } else {
                    self.builder.ins().fcvt_from_uint(to.ty, wide)
                }
            }
            (true, false) => {
                // Saturating, with NaN becoming zero, as `as` requires.
                if to.ty.bits() >= 32 {
                    if to.signed {
                        ins.fcvt_to_sint_sat(to.ty, value)
                    } else {
                        ins.fcvt_to_uint_sat(to.ty, value)
                    }
                } else {
                    let wide = ins.fcvt_to_sint_sat(types::I32, value);
                    let bits = to.ty.bits();
                    let (min, max) = if to.signed {
                        (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1)
                    } else {
                        (0, (1i64 << bits) - 1)
                    };
                    let min = self.builder.ins().iconst(types::I32, min);
                    let max = self.builder.ins().iconst(types::I32, max);
                    let clamped = self.builder.ins().smax(wide, min);
                    let clamped = self.builder.ins().smin(clamped, max);
                    self.builder.ins().ireduce(to.ty, clamped)
                }
            }
            (false, false) => unreachable!("float cast between integers"),
        })
    }

    /// Branches to a call of rustc's `panic_const_*` when `cond` is set.
    fn panic_if(&mut self, cond: Value, op: OverflowOp) -> Result<()> {
        let name = match op {
            OverflowOp::Add => "add",
            OverflowOp::Sub => "sub",
            OverflowOp::Mul => "mul",
            OverflowOp::Div => "div",
            OverflowOp::Rem => "rem",
            OverflowOp::Neg => "neg",
            OverflowOp::Shl => "shl",
            OverflowOp::Shr => "shr",
        };
        let path = format!("core::panicking::panic_const::panic_const_{name}_overflow");
        let Some(symbol) = self.cg.symbols.path(&path) else {
            self.builder.ins().trapnz(cond, TrapCode::INTEGER_OVERFLOW);
            return Ok(());
        };
        let panic_block = self.builder.create_block();
        let ok_block = self.builder.create_block();
        self.builder.set_cold_block(panic_block);
        self.builder.ins().brif(cond, panic_block, &[], ok_block, &[]);

        self.builder.switch_to_block(panic_block);
        let location = self.caller_location()?;
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.push(AbiParam::new(PTR));
        let callee = self.cg.import(&symbol, &sig)?;
        let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
        self.builder.ins().call(callee, &[location]);
        self.builder.ins().trap(TrapCode::user(1).expect("non-zero trap code"));

        self.builder.switch_to_block(ok_block);
        Ok(())
    }

    /// A `core::panic::Location` (`{ file: &str, line: u32, col: u32 }`) for `#[track_caller]`.
    fn caller_location(&mut self) -> Result<Value> {
        // FIXME: real spans; the layout of `Location` should come from `layout_of_ty`.
        let backend = |e: cranelift_module::ModuleError| CodegenError::Backend(e.to_string());
        let n = self.cg.location_counter;
        self.cg.location_counter += 1;
        let file = b"<rustd>";
        let file_id = self
            .cg
            .module
            .declare_data(&format!(".Lrustd.file.{n}"), Linkage::Local, false, false)
            .map_err(backend)?;
        let mut file_data = DataDescription::new();
        file_data.define(file.to_vec().into_boxed_slice());
        self.cg.module.define_data(file_id, &file_data).map_err(backend)?;

        let loc_id = self
            .cg
            .module
            .declare_data(&format!(".Lrustd.loc.{n}"), Linkage::Local, false, false)
            .map_err(backend)?;
        let mut loc_data = DataDescription::new();
        let mut bytes = vec![0u8; 24];
        bytes[8..16].copy_from_slice(&(file.len() as u64).to_le_bytes());
        loc_data.define(bytes.into_boxed_slice());
        loc_data.set_align(8);
        let file_ref = self.cg.module.declare_data_in_data(file_id, &mut loc_data);
        loc_data.write_data_addr(0, file_ref, 0);
        self.cg.module.define_data(loc_id, &loc_data).map_err(backend)?;

        let loc_ref = self.cg.module.declare_data_in_func(loc_id, self.builder.func);
        Ok(self.builder.ins().symbol_value(PTR, loc_ref))
    }

    fn terminator(&mut self, terminator: &TerminatorKind) -> Result<()> {
        match terminator {
            TerminatorKind::Goto { target }
            | TerminatorKind::FalseEdge { real_target: target, .. }
            | TerminatorKind::FalseUnwind { real_target: target, .. } => {
                self.builder.ins().jump(self.blocks[*target], &[]);
            }
            TerminatorKind::SwitchInt { discr, targets } => {
                if let Some(bits) = self.const_bits(discr) {
                    let target = self.blocks[targets.target_for_value(bits)];
                    self.builder.ins().jump(target, &[]);
                    return Ok(());
                }
                let (value, _) = self.load_scalar(discr)?;
                let mut switch = Switch::new();
                for (case, target) in targets.iter() {
                    switch.set_entry(case, self.blocks[target]);
                }
                switch.emit(&mut self.builder, value, self.blocks[targets.otherwise()]);
            }
            TerminatorKind::Return => {
                let addr = self.locals[return_slot()].addr;
                if let PassMode::Cast(ty) = self.ret_mode {
                    let size = self
                        .layout(self.body.expect("MIR body").locals[return_slot()].ty.as_ref())?
                        .size;
                    let value = self.load_cast(ty, addr, size);
                    self.builder.ins().return_(&[value]);
                    return Ok(());
                }
                let mut load = |ty, offset| self.builder.ins().load(ty, MEM, addr, offset);
                let values = match self.ret_mode {
                    PassMode::Ignore | PassMode::Indirect | PassMode::Cast(_) => vec![],
                    PassMode::Direct(s) => vec![load(s.ty, 0)],
                    PassMode::Pair(a, b, offset) => {
                        vec![load(a.ty, 0), load(b.ty, offset.bytes() as i32)]
                    }
                };
                self.builder.ins().return_(&values);
            }
            TerminatorKind::Unreachable => {
                self.builder.ins().trap(TrapCode::user(2).expect("non-zero trap code"));
            }
            TerminatorKind::Drop { place, target, .. } => {
                let target = self.blocks[*target];
                let flag = match place.projection.as_slice() {
                    [] => self.drop_flags.get(place.local).copied(),
                    _ => None,
                };
                if let Some(flag) = flag {
                    let live = self.builder.ins().load(types::I8, MEM, flag, 0);
                    let drop = self.builder.create_block();
                    self.builder.ins().brif(live, drop, &[], target, &[]);
                    self.builder.switch_to_block(drop);
                    let dead = self.builder.ins().iconst(types::I8, 0);
                    self.builder.ins().store(MEM, dead, flag, 0);
                }
                let place = self.place_ref(place)?;
                self.drop_value(place.addr, place.meta, place.ty.ty)?;
                self.builder.ins().jump(target, &[]);
            }
            TerminatorKind::Call { func, args, destination, target, .. } => {
                self.call(func, args, destination, *target)?
            }
            _ => unsupported!("terminator {terminator:?}"),
        }
        Ok(())
    }

    fn call(
        &mut self,
        func: &Operand,
        args: &[Operand],
        destination: &StoredPlace,
        target: Option<BasicBlockId>,
    ) -> Result<()> {
        let (dst, dst_ty) = self.place(destination)?;
        let fn_ty = self.operand_ty(func)?;
        if let TyKind::FnPtr(..) = fn_ty.kind() {
            let (addr, _) = self.operand(func)?;
            let pointer = self.builder.ins().load(PTR, MEM, addr, 0);
            let values = self.operands(args)?;
            self.emit_call(Callee::Pointer(pointer), &values, dst, dst_ty.ty)?;
            return self.finish_call(destination, target);
        }
        let TyKind::FnDef(def, generic_args) = fn_ty.kind() else {
            unsupported!("call of {fn_ty:?}");
        };
        let func = match def.0 {
            CallableDefId::FunctionId(func) => func,
            CallableDefId::StructId(id) => {
                let layout = self.variant_layout(dst, id.into(), generic_args)?;
                self.write_fields(dst, &layout, args)?;
                return self.finish_call(destination, target);
            }
            CallableDefId::EnumVariantId(id) => {
                let layout = self.variant_layout(dst, id.into(), generic_args)?;
                self.write_fields(dst, &layout, args)?;
                return self.finish_call(destination, target);
            }
        };
        let db = self.cg.db;
        let lang_items = self.interner.lang_items();
        if [lang_items.Fn_call, lang_items.FnMut_call_mut, lang_items.FnOnce_call_once]
            .contains(&Some(func))
        {
            self.call_fn_trait(func, generic_args, args, dst, dst_ty.ty)?;
            return self.finish_call(destination, target);
        }
        if [lang_items.DropInPlace, lang_items.DropGlue].contains(&Some(func)) {
            // The body is a placeholder for what the compiler generates.
            let [pointer] = args else {
                unsupported!("drop glue with {} arguments", args.len());
            };
            let ty = generic_args.type_at(0);
            let (src, _) = self.operand(pointer)?;
            let addr = self.builder.ins().load(PTR, MEM, src, 0);
            let meta = is_unsized(ty).then(|| self.builder.ins().load(PTR, MEM, src, 8));
            self.drop_value(addr, meta, ty)?;
            return self.finish_call(destination, target);
        }
        if FunctionSignature::is_intrinsic(db, func)
            && FunctionSignature::of(db, func).name.as_str() == "const_eval_select"
        {
            // `const_eval_select((a, b), const_fn, runtime_fn)` is `runtime_fn(a, b)` at run time.
            let [tuple, _, runtime] = args else {
                unsupported!("const_eval_select with {} arguments", args.len());
            };
            let TyKind::FnDef(runtime, runtime_args) = self.operand_ty(runtime)?.kind() else {
                unsupported!("const_eval_select with a non-fn-item runtime function");
            };
            let CallableDefId::FunctionId(runtime) = runtime.0 else {
                unsupported!("const_eval_select with a constructor");
            };
            let (tuple_addr, tuple_ty) = self.operand(tuple)?;
            let TyKind::Tuple(fields) = tuple_ty.kind() else {
                unsupported!("const_eval_select with non-tuple arguments");
            };
            let layout = self.layout(tuple_ty)?;
            let mut values = Vec::new();
            for (i, ty) in fields.iter().enumerate() {
                let offset = layout.fields.offset(i).bytes() as i64;
                values.push((self.builder.ins().iadd_imm(tuple_addr, offset), ty));
            }
            self.call_direct(runtime, runtime_args, &values, dst, dst_ty.ty)?;
            return self.finish_call(destination, target);
        }
        if FunctionSignature::is_intrinsic(db, func) {
            match self.intrinsic(func, generic_args, args, dst, dst_ty.ty)? {
                Intrinsic::Inline => return self.finish_call(destination, target),
                Intrinsic::Diverges => return Ok(()),
                Intrinsic::Unknown => {
                    let overridden = AttrFlags::query(db, func.into())
                        .contains(AttrFlags::RUSTC_INTRINSIC_MUST_BE_OVERRIDDEN);
                    let signature = FunctionSignature::of(db, func);
                    if overridden || !signature.has_body() {
                        unsupported!("intrinsic `{}`", signature.name.as_str());
                    }
                }
            }
        }
        if let Some(name) = self.cg.link_name(func)
            && let Some(name) = name.strip_prefix("llvm.")
        {
            self.llvm_intrinsic(name, args, dst, dst_ty.ty)?;
            return self.finish_call(destination, target);
        }
        let values = self.operands(args)?;
        self.call_direct(func, generic_args, &values, dst, dst_ty.ty)?;
        self.finish_call(destination, target)
    }

    fn operands(&mut self, operands: &[Operand]) -> Result<Vec<(Value, Ty<'db>)>> {
        operands.iter().map(|it| self.operand(it)).collect()
    }

    /// `Fn::call`, `FnMut::call_mut` or `FnOnce::call_once`. MIR passes the callee first and
    /// then the arguments one by one, not as a tuple.
    fn call_fn_trait(
        &mut self,
        method: FunctionId,
        method_args: GenericArgs<'db>,
        args: &[Operand],
        dst: Value,
        dst_ty: Ty<'db>,
    ) -> Result<()> {
        let Some((callee, rest)) = args.split_first() else {
            unsupported!("Fn-trait call without a callee");
        };
        let (mut addr, mut ty) = self.operand(callee)?;
        // The innermost reference to the callee and the address it is stored at.
        let mut by_ref = None;
        while let TyKind::Ref(_, inner, mutability) = ty.kind() {
            by_ref = Some((addr, ty, mutability));
            if let TyKind::Dynamic(bounds, _) = inner.kind() {
                // Which method is in the vtable depends on the object's trait, not on the
                // trait the call was written against.
                let lang_items = self.interner.lang_items();
                let principal = bounds.principal_def_id().map(|it| it.0);
                let method = match principal {
                    Some(it) if Some(it) == lang_items.Fn => lang_items.Fn_call,
                    Some(it) if Some(it) == lang_items.FnMut => lang_items.FnMut_call_mut,
                    _ => None,
                };
                let Some(method) = method else {
                    unsupported!("Fn-trait call on {inner:?}");
                };
                let values = self.operands(rest)?;
                return self.call_virtual(method, inner, addr, ty, &values, dst, dst_ty);
            }
            addr = self.builder.ins().load(PTR, MEM, addr, 0);
            ty = inner;
        }
        let mut values = self.operands(rest)?;
        match ty.kind() {
            TyKind::FnDef(def, generic_args) => {
                let func = match def.0 {
                    CallableDefId::FunctionId(func) => func,
                    CallableDefId::StructId(_) | CallableDefId::EnumVariantId(_) => {
                        let TyKind::Adt(_, adt_args) = dst_ty.kind() else {
                            unsupported!("constructor of {dst_ty:?}");
                        };
                        let layout = match def.0 {
                            CallableDefId::EnumVariantId(id) => {
                                self.variant_layout(dst, id.into(), adt_args)?
                            }
                            _ => self.layout(dst_ty)?,
                        };
                        for (i, &(src, ty)) in values.iter().enumerate() {
                            let size = self.layout(ty)?.size;
                            let offset = layout.fields.offset(i).bytes() as i64;
                            let field = self.builder.ins().iadd_imm(dst, offset);
                            self.copy(field, src, size);
                        }
                        return Ok(());
                    }
                };
                if FunctionSignature::is_intrinsic(self.cg.db, func) {
                    unsupported!("intrinsic called through an Fn trait");
                }
                self.call_direct(func, generic_args, &values, dst, dst_ty)
            }
            TyKind::FnPtr(..) => {
                let pointer = self.builder.ins().load(PTR, MEM, addr, 0);
                self.emit_call(Callee::Pointer(pointer), &values, dst, dst_ty)
            }
            TyKind::Closure(id, generic_args) => {
                let parent_args =
                    GenericArgs::new_from_slice(generic_args.as_closure().parent_args());
                let body = self
                    .cg
                    .db
                    .monomorphized_mir_body_for_closure(id.0, parent_args.store(), self.env.store())
                    .map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
                if body.param_locals.len() != args.len() {
                    unsupported!("closure called with a tuple of arguments");
                }
                // The body takes the closure by reference or by value depending on its kind,
                // whichever trait method the call went through.
                let self_ty = body.locals[body.param_locals[0]].ty.as_ref();
                let self_arg = if self_ty.as_reference().is_some() {
                    let slot = self.scratch();
                    self.builder.ins().store(MEM, addr, slot, 0);
                    (slot, self_ty)
                } else {
                    (addr, ty)
                };
                values.insert(0, self_arg);
                let def = InstanceDef::Closure(id.0.as_id());
                self.emit_call(Callee::Instance(def, parent_args), &values, dst, dst_ty)
            }
            // A type with its own `Fn` impls, which take the arguments as a tuple.
            TyKind::Adt(..) => {
                let tuple_ty = method_args.type_at(1);
                let layout = self.layout(tuple_ty)?;
                let tuple = self.stack_slot(&layout);
                for (i, &(src, ty)) in values.iter().enumerate() {
                    let size = self.layout(ty)?.size;
                    let offset = layout.fields.offset(i).bytes() as i64;
                    let field = self.builder.ins().iadd_imm(tuple, offset);
                    self.copy(field, src, size);
                }
                let lang_items = self.interner.lang_items();
                let (method, self_arg) = match by_ref {
                    None => (Some(method), (addr, ty)),
                    Some((slot, ref_ty, Mutability::Not)) => (lang_items.Fn_call, (slot, ref_ty)),
                    Some((slot, ref_ty, Mutability::Mut)) => {
                        (lang_items.FnMut_call_mut, (slot, ref_ty))
                    }
                };
                let Some(method) = method else {
                    unsupported!("Fn-trait call on {ty:?}");
                };
                let args = GenericArgs::new_from_slice(&[ty.into(), tuple_ty.into()]);
                self.call_direct(method, args, &[self_arg, (tuple, tuple_ty)], dst, dst_ty)
            }
            _ => unsupported!("Fn-trait call on {ty:?}"),
        }
    }

    /// Calls trait method `func` on the trait object behind the fat pointer at `receiver`.
    #[allow(clippy::too_many_arguments)]
    fn call_virtual(
        &mut self,
        func: FunctionId,
        dyn_ty: Ty<'db>,
        receiver: Value,
        receiver_ty: Ty<'db>,
        rest: &[(Value, Ty<'db>)],
        dst: Value,
        dst_ty: Ty<'db>,
    ) -> Result<()> {
        let receiver_layout = self.layout(receiver_ty)?;
        if !matches!(receiver_layout.backend_repr, BackendRepr::ScalarPair(..))
            || receiver_layout.size.bytes() != 16
        {
            unsupported!("vtable call with receiver {receiver_ty:?}");
        }
        let Some(trait_ref) = self.principal(dyn_ty, dyn_ty) else {
            unsupported!("vtable call on {dyn_ty:?}");
        };
        let Some(slot) = VtableLayout::of(self.interner, trait_ref).method(func) else {
            unsupported!("method missing from the vtable of {dyn_ty:?}");
        };
        let data = self.builder.ins().load(PTR, MEM, receiver, 0);
        let vtable = self.builder.ins().load(PTR, MEM, receiver, 8);
        let pointer = self.builder.ins().load(PTR, MEM, vtable, slot as i32 * 8);
        // The method takes the data pointer alone where the caller holds a fat pointer.
        let thin = self.scratch();
        self.builder.ins().store(MEM, data, thin, 0);
        let thin_ty = Ty::new_imm_ptr(self.interner, Ty::new_unit(self.interner));
        let mut values = vec![(thin, thin_ty)];
        values.extend_from_slice(rest);
        self.emit_call(Callee::Pointer(pointer), &values, dst, dst_ty)
    }

    /// The principal trait of `dyn_ty`, as implemented by `self_ty`.
    fn principal(&self, dyn_ty: Ty<'db>, self_ty: Ty<'db>) -> Option<TraitRef<'db>> {
        let TyKind::Dynamic(bounds, _) = dyn_ty.kind() else {
            return None;
        };
        let principal = bounds.principal()?.with_self_ty(self.interner, self_ty);
        Some(self.interner.instantiate_bound_regions_with_erased(principal))
    }

    /// The type behind the pointer that an unsizing coercion from `ty` to `other` changes.
    fn coerced_pointee(&self, ty: Ty<'db>, other: Ty<'db>) -> Option<Ty<'db>> {
        match (ty.kind(), other.kind()) {
            (TyKind::RawPtr(pointee, _) | TyKind::Ref(_, pointee, _), _) => Some(pointee),
            // `NonNull` holds its pointer as a pattern type.
            (TyKind::Pat(ty, _), TyKind::Pat(other, _)) => self.coerced_pointee(ty, other),
            (TyKind::Adt(def, args), TyKind::Adt(other_def, other_args))
                if def.def_id() == other_def.def_id() =>
            {
                let AdtId::StructId(id) = def.def_id() else {
                    return None;
                };
                // `CoerceUnsized` allows one field to change, `PhantomData` aside.
                let db = self.cg.db;
                let field_types = db.field_types(id.into());
                let mut coerced = field_types.iter().filter_map(|(_, field)| {
                    let ty = field.ty().instantiate(self.interner, args).skip_norm_wip();
                    let other = field.ty().instantiate(self.interner, other_args).skip_norm_wip();
                    let phantom = match ty.kind() {
                        TyKind::Adt(def, _) => match def.def_id() {
                            AdtId::StructId(id) => StructSignature::of(db, id)
                                .flags
                                .contains(StructFlags::IS_PHANTOM_DATA),
                            _ => false,
                        },
                        _ => false,
                    };
                    (ty != other && !phantom).then_some((ty, other))
                });
                let (ty, other) = coerced.next()?;
                if coerced.next().is_some() {
                    return None;
                }
                self.coerced_pointee(ty, other)
            }
            _ => None,
        }
    }

    /// The metadata of a pointer to `from` after unsizing its pointee to `to`. `old` is the
    /// metadata the pointer has now, if it is fat already.
    fn unsize_metadata(&mut self, from: Ty<'db>, to: Ty<'db>, old: Option<Value>) -> Result<Value> {
        match (from.kind(), to.kind()) {
            (TyKind::Array(_, len), TyKind::Slice(_)) => {
                let Some(len) = try_const_usize(self.cg.db, len) else {
                    unsupported!("array with unevaluated length");
                };
                Ok(self.builder.ins().iconst(PTR, len as i64))
            }
            (TyKind::Dynamic(from_bounds, _), TyKind::Dynamic(to_bounds, _)) => {
                let Some(vtable) = old else {
                    unsupported!("upcast of a thin pointer");
                };
                let (source, target) =
                    (from_bounds.principal_def_id(), to_bounds.principal_def_id());
                let Some(target) = target.filter(|_| source != target) else {
                    // Dropping auto traits or the principal: every vtable has the header.
                    return Ok(vtable);
                };
                let Some(trait_ref) = self.principal(from, from) else {
                    unsupported!("upcast of {from:?}");
                };
                match VtableLayout::of(self.interner, trait_ref).upcast(target.0) {
                    Some(Upcast::Same) => Ok(vtable),
                    Some(Upcast::Load(slot)) => {
                        Ok(self.builder.ins().load(PTR, MEM, vtable, slot as i32 * 8))
                    }
                    None => unsupported!("upcast of {from:?} to {to:?}"),
                }
            }
            (_, TyKind::Dynamic(..)) => {
                if is_unsized(from) {
                    unsupported!("unsizing {from:?} to {to:?}");
                }
                let trait_ref = self.principal(to, from);
                let vtable = self.vtable(from, trait_ref)?;
                let vtable = self.cg.module.declare_data_in_func(vtable, self.builder.func);
                Ok(self.builder.ins().symbol_value(PTR, vtable))
            }
            (TyKind::Adt(def, args), TyKind::Adt(to_def, to_args))
                if def.def_id() == to_def.def_id() =>
            {
                // A struct is unsized through its last field.
                let AdtId::StructId(id) = def.def_id() else {
                    unsupported!("unsizing {from:?}");
                };
                let field_types = self.cg.db.field_types(id.into());
                let Some((_, last)) = field_types.iter().last() else {
                    unsupported!("unsizing a struct without fields");
                };
                let from = last.ty().instantiate(self.interner, args).skip_norm_wip();
                let to = last.ty().instantiate(self.interner, to_args).skip_norm_wip();
                self.unsize_metadata(from, to, old)
            }
            _ => unsupported!("unsizing {from:?} to {to:?}"),
        }
    }

    /// The vtable of `ty` for `trait_ref`, or with the header alone when the trait object has
    /// no principal trait.
    fn vtable(&mut self, ty: Ty<'db>, trait_ref: Option<TraitRef<'db>>) -> Result<DataId> {
        let key = (ty.store(), trait_ref.map(|it| (it.def_id.0, it.args.store())));
        if let Some(&id) = self.cg.vtables.get(&key) {
            return Ok(id);
        }
        let backend = |e: cranelift_module::ModuleError| CodegenError::Backend(e.to_string());
        let layout = self.layout(ty)?;
        let slots = trait_ref.map_or_else(Vec::new, |it| VtableLayout::of(self.interner, it).slots);
        let mut bytes = vec![0u8; (vtable::HEADER + slots.len()) * 8];
        bytes[8..16].copy_from_slice(&layout.size.bytes().to_le_bytes());
        bytes[16..24].copy_from_slice(&layout.align.abi.bytes().to_le_bytes());
        let mut data = DataDescription::new();
        data.define(bytes.into_boxed_slice());
        data.set_align(8);
        if has_drop_glue(&self.infcx, ty, self.env.param_env) != DropGlue::None {
            let mut sig = Signature::new(CallConv::SystemV);
            sig.params.push(AbiParam::new(PTR));
            let args = GenericArgs::new_from_slice(&[ty.into()]);
            let glue = self.cg.callee(InstanceDef::DropGlue, args, self.env, &sig)?;
            let glue = self.cg.module.declare_func_in_data(glue, &mut data);
            data.write_function_addr(0, glue);
        }
        for (index, slot) in slots.into_iter().enumerate() {
            let offset = ((vtable::HEADER + index) * 8) as u32;
            match slot {
                Slot::Method(func, trait_ref) => {
                    let method = self.vtable_method(func, trait_ref)?;
                    let method = self.cg.module.declare_func_in_data(method, &mut data);
                    data.write_function_addr(offset, method);
                }
                Slot::Vptr(trait_ref) => {
                    let vptr = self.vtable(ty, Some(trait_ref))?;
                    let vptr = self.cg.module.declare_data_in_data(vptr, &mut data);
                    data.write_data_addr(offset, vptr, 0);
                }
            }
        }
        let n = self.cg.vtables.len();
        let id = self
            .cg
            .module
            .declare_data(&format!(".Lrustd.vtable.{n}"), Linkage::Local, false, false)
            .map_err(backend)?;
        self.cg.module.define_data(id, &data).map_err(backend)?;
        self.cg.vtables.insert(key, id);
        Ok(id)
    }

    /// The function a vtable slot for trait method `func` points to.
    fn vtable_method(&mut self, func: FunctionId, trait_ref: TraitRef<'db>) -> Result<FuncId> {
        let db = self.cg.db;
        let interner = self.interner;
        let self_ty = trait_ref.args.type_at(0);
        let lang_items = interner.lang_items();
        let fn_methods =
            [lang_items.Fn_call, lang_items.FnMut_call_mut, lang_items.FnOnce_call_once];
        if fn_methods.contains(&Some(func)) {
            return self.vtable_fn_method(self_ty, Some(func) == lang_items.FnOnce_call_once);
        }
        // Only lifetimes can be generic in a method that is in a vtable.
        let mut generic = false;
        let args =
            GenericArgs::fill_rest(interner, func.into(), trait_ref.args.iter(), |_, id, _| {
                generic |= !matches!(id, hir_def::GenericParamId::LifetimeParamId(_));
                Region::new_erased(interner).into()
            });
        if generic {
            unsupported!("generic method in a vtable");
        }
        let (callee, callee_args) = db.lookup_impl_method(self.env, func, args);
        let either::Either::Left(callee) = callee else {
            unsupported!("builtin derive method in a vtable");
        };
        if !FunctionSignature::of(db, callee).has_body() {
            unsupported!("vtable of {self_ty:?}: no impl found for a method");
        }
        let body = db
            .monomorphized_mir_body(callee.into(), callee_args.store(), self.env.store())
            .map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
        let Some(&receiver) = body.param_locals.first() else {
            unsupported!("vtable method without a receiver");
        };
        // A by-value `self` reaches the method through a pointer and needs a shim.
        let receiver = self.layout(body.locals[receiver].ty.as_ref())?;
        let thin = match receiver.backend_repr {
            BackendRepr::Scalar(scalar) => matches!(scalar.primitive(), Primitive::Pointer(_)),
            _ => false,
        };
        if !thin {
            unsupported!("vtable method taking `self` by value");
        }
        let sig = self.body_signature(body)?;
        self.cg.callee(InstanceDef::Fn(callee), callee_args, self.env, &sig)
    }

    /// `call`, `call_mut` or `call_once` in the vtable of a closure.
    fn vtable_fn_method(&mut self, ty: Ty<'db>, once: bool) -> Result<FuncId> {
        let TyKind::Closure(id, generic_args) = ty.kind() else {
            unsupported!("Fn-trait vtable of {ty:?}");
        };
        let parent_args = GenericArgs::new_from_slice(generic_args.as_closure().parent_args());
        let body = self
            .cg
            .db
            .monomorphized_mir_body_for_closure(id.0, parent_args.store(), self.env.store())
            .map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
        let sig = self.body_signature(body)?;
        let Some(&receiver) = body.param_locals.first() else {
            unsupported!("closure without a receiver");
        };
        let receiver_ty = body.locals[receiver].ty.as_ref();
        let closure = InstanceDef::Closure(id.0.as_id());
        if receiver_ty.as_reference().is_none() {
            // The body takes the closure by value. Only when that means "through a pointer to
            // memory the body owns" is it what the `call_once` slot is called with.
            let layout = self.layout(receiver_ty)?;
            if !once || !matches!(self.pass_mode(&layout, false)?, PassMode::Indirect) {
                unsupported!("vtable shim for a closure taken by value");
            }
            return self.cg.callee(closure, parent_args, self.env, &sig);
        }
        if once {
            let args = GenericArgs::new_from_slice(&[ty.into()]);
            return self.cg.callee(InstanceDef::CallOnceShim, args, self.env, &sig);
        }
        self.cg.callee(closure, parent_args, self.env, &sig)
    }

    /// The body of a `CallOnceShim`: same signature as the closure body, which borrows the
    /// closure, but the shim owns it and so drops it after the call.
    fn call_once_shim(&mut self, ty: Ty<'db>) -> Result<()> {
        let TyKind::Closure(id, generic_args) = ty.kind() else {
            unsupported!("call_once shim for {ty:?}");
        };
        let parent_args = GenericArgs::new_from_slice(generic_args.as_closure().parent_args());
        let body = self
            .cg
            .db
            .monomorphized_mir_body_for_closure(id.0, parent_args.store(), self.env.store())
            .map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
        let sig = self.body_signature(body)?;
        self.builder.func.signature = sig.clone();
        let entry = self.builder.create_block();
        self.builder.append_block_params_for_function_params(entry);
        self.builder.switch_to_block(entry);
        let params = self.builder.block_params(entry).to_vec();
        let sret = sig.params.first().is_some_and(|it| it.purpose == ArgumentPurpose::StructReturn);
        let Some(&closure) = params.get(sret as usize) else {
            unsupported!("call_once shim for a closure that is not passed");
        };
        let callee =
            self.cg.callee(InstanceDef::Closure(id.0.as_id()), parent_args, self.env, &sig)?;
        let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
        let call = self.builder.ins().call(callee, &params);
        let results = self.builder.inst_results(call).to_vec();
        self.drop_value(closure, None, ty)?;
        self.builder.ins().return_(&results);
        Ok(())
    }

    /// Calls `func` with arguments that live at the given addresses.
    fn call_direct(
        &mut self,
        func: FunctionId,
        generic_args: GenericArgs<'db>,
        args: &[(Value, Ty<'db>)],
        dst: Value,
        dst_ty: Ty<'db>,
    ) -> Result<()> {
        let db = self.cg.db;
        if is_dyn_method(self.interner, self.env.param_env, func, generic_args).is_some() {
            let Some((&(receiver, receiver_ty), rest)) = args.split_first() else {
                unsupported!("vtable call without a receiver");
            };
            let dyn_ty = generic_args.type_at(0);
            return self.call_virtual(func, dyn_ty, receiver, receiver_ty, rest, dst, dst_ty);
        }
        let (callee, callee_args) = db.lookup_impl_method(self.env, func, generic_args);
        let either::Either::Left(callee) = callee else {
            unsupported!("call to builtin derive method");
        };
        self.emit_call(Callee::Instance(InstanceDef::Fn(callee), callee_args), args, dst, dst_ty)
    }

    /// Passes the arguments, which live at the given addresses, the way the Rust ABI wants them
    /// and stores the result in `dst`.
    fn emit_call(
        &mut self,
        callee: Callee<'db>,
        args: &[(Value, Ty<'db>)],
        dst: Value,
        dst_ty: Ty<'db>,
    ) -> Result<()> {
        let ret_layout = self.layout(dst_ty)?;
        let ret_mode = self.pass_mode(&ret_layout, true)?;
        let mut arg_modes = Vec::with_capacity(args.len());
        let mut values = Vec::new();
        if let PassMode::Indirect = ret_mode {
            values.push(dst);
        }
        for &(addr, ty) in args {
            let layout = self.layout(ty)?;
            let mode = self.pass_mode(&layout, false)?;
            arg_modes.push(mode);
            if let PassMode::Cast(ty) = mode {
                values.push(self.load_cast(ty, addr, layout.size));
                continue;
            }
            let mut load = |ty, offset| self.builder.ins().load(ty, MEM, addr, offset);
            match mode {
                PassMode::Ignore | PassMode::Cast(_) => (),
                PassMode::Direct(s) => values.push(load(s.ty, 0)),
                PassMode::Pair(a, b, offset) => {
                    values.extend([load(a.ty, 0), load(b.ty, offset.bytes() as i32)])
                }
                PassMode::Indirect => {
                    // The callee owns the memory it is handed.
                    let copy = self.stack_slot(&layout);
                    self.copy(copy, addr, layout.size);
                    values.push(copy);
                }
            }
        }
        let sig = self.signature(&arg_modes, ret_mode);
        let call = match callee {
            Callee::Instance(def, generic_args) => {
                let callee = self.cg.callee(def, generic_args, self.env, &sig)?;
                let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
                self.builder.ins().call(callee, &values)
            }
            Callee::Pointer(pointer) => {
                let sig = self.builder.import_signature(sig);
                self.builder.ins().call_indirect(sig, pointer, &values)
            }
        };
        let results = self.builder.inst_results(call).to_vec();
        match ret_mode {
            PassMode::Ignore | PassMode::Indirect => (),
            PassMode::Direct(_) => {
                self.builder.ins().store(MEM, results[0], dst, 0);
            }
            PassMode::Cast(_) => self.store_cast(results[0], dst, ret_layout.size),
            PassMode::Pair(_, _, offset) => {
                self.builder.ins().store(MEM, results[0], dst, 0);
                self.builder.ins().store(MEM, results[1], dst, offset.bytes() as i32);
            }
        }
        Ok(())
    }

    fn intrinsic(
        &mut self,
        func: FunctionId,
        generic_args: GenericArgs<'db>,
        args: &[Operand],
        dst: Value,
        dst_ty: Ty<'db>,
    ) -> Result<Intrinsic> {
        let name = FunctionSignature::of(self.cg.db, func).name.as_str();
        match (name, args) {
            (
                "wrapping_add" | "unchecked_add" | "wrapping_sub" | "unchecked_sub"
                | "wrapping_mul" | "unchecked_mul" | "unchecked_div" | "exact_div"
                | "unchecked_rem" | "unchecked_shl" | "unchecked_shr" | "rotate_left"
                | "rotate_right" | "saturating_add" | "saturating_sub" | "three_way_compare",
                [lhs, rhs],
            ) => {
                let (l, scalar) = self.load_scalar(lhs)?;
                let (r, r_scalar) = self.load_scalar(rhs)?;
                let signed = scalar.signed;
                let value = match name {
                    "wrapping_add" | "unchecked_add" => self.builder.ins().iadd(l, r),
                    "wrapping_sub" | "unchecked_sub" => self.builder.ins().isub(l, r),
                    "wrapping_mul" | "unchecked_mul" => self.builder.ins().imul(l, r),
                    "unchecked_div" | "exact_div" if signed => self.builder.ins().sdiv(l, r),
                    "unchecked_div" | "exact_div" => self.builder.ins().udiv(l, r),
                    "unchecked_rem" if signed => self.builder.ins().srem(l, r),
                    "unchecked_rem" => self.builder.ins().urem(l, r),
                    "unchecked_shl" => self.builder.ins().ishl(l, r),
                    "unchecked_shr" if signed => self.builder.ins().sshr(l, r),
                    "unchecked_shr" => self.builder.ins().ushr(l, r),
                    "rotate_left" => self.builder.ins().rotl(l, r),
                    "rotate_right" => self.builder.ins().rotr(l, r),
                    "three_way_compare" => {
                        let (gt, lt) = if signed {
                            (IntCC::SignedGreaterThan, IntCC::SignedLessThan)
                        } else {
                            (IntCC::UnsignedGreaterThan, IntCC::UnsignedLessThan)
                        };
                        let gt = self.builder.ins().icmp(gt, l, r);
                        let lt = self.builder.ins().icmp(lt, l, r);
                        self.builder.ins().isub(gt, lt)
                    }
                    _ => self.saturating(name == "saturating_add", l, r, scalar),
                };
                let _ = r_scalar;
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("add_with_overflow" | "sub_with_overflow" | "mul_with_overflow", [lhs, rhs]) => {
                let (l, scalar) = self.load_scalar(lhs)?;
                let (r, _) = self.load_scalar(rhs)?;
                let wide = name == "mul_with_overflow" && scalar.ty == types::I128;
                let ins = self.builder.ins();
                let (value, overflow) = match (name, scalar.signed) {
                    (_, signed) if wide => self.wide_mul_overflow(l, r, signed)?,
                    ("add_with_overflow", true) => ins.sadd_overflow(l, r),
                    ("add_with_overflow", false) => ins.uadd_overflow(l, r),
                    ("sub_with_overflow", true) => ins.ssub_overflow(l, r),
                    ("sub_with_overflow", false) => ins.usub_overflow(l, r),
                    (_, true) => ins.smul_overflow(l, r),
                    (_, false) => ins.umul_overflow(l, r),
                };
                let layout = self.layout(dst_ty)?;
                self.builder.ins().store(MEM, value, dst, layout.fields.offset(0).bytes() as i32);
                self.builder.ins().store(
                    MEM,
                    overflow,
                    dst,
                    layout.fields.offset(1).bytes() as i32,
                );
            }
            (
                "ctpop" | "ctlz" | "ctlz_nonzero" | "cttz" | "cttz_nonzero" | "bswap"
                | "bitreverse",
                [arg],
            ) => {
                let (value, from) = self.load_scalar(arg)?;
                let value = match name {
                    "ctpop" => self.builder.ins().popcnt(value),
                    "ctlz" | "ctlz_nonzero" => self.builder.ins().clz(value),
                    "cttz" | "cttz_nonzero" => self.builder.ins().ctz(value),
                    "bswap" if from.ty == types::I8 => value,
                    "bswap" => self.builder.ins().bswap(value),
                    _ => self.builder.ins().bitrev(value),
                };
                let to = self.scalar_of(dst_ty)?;
                let value = self.int_cast(value, AbiScalar { signed: false, ..from }, to);
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("size_of" | "align_of" | "min_align_of" | "pref_align_of", []) => {
                let layout = self.layout(generic_args.type_at(0))?;
                let bytes =
                    if name == "size_of" { layout.size.bytes() } else { layout.align.abi.bytes() };
                let value = self.builder.ins().iconst(PTR, bytes as i64);
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("needs_drop", []) => {
                let ty = generic_args.type_at(0);
                let needs = has_drop_glue(&self.infcx, ty, self.env.param_env) != DropGlue::None;
                let value = self.builder.ins().iconst(types::I8, i64::from(needs));
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("ub_checks" | "contract_checks" | "is_val_statically_known", _) => {
                let value = self.builder.ins().iconst(types::I8, 0);
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("overflow_checks", []) => {
                let value = self.builder.ins().iconst(types::I8, 1);
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("transmute" | "transmute_unchecked" | "black_box" | "likely" | "unlikely", [arg]) => {
                let (src, ty) = self.operand(arg)?;
                let size = self.layout(ty)?.size;
                if size != self.layout(dst_ty)?.size {
                    unsupported!("transmute between differently sized types");
                }
                self.copy(dst, src, size);
            }
            // Cranelift's atomics are always sequentially consistent, the strongest ordering.
            ("atomic_load", [ptr]) => {
                let scalar = self.scalar_of(dst_ty)?;
                let (ptr, _) = self.load_scalar(ptr)?;
                let value = self.builder.ins().atomic_load(scalar.ty, MEM, ptr);
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("atomic_store", [ptr, value]) => {
                let (ptr, _) = self.load_scalar(ptr)?;
                let (value, _) = self.load_scalar(value)?;
                self.builder.ins().atomic_store(MEM, value, ptr);
            }
            (
                "atomic_xchg" | "atomic_xadd" | "atomic_xsub" | "atomic_and" | "atomic_nand"
                | "atomic_or" | "atomic_xor" | "atomic_max" | "atomic_min" | "atomic_umax"
                | "atomic_umin",
                [ptr, value],
            ) => {
                let op = match name {
                    "atomic_xchg" => AtomicRmwOp::Xchg,
                    "atomic_xadd" => AtomicRmwOp::Add,
                    "atomic_xsub" => AtomicRmwOp::Sub,
                    "atomic_and" => AtomicRmwOp::And,
                    "atomic_nand" => AtomicRmwOp::Nand,
                    "atomic_or" => AtomicRmwOp::Or,
                    "atomic_xor" => AtomicRmwOp::Xor,
                    "atomic_max" => AtomicRmwOp::Smax,
                    "atomic_min" => AtomicRmwOp::Smin,
                    "atomic_umax" => AtomicRmwOp::Umax,
                    _ => AtomicRmwOp::Umin,
                };
                let scalar = self.scalar_of(dst_ty)?;
                let (ptr, _) = self.load_scalar(ptr)?;
                let (value, _) = self.load_scalar(value)?;
                let old = self.builder.ins().atomic_rmw(scalar.ty, MEM, op, ptr, value);
                self.builder.ins().store(MEM, old, dst, 0);
            }
            ("atomic_cxchg" | "atomic_cxchgweak", [ptr, expected, new]) => {
                let (ptr, _) = self.load_scalar(ptr)?;
                let (expected, _) = self.load_scalar(expected)?;
                let (new, _) = self.load_scalar(new)?;
                let old = self.builder.ins().atomic_cas(MEM, ptr, expected, new);
                let swapped = self.builder.ins().icmp(IntCC::Equal, old, expected);
                let layout = self.layout(dst_ty)?;
                self.builder.ins().store(MEM, old, dst, layout.fields.offset(0).bytes() as i32);
                self.builder.ins().store(MEM, swapped, dst, layout.fields.offset(1).bytes() as i32);
            }
            ("atomic_fence" | "atomic_singlethreadfence", []) => {
                self.builder.ins().fence();
            }
            ("read_via_copy" | "volatile_load" | "unaligned_volatile_load", [ptr]) => {
                let (ptr, _) = self.load_scalar(ptr)?;
                let size = self.layout(dst_ty)?.size;
                self.copy(dst, ptr, size);
            }
            ("write_via_move" | "volatile_store" | "unaligned_volatile_store", [ptr, value]) => {
                let (ptr, _) = self.load_scalar(ptr)?;
                let (src, ty) = self.operand(value)?;
                let size = self.layout(ty)?.size;
                self.copy(ptr, src, size);
            }
            ("write_box_via_move", [boxed, value]) => {
                let (boxed, boxed_ty) = self.operand(boxed)?;
                let (src, ty) = self.operand(value)?;
                let size = self.layout(ty)?.size;
                let ptr = self.builder.ins().load(PTR, MEM, boxed, 0);
                self.copy(ptr, src, size);
                let size = self.layout(boxed_ty)?.size;
                self.copy(dst, boxed, size);
            }
            ("offset" | "arith_offset", [ptr, count]) => {
                let Some(pointee) = self.operand_ty(ptr)?.builtin_deref(true) else {
                    unsupported!("offset of a non-pointer");
                };
                let size = self.layout(pointee)?.size.bytes() as i64;
                let (ptr, _) = self.load_scalar(ptr)?;
                let (count, _) = self.load_scalar(count)?;
                let bytes = self.builder.ins().imul_imm(count, size);
                let value = self.builder.ins().iadd(ptr, bytes);
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("ptr_offset_from" | "ptr_offset_from_unsigned", [a, b]) => {
                let size = self.layout(generic_args.type_at(0))?.size.bytes() as i64;
                let (a, _) = self.load_scalar(a)?;
                let (b, _) = self.load_scalar(b)?;
                let bytes = self.builder.ins().isub(a, b);
                let size = self.builder.ins().iconst(PTR, size);
                let value = self.builder.ins().sdiv(bytes, size);
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("copy" | "copy_nonoverlapping" | "write_bytes", [a, b, count]) => {
                let size = self.layout(generic_args.type_at(0))?.size.bytes() as i64;
                let (a, _) = self.load_scalar(a)?;
                let (b, _) = self.load_scalar(b)?;
                let (count, _) = self.load_scalar(count)?;
                let bytes = self.builder.ins().imul_imm(count, size);
                let config = self.cg.module.target_config();
                match name {
                    // `copy(src, dst, count)`
                    "copy" => self.builder.call_memmove(config, b, a, bytes),
                    "copy_nonoverlapping" => self.builder.call_memcpy(config, b, a, bytes),
                    // `write_bytes(dst, value, count)`
                    _ => self.builder.call_memset(config, a, b, bytes),
                }
            }
            (
                "assume"
                | "cold_path"
                | "forget"
                | "assert_inhabited"
                | "assert_zero_valid"
                | "assert_mem_uninitialized_valid",
                _,
            ) => (),
            ("unreachable" | "abort" | "breakpoint", _) => {
                self.builder.ins().trap(TrapCode::user(2).expect("non-zero trap code"));
                return Ok(Intrinsic::Diverges);
            }
            ("raw_eq" | "compare_bytes", [a, b, ..]) => {
                let (a, _) = self.load_scalar(a)?;
                let (b, _) = self.load_scalar(b)?;
                let len = match args {
                    [_, _, len] => self.load_scalar(len)?.0,
                    _ => {
                        let size = self.layout(generic_args.type_at(0))?.size.bytes();
                        self.builder.ins().iconst(PTR, size as i64)
                    }
                };
                let mut value = self.memcmp(a, b, len)?;
                if name == "raw_eq" {
                    value = self.builder.ins().icmp_imm(IntCC::Equal, value, 0);
                }
                self.builder.ins().store(MEM, value, dst, 0);
            }
            (
                "simd_and" | "simd_or" | "simd_xor" | "simd_add" | "simd_sub" | "simd_mul"
                | "simd_shl" | "simd_shr" | "simd_eq" | "simd_ne" | "simd_lt" | "simd_le"
                | "simd_gt" | "simd_ge",
                [lhs, rhs],
            ) => {
                let (l, ty) = self.operand(lhs)?;
                let (r, _) = self.operand(rhs)?;
                let (lane, count) = self.simd_lanes(ty)?;
                let (out, _) = self.simd_lanes(dst_ty)?;
                let signed = lane.signed;
                for i in 0..count {
                    let offset = (i * lane.ty.bytes() as u64) as i32;
                    let a = self.builder.ins().load(lane.ty, MEM, l, offset);
                    let b = self.builder.ins().load(lane.ty, MEM, r, offset);
                    let cc = match (name, signed) {
                        ("simd_eq", _) => Some(IntCC::Equal),
                        ("simd_ne", _) => Some(IntCC::NotEqual),
                        ("simd_lt", true) => Some(IntCC::SignedLessThan),
                        ("simd_lt", false) => Some(IntCC::UnsignedLessThan),
                        ("simd_le", true) => Some(IntCC::SignedLessThanOrEqual),
                        ("simd_le", false) => Some(IntCC::UnsignedLessThanOrEqual),
                        ("simd_gt", true) => Some(IntCC::SignedGreaterThan),
                        ("simd_gt", false) => Some(IntCC::UnsignedGreaterThan),
                        ("simd_ge", true) => Some(IntCC::SignedGreaterThanOrEqual),
                        ("simd_ge", false) => Some(IntCC::UnsignedGreaterThanOrEqual),
                        _ => None,
                    };
                    let value = match (cc, name) {
                        // A true lane is all ones.
                        (Some(cc), _) => {
                            let flag = self.builder.ins().icmp(cc, a, b);
                            let flag = self.builder.ins().uextend(types::I64, flag);
                            let mask = self.builder.ins().ineg(flag);
                            if out.ty.bits() < 64 {
                                self.builder.ins().ireduce(out.ty, mask)
                            } else {
                                mask
                            }
                        }
                        (None, "simd_and") => self.builder.ins().band(a, b),
                        (None, "simd_or") => self.builder.ins().bor(a, b),
                        (None, "simd_xor") => self.builder.ins().bxor(a, b),
                        (None, "simd_add") => self.builder.ins().iadd(a, b),
                        (None, "simd_sub") => self.builder.ins().isub(a, b),
                        (None, "simd_shl") => self.builder.ins().ishl(a, b),
                        (None, "simd_shr") if signed => self.builder.ins().sshr(a, b),
                        (None, "simd_shr") => self.builder.ins().ushr(a, b),
                        (None, _) => self.builder.ins().imul(a, b),
                    };
                    self.builder.ins().store(MEM, value, dst, (i * out.ty.bytes() as u64) as i32);
                }
            }
            ("simd_select", [mask, on_true, on_false]) => {
                let (mask, mask_ty) = self.operand(mask)?;
                let (on_true, _) = self.operand(on_true)?;
                let (on_false, _) = self.operand(on_false)?;
                let (mask_lane, count) = self.simd_lanes(mask_ty)?;
                let (lane, _) = self.simd_lanes(dst_ty)?;
                for i in 0..count {
                    let offset = (i * lane.ty.bytes() as u64) as i32;
                    let mask_offset = (i * mask_lane.ty.bytes() as u64) as i32;
                    // The top bit of a mask lane decides.
                    let chosen = self.builder.ins().load(mask_lane.ty, MEM, mask, mask_offset);
                    let chosen = self.builder.ins().icmp_imm(IntCC::SignedLessThan, chosen, 0);
                    let a = self.builder.ins().load(lane.ty, MEM, on_true, offset);
                    let b = self.builder.ins().load(lane.ty, MEM, on_false, offset);
                    let value = self.builder.ins().select(chosen, a, b);
                    self.builder.ins().store(MEM, value, dst, offset);
                }
            }
            ("simd_extract", [vector, index]) => {
                let (vector, ty) = self.operand(vector)?;
                let (index, _) = self.load_scalar(index)?;
                let (lane, _) = self.simd_lanes(ty)?;
                let index = self.builder.ins().uextend(PTR, index);
                let offset = self.builder.ins().imul_imm(index, lane.ty.bytes() as i64);
                let addr = self.builder.ins().iadd(vector, offset);
                let value = self.builder.ins().load(lane.ty, MEM, addr, 0);
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("simd_insert", [vector, index, value]) => {
                let (vector, ty) = self.operand(vector)?;
                let (index, _) = self.load_scalar(index)?;
                let (value, _) = self.load_scalar(value)?;
                let (lane, _) = self.simd_lanes(ty)?;
                let size = self.layout(ty)?.size;
                self.copy(dst, vector, size);
                let index = self.builder.ins().uextend(PTR, index);
                let offset = self.builder.ins().imul_imm(index, lane.ty.bytes() as i64);
                let addr = self.builder.ins().iadd(dst, offset);
                self.builder.ins().store(MEM, value, addr, 0);
            }
            ("simd_splat", [arg]) => {
                let (src, _) = self.operand(arg)?;
                let (lane, count) = self.simd_lanes(dst_ty)?;
                let value = self.builder.ins().load(lane.ty, MEM, src, 0);
                for i in 0..count {
                    let offset = (i * lane.ty.bytes() as u64) as i32;
                    self.builder.ins().store(MEM, value, dst, offset);
                }
            }
            ("simd_bitmask", [arg]) => {
                let (src, ty) = self.operand(arg)?;
                let (lane, count) = self.simd_lanes(ty)?;
                let BackendRepr::Scalar(_) = self.layout(dst_ty)?.backend_repr else {
                    unsupported!("simd_bitmask into {dst_ty:?}");
                };
                let to = self.scalar_of(dst_ty)?;
                if count > 64 || lane.ty.bits() > 64 {
                    unsupported!("simd_bitmask of {count} lanes");
                }
                // Bit `i` of the result is the top bit of lane `i`.
                let mut bits = self.builder.ins().iconst(types::I64, 0);
                for i in 0..count {
                    let offset = (i * lane.ty.bytes() as u64) as i32;
                    let value = self.builder.ins().load(lane.ty, MEM, src, offset);
                    let top = self.builder.ins().ushr_imm(value, lane.ty.bits() as i64 - 1);
                    let top = if lane.ty.bits() < 64 {
                        self.builder.ins().uextend(types::I64, top)
                    } else {
                        top
                    };
                    let top = self.builder.ins().ishl_imm(top, i as i64);
                    bits = self.builder.ins().bor(bits, top);
                }
                let from = AbiScalar { ty: types::I64, signed: false };
                let value = self.int_cast(bits, from, to);
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("simd_reduce_all" | "simd_reduce_any", [arg]) => {
                let (src, ty) = self.operand(arg)?;
                let (lane, count) = self.simd_lanes(ty)?;
                let all = name == "simd_reduce_all";
                let mut acc = self.builder.ins().iconst(types::I8, all as i64);
                for i in 0..count {
                    let offset = (i * lane.ty.bytes() as u64) as i32;
                    let value = self.builder.ins().load(lane.ty, MEM, src, offset);
                    let set = self.builder.ins().icmp_imm(IntCC::NotEqual, value, 0);
                    acc = if all {
                        self.builder.ins().band(acc, set)
                    } else {
                        self.builder.ins().bor(acc, set)
                    };
                }
                self.builder.ins().store(MEM, acc, dst, 0);
            }
            (
                "fabs" | "fabsf32" | "fabsf64" | "sqrtf32" | "sqrtf64" | "floorf32" | "floorf64"
                | "ceilf32" | "ceilf64" | "truncf32" | "truncf64" | "round_ties_even_f32"
                | "round_ties_even_f64" | "roundf32" | "roundf64",
                [arg],
            ) => {
                let (value, scalar) = self.load_scalar(arg)?;
                if !scalar.ty.is_float() {
                    unsupported!("intrinsic `{name}` on {:?}", scalar.ty);
                }
                let op = name.trim_end_matches("f32").trim_end_matches("f64").trim_end_matches('_');
                let value = match op {
                    "fabs" => self.builder.ins().fabs(value),
                    "sqrt" => self.builder.ins().sqrt(value),
                    "floor" => self.builder.ins().floor(value),
                    "ceil" => self.builder.ins().ceil(value),
                    "trunc" => self.builder.ins().trunc(value),
                    "round_ties_even" => self.builder.ins().nearest(value),
                    _ => {
                        let libm = if scalar.ty == types::F32 { "roundf" } else { "round" };
                        self.float_libcall(libm, scalar.ty, &[value])?
                    }
                };
                self.builder.ins().store(MEM, value, dst, 0);
            }
            (
                "copysignf32" | "copysignf64" | "minnumf32" | "minnumf64" | "maxnumf32"
                | "maxnumf64" | "powf32" | "powf64" | "fadd_fast" | "fsub_fast" | "fmul_fast"
                | "fdiv_fast" | "fadd_algebraic" | "fsub_algebraic" | "fmul_algebraic"
                | "fdiv_algebraic",
                [a, b],
            ) => {
                let (l, scalar) = self.load_scalar(a)?;
                let (r, _) = self.load_scalar(b)?;
                if !scalar.ty.is_float() {
                    unsupported!("intrinsic `{name}` on {:?}", scalar.ty);
                }
                let single = scalar.ty == types::F32;
                let value = match name {
                    "copysignf32" | "copysignf64" => self.builder.ins().fcopysign(l, r),
                    "minnumf32" | "minnumf64" => {
                        self.float_libcall(if single { "fminf" } else { "fmin" }, scalar.ty, &[l, r])?
                    }
                    "maxnumf32" | "maxnumf64" => {
                        self.float_libcall(if single { "fmaxf" } else { "fmax" }, scalar.ty, &[l, r])?
                    }
                    "powf32" | "powf64" => {
                        self.float_libcall(if single { "powf" } else { "pow" }, scalar.ty, &[l, r])?
                    }
                    "fadd_fast" | "fadd_algebraic" => self.builder.ins().fadd(l, r),
                    "fsub_fast" | "fsub_algebraic" => self.builder.ins().fsub(l, r),
                    "fmul_fast" | "fmul_algebraic" => self.builder.ins().fmul(l, r),
                    _ => self.builder.ins().fdiv(l, r),
                };
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("fmaf32" | "fmaf64" | "fmuladdf32" | "fmuladdf64", [a, b, c]) => {
                let (a, scalar) = self.load_scalar(a)?;
                let (b, _) = self.load_scalar(b)?;
                let (c, _) = self.load_scalar(c)?;
                let value = if name.starts_with("fmuladd") {
                    let product = self.builder.ins().fmul(a, b);
                    self.builder.ins().fadd(product, c)
                } else {
                    let libm = if scalar.ty == types::F32 { "fmaf" } else { "fma" };
                    self.float_libcall(libm, scalar.ty, &[a, b, c])?
                };
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("discriminant_value", [arg]) => {
                let (ptr, _) = self.load_scalar(arg)?;
                let to = self.scalar_of(dst_ty)?;
                let value = self.discriminant(ptr, generic_args.type_at(0), to)?;
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("ptr_metadata", [arg]) => {
                let (src, ty) = self.operand(arg)?;
                if let BackendRepr::ScalarPair(..) = self.layout(ty)?.backend_repr {
                    let meta = self.builder.ins().load(PTR, MEM, src, 8);
                    self.builder.ins().store(MEM, meta, dst, 0);
                }
            }
            ("aggregate_raw_ptr", [data, meta]) => {
                let (data, _) = self.operand(data)?;
                let data = self.builder.ins().load(PTR, MEM, data, 0);
                self.builder.ins().store(MEM, data, dst, 0);
                if let BackendRepr::ScalarPair(..) = self.layout(dst_ty)?.backend_repr {
                    let (meta, _) = self.load_scalar(meta)?;
                    self.builder.ins().store(MEM, meta, dst, 8);
                }
            }
            ("slice_get_unchecked", [slice, index]) => {
                let size = self.layout(generic_args.type_at(2))?.size.bytes() as i64;
                let (slice, _) = self.operand(slice)?;
                let ptr = self.builder.ins().load(PTR, MEM, slice, 0);
                let (index, _) = self.load_scalar(index)?;
                let offset = self.builder.ins().imul_imm(index, size);
                let value = self.builder.ins().iadd(ptr, offset);
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("size_of_val", [arg]) => {
                let (src, _) = self.operand(arg)?;
                let ty = generic_args.type_at(0);
                let value = match ty.kind() {
                    TyKind::Str => self.builder.ins().load(PTR, MEM, src, 8),
                    TyKind::Slice(elem) => {
                        let size = self.layout(elem)?.size.bytes() as i64;
                        let len = self.builder.ins().load(PTR, MEM, src, 8);
                        self.builder.ins().imul_imm(len, size)
                    }
                    TyKind::Dynamic(..) => {
                        // Size and align are the vtable's second and third words.
                        let vtable = self.builder.ins().load(PTR, MEM, src, 8);
                        self.builder.ins().load(PTR, MEM, vtable, 8)
                    }
                    _ => {
                        let size = self.layout(ty)?.size.bytes() as i64;
                        self.builder.ins().iconst(PTR, size)
                    }
                };
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("align_of_val", [arg]) => {
                let (src, _) = self.operand(arg)?;
                let ty = generic_args.type_at(0);
                let value = match ty.kind() {
                    TyKind::Dynamic(..) => {
                        let vtable = self.builder.ins().load(PTR, MEM, src, 8);
                        self.builder.ins().load(PTR, MEM, vtable, 16)
                    }
                    TyKind::Str => self.builder.ins().iconst(PTR, 1),
                    TyKind::Slice(elem) => {
                        let align = self.layout(elem)?.align.abi.bytes() as i64;
                        self.builder.ins().iconst(PTR, align)
                    }
                    _ => {
                        let align = self.layout(ty)?.align.abi.bytes() as i64;
                        self.builder.ins().iconst(PTR, align)
                    }
                };
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("caller_location", []) => {
                let value = self.caller_location()?;
                self.builder.ins().store(MEM, value, dst, 0);
            }
            _ => return Ok(Intrinsic::Unknown),
        }
        Ok(Intrinsic::Inline)
    }

    /// A function `core::arch` declares as `llvm.{name}`: something LLVM implements, with no
    /// body anywhere.
    fn llvm_intrinsic(
        &mut self,
        name: &str,
        args: &[Operand],
        dst: Value,
        dst_ty: Ty<'db>,
    ) -> Result<()> {
        match (name, args) {
            ("x86.sse3.ldu.dq" | "x86.avx.ldu.dq.256", [ptr]) => {
                let (ptr, _) = self.load_scalar(ptr)?;
                let size = self.layout(dst_ty)?.size;
                self.copy(dst, ptr, size);
            }
            ("x86.ssse3.pshuf.b.128" | "x86.avx2.pshuf.b", [table, indices]) => {
                let (table, ty) = self.operand(table)?;
                let (indices, _) = self.operand(indices)?;
                let size = self.layout(ty)?.size.bytes();
                // Each 16-byte half looks up bytes in itself.
                for i in 0..size {
                    let index = self.builder.ins().load(types::I8, MEM, indices, i as i32);
                    let slot = self.builder.ins().band_imm(index, 0x0f);
                    let slot = self.builder.ins().uextend(PTR, slot);
                    let addr = self.builder.ins().iadd(table, slot);
                    let found = self.builder.ins().load(types::I8, MEM, addr, (i & !15) as i32);
                    let zero = self.builder.ins().iconst(types::I8, 0);
                    let cleared = self.builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
                    let value = self.builder.ins().select(cleared, zero, found);
                    self.builder.ins().store(MEM, value, dst, i as i32);
                }
            }
            _ => unsupported!("LLVM intrinsic `{name}`"),
        }
        Ok(())
    }

    /// The lane type and lane count of a vector type. Lanes sit in memory in order.
    fn simd_lanes(&self, ty: Ty<'db>) -> Result<(AbiScalar, u64)> {
        match self.layout(ty)?.backend_repr {
            BackendRepr::SimdVector { element, count } => Ok((self.scalar(element)?, count)),
            _ => unsupported!("vector operation on {ty:?}"),
        }
    }

    /// Cranelift has no 128-bit multiply that reports overflow, so this calls the one in
    /// `compiler_builtins`.
    fn wide_mul_overflow(&mut self, l: Value, r: Value, signed: bool) -> Result<(Value, Value)> {
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.extend([AbiParam::new(types::I128), AbiParam::new(types::I128)]);
        sig.params.push(AbiParam::new(PTR));
        sig.returns.push(AbiParam::new(types::I128));
        let name = if signed { "__rust_i128_mulo" } else { "__rust_u128_mulo" };
        let callee = self.cg.import(name, &sig)?;
        let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
        let overflow = self.scratch();
        let call = self.builder.ins().call(callee, &[l, r, overflow]);
        let value = self.builder.inst_results(call)[0];
        let overflow = self.builder.ins().load(types::I32, MEM, overflow, 0);
        let overflow = self.builder.ins().icmp_imm(IntCC::NotEqual, overflow, 0);
        Ok((value, overflow))
    }

    fn memcmp(&mut self, a: Value, b: Value, len: Value) -> Result<Value> {
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.extend([AbiParam::new(PTR); 3]);
        sig.returns.push(AbiParam::new(types::I32));
        let memcmp = self.cg.import("memcmp", &sig)?;
        let memcmp = self.cg.module.declare_func_in_func(memcmp, self.builder.func);
        let call = self.builder.ins().call(memcmp, &[a, b, len]);
        Ok(self.builder.inst_results(call)[0])
    }

    fn saturating(&mut self, add: bool, l: Value, r: Value, scalar: AbiScalar) -> Value {
        let bits = scalar.ty.bits();
        let ins = self.builder.ins();
        let (value, overflow) = match (add, scalar.signed) {
            (true, true) => ins.sadd_overflow(l, r),
            (true, false) => ins.uadd_overflow(l, r),
            (false, true) => ins.ssub_overflow(l, r),
            (false, false) => ins.usub_overflow(l, r),
        };
        let limit = if scalar.signed {
            // Overflow went past MAX when the left operand is non-negative, past MIN otherwise.
            let min = self.iconst(scalar.ty, 1u128 << (bits - 1));
            let max = self.iconst(scalar.ty, (1u128 << (bits - 1)) - 1);
            let negative = self.builder.ins().icmp_imm(IntCC::SignedLessThan, l, 0);
            self.builder.ins().select(negative, min, max)
        } else if add {
            self.iconst(scalar.ty, u128::MAX >> (128 - bits))
        } else {
            self.iconst(scalar.ty, 0)
        };
        self.builder.ins().select(overflow, limit, value)
    }

    fn int_cast(&mut self, value: Value, from: AbiScalar, to: AbiScalar) -> Value {
        if to.ty.bits() < from.ty.bits() {
            self.builder.ins().ireduce(to.ty, value)
        } else if to.ty.bits() == from.ty.bits() {
            value
        } else if from.signed {
            self.builder.ins().sextend(to.ty, value)
        } else {
            self.builder.ins().uextend(to.ty, value)
        }
    }

    fn set_drop_flag(&mut self, place: &StoredPlace, live: bool) {
        if !place.projection.as_slice().is_empty() {
            return;
        }
        if let Some(&flag) = self.drop_flags.get(place.local) {
            let live = self.builder.ins().iconst(types::I8, live as i64);
            self.builder.ins().store(MEM, live, flag, 0);
        }
    }

    /// Drops the value at `addr` in place.
    fn drop_value(&mut self, addr: Value, meta: Option<Value>, ty: Ty<'db>) -> Result<()> {
        // Field types can still be projections such as `<str as ToOwned>::Owned`.
        let ty = crate::next_solver::obligation_ctxt::ObligationCtxt::new(&self.infcx)
            .structurally_normalize_ty(
                &crate::next_solver::infer::traits::ObligationCause::dummy(),
                self.env.param_env,
                ty,
            )
            .unwrap_or(ty);
        if has_drop_glue(&self.infcx, ty, self.env.param_env) == DropGlue::None {
            return Ok(());
        }
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.push(AbiParam::new(PTR));
        let mut values = vec![addr];
        if is_unsized(ty) {
            let Some(meta) = meta else {
                unsupported!("drop of unsized {ty:?} without metadata");
            };
            sig.params.push(AbiParam::new(PTR));
            values.push(meta);
        }
        let args = GenericArgs::new_from_slice(&[ty.into()]);
        let callee = self.cg.callee(InstanceDef::DropGlue, args, self.env, &sig)?;
        let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
        self.builder.ins().call(callee, &values);
        Ok(())
    }

    /// The body of `drop_in_place::<ty>`.
    fn drop_glue(&mut self, ty: Ty<'db>) -> Result<()> {
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.push(AbiParam::new(PTR));
        if is_unsized(ty) {
            sig.params.push(AbiParam::new(PTR));
        }
        self.builder.func.signature = sig;
        let entry = self.builder.create_block();
        self.builder.append_block_params_for_function_params(entry);
        self.builder.switch_to_block(entry);
        let params = self.builder.block_params(entry).to_vec();
        self.drop_contents(params[0], params.get(1).copied(), ty)?;
        self.builder.ins().return_(&[]);
        Ok(())
    }

    fn drop_contents(&mut self, addr: Value, meta: Option<Value>, ty: Ty<'db>) -> Result<()> {
        let db = self.cg.db;
        match ty.kind() {
            TyKind::Adt(adt_def, subst) => {
                if adt_def.is_box() {
                    // The compiler drops the pointee; `Box`'s `Drop` impl only frees the memory.
                    let pointee = subst.type_at(0);
                    let pointer = self.builder.ins().load(PTR, MEM, addr, 0);
                    let pointee_meta =
                        is_unsized(pointee).then(|| self.builder.ins().load(PTR, MEM, addr, 8));
                    self.drop_value(pointer, pointee_meta, pointee)?;
                }
                if adt_def.destructor(self.interner).is_some() {
                    self.call_drop_impl(addr, ty)?;
                }
                let layout = self.layout(ty)?;
                match adt_def.def_id() {
                    AdtId::UnionId(_) => (),
                    AdtId::StructId(id) => {
                        let manually_drop = StructSignature::of(db, id)
                            .flags
                            .contains(StructFlags::IS_MANUALLY_DROP);
                        if !manually_drop {
                            self.drop_fields(addr, &layout, id.into(), subst)?;
                        }
                    }
                    AdtId::EnumId(id) => match &layout.variants {
                        Variants::Empty => (),
                        Variants::Single { index } => {
                            let variant = id
                                .enum_variants(db)
                                .variants
                                .values()
                                .map(|&(variant, _)| variant)
                                .find(|variant| variant.index(db) == index.0);
                            if let Some(variant) = variant {
                                self.drop_fields(addr, &layout, variant.into(), subst)?;
                            }
                        }
                        Variants::Multiple { variants, .. } => {
                            let wide = AbiScalar { ty: types::I64, signed: true };
                            let discr = self.discriminant(addr, ty, wide)?;
                            let done = self.builder.create_block();
                            let mut switch = Switch::new();
                            let mut arms = Vec::new();
                            for &(variant, _) in id.enum_variants(db).variants.values() {
                                let value = db.const_eval_discriminant(variant).map_err(|e| {
                                    CodegenError::Frontend(format!("discriminant: {e:?}"))
                                })?;
                                let block = self.builder.create_block();
                                switch.set_entry(value as i64 as u64 as u128, block);
                                arms.push((block, variant));
                            }
                            switch.emit(&mut self.builder, discr, done);
                            for (block, variant) in arms {
                                self.builder.switch_to_block(block);
                                let layout = &variants[RustcEnumVariantIdx(variant.index(db))];
                                self.drop_fields(addr, layout, variant.into(), subst)?;
                                self.builder.ins().jump(done, &[]);
                            }
                            self.builder.switch_to_block(done);
                        }
                    },
                }
            }
            TyKind::Tuple(tys) => {
                let layout = self.layout(ty)?;
                for (i, field) in tys.iter().enumerate() {
                    let offset = layout.fields.offset(i).bytes() as i64;
                    let field_addr = self.builder.ins().iadd_imm(addr, offset);
                    self.drop_value(field_addr, None, field)?;
                }
            }
            TyKind::Closure(_, args) => {
                let TyKind::Tuple(tys) = args.as_closure().tupled_upvars_ty().kind() else {
                    unsupported!("drop glue for a closure without tupled upvars");
                };
                let layout = self.layout(ty)?;
                for (i, field) in tys.iter().enumerate() {
                    let offset = layout.fields.offset(i).bytes() as i64;
                    let field_addr = self.builder.ins().iadd_imm(addr, offset);
                    self.drop_value(field_addr, None, field)?;
                }
            }
            TyKind::Array(element, len) => {
                let Some(len) = try_const_usize(db, len) else {
                    unsupported!("array with unevaluated length");
                };
                let len = self.builder.ins().iconst(PTR, len as i64);
                self.drop_elements(addr, len, element)?;
            }
            TyKind::Slice(element) => {
                let Some(len) = meta else {
                    unsupported!("drop of a slice without a length");
                };
                self.drop_elements(addr, len, element)?;
            }
            TyKind::Dynamic(..) => {
                let Some(vtable) = meta else {
                    unsupported!("drop of `dyn` without a vtable");
                };
                // The first vtable entry is `drop_in_place`, null when there is nothing to drop.
                let glue = self.builder.ins().load(PTR, MEM, vtable, 0);
                let call = self.builder.create_block();
                let done = self.builder.create_block();
                self.builder.ins().brif(glue, call, &[], done, &[]);
                self.builder.switch_to_block(call);
                let mut sig = Signature::new(CallConv::SystemV);
                sig.params.push(AbiParam::new(PTR));
                let sig = self.builder.import_signature(sig);
                self.builder.ins().call_indirect(sig, glue, &[addr]);
                self.builder.ins().jump(done, &[]);
                self.builder.switch_to_block(done);
            }
            _ => unsupported!("drop glue for {ty:?}"),
        }
        Ok(())
    }

    fn drop_fields(
        &mut self,
        addr: Value,
        layout: &Layout,
        variant: VariantId,
        subst: GenericArgs<'db>,
    ) -> Result<()> {
        let field_types = self.cg.db.field_types(variant);
        for (field, ty) in field_types.iter() {
            let ty = ty.ty().instantiate(self.interner, subst).skip_norm_wip();
            if is_unsized(ty) {
                unsupported!("drop glue for an unsized field");
            }
            let offset = layout.fields.offset(u32::from(field.into_raw()) as usize).bytes();
            let field_addr = self.builder.ins().iadd_imm(addr, offset as i64);
            self.drop_value(field_addr, None, ty)?;
        }
        Ok(())
    }

    fn drop_elements(&mut self, addr: Value, len: Value, element: Ty<'db>) -> Result<()> {
        let size = self.layout(element)?.size.bytes() as i64;
        let header = self.builder.create_block();
        let body = self.builder.create_block();
        let done = self.builder.create_block();
        self.builder.append_block_param(header, PTR);
        let zero = self.builder.ins().iconst(PTR, 0);
        self.builder.ins().jump(header, &[zero.into()]);
        self.builder.switch_to_block(header);
        let index = self.builder.block_params(header)[0];
        let more = self.builder.ins().icmp(IntCC::UnsignedLessThan, index, len);
        self.builder.ins().brif(more, body, &[], done, &[]);
        self.builder.switch_to_block(body);
        let offset = self.builder.ins().imul_imm(index, size);
        let element_addr = self.builder.ins().iadd(addr, offset);
        self.drop_value(element_addr, None, element)?;
        let next = self.builder.ins().iadd_imm(index, 1);
        self.builder.ins().jump(header, &[next.into()]);
        self.builder.switch_to_block(done);
        Ok(())
    }

    /// Calls `<ty as Drop>::drop` on the value at `addr`.
    fn call_drop_impl(&mut self, addr: Value, ty: Ty<'db>) -> Result<()> {
        let Some(drop_fn) = self.interner.lang_items().Drop_drop else {
            unsupported!("no `Drop::drop` lang item");
        };
        let reference =
            Ty::new_ref(self.interner, Region::new_erased(self.interner), ty, Mutability::Mut);
        let slot = self.scratch();
        self.builder.ins().store(MEM, addr, slot, 0);
        let args = GenericArgs::new_from_slice(&[ty.into()]);
        let unit = Ty::new_unit(self.interner);
        self.call_direct(drop_fn, args, &[(slot, reference)], slot, unit)
    }

    fn finish_call(
        &mut self,
        destination: &StoredPlace,
        target: Option<BasicBlockId>,
    ) -> Result<()> {
        self.set_drop_flag(destination, true);
        match target {
            Some(target) => self.builder.ins().jump(self.blocks[target], &[]),
            None => self.builder.ins().trap(TrapCode::user(2).expect("non-zero trap code")),
        };
        Ok(())
    }
}
