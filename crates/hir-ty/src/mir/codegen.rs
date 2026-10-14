//! rustd spike: lower RA MIR to native code with Cranelift.
//!
//! Every local lives in memory. Calls follow rustc's (unstable) Rust ABI so the
//! output can be linked against rustc-compiled objects.

use std::fmt;

use base_db::Crate;
use cranelift_codegen::{
    ir::{
        self, AbiParam, ArgumentPurpose, AtomicRmwOp, InstBuilder, MemFlagsData, Signature,
        StackSlotData, StackSlotKind, TrapCode, Type, Value,
        condcodes::{FloatCC, IntCC},
        types,
    },
    isa::{self, CallConv},
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch};
use cranelift_module::{
    DataDescription, DataId, FuncId, Linkage, Module, ModuleReloc, default_libcall_names,
};
use cranelift_object::{ObjectBuilder, ObjectModule};
use hir_def::{
    AdtId, DefWithBodyId, FunctionId, HasModule, ItemContainerId, Lookup, StaticId, TraitId,
    VariantId,
    attrs::AttrFlags,
    expr_store::Body,
    hir::ExprId,
    layout::{BackendRepr, Primitive, Scalar, TagEncoding, Variants},
    signatures::{FunctionSignature, StaticFlags, StaticSignature, StructFlags, StructSignature},
};
use hir_expand::{FileRange, HirFileId, InFile, MacroDefKind, builtin::BuiltinFnLikeExpander};
use la_arena::ArenaMap;
use rustc_abi::{ExternAbi, Integer, Size};
use rustc_ast_ir::Mutability;
use rustc_hash::{FxHashMap, FxHashSet};
use rustc_index::Idx;
use rustc_type_ir::UintTy;
use rustc_type_ir::inherent::{
    AdtDef as _, BoundExistentialPredicates as _, GenericArgs as _, IntoKind, SliceLike, Ty as _,
};
use salsa::plumbing::{AsId, FromId};
use syntax::{AstNode, SyntaxKind, SyntaxNode, TextRange, ast};
use triomphe::Arc;

use crate::{
    CallableDefId, InferBodyId, InferenceResult, MemoryMap, ParamEnvAndCrate,
    consteval::try_const_usize,
    db::HirDatabase,
    db::{GeneralConstId, InternedClosureId, InternedCoroutineId},
    drop::{DropGlue, has_drop_glue},
    infer::PointerCast,
    layout::{CoroutineFrame, Layout, RustcEnumVariantIdx},
    method_resolution::{is_dyn_method, lookup_impl_const},
    next_solver::{
        Allocation, AllocationData, ConstKind, DbInterner, GenericArgs, Region, StoredGenericArgs,
        StoredTy, TraitRef, Ty, TyKind, TypingMode, UnevaluatedConst, ValTree,
        infer::{DbInternerInferExt, InferCtxt},
    },
    utils::detect_variant_from_bytes,
};

use self::unwind::UnwindContext;
use self::vtable::{Slot, Upcast, VtableLayout};
use super::{
    AggregateKind, BasicBlockId, BinOp, CastKind, LocalId, MirBody, MirSpan, Operand, OperandKind,
    PlaceTy, ProjectionElem, Rvalue, StatementKind, StoredPlace, TerminatorKind, UnOp,
    eval::interpret_mir, monomorphized_promoted_mir_body_query, return_slot,
};

mod type_id;
mod unwind;
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
    /// The path of an item, e.g. `core::any::TypeId`.
    fn item(&self, item: hir_def::ModuleDefId) -> Option<String>;
    /// The name rustc gives a source file in panic messages.
    fn source_path(&self, file: base_db::FileId) -> Option<String>;
    /// rustc's `StableCrateId` of a crate, which its symbols carry.
    fn stable_crate_id(&self, krate: Crate) -> Option<u64>;
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
    DivByZero,
    RemByZero,
}

/// How a value crosses a call boundary in rustc's Rust ABI.
#[derive(Debug, Clone, Copy)]
enum PassMode {
    Ignore,
    Direct(AbiScalar),
    Pair(AbiScalar, AbiScalar, Size),
    /// Small aggregate passed as one integer register.
    Cast(Type),
    /// Aggregate the C ABI splits over two registers, the second holding what lies eight
    /// bytes in.
    CastPair(Type, Type),
    Indirect,
    /// The C ABI's by-value aggregate in memory: the caller copies this many bytes into its
    /// outgoing arguments, which Cranelift does given a pointer.
    OnStack(u32),
}

/// How every value of a call crosses the boundary.
struct FnAbi {
    ret: PassMode,
    args: Vec<PassMode>,
}

/// What the System V x86-64 ABI calls the class of an eightbyte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RegClass {
    Int,
    Sse,
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
        denied: &FxHashSet<FunctionId>,
        known: &mut Rejections,
    ) -> Result<CodegenUnit> {
        // FIXME: a rejection outlives an edit that would lift it. Dropping them here costs one
        // full pass per rejected call chain, and they go away with the rustc fallback.
        let Rejections { rejected, fingerprints, code, stats } = known;
        (code.hits, code.misses) = (0, 0);
        let track = std::env::var_os("RUSTD_STATS").is_some();
        let mut own_statics = roots.first().map(|root| root.krate(db));
        // Roots given up on because a function left to rustc handles the same future.
        let mut left = FxHashSet::default();
        let mut rounds = 0;
        loop {
            rounds += 1;
            let mut cg = ObjectCodegen::new(db, symbols, rejected, code, own_statics)?;
            cg.track = track;
            let mut compiled = Vec::new();
            let mut skipped = Vec::new();
            for &root in roots {
                if denied.contains(&root) {
                    skipped.push((
                        root,
                        CodegenError::Unsupported("left to rustc on request".to_owned()),
                    ));
                    continue;
                }
                if left.contains(&root) {
                    let reason = "shares an `async` block with a function left to rustc";
                    skipped.push((root, CodegenError::Unsupported(reason.to_owned())));
                    continue;
                }
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
            if failed.is_empty() && from_rustc {
                let mut theirs = Some(FxHashSet::default());
                for (root, e) in &skipped {
                    if !matches!(e, CodegenError::NoSymbol) {
                        let blocks = ObjectCodegen::async_blocks(db, *root);
                        theirs = theirs.zip(blocks).map(|(mut all, blocks)| {
                            all.extend(blocks);
                            all
                        });
                    }
                }
                let before = left.len();
                left.extend(compiled.iter().map(|it| it.0).filter(|&root| {
                    match (ObjectCodegen::async_blocks(db, root), &theirs) {
                        (Some(ours), Some(theirs)) => !ours.is_disjoint(theirs),
                        (Some(ours), None) => !ours.is_empty(),
                        (None, _) => true,
                    }
                }));
                if left.len() != before {
                    continue;
                }
            }
            if failed.is_empty() {
                let instances = cg.instances.len();
                *stats = BuildStats {
                    mir: cg.mir_time,
                    bodies: cg.seen.len(),
                    changed: 0,
                    reused: cg.code.hits,
                    compiled: cg.code.misses,
                    phases: cg.phases,
                    rounds,
                };
                for (key, print) in cg.seen.drain(..) {
                    if fingerprints.insert(key, print) != Some(print) {
                        stats.changed += 1;
                    }
                }
                if let (Some(path), Some(layouts)) =
                    (std::env::var_os("RUSTD_LAYOUTS"), cg.layouts.take())
                {
                    let lines = layouts
                        .into_inner()
                        .into_iter()
                        .map(|(ty, (size, align))| format!("{ty:?}\t{size}\t{align}\n"))
                        .collect::<String>();
                    // Appended: isolated builds of single functions go through here too.
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                        .and_then(|mut file| std::io::Write::write_all(&mut file, lines.as_bytes()))
                        .map_err(|e| CodegenError::Backend(format!("layout log: {e}")))?;
                }
                let start = std::time::Instant::now();
                let object = cg.finish()?;
                stats.phases.object = start.elapsed();
                return Ok(CodegenUnit { object, compiled, skipped, instances });
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
    /// Where the last round of the build spent its time.
    pub phases: Phases,
    /// How many times codegen started over because a callee turned out not to compile.
    pub rounds: usize,
}

/// Time spent in each step below MIR.
#[derive(Default, Clone, Copy)]
pub struct Phases {
    /// Hashing each body's MIR, which only happens for these measurements.
    pub fingerprint: std::time::Duration,
    /// Building Cranelift IR from MIR.
    pub ir: std::time::Duration,
    /// Hashing the IR, and on a miss compiling it; then handing the code to the object.
    pub emit: std::time::Duration,
    /// Writing the object file.
    pub object: std::time::Duration,
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
    /// An `InternedCoroutineId`: the `Future::poll` of an `async` block. Polled with a null
    /// context, it drops whatever the block holds instead.
    Coroutine(salsa::Id),
    /// `drop_in_place` for the type that is the instance's only generic argument.
    DropGlue,
    /// `FnOnce::call_once` in the vtable of the closure type that is the instance's only
    /// generic argument: calls the closure through the pointer it is handed, then drops it.
    CallOnceShim,
    /// What a closure that captures nothing coerces to as a function pointer: calls the
    /// closure type that is the instance's only generic argument, which borrows a receiver,
    /// with a dangling one.
    FnPointerShim,
    /// A function to point at for the fn item or fn pointer type that is the instance's only
    /// generic argument, where what the type names has no address of its own: a constructor,
    /// or a trait method called on a `dyn`. With `receiver`, the function is what the
    /// `Fn`-trait vtable of the type holds, and so also takes the `self` it has no use for.
    ReifyShim {
        receiver: bool,
    },
}

/// What a call transfers control to.
enum Callee<'db> {
    Instance(InstanceDef, GenericArgs<'db>),
    Pointer(Value, ExternAbi),
}

struct ObjectCodegen<'a, 'db> {
    /// Size and alignment of every type laid out, collected when `RUSTD_LAYOUTS` names a file
    /// to write them to. The harness compares them with rustc's.
    layouts: Option<std::cell::RefCell<FxHashMap<Ty<'db>, (u64, u64)>>>,
    db: &'db dyn HirDatabase,
    symbols: &'a dyn SymbolResolver,
    module: ObjectModule,
    unwind: UnwindContext,
    imports: FxHashMap<String, FuncId>,
    instances: FxHashMap<Instance, FuncId>,
    pending: Vec<(Instance, ParamEnvAndCrate<'db>, FuncId)>,
    /// Instances that failed in an earlier round, with the reason.
    rejected: &'a FxHashMap<Instance, String>,
    code: &'a mut CodeCache,
    track: bool,
    mir_time: std::time::Duration,
    phases: Phases,
    seen: Vec<(Instance, u64)>,
    vtables: FxHashMap<(StoredTy, Option<(TraitId, StoredGenericArgs)>), DataId>,
    /// The crate whose writable statics we define ourselves when rustc exports no symbol for
    /// them. Only sound when every function of that crate is ours.
    own_statics: Option<Crate>,
    statics: FxHashMap<StaticId, DataId>,
    /// Statics nothing exports and nothing can write to, which we give a copy of the value.
    frozen_statics: FxHashMap<StaticId, DataId>,
    consts: FxHashMap<ConstBlob, DataId>,
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
        flags.set("tls_model", "elf_gd").map_err(|e| backend(&e))?;
        flags.set("opt_level", "none").map_err(|e| backend(&e))?;
        flags.set("enable_llvm_abi_extensions", "true").map_err(|e| backend(&e))?;
        let isa = isa::lookup_by_name("x86_64-unknown-linux-gnu")
            .map_err(|e| backend(&e))?
            .finish(settings::Flags::new(flags))
            .map_err(|e| backend(&e))?;
        let builder =
            ObjectBuilder::new(isa, "rustd", default_libcall_names()).map_err(|e| backend(&e))?;
        let mut module = ObjectModule::new(builder);
        let unwind = UnwindContext::new(&mut module).map_err(CodegenError::Backend)?;
        Ok(ObjectCodegen {
            db,
            symbols,
            module,
            unwind,
            imports: FxHashMap::default(),
            instances: FxHashMap::default(),
            pending: Vec::new(),
            rejected,
            code,
            track: false,
            layouts: std::env::var_os("RUSTD_LAYOUTS").map(|_| Default::default()),
            mir_time: Default::default(),
            phases: Phases::default(),
            seen: Vec::new(),
            vtables: FxHashMap::default(),
            own_statics,
            statics: FxHashMap::default(),
            frozen_statics: FxHashMap::default(),
            consts: FxHashMap::default(),
            builder_ctx: FunctionBuilderContext::new(),
            location_counter: 0,
        })
    }

    fn finish(self) -> Result<Vec<u8>> {
        let mut product = self.module.finish();
        self.unwind.emit(&mut product).map_err(CodegenError::Backend)?;
        product.emit().map_err(|e| CodegenError::Backend(e.to_string()))
    }

    /// The body of a non-generic function, with the types it only knows as `impl Trait`
    /// resolved.
    fn root_body(
        db: &'db dyn HirDatabase,
        func: FunctionId,
    ) -> Result<(&'db MirBody<'db>, ParamEnvAndCrate<'db>)> {
        let owner: InferBodyId<'db> = func.into();
        let body = db.mir_body(owner);
        let body = body.map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
        let krate = owner.module(db).krate(db);
        // Not the function's own bounds: with nothing generic they are bounds on concrete
        // types, which hold through impls, and the calls have to resolve to those.
        let env = ParamEnvAndCrate { param_env: crate::next_solver::ParamEnv::empty(), krate };
        // The future an `async fn` returns is `impl Future` to the body that calls it.
        let opaque = body.locals.iter().any(|(_, local)| {
            rustc_type_ir::TypeVisitableExt::has_type_flags(
                &local.ty.as_ref(),
                rustc_type_ir::TypeFlags::HAS_TY_OPAQUE,
            )
        });
        let body = if opaque {
            db.monomorphized_mir_body(owner, GenericArgs::new_from_slice(&[]).store(), env.store())
                .map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?
        } else {
            body
        };
        Ok((body, env))
    }

    /// The `async` blocks whose futures the body of `func` holds, or `None` when the body
    /// can't be read. rustc lays a future out differently, so every function that touches one
    /// has to come from the same compiler.
    fn async_blocks(db: &'db dyn HirDatabase, func: FunctionId) -> Option<FxHashSet<salsa::Id>> {
        struct Collector<'db>(Vec<Ty<'db>>);
        impl<'db> rustc_type_ir::TypeVisitor<DbInterner<'db>> for Collector<'db> {
            type Result = ();

            fn visit_ty(&mut self, ty: Ty<'db>) {
                if let TyKind::Coroutine(..) | TyKind::Closure(..) = ty.kind() {
                    self.0.push(ty);
                }
                rustc_type_ir::TypeSuperVisitable::super_visit_with(&ty, self)
            }
        }
        let (body, env) = Self::root_body(db, func).ok()?;
        let (mut found, mut seen) = (FxHashSet::default(), FxHashSet::default());
        let mut bodies = vec![body];
        while let Some(body) = bodies.pop() {
            let mut collector = Collector(Vec::new());
            for (_, local) in body.locals.iter() {
                rustc_type_ir::TypeVisitable::visit_with(&local.ty.as_ref(), &mut collector);
            }
            for ty in collector.0 {
                if !seen.insert(ty) {
                    continue;
                }
                let body = match ty.kind() {
                    TyKind::Coroutine(id, args) => {
                        found.insert(id.0.as_id());
                        let parent = GenericArgs::new_from_slice(args.as_coroutine().parent_args());
                        db.monomorphized_mir_body_for_coroutine(id.0, parent.store(), env.store())
                    }
                    TyKind::Closure(id, args) => {
                        let parent = GenericArgs::new_from_slice(args.as_closure().parent_args());
                        db.monomorphized_mir_body_for_closure(id.0, parent.store(), env.store())
                    }
                    _ => continue,
                };
                bodies.push(body.ok()?);
            }
        }
        Some(found)
    }

    /// Compiles a non-generic function and exports it under its rustc symbol name.
    fn define_function(&mut self, func: FunctionId) -> Result<String> {
        let db = self.db;
        let start = std::time::Instant::now();
        let body = db.mir_body(func.into());
        self.mir_time += start.elapsed();
        let body = body.map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
        let root =
            Instance { def: InstanceDef::Fn(func), args: GenericArgs::new_from_slice(&[]).store() };
        self.fingerprint(root, body);
        let Some(symbol) = self.symbols.function(func) else {
            return Err(CodegenError::NoSymbol);
        };
        let (body, env) = Self::root_body(db, func)?;
        if std::env::var("RUSTD_DUMP_MIR")
            .is_ok_and(|want| FunctionSignature::of(db, func).name.as_str() == want)
        {
            dump_mir(body);
        }
        let abi = FunctionSignature::of(db, func).abi;
        let start = std::time::Instant::now();
        let ctx = self.compile_body(
            body,
            env,
            track_caller(db, func),
            abi,
            GenericArgs::new_from_slice(&[]).store(),
        );
        self.phases.ir += start.elapsed();
        let mut ctx = ctx?;
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
        let start = std::time::Instant::now();
        let mut hasher = rustc_hash::FxHasher::default();
        format!("{:?}{:?}", body.basic_blocks, body.locals).hash(&mut hasher);
        self.seen.push((instance, hasher.finish()));
        self.phases.fingerprint += start.elapsed();
    }

    /// Compiles the instances requested by calls. Returns the ones that failed.
    fn define_instances(&mut self) -> Vec<(Instance, String)> {
        let mut failed = Vec::new();
        while let Some((instance, env, id)) = self.pending.pop() {
            if let Err(e) = self.define_instance(&instance, env, id)
                && self.forward_to_rustc(&instance, id).is_none()
            {
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
            let mut ctx = self.build(env, None, false, |fx| fx.drop_glue(ty))?;
            if std::env::var_os("RUSTD_DUMP_CLIF").is_some() {
                eprintln!("drop glue for {ty:?}\n{}", ctx.func.display());
            }
            return self.emit(id, &mut ctx);
        }
        if let InstanceDef::CallOnceShim = instance.def {
            let ty = instance.args.as_ref().type_at(0);
            let mut ctx = self.build(env, None, false, |fx| fx.call_once_shim(ty))?;
            return self.emit(id, &mut ctx);
        }
        if let InstanceDef::FnPointerShim = instance.def {
            let ty = instance.args.as_ref().type_at(0);
            let mut ctx = self.build(env, None, false, |fx| fx.fn_pointer_shim(ty))?;
            return self.emit(id, &mut ctx);
        }
        if let InstanceDef::ReifyShim { receiver } = instance.def {
            let ty = instance.args.as_ref().type_at(0);
            let mut ctx = self.build(env, None, false, |fx| fx.reify_shim(ty, receiver))?;
            return self.emit(id, &mut ctx);
        }
        let start = std::time::Instant::now();
        let body = match instance.def {
            InstanceDef::DropGlue
            | InstanceDef::CallOnceShim
            | InstanceDef::FnPointerShim
            | InstanceDef::ReifyShim { .. } => {
                unreachable!()
            }
            InstanceDef::Fn(func) => {
                self.db.monomorphized_mir_body(func.into(), instance.args.clone(), env.store())
            }
            InstanceDef::Closure(id) => self.db.monomorphized_mir_body_for_closure(
                InternedClosureId::from_id(id),
                instance.args.clone(),
                env.store(),
            ),
            InstanceDef::Coroutine(id) => self.db.monomorphized_mir_body_for_coroutine(
                InternedCoroutineId::from_id(id),
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
            dump_mir(body);
        }
        if matches!(instance.def, InstanceDef::Closure(_) | InstanceDef::Coroutine(_))
            && std::env::var("RUSTD_DUMP_MIR").is_ok_and(|want| want == "{closure}")
        {
            dump_mir(body);
        }
        let (tracked, abi) = match instance.def {
            InstanceDef::Fn(func) => {
                (track_caller(self.db, func), FunctionSignature::of(self.db, func).abi)
            }
            InstanceDef::Closure(_)
            | InstanceDef::Coroutine(_)
            | InstanceDef::DropGlue
            | InstanceDef::CallOnceShim
            | InstanceDef::FnPointerShim
            | InstanceDef::ReifyShim { .. } => (false, ExternAbi::Rust),
        };
        let start = std::time::Instant::now();
        let ctx = self.compile_body(body, env, tracked, abi, instance.args.clone());
        self.phases.ir += start.elapsed();
        let mut ctx = ctx?;
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
        let (module, code, unwind) = (&mut self.module, &mut *self.code, &mut self.unwind);
        let start = std::time::Instant::now();
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
            unwind.add_function(module, id, ctx)?;
            Ok::<_, String>(hit)
        }));
        self.phases.emit += start.elapsed();
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
        tracked: bool,
        abi: ExternAbi,
        args: StoredGenericArgs,
    ) -> Result<cranelift_codegen::Context> {
        self.build(env, Some(body), tracked, |fx| {
            fx.abi = abi;
            fx.args = Some(args);
            fx.codegen()
        })
    }

    fn build(
        &mut self,
        env: ParamEnvAndCrate<'db>,
        body: Option<&'db MirBody<'db>>,
        tracked: bool,
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
                moved_paths: Vec::new(),
                deferred_drops: Vec::new(),
                cleanup: None,
                inner_pad: None,
                drop_order: Vec::new(),
                operand_temps: Default::default(),
                in_cleanup: false,
                deferred_clears: Vec::new(),
                ret_mode: PassMode::Ignore,
                tracked,
                abi: ExternAbi::Rust,
                location: None,
                span: MirSpan::Unknown,
                constant_locals: Vec::new(),
                args: None,
                suspended: None,
                suspended_drop: None,
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
        // Lifetimes change nothing about the code, and the ones a caller passes on are its
        // own parameters, which mean nothing in the callee.
        let interner = DbInterner::new_with(self.db, env.krate);
        let args = rustc_type_ir::fold_regions(interner, args, |region, _| match region.kind() {
            rustc_type_ir::RegionKind::ReBound(..) => region,
            _ => Region::new_erased(interner),
        });
        let instance = Instance { def, args: args.store() };
        let signature = match def {
            InstanceDef::Fn(func) => Some(FunctionSignature::of(self.db, func)),
            InstanceDef::Closure(_)
            | InstanceDef::Coroutine(_)
            | InstanceDef::DropGlue
            | InstanceDef::CallOnceShim
            | InstanceDef::FnPointerShim
            | InstanceDef::ReifyShim { .. } => None,
        };
        let name = match def {
            InstanceDef::Fn(_) => signature.map_or("", |it| it.name.as_str()),
            InstanceDef::Closure(_) => "{closure}",
            InstanceDef::Coroutine(_) => "{async}",
            InstanceDef::DropGlue => "drop_in_place",
            InstanceDef::CallOnceShim => "call_once",
            InstanceDef::FnPointerShim => "{closure}::fn_pointer",
            InstanceDef::ReifyShim { .. } => "{reify}",
        };
        if let Some(reason) = self.rejected.get(&instance) {
            unsupported!("callee `{name}`: {reason}");
        }
        if let Some(&id) = self.instances.get(&instance) {
            return Ok(id);
        }
        if let InstanceDef::Fn(func) = def
            && let Some(symbol) = self.any_method(func, args)
        {
            return self.import(&symbol, sig);
        }
        // A `TypeId` is a hash only rustc can compute, and the two compilers have to agree on
        // it for `downcast` to work between their code. So take rustc's.
        if let InstanceDef::Fn(func) = def
            && let Some(of) = self.type_id_of(func, args)
        {
            let hash = type_id::TypeIdHasher::new(self.db, self.symbols).type_id(of);
            if let Some(hash) = hash {
                return self.constant_type_id(instance, sig, hash);
            }
            let Some(ty) = self.symbol_type(of) else {
                unsupported!("`TypeId` of a type neither hashed nor named by a rustc symbol");
            };
            if let Some(symbol) = self.symbols.path(&format!("<core::any::TypeId>::of::<{ty}>")) {
                return self.import(&symbol, sig);
            }
            // rustc inlines `TypeId::of` into most of its callers. What is left then is the
            // `Any` method a `dyn Any` of the type calls, which makes no use of its `self`.
            let Some(symbol) = self.symbols.path(&format!("<{ty} as core::any::Any>::type_id"))
            else {
                unsupported!("`TypeId` of `{ty}`, which rustc has no symbol for");
            };
            if let Some(&id) = self.instances.get(&instance) {
                return Ok(id);
            }
            let mut method = sig.clone();
            method.params.push(AbiParam::new(PTR));
            let method = self.import(&symbol, &method)?;
            let id = self
                .module
                .declare_function(
                    &format!("rustd::type_id::{}", self.instances.len()),
                    Linkage::Local,
                    sig,
                )
                .map_err(|e| CodegenError::Backend(e.to_string()))?;
            let mut ctx = self.module.make_context();
            ctx.func.signature = sig.clone();
            let mut builder_ctx = std::mem::take(&mut self.builder_ctx);
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            let block = builder.create_block();
            builder.append_block_params_for_function_params(block);
            builder.switch_to_block(block);
            let mut values = builder.block_params(block).to_vec();
            // A `&self` that points at nothing: any aligned address will do.
            values.push(builder.ins().iconst(PTR, 16));
            let method = self.module.declare_func_in_func(method, builder.func);
            let call = builder.ins().call(method, &values);
            let results = builder.inst_results(call).to_vec();
            builder.ins().return_(&results);
            builder.seal_all_blocks();
            builder.finalize(self.module.isa().frontend_config());
            self.builder_ctx = builder_ctx;
            self.emit(id, &mut ctx)?;
            self.instances.insert(instance, id);
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

    /// Makes `id` a call of the copy of `instance` rustc compiled, when there is one: an
    /// instance we cannot compile is still one we can call.
    fn forward_to_rustc(&mut self, instance: &Instance, id: FuncId) -> Option<()> {
        let InstanceDef::Fn(func) = instance.def else { return None };
        let symbol = self.symbols.path(&self.instance_path(func, instance.args.as_ref())?)?;
        let sig = self.module.declarations().get_function_decl(id).signature.clone();
        let target = self.import(&symbol, &sig).ok()?;
        let mut ctx = self.module.make_context();
        ctx.func.signature = sig;
        let mut builder_ctx = std::mem::take(&mut self.builder_ctx);
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        let values = builder.block_params(block).to_vec();
        let target = self.module.declare_func_in_func(target, builder.func);
        let call = builder.ins().call(target, &values);
        let results = builder.inst_results(call).to_vec();
        builder.ins().return_(&results);
        builder.seal_all_blocks();
        builder.finalize(self.module.isa().frontend_config());
        self.builder_ctx = builder_ctx;
        self.emit(id, &mut ctx).ok()
    }

    /// A generic function with its arguments as it reads in a demangled rustc symbol:
    /// `<Vec<u8> as Clone>::clone`, `<Option<u8>>::map::<u8, F>`, `core::mem::swap::<u8>`.
    fn instance_path(&self, func: FunctionId, args: GenericArgs<'db>) -> Option<String> {
        let db = self.db;
        let signature = FunctionSignature::of(db, func);
        // The future of an `async fn` is laid out differently by the two compilers.
        if signature.is_async() {
            return None;
        }
        let name = signature.name.as_str();
        let interner = DbInterner::new_no_crate(db);
        let types = |args: &[crate::next_solver::GenericArg<'db>]| {
            args.iter()
                .filter(|it| it.region().is_none())
                .map(|it| self.symbol_type(it.ty()?))
                .collect::<Option<Vec<_>>>()
        };
        let parent = crate::generics::generics(db, func.into()).len_parent();
        let (parent, own) = args.as_slice().split_at_checked(parent)?;
        let mut path = match func.lookup(db).container {
            ItemContainerId::ModuleId(_) => self.symbols.item(func.into())?,
            ItemContainerId::TraitId(trait_) => {
                let (self_ty, rest) = parent.split_first()?;
                let mut path = format!(
                    "<{} as {}",
                    self.symbol_type(self_ty.ty()?)?,
                    self.symbols.item(trait_.into())?
                );
                let rest = types(rest)?;
                if !rest.is_empty() {
                    path.push_str(&format!("<{}>", rest.join(", ")));
                }
                format!("{path}>::{name}")
            }
            ItemContainerId::ImplId(impl_) => {
                let parent_args = GenericArgs::new_from_slice(parent);
                let self_ty =
                    db.impl_self_ty(impl_).instantiate(interner, parent_args).skip_norm_wip();
                let self_ty = self.symbol_type(self_ty)?;
                match db.impl_trait(impl_) {
                    None => format!("<{self_ty}>::{name}"),
                    Some(trait_ref) => {
                        let trait_ref =
                            trait_ref.instantiate(interner, parent_args).skip_norm_wip();
                        let mut path = format!(
                            "<{self_ty} as {}",
                            self.symbols.item(trait_ref.def_id.0.into())?
                        );
                        let rest = types(&trait_ref.args.as_slice()[1..])?;
                        if !rest.is_empty() {
                            path.push_str(&format!("<{}>", rest.join(", ")));
                        }
                        format!("{path}>::{name}")
                    }
                }
            }
            ItemContainerId::ExternBlockId(_) => return None,
        };
        let own = types(own)?;
        if !own.is_empty() {
            path.push_str(&format!("::<{}>", own.join(", ")));
        }
        Some(path)
    }

    /// A `TypeId::of::<T>` that returns `hash`, the two words of which are its value.
    fn constant_type_id(
        &mut self,
        instance: Instance,
        sig: &Signature,
        hash: u128,
    ) -> Result<FuncId> {
        if let Some(&id) = self.instances.get(&instance) {
            return Ok(id);
        }
        let name = format!("rustd::type_id::{}", self.instances.len());
        let id = self
            .module
            .declare_function(&name, Linkage::Local, sig)
            .map_err(|e| CodegenError::Backend(e.to_string()))?;
        let mut ctx = self.module.make_context();
        ctx.func.signature = sig.clone();
        let mut builder_ctx = std::mem::take(&mut self.builder_ctx);
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        let words = [hash as u64, (hash >> 64) as u64].map(|it| it as i64);
        let returned = match *sig.returns.as_slice() {
            [] => {
                let &[out] = builder.block_params(block) else {
                    unsupported!("a `TypeId` returned as `{sig}`");
                };
                for (word, offset) in words.into_iter().zip([0, 8]) {
                    let word = builder.ins().iconst(types::I64, word);
                    builder.ins().store(MEM, word, out, offset);
                }
                Vec::new()
            }
            [low, high] if low.value_type == types::I64 && high.value_type == types::I64 => {
                words.map(|it| builder.ins().iconst(types::I64, it)).to_vec()
            }
            [whole] if whole.value_type == types::I128 => {
                let [low, high] = words.map(|it| builder.ins().iconst(types::I64, it));
                vec![builder.ins().iconcat(low, high)]
            }
            _ => unsupported!("a `TypeId` returned as `{sig}`"),
        };
        builder.ins().return_(&returned);
        builder.seal_all_blocks();
        builder.finalize(self.module.isa().frontend_config());
        self.builder_ctx = builder_ctx;
        self.emit(id, &mut ctx)?;
        self.instances.insert(instance, id);
        Ok(id)
    }

    /// The `T` of `TypeId::of::<T>`, when `func` is that function.
    fn type_id_of(&self, func: FunctionId, args: GenericArgs<'db>) -> Option<Ty<'db>> {
        let db = self.db;
        if FunctionSignature::of(db, func).name.as_str() != "of" {
            return None;
        }
        let ItemContainerId::ImplId(impl_) = func.lookup(db).container else { return None };
        let TyKind::Adt(adt, _) = db.impl_self_ty(impl_).skip_binder().kind() else { return None };
        if self.symbols.item(adt.def_id().into())? != "core::any::TypeId" {
            return None;
        }
        args.as_slice().first()?.ty()
    }

    /// A type as it reads in a demangled rustc symbol. `None` for the types whose spelling
    /// there this does not know: closures, `dyn`, function pointers, anything with a lifetime
    /// argument.
    fn symbol_type(&self, ty: Ty<'db>) -> Option<String> {
        use std::fmt::Write;
        let db = self.db;
        let list = |tys: &mut dyn Iterator<Item = Ty<'db>>| {
            tys.map(|it| self.symbol_type(it)).collect::<Option<Vec<_>>>().map(|it| it.join(", "))
        };
        Some(match ty.kind() {
            TyKind::Bool => "bool".to_owned(),
            TyKind::Char => "char".to_owned(),
            TyKind::Str => "str".to_owned(),
            TyKind::Never => "!".to_owned(),
            TyKind::Int(it) => it.name_str().to_owned(),
            TyKind::Uint(it) => it.name_str().to_owned(),
            TyKind::Float(it) => it.name_str().to_owned(),
            TyKind::Ref(_, pointee, Mutability::Not) => format!("&{}", self.symbol_type(pointee)?),
            TyKind::Ref(_, pointee, Mutability::Mut) => {
                format!("&mut {}", self.symbol_type(pointee)?)
            }
            TyKind::RawPtr(pointee, Mutability::Not) => {
                format!("*const {}", self.symbol_type(pointee)?)
            }
            TyKind::RawPtr(pointee, Mutability::Mut) => {
                format!("*mut {}", self.symbol_type(pointee)?)
            }
            TyKind::Slice(element) => format!("[{}]", self.symbol_type(element)?),
            TyKind::Array(element, len) => {
                format!("[{}; {}]", self.symbol_type(element)?, try_const_usize(db, len)?)
            }
            TyKind::Tuple(tys) if tys.len() == 1 => format!("({},)", list(&mut tys.iter())?),
            TyKind::Tuple(tys) => format!("({})", list(&mut tys.iter())?),
            TyKind::Adt(adt, args) => {
                let mut path = self.symbols.item(adt.def_id().into())?;
                let interner = DbInterner::new_no_crate(db);
                let defaults = db.generic_defaults(adt.def_id().into());
                let mut args = args.as_slice();
                // rustc leaves out the arguments that say what the default says.
                while let Some((last, rest)) = args.split_last()
                    && let Some(default) = defaults.get(rest.len())
                    && default.instantiate(interner, rest).skip_norm_wip() == *last
                {
                    args = rest;
                }
                let tys = args.iter().map(|it| it.ty()).collect::<Option<Vec<_>>>()?;
                if !tys.is_empty() {
                    write!(path, "<{}>", list(&mut tys.into_iter())?).ok()?;
                }
                path
            }
            TyKind::FnPtr(..) => {
                let sig = ty.fn_sig(DbInterner::new_no_crate(db)).no_bound_vars()?;
                if sig.abi() != ExternAbi::Rust
                    || sig.c_variadic()
                    || sig.safety() != crate::next_solver::abi::Safety::Safe
                {
                    return None;
                }
                let mut path = format!("fn({})", list(&mut sig.inputs().iter().copied())?);
                if !sig.output().is_unit() {
                    write!(path, " -> {}", self.symbol_type(sig.output())?).ok()?;
                }
                path
            }
            TyKind::Dynamic(bounds, _) => {
                let principal = bounds.principal()?.skip_binder();
                if principal.args.len() != 0 || bounds.projection_bounds().into_iter().count() != 0
                {
                    return None;
                }
                let mut path = format!("dyn {}", self.symbols.item(principal.def_id.0.into())?);
                for auto in bounds.auto_traits() {
                    write!(path, " + {}", self.symbols.item(auto.0.into())?).ok()?;
                }
                path
            }
            _ => return None,
        })
    }

    /// rustc's own `<dyn Any>::is::<T>` or `downcast_ref::<T>`, when `func` is such a method
    /// and rustc compiled it for this `T`. It has the `TypeId` of `T` inlined in it, which may
    /// be the one place to get it.
    fn any_method(&self, func: FunctionId, args: GenericArgs<'db>) -> Option<String> {
        let db = self.db;
        let name = FunctionSignature::of(db, func).name.as_str();
        let ItemContainerId::ImplId(impl_) = func.lookup(db).container else { return None };
        let self_ty = db.impl_self_ty(impl_).skip_binder();
        let TyKind::Dynamic(bounds, _) = self_ty.kind() else { return None };
        let principal = bounds.principal_def_id()?.0;
        if self.symbols.item(principal.into())? != "core::any::Any" {
            return None;
        }
        let ty = self.symbol_type(args.as_slice().first()?.ty()?)?;
        self.symbols.path(&format!("<{}>::{name}::<{ty}>", self.symbol_type(self_ty)?))
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
        let identity = if writable { DataIdentity::Writable } else { DataIdentity::Shared };
        self.define_data_as(blob, identity)
    }

    fn define_data_as(&mut self, blob: ConstBlob, identity: DataIdentity) -> Result<DataId> {
        let backend = |e: cranelift_module::ModuleError| CodegenError::Backend(e.to_string());
        let writable = identity == DataIdentity::Writable;
        let shared = (identity == DataIdentity::Shared).then(|| blob.clone());
        if let Some(&id) = shared.as_ref().and_then(|blob| self.consts.get(blob)) {
            return Ok(id);
        }
        let n = self.location_counter;
        self.location_counter += 1;
        let id = self
            .module
            .declare_data(&format!(".Lrustd.const.{n}"), Linkage::Local, writable, false)
            .map_err(backend)?;
        let mut data = DataDescription::new();
        data.define(blob.bytes.into_boxed_slice());
        data.set_align(blob.align);
        for (offset, target, addend) in blob.relocs {
            let target = self.module.declare_data_in_data(target, &mut data);
            data.write_data_addr(offset, target, addend);
        }
        for (offset, target) in blob.fn_relocs {
            let target = self.module.declare_func_in_data(target, &mut data);
            data.write_function_addr(offset, target);
        }
        self.module.define_data(id, &data).map_err(backend)?;
        if let Some(blob) = shared {
            self.consts.insert(blob, id);
        }
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

/// The pointers of one constant, and the data objects made for what they point to.
struct Relocations<'a, 'db> {
    map: &'a MemoryMap<'db>,
    in_static: bool,
    defined: FxHashMap<usize, DataId>,
}

/// Whether a data object can be the same object as another with the same content.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DataIdentity {
    /// Read-only and one object with every equal constant, as the same constant is for rustc
    /// wherever it is named.
    Shared,
    /// Read-only with an address of its own, as a static has.
    Own,
    Writable,
}

/// Read-only data whose pointers refer to other data objects.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ConstBlob {
    bytes: Vec<u8>,
    relocs: Vec<(u32, DataId, i64)>,
    fn_relocs: Vec<(u32, FuncId)>,
    align: u64,
}

impl ConstBlob {
    /// Makes the pointer at `offset` the address `at` bytes into `target`.
    fn point_to(&mut self, offset: usize, target: DataId, at: usize) {
        self.bytes[offset..offset + 8].fill(0);
        self.relocs.push((offset as u32, target, at as i64));
    }

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

/// A field that some statement moves out of a local, as the field indices leading to it.
/// The integer type drop glue reads an enum's discriminant as, to pick the variant.
#[derive(Clone, Copy)]
enum DiscriminantWidth {
    Word,
    /// `#[repr(i128)]` and `#[repr(u128)]` enums.
    Wide,
}

impl DiscriminantWidth {
    fn of(tag: &rustc_abi::Scalar) -> Self {
        match tag.primitive() {
            Primitive::Int(Integer::I128, _) => Self::Wide,
            _ => Self::Word,
        }
    }

    fn scalar(self) -> AbiScalar {
        match self {
            Self::Word => AbiScalar { ty: types::I64, signed: true },
            Self::Wide => AbiScalar { ty: types::I128, signed: true },
        }
    }

    /// The switch entry for a variant whose discriminant is `value`.
    fn entry(self, value: i128) -> u128 {
        match self {
            Self::Word => value as i64 as u64 as u128,
            Self::Wide => value as u128,
        }
    }
}

/// The step in a [`MovedPath`] from a `Box` to what it owns.
const BOX_CONTENTS: u32 = u32::MAX;
/// The step in a [`MovedPath`] from an enum to the fields of one of its variants, combined with
/// the variant's index.
const VARIANT_FIELDS: u32 = 1 << 30;

struct MovedPath {
    local: LocalId,
    path: Vec<u32>,
    bit: u32,
}

#[derive(Clone, Copy)]
struct LocalSlot {
    addr: Value,
    /// The local's type is unsized, and the slot holds a fat pointer to the value where it
    /// was moved from in place of the value.
    borrowed: bool,
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
    /// Address of a word per local that needs dropping. Bit 0 says the local holds a value, and
    /// the bits `moved_paths` hands out say which of its fields have been moved out since.
    drop_flags: ArenaMap<LocalId, Value>,
    moved_paths: Vec<MovedPath>,
    /// The block to generate each `Drop` terminator in, with its place and target.
    deferred_drops: Vec<(ir::Block, StoredPlace, BasicBlockId)>,
    /// Where a call that unwinds lands, taking the exception.
    cleanup: Option<ir::Block>,
    /// A landing pad that takes over from `cleanup` for the calls made while it is set.
    inner_pad: Option<ir::Block>,
    /// The locals the body drops, in the order it gets to dropping them.
    drop_order: Vec<LocalId>,
    /// Temporaries that hold an operand: never borrowed, only moved whole.
    operand_temps: rustc_hash::FxHashSet<LocalId>,
    in_cleanup: bool,
    deferred_clears: Vec<(ir::Block, StoredPlace, ir::Block)>,
    ret_mode: PassMode,
    /// Whether the function is `#[track_caller]` and so takes its caller's location as a last,
    /// hidden argument.
    tracked: bool,
    /// The calling convention of the function being built.
    abi: ExternAbi,
    location: Option<Value>,
    /// Where the statement being compiled comes from.
    span: MirSpan,
    /// Promoted locals that are read-only data instead of a slot the body writes to.
    constant_locals: Vec<LocalId>,
    /// What the body's generic parameters stand for, when it is the body of an instance.
    args: Option<StoredGenericArgs>,
    /// While compiling an `async` body: the address of the word that says where it left off,
    /// and the blocks it can be resumed in.
    suspended: Option<(Value, Vec<BasicBlockId>)>,
    /// Where a suspended future is dropped, and where that carries on.
    suspended_drop: Option<(ir::Block, ir::Block)>,
}

/// Whether calls to `func` pass a `&Location` after the declared arguments. A method of a trait
/// impl inherits the attribute from the trait's declaration.
fn track_caller(db: &dyn HirDatabase, func: FunctionId) -> bool {
    let has =
        |func: FunctionId| AttrFlags::query(db, func.into()).contains(AttrFlags::TRACK_CALLER);
    if has(func) {
        return true;
    }
    let ItemContainerId::ImplId(impl_id) = func.lookup(db).container else {
        return false;
    };
    let Some(trait_ref) = db.impl_trait(impl_id) else {
        return false;
    };
    let name = &FunctionSignature::of(db, func).name;
    trait_ref.skip_binder().def_id.0.trait_items(db).method_by_name(name).is_some_and(has)
}

const PTR: Type = types::I64;

/// Where an `async` body left off: before its first statement, after its last, part of the way
/// through unwinding, or at the await with the index that is added to `SUSPENDED`.
const UNRESUMED: i64 = 0;
const RETURNED: i64 = 1;
const PANICKED: i64 = 2;
const SUSPENDED: i64 = 3;

/// The local a body's caller gets the value of. An `async` body computes the value of its
/// block in the return slot, and hands back a `Poll` of it.
fn returned_local(body: &MirBody<'_>) -> LocalId {
    body.coroutine.map_or(return_slot(), |it| it.poll)
}
/// Field accesses can be unaligned (packed structs, chunked copies).
const MEM: MemFlagsData = MemFlagsData::new().with_notrap();

impl<'cx, 'a, 'db> FunctionCx<'cx, 'a, 'db> {
    /// Whether pointers to `ty` carry metadata: slices, `str`, trait objects and structs
    /// ending in one.
    fn is_unsized(&self, ty: Ty<'db>) -> bool {
        match ty.kind() {
            TyKind::Slice(_) | TyKind::Str | TyKind::Dynamic(..) => true,
            TyKind::Adt(..) | TyKind::Tuple(_) => {
                self.layout(ty).is_ok_and(|layout| layout.is_unsized())
            }
            _ => false,
        }
    }

    /// The last field of a struct or tuple, which is the only one that can be unsized.
    fn last_field(&self, ty: Ty<'db>) -> Result<(usize, Ty<'db>)> {
        match ty.kind() {
            TyKind::Tuple(tys) if !tys.is_empty() => {
                Ok((tys.len() - 1, tys.iter().last().expect("non-empty tuple")))
            }
            TyKind::Adt(adt_def, subst) => {
                let AdtId::StructId(id) = adt_def.def_id() else {
                    unsupported!("unsized {ty:?}");
                };
                let field_types = self.cg.db.field_types(id.into());
                let Some((field, field_ty)) = field_types.iter().last() else {
                    unsupported!("unsized {ty:?} without fields");
                };
                let field_ty = field_ty.ty().instantiate(self.interner, subst).skip_norm_wip();
                Ok((u32::from(field.into_raw()) as usize, self.normalize(field_ty)))
            }
            _ => unsupported!("unsized {ty:?}"),
        }
    }

    /// `packed(N)` of a struct, which caps the alignment of its fields.
    fn pack(&self, ty: Ty<'db>) -> Option<u64> {
        let TyKind::Adt(adt_def, _) = ty.kind() else { return None };
        let repr = hir_def::attrs::AttrFlags::repr(self.cg.db, adt_def.def_id())?;
        repr.pack.map(|pack| pack.bytes())
    }

    /// Size and alignment of the value behind a pointer with metadata `meta`, as
    /// `size_and_align_of_dst` in rustc's backends.
    fn size_and_align_of_val(
        &mut self,
        ty: Ty<'db>,
        meta: Option<Value>,
    ) -> Result<(Value, Value)> {
        let layout = self.layout(ty)?;
        let align = layout.align.abi.bytes() as i64;
        let meta = match meta {
            Some(meta) if self.is_unsized(ty) => meta,
            _ => {
                let size = self.builder.ins().iconst(PTR, layout.size.bytes() as i64);
                return Ok((size, self.builder.ins().iconst(PTR, align)));
            }
        };
        match ty.kind() {
            TyKind::Str => Ok((meta, self.builder.ins().iconst(PTR, 1))),
            TyKind::Slice(elem) => {
                let elem = self.layout(elem)?;
                let size = self.builder.ins().imul_imm(meta, elem.size.bytes() as i64);
                Ok((size, self.builder.ins().iconst(PTR, elem.align.abi.bytes() as i64)))
            }
            // Size and align are the vtable's second and third words.
            TyKind::Dynamic(..) => Ok((
                self.builder.ins().load(PTR, MEM, meta, 8),
                self.builder.ins().load(PTR, MEM, meta, 16),
            )),
            _ => {
                let (field, field_ty) = self.last_field(ty)?;
                let offset = layout.fields.offset(field).bytes() as i64;
                let (tail_size, mut tail_align) =
                    self.size_and_align_of_val(field_ty, Some(meta))?;
                if let Some(pack) = self.pack(ty) {
                    let pack = self.builder.ins().iconst(PTR, pack as i64);
                    tail_align = self.builder.ins().umin(tail_align, pack);
                }
                let sized_align = self.builder.ins().iconst(PTR, align);
                let full_align = self.builder.ins().umax(tail_align, sized_align);
                let offset = self.builder.ins().iconst(PTR, offset);
                let offset = self.align_up(offset, tail_align);
                let size = self.builder.ins().iadd(offset, tail_size);
                Ok((self.align_up(size, full_align), full_align))
            }
        }
    }

    /// Where to drop a field of `container`: destructors take an aligned `&mut self`, so
    /// a field a packed struct leaves misaligned is moved out first, as rustc does.
    fn aligned_for_drop(
        &mut self,
        addr: Value,
        field: Ty<'db>,
        container: Ty<'db>,
    ) -> Result<Value> {
        let Some(pack) = self.pack(container) else { return Ok(addr) };
        let field = self.normalize(field);
        let layout = self.layout(field)?;
        if layout.align.abi.bytes() <= pack
            || has_drop_glue(&self.infcx, field, self.env.param_env) == DropGlue::None
        {
            return Ok(addr);
        }
        let slot = self.stack_slot(&layout);
        self.copy(slot, addr, layout.size);
        Ok(slot)
    }

    fn align_up(&mut self, value: Value, align: Value) -> Value {
        let mask = self.builder.ins().iadd_imm(align, -1);
        let value = self.builder.ins().iadd(value, mask);
        self.builder.ins().band_not(value, mask)
    }

    /// Address of a field whose type is unsized. A trait object's alignment, and so the
    /// field's offset, is only known from the vtable.
    fn unsized_field_addr(
        &mut self,
        addr: Value,
        meta: Option<Value>,
        container: Ty<'db>,
        offset: Size,
        field_ty: Ty<'db>,
    ) -> Result<Value> {
        let field_ty = self.normalize(field_ty);
        let mut tail = field_ty;
        while matches!(tail.kind(), TyKind::Adt(..) | TyKind::Tuple(_)) && self.is_unsized(tail) {
            tail = self.last_field(tail)?.1;
        }
        let (Some(meta), TyKind::Dynamic(..)) = (meta, tail.kind()) else {
            return Ok(self.builder.ins().iadd_imm(addr, offset.bytes() as i64));
        };
        let (_, mut align) = self.size_and_align_of_val(field_ty, Some(meta))?;
        if let Some(pack) = self.pack(container) {
            let pack = self.builder.ins().iconst(PTR, pack as i64);
            align = self.builder.ins().umin(align, pack);
        }
        let offset = self.builder.ins().iconst(PTR, offset.bytes() as i64);
        let offset = self.align_up(offset, align);
        Ok(self.builder.ins().iadd(addr, offset))
    }

    fn layout(&self, ty: Ty<'db>) -> Result<Arc<Layout>> {
        let layout = self
            .cg
            .db
            .layout_of_ty(ty.store(), self.env.store())
            .map_err(|e| CodegenError::Frontend(format!("layout of {ty:?}: {e:?}")))?;
        if let Some(log) = &self.cg.layouts {
            log.borrow_mut().entry(ty).or_insert((layout.size.bytes(), layout.align.abi.bytes()));
        }
        Ok(layout)
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

    /// How a call with these types passes its values. Mirrors rustc: `adjust_for_rust_abi`
    /// for Rust's own conventions, and `callconv/x86_64.rs` for C's.
    fn fn_abi(
        &self,
        abi: ExternAbi,
        ret: Ty<'db>,
        args: impl IntoIterator<Item = Ty<'db>>,
    ) -> Result<FnAbi> {
        if abi.is_rustic_abi() {
            let ret = self.pass_mode(&*self.layout(ret)?, true)?;
            let args = args
                .into_iter()
                .map(|ty| self.pass_mode(&*self.layout(ty)?, false))
                .collect::<Result<_>>()?;
            return Ok(FnAbi { ret, args });
        }
        if !matches!(abi, ExternAbi::C { .. } | ExternAbi::System { .. } | ExternAbi::SysV64 { .. })
        {
            unsupported!("calling convention {abi:?}");
        }
        let (mut int_regs, mut sse_regs) = (6usize, 8usize);
        let ret = match self.c_pass_mode(ret)? {
            PassMode::OnStack(_) => {
                // The pointer to the return value takes the first integer register.
                int_regs -= 1;
                PassMode::Indirect
            }
            mode => mode,
        };
        let mut modes = Vec::new();
        for ty in args {
            let mode = self.c_pass_mode(ty)?;
            let (ints, sses) = match mode {
                PassMode::Ignore | PassMode::Indirect | PassMode::OnStack(_) => (0, 0),
                PassMode::Direct(s) => self.count_regs(&[s.ty]),
                PassMode::Pair(a, b, _) => self.count_regs(&[a.ty, b.ty]),
                PassMode::Cast(ty) => self.count_regs(&[ty]),
                PassMode::CastPair(a, b) => self.count_regs(&[a, b]),
            };
            let aggregate = matches!(mode, PassMode::Cast(_) | PassMode::CastPair(..));
            modes.push(match (int_regs.checked_sub(ints), sse_regs.checked_sub(sses)) {
                (Some(int_left), Some(sse_left)) => {
                    (int_regs, sse_regs) = (int_left, sse_left);
                    mode
                }
                // Out of registers: an aggregate goes to memory whole. Cranelift moves
                // anything else to the stack by itself.
                _ if aggregate => PassMode::OnStack(self.layout(ty)?.size.bytes() as u32),
                _ => mode,
            });
        }
        Ok(FnAbi { ret, args: modes })
    }

    fn count_regs(&self, types: &[Type]) -> (usize, usize) {
        let floats = types.iter().filter(|ty| ty.is_float()).count();
        let ints = types.iter().filter(|ty| !ty.is_float()).map(|ty| ty.bytes().div_ceil(8));
        (ints.sum::<u32>() as usize, floats)
    }

    /// How the C ABI passes one value when registers are left for it.
    fn c_pass_mode(&self, ty: Ty<'db>) -> Result<PassMode> {
        let layout = self.layout(ty)?;
        if layout.is_zst() {
            return Ok(PassMode::Ignore);
        }
        let size = layout.size.bytes();
        let on_stack = PassMode::OnStack(size.next_multiple_of(8) as u32);
        match layout.backend_repr {
            BackendRepr::Scalar(scalar) => return Ok(PassMode::Direct(self.scalar(scalar)?)),
            BackendRepr::SimdVector { .. } => unsupported!("vector in the C ABI"),
            _ => (),
        }
        if size > 16 {
            return Ok(on_stack);
        }
        let mut classes = [None; 2];
        if !self.classify(ty, 0, &mut classes)? {
            return Ok(on_stack);
        }
        let reg = |class: Option<RegClass>, bytes: u64| match (class, bytes) {
            (Some(RegClass::Sse), 4) => types::F32,
            (Some(RegClass::Sse), _) => types::F64,
            (_, 1) => types::I8,
            (_, 2) => types::I16,
            (_, 3 | 4) => types::I32,
            _ => types::I64,
        };
        Ok(if size <= 8 {
            PassMode::Cast(reg(classes[0], size))
        } else {
            PassMode::CastPair(reg(classes[0], 8), reg(classes[1], size - 8))
        })
    }

    /// Classifies the eightbytes of a value of at most sixteen bytes at `offset`; false if it
    /// has to go in memory.
    fn classify(
        &self,
        ty: Ty<'db>,
        offset: u64,
        classes: &mut [Option<RegClass>; 2],
    ) -> Result<bool> {
        let layout = self.layout(ty)?;
        if layout.is_zst() {
            return Ok(true);
        }
        if offset % layout.align.abi.bytes() != 0 {
            return Ok(false);
        }
        let mut scalar = |scalar: Scalar, offset: u64| {
            let class = match scalar.primitive() {
                Primitive::Float(_) => RegClass::Sse,
                Primitive::Int(..) | Primitive::Pointer(_) => RegClass::Int,
            };
            let last = (offset + scalar.size(self.dl()).bytes() - 1) / 8;
            for slot in &mut classes[(offset / 8) as usize..=last as usize] {
                *slot = Some(slot.map_or(class, |old: RegClass| old.min(class)));
            }
        };
        match layout.backend_repr {
            BackendRepr::Scalar(a) => scalar(a, offset),
            BackendRepr::ScalarPair(a, b) => {
                scalar(a, offset);
                scalar(b, offset + a.size(self.dl()).align_to(b.align(self.dl()).abi).bytes());
            }
            BackendRepr::Memory { sized: true } => match ty.kind() {
                TyKind::Tuple(tys) => {
                    for (i, ty) in tys.iter().enumerate() {
                        if !self.classify(ty, offset + layout.fields.offset(i).bytes(), classes)? {
                            return Ok(false);
                        }
                    }
                }
                TyKind::Array(element, _) => {
                    let stride = self.layout(element)?.size.bytes();
                    for i in 0..layout.fields.count() as u64 {
                        if !self.classify(element, offset + i * stride, classes)? {
                            return Ok(false);
                        }
                    }
                }
                TyKind::Adt(def, subst) => {
                    let variant: VariantId = match def.def_id() {
                        AdtId::StructId(id) => id.into(),
                        AdtId::UnionId(id) => id.into(),
                        AdtId::EnumId(_) => unsupported!("enum {ty:?} in the C ABI"),
                    };
                    for (field, ty) in self.cg.db.field_types(variant).iter() {
                        let ty = ty.ty().instantiate(self.interner, subst).skip_norm_wip();
                        let field = layout.fields.offset(u32::from(field.into_raw()) as usize);
                        if !self.classify(ty, offset + field.bytes(), classes)? {
                            return Ok(false);
                        }
                    }
                }
                _ => unsupported!("{ty:?} in the C ABI"),
            },
            _ => unsupported!("{ty:?} in the C ABI"),
        }
        Ok(true)
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
            PassMode::CastPair(a, b) => params.extend([AbiParam::new(a), AbiParam::new(b)]),
            PassMode::Indirect => params.push(AbiParam::new(PTR)),
            PassMode::OnStack(size) => {
                params.push(AbiParam::special(PTR, ArgumentPurpose::StructArgument(size)))
            }
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

    /// The values that carry the argument at `addr` to a callee.
    fn pass(&mut self, mode: PassMode, addr: Value, layout: &Layout, values: &mut Vec<Value>) {
        match mode {
            PassMode::Ignore => (),
            PassMode::Direct(s) => values.push(self.builder.ins().load(s.ty, MEM, addr, 0)),
            PassMode::Pair(a, b, offset) => {
                values.push(self.builder.ins().load(a.ty, MEM, addr, 0));
                values.push(self.builder.ins().load(b.ty, MEM, addr, offset.bytes() as i32));
            }
            PassMode::Cast(ty) => values.push(self.load_cast(ty, addr, layout.size)),
            PassMode::CastPair(a, b) => {
                values.push(self.builder.ins().load(a, MEM, addr, 0));
                let rest = self.builder.ins().iadd_imm(addr, 8);
                values.push(self.load_cast(b, rest, layout.size - Size::from_bytes(8)));
            }
            PassMode::Indirect => {
                // The callee owns the memory it is handed.
                let copy = self.stack_slot(layout);
                self.copy(copy, addr, layout.size);
                values.push(copy);
            }
            PassMode::OnStack(_) => values.push(addr),
        }
    }

    /// Stores what `pass` or a return sent, taking the values off the front of `values`.
    fn receive(
        &mut self,
        mode: PassMode,
        values: &mut impl Iterator<Item = Value>,
        addr: Value,
        layout: &Layout,
    ) {
        let mut next = || values.next().expect("a value for every abi parameter");
        match mode {
            PassMode::Ignore | PassMode::Indirect | PassMode::OnStack(_) => (),
            PassMode::Direct(_) => {
                let value = next();
                self.builder.ins().store(MEM, value, addr, 0);
            }
            PassMode::Pair(_, _, offset) => {
                let (a, b) = (next(), next());
                self.builder.ins().store(MEM, a, addr, 0);
                self.builder.ins().store(MEM, b, addr, offset.bytes() as i32);
            }
            PassMode::Cast(_) => {
                let value = next();
                self.store_cast(value, addr, layout.size);
            }
            PassMode::CastPair(..) => {
                let (a, b) = (next(), next());
                self.builder.ins().store(MEM, a, addr, 0);
                let rest = self.builder.ins().iadd_imm(addr, 8);
                self.store_cast(b, rest, layout.size - Size::from_bytes(8));
            }
        }
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

    /// Storage for a fat pointer.
    fn scratch_pair(&mut self) -> Value {
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            16,
            3,
        ));
        self.builder.ins().stack_addr(PTR, slot, 0)
    }

    /// Storage for a temporary that is borrowed for `'static`. The function writes the same
    /// constant there every time it runs.
    // FIXME: evaluate the constant and make this read-only data.
    fn promoted_slot(&mut self, layout: &Layout) -> Result<Value> {
        let align = layout.align.abi.bytes();
        if layout.size.bytes() == 0 {
            return Ok(self.builder.ins().iconst(PTR, align as i64));
        }
        let blob = ConstBlob {
            bytes: vec![0; layout.size.bytes_usize()],
            relocs: Vec::new(),
            fn_relocs: Vec::new(),
            align,
        };
        let data = self.cg.define_data(blob, true)?;
        let data = self.cg.module.declare_data_in_func(data, self.builder.func);
        Ok(self.builder.ins().symbol_value(PTR, data))
    }

    /// The read-only data a promoted `&expr` points to, when `expr` can be evaluated now.
    /// Equal constants share an address, as they do for rustc.
    fn promoted_constant(
        &mut self,
        body: &'db MirBody<'db>,
        expr: ExprId,
        size: usize,
    ) -> Option<Value> {
        let db = self.cg.db;
        if self.mentions_location(body, expr) {
            return None;
        }
        let constant = monomorphized_promoted_mir_body_query(
            db,
            body.owner,
            expr,
            self.args.clone()?,
            self.env.store(),
        )
        .ok()?;
        let allocation = interpret_mir(db, constant, false, Some(self.env)).ok()?.0.ok()?;
        let blob = self.allocation_blob(allocation, false).ok()?;
        if blob.bytes.len() != size {
            return None;
        }
        self.blob_addr(blob).ok()
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
    fn body_abi(&self, body: &MirBody<'db>, abi: ExternAbi) -> Result<FnAbi> {
        self.fn_abi(
            abi,
            body.locals[returned_local(body)].ty.as_ref(),
            body.param_locals.iter().map(|&local| body.locals[local].ty.as_ref()),
        )
    }

    fn body_signature(&self, body: &MirBody<'db>, tracked: bool) -> Result<Signature> {
        let abi = self.body_abi(body, ExternAbi::Rust)?;
        let mut sig = self.signature(&abi.args, abi.ret);
        if tracked {
            sig.params.push(AbiParam::new(PTR));
        }
        Ok(sig)
    }

    fn codegen(&mut self) -> Result<()> {
        let body = self.body.expect("MIR body");
        let FnAbi { ret, args: param_modes } = self.body_abi(body, self.abi)?;
        self.ret_mode = ret;
        let param_layouts = body
            .param_locals
            .iter()
            .map(|&l| self.layout(body.locals[l].ty.as_ref()))
            .collect::<Result<Vec<_>>>()?;
        let mut sig = self.signature(&param_modes, ret);
        if self.tracked {
            sig.params.push(AbiParam::new(PTR));
        }
        self.builder.func.signature = sig;

        let entry = self.builder.create_block();
        self.builder.append_block_params_for_function_params(entry);
        self.builder.switch_to_block(entry);
        let mut incoming = self.builder.block_params(entry).to_vec().into_iter();

        if let PassMode::Indirect = self.ret_mode {
            let addr = incoming.next().expect("sret param");
            self.locals.insert(returned_local(body), LocalSlot { addr, borrowed: false });
        }
        for ((&local, layout), mode) in
            body.param_locals.iter().zip(&param_layouts).zip(param_modes)
        {
            let addr = match mode {
                PassMode::Indirect | PassMode::OnStack(_) => {
                    incoming.next().expect("indirect param")
                }
                _ => self.stack_slot(layout),
            };
            self.receive(mode, &mut incoming, addr, layout);
            self.locals.insert(local, LocalSlot { addr, borrowed: false });
        }
        if self.tracked {
            self.location = incoming.next();
        }
        // An `async` body keeps its locals in the future, where they survive a return.
        let frame = match body.coroutine {
            Some(_) => {
                let receiver = body.param_locals[0];
                let Some(ty) = body.locals[receiver].ty.as_ref().builtin_deref(true) else {
                    unsupported!("async body without a reference to its future");
                };
                let TyKind::Coroutine(_, args) = ty.kind() else {
                    unsupported!("async body of {ty:?}");
                };
                let upvars = args.as_coroutine().tupled_upvars_ty().tuple_fields().len();
                let frame = CoroutineFrame { upvars, locals: body.locals.len() };
                let addr = self.builder.ins().load(PTR, MEM, self.locals[receiver].addr, 0);
                Some((frame, self.layout(ty)?, addr, ty))
            }
            None => None,
        };
        let in_frame = |this: &mut Self, field: usize| {
            let (_, layout, addr, _) = frame.as_ref().expect("frame");
            this.builder.ins().iadd_imm(*addr, layout.fields.offset(field).bytes() as i64)
        };
        for (local, data) in body.locals.iter() {
            if self.locals.contains_idx(local) {
                continue;
            }
            let layout = self.layout(data.ty.as_ref())?;
            if let Some((frame, ..)) = &frame
                && Some(local) != body.coroutine.map(|it| it.poll)
            {
                let addr = in_frame(self, frame.local(local.into_raw().into_u32() as usize));
                self.locals.insert(local, LocalSlot { addr, borrowed: false });
                continue;
            }
            let promoted = body.promoted_locals.iter().find(|it| it.0 == local);
            let addr = if let Some(&(_, expr)) = promoted
                && self.is_freeze(data.ty.as_ref())
                && has_drop_glue(&self.infcx, data.ty.as_ref(), self.env.param_env)
                    == DropGlue::None
            {
                match self.promoted_constant(body, expr, layout.size.bytes_usize()) {
                    Some(addr) if layout.size.bytes() != 0 => {
                        self.constant_locals.push(local);
                        addr
                    }
                    _ => self.promoted_slot(&layout)?,
                }
            } else {
                self.stack_slot(&layout)
            };
            if layout.is_unsized() {
                let addr = self.scratch_pair();
                self.locals.insert(local, LocalSlot { addr, borrowed: true });
                continue;
            }
            self.locals.insert(local, LocalSlot { addr, borrowed: false });
        }
        let mut frame_flags = Vec::new();
        for (local, data) in body.locals.iter() {
            let ty = data.ty.as_ref();
            if local == returned_local(body)
                || has_drop_glue(&self.infcx, ty, self.env.param_env) == DropGlue::None
            {
                continue;
            }
            let is_param = body.param_locals.contains(&local);
            let flag = match &frame {
                Some((frame, ..)) if !is_param => {
                    let flag =
                        in_frame(self, frame.drop_flag(local.into_raw().into_u32() as usize));
                    frame_flags.push(flag);
                    flag
                }
                _ => {
                    let flag = self.scratch();
                    let live = self.builder.ins().iconst(types::I64, is_param as i64);
                    self.builder.ins().store(MEM, live, flag, 0);
                    flag
                }
            };
            self.drop_flags.insert(local, flag);
        }
        for (id, _) in body.basic_blocks.iter() {
            let block = self.builder.create_block();
            self.blocks.insert(id, block);
        }

        // Only blocks that can run are compiled: `if cfg!(miri) { .. }` calls functions that do
        // not exist.
        let mut reachable = rustc_hash::FxHashSet::default();
        // Switches on a value known now, and the one way they go.
        let mut decided = FxHashMap::default();
        let mut worklist = vec![body.start_block];
        while let Some(id) = worklist.pop() {
            if !reachable.insert(id) {
                continue;
            }
            match body.basic_blocks[id].terminator.as_ref().map(|it| &it.kind) {
                Some(TerminatorKind::SwitchInt { discr, targets }) => {
                    match self.switch_bits(&body.basic_blocks[id], discr) {
                        Some(bits) => {
                            let target = targets.target_for_value(bits);
                            decided.insert(id, target);
                            worklist.push(target)
                        }
                        None => worklist.extend_from_slice(targets.all_targets()),
                    }
                }
                Some(
                    TerminatorKind::Goto { target }
                    | TerminatorKind::Drop { target, .. }
                    | TerminatorKind::DropAndReplace { target, .. }
                    | TerminatorKind::Assert { target, .. }
                    | TerminatorKind::FalseEdge { real_target: target, .. }
                    | TerminatorKind::FalseUnwind { real_target: target, .. },
                ) => worklist.push(*target),
                Some(TerminatorKind::Call { target, .. }) => worklist.extend(*target),
                Some(TerminatorKind::Yield { resume, .. }) => worklist.push(*resume),
                _ => (),
            }
        }
        let state = frame.as_ref().map(|(frame, ..)| frame.state()).map(|it| in_frame(self, it));
        match (frame, state) {
            (Some((_, layout, addr, ty)), Some(state)) => {
                let resumes: Vec<BasicBlockId> = body
                    .basic_blocks
                    .iter()
                    .filter(|(id, _)| reachable.contains(id))
                    .filter_map(|(_, block)| match block.terminator.as_ref()?.kind {
                        TerminatorKind::Yield { resume, .. } => Some(resume),
                        _ => None,
                    })
                    .collect();
                self.enter_coroutine(body, state, &resumes, &frame_flags, addr, ty, &layout)?;
                self.suspended = Some((state, resumes));
            }
            _ => {
                self.builder.ins().jump(self.blocks[body.start_block], &[]);
            }
        }
        self.drop_order = drop_order(body, &reachable);
        self.operand_temps = operand_temps(body);
        for (id, block) in body.basic_blocks.iter() {
            if !reachable.contains(&id) {
                continue;
            }
            self.builder.switch_to_block(self.blocks[id]);
            for statement in &block.statements {
                self.span = statement.span;
                match &statement.kind {
                    // A promoted constant already holds its value.
                    StatementKind::Assign(place, _)
                        if self.constant_locals.contains(&place.local) => {}
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
            self.span = terminator.span;
            let target = match &terminator.kind {
                // A promoted call already holds its value, and that value is read-only.
                TerminatorKind::Call { destination, target: Some(target), .. }
                    if self.constant_locals.contains(&destination.local) =>
                {
                    Some(*target)
                }
                _ => decided.get(&id).copied(),
            };
            match target {
                Some(target) => {
                    self.builder.ins().jump(self.blocks[target], &[]);
                }
                None => self.terminator(&terminator.kind)?,
            }
        }
        // Drops are generated last, when every field that gets moved out of a local is known.
        for (block, place, target) in std::mem::take(&mut self.deferred_clears) {
            self.builder.switch_to_block(block);
            self.clear_moved_now(&place);
            self.builder.ins().jump(target, &[]);
        }
        for (block, place, target) in std::mem::take(&mut self.deferred_drops) {
            self.builder.switch_to_block(block);
            // `place = value` drops what `place` held and then writes to it. If that drop
            // unwinds the write still happens, so that `place` is not dropped twice.
            let replace =
                body.basic_blocks[target].statements.iter().find_map(|it| match &it.kind {
                    StatementKind::Assign(to, rvalue) => Some((to, rvalue)),
                    _ => None,
                });
            let target = self.blocks[target];
            match replace {
                Some((to, rvalue))
                    if *to == place
                        && !place.projection.as_slice().is_empty()
                        && !self.in_cleanup =>
                {
                    let dropped = self.builder.create_block();
                    self.guarded(
                        |this| {
                            this.drop_place(&place, dropped)?;
                            this.builder.switch_to_block(dropped);
                            Ok(())
                        },
                        |this| {
                            this.assign(to, rvalue)?;
                            this.clear_moved_now(to);
                            Ok(())
                        },
                    )?;
                    self.builder.ins().jump(target, &[]);
                }
                _ => self.drop_place(&place, target)?,
            }
        }
        // So is dropping a suspended future, which drops the same locals as unwinding does.
        // Nothing is unwinding then, so a destructor that panics leaves the rest to be dropped.
        if let Some((suspended, done)) = self.suspended_drop.take() {
            self.builder.switch_to_block(suspended);
            self.drop_live_locals()?;
            self.builder.ins().jump(done, &[]);
        }
        self.emit_cleanup()
    }

    /// The fields a place selects in its local, if it is nothing but fields of structs, tuples
    /// and closures. Those are the places that can be moved out of on their own.
    fn field_path(&self, place: &StoredPlace) -> Option<Vec<u32>> {
        Some(self.path_of(place.local, place.projection.as_slice())?.0)
    }

    /// The paths a move out of `place` takes away: its own, or one for each element when it
    /// is a subslice of an array.
    fn moved_out_paths(&self, place: &StoredPlace) -> Option<Vec<Vec<u32>>> {
        let projection = place.projection.as_slice();
        let Some((&ProjectionElem::Subslice { from, to }, array)) = projection.split_last() else {
            return Some(vec![self.field_path(place)?]);
        };
        let (path, ty) = self.path_of(place.local, array)?;
        let TyKind::Array(_, len) = ty.ty.kind() else { return None };
        let len = crate::consteval::try_const_usize(self.cg.db, len)? as u64;
        (from..len.checked_sub(to)?)
            .map(|index| {
                let index = u32::try_from(index).ok().filter(|&it| it < VARIANT_FIELDS)?;
                Some(path.iter().copied().chain([index]).collect())
            })
            .collect()
    }

    fn path_of(
        &self,
        local: LocalId,
        projection: &[crate::mir::PlaceElem],
    ) -> Option<(Vec<u32>, PlaceTy<'db>)> {
        let mut ty = PlaceTy::from_ty(self.body?.locals[local].ty.as_ref());
        let mut path = Vec::new();
        for proj in projection {
            if let (ProjectionElem::Deref, TyKind::Adt(adt_def, _)) = (proj, ty.ty.kind())
                && adt_def.is_box()
            {
                path.push(BOX_CONTENTS);
                ty = ty.projection_ty(&self.infcx, proj, self.env.param_env);
                continue;
            }
            if let (ProjectionElem::Downcast(VariantId::EnumVariantId(variant)), TyKind::Adt(..)) =
                (proj, ty.ty.kind())
            {
                path.push(VARIANT_FIELDS | variant.index(self.cg.db) as u32);
                ty = ty.projection_ty(&self.infcx, proj, self.env.param_env);
                continue;
            }
            if let (&ProjectionElem::ConstantIndex { offset, from_end }, TyKind::Array(_, len)) =
                (proj, ty.ty.kind())
            {
                let len = crate::consteval::try_const_usize(self.cg.db, len)? as u64;
                // Offset 0 from the end is the last element.
                let index = if from_end { len.checked_sub(offset + 1)? } else { offset };
                path.push(u32::try_from(index).ok().filter(|&it| it < VARIANT_FIELDS)?);
                ty = ty.projection_ty(&self.infcx, proj, self.env.param_env);
                continue;
            }
            let ProjectionElem::Field(field) = proj else { return None };
            match ty.ty.kind() {
                TyKind::Tuple(_) | TyKind::Closure(..) => (),
                TyKind::Adt(..) if ty.variant_id.is_some() => (),
                TyKind::Adt(adt_def, _)
                    if matches!(adt_def.def_id(), AdtId::StructId(_)) && !adt_def.is_box() => {}
                _ => return None,
            }
            path.push(field.0 as u32);
            ty = ty.projection_ty(&self.infcx, proj, self.env.param_env);
        }
        Some((path, ty))
    }

    /// Says that the fields of `place` are all there again, after an assignment to it.
    fn clear_moved(&mut self, place: &StoredPlace) {
        if !self.drop_flags.contains_idx(place.local)
            || self.field_path(place).is_none_or(|it| it.is_empty())
        {
            return;
        }
        // Filled in last, when every field that gets moved out of the local is known.
        let clear = self.builder.create_block();
        let after = self.builder.create_block();
        self.builder.ins().jump(clear, &[]);
        self.builder.switch_to_block(after);
        self.deferred_clears.push((clear, place.clone(), after));
    }

    fn clear_moved_now(&mut self, place: &StoredPlace) {
        let (Some(&flag), Some(path)) = (self.drop_flags.get(place.local), self.field_path(place))
        else {
            return;
        };
        let within = |it: &&MovedPath| it.local == place.local && it.path.starts_with(&path);
        let mask = self.moved_paths.iter().filter(within).fold(0u64, |mask, it| mask | 1 << it.bit);
        if mask != 0 {
            let state = self.builder.ins().load(types::I64, MEM, flag, 0);
            let state = self.builder.ins().band_imm(state, !mask as i64);
            self.builder.ins().store(MEM, state, flag, 0);
        }
    }

    /// The bit in a local's drop flag that says one of its fields has been moved out.
    fn moved_bit(&mut self, local: LocalId, path: Vec<u32>) -> Option<u32> {
        let known = self.moved_paths.iter().find(|it| it.local == local && it.path == path);
        if let Some(known) = known {
            return Some(known.bit);
        }
        let bit = 1 + self.moved_paths.iter().filter(|it| it.local == local).count() as u32;
        if bit >= 64 {
            return None;
        }
        self.moved_paths.push(MovedPath { local, path, bit });
        Some(bit)
    }

    /// Drops what is left of a place, which is nothing if its local holds no value.
    fn drop_place(&mut self, place: &StoredPlace, target: ir::Block) -> Result<()> {
        let flag = self.drop_flags.get(place.local).copied();
        let (Some(flag), Some(mut path)) = (flag, self.field_path(place)) else {
            let place = self.place_ref(place)?;
            self.drop_value(place.addr, place.meta, place.ty.ty)?;
            self.builder.ins().jump(target, &[]);
            return Ok(());
        };
        let state = self.builder.ins().load(types::I64, MEM, flag, 0);
        let live = self.builder.ins().band_imm(state, 1);
        let drop = self.builder.create_block();
        self.builder.ins().brif(live, drop, &[], target, &[]);
        self.builder.switch_to_block(drop);
        if path.is_empty() {
            let dead = self.builder.ins().iconst(types::I64, 0);
            self.builder.ins().store(MEM, dead, flag, 0);
        }
        let local = place.local;
        let place = self.place_ref(place)?;
        self.drop_unmoved(place.addr, place.meta, place.ty.ty, local, &mut path, state)?;
        self.builder.ins().jump(target, &[]);
        Ok(())
    }

    /// Drops the value at `path` in `local`, except for the fields that `state` says are moved
    /// out.
    fn drop_unmoved(
        &mut self,
        addr: Value,
        meta: Option<Value>,
        ty: Ty<'db>,
        local: LocalId,
        path: &mut Vec<u32>,
        state: Value,
    ) -> Result<()> {
        let within = |it: &&MovedPath| it.local == local && it.path.starts_with(path);
        let exact = self.moved_paths.iter().filter(within).find(|it| it.path.len() == path.len());
        let exact = exact.map(|it| it.bit);
        let nested = self.moved_paths.iter().filter(within).any(|it| it.path.len() > path.len());
        if exact.is_none() && !nested {
            return self.drop_value(addr, meta, ty);
        }
        let done = self.builder.create_block();
        if let Some(bit) = exact {
            let moved = self.builder.ins().band_imm(state, 1i64 << bit);
            let present = self.builder.create_block();
            self.builder.ins().brif(moved, done, &[], present, &[]);
            self.builder.switch_to_block(present);
        }
        if nested {
            let ty = self.normalize(ty);
            let fields: Vec<Ty<'db>> = match ty.kind() {
                TyKind::Tuple(tys) => tys.iter().collect(),
                TyKind::Array(element, len) => {
                    match crate::consteval::try_const_usize(self.cg.db, len) {
                        Some(len) => vec![element; len as usize],
                        None => unsupported!("partial move out of {ty:?}"),
                    }
                }
                TyKind::Closure(_, args) => match args.as_closure().tupled_upvars_ty().kind() {
                    TyKind::Tuple(tys) => tys.iter().collect(),
                    _ => unsupported!("partial move out of a closure without tupled upvars"),
                },
                TyKind::Adt(adt_def, subst) if adt_def.is_box() => {
                    let pointee = subst.type_at(0);
                    let pointer = self.builder.ins().load(PTR, MEM, addr, 0);
                    let pointee_meta = self
                        .is_unsized(pointee)
                        .then(|| self.builder.ins().load(PTR, MEM, addr, 8));
                    self.drop_ladder(2, |this, at| {
                        if at == 0 {
                            path.push(BOX_CONTENTS);
                            let dropped = this.drop_unmoved(
                                pointer,
                                pointee_meta,
                                pointee,
                                local,
                                path,
                                state,
                            );
                            path.pop();
                            return dropped;
                        }
                        this.call_drop_impl(addr, ty)?;
                        if let AdtId::StructId(id) = adt_def.def_id() {
                            let layout = this.layout(ty)?;
                            this.drop_fields(addr, &layout, id.into(), subst, ty, meta)?;
                        }
                        Ok(())
                    })?;
                    self.builder.ins().jump(done, &[]);
                    self.builder.switch_to_block(done);
                    return Ok(());
                }
                TyKind::Adt(adt_def, subst) if matches!(adt_def.def_id(), AdtId::EnumId(_)) => {
                    let AdtId::EnumId(id) = adt_def.def_id() else { unreachable!() };
                    let db = self.cg.db;
                    let layout = self.layout(ty)?;
                    let wide = match &layout.variants {
                        Variants::Multiple { tag, .. } => DiscriminantWidth::of(tag),
                        _ => DiscriminantWidth::Word,
                    };
                    let single = match &layout.variants {
                        Variants::Empty => {
                            self.builder.ins().jump(done, &[]);
                            self.builder.switch_to_block(done);
                            return Ok(());
                        }
                        Variants::Single { index } => Some(index.0),
                        Variants::Multiple { .. } => None,
                    };
                    let mut switch = Switch::new();
                    let mut arms = Vec::new();
                    for &(variant, _) in id.enum_variants(db).variants.values() {
                        if single.is_some_and(|index| index != variant.index(db)) {
                            continue;
                        }
                        let value = db
                            .const_eval_discriminant(variant)
                            .map_err(|e| CodegenError::Frontend(format!("discriminant: {e:?}")))?;
                        let block = self.builder.create_block();
                        if single.is_none() {
                            switch.set_entry(wide.entry(value), block);
                        }
                        arms.push((block, variant));
                    }
                    if single.is_some() {
                        match arms.first() {
                            Some(&(block, _)) => self.builder.ins().jump(block, &[]),
                            None => self.builder.ins().jump(done, &[]),
                        };
                    } else {
                        let discr = self.discriminant(addr, ty, wide.scalar())?;
                        switch.emit(&mut self.builder, discr, done);
                    }
                    for (block, variant) in arms {
                        self.builder.switch_to_block(block);
                        let index = variant.index(db);
                        let variant_layout = match &layout.variants {
                            Variants::Multiple { variants, .. } => {
                                variants[RustcEnumVariantIdx(index)].clone()
                            }
                            _ => (*layout).clone(),
                        };
                        path.push(VARIANT_FIELDS | index as u32);
                        let field_types = db.field_types(variant.into());
                        let fields: Vec<_> = field_types
                            .iter()
                            .map(|(field, ty)| {
                                let ty = ty.ty().instantiate(self.interner, subst).skip_norm_wip();
                                (u32::from(field.into_raw()), ty)
                            })
                            .filter(|&(_, ty)| self.needs_drop(ty))
                            .collect();
                        self.drop_ladder(fields.len(), |this, at| {
                            let (field, field_ty) = fields[at];
                            let offset = variant_layout.fields.offset(field as usize).bytes();
                            let field_addr = this.builder.ins().iadd_imm(addr, offset as i64);
                            path.push(field);
                            let dropped =
                                this.drop_unmoved(field_addr, None, field_ty, local, path, state);
                            path.pop();
                            dropped
                        })?;
                        path.pop();
                        self.builder.ins().jump(done, &[]);
                    }
                    self.builder.switch_to_block(done);
                    return Ok(());
                }
                TyKind::Adt(adt_def, subst) => match adt_def.def_id() {
                    AdtId::StructId(id) => self
                        .cg
                        .db
                        .field_types(id.into())
                        .iter()
                        .map(|(_, ty)| ty.ty().instantiate(self.interner, subst).skip_norm_wip())
                        .collect(),
                    _ => unsupported!("partial move out of {ty:?}"),
                },
                _ => unsupported!("partial move out of {ty:?}"),
            };
            let layout = self.layout(ty)?;
            let mut fields: Vec<_> = fields.into_iter().enumerate().collect();
            if matches!(ty.kind(), TyKind::Array(..)) {
                // An array that has lost elements drops what is left from the back, except
                // that a run of whole elements is dropped front to back.
                let mut runs: Vec<Vec<(usize, Ty<'db>)>> = Vec::new();
                let mut in_run = false;
                for (i, field) in fields {
                    path.push(i as u32);
                    let whole = !self
                        .moved_paths
                        .iter()
                        .any(|it| it.local == local && it.path.starts_with(path));
                    path.pop();
                    match runs.last_mut() {
                        Some(run) if whole && in_run => run.push((i, field)),
                        _ => runs.push(vec![(i, field)]),
                    }
                    in_run = whole;
                }
                runs.reverse();
                fields = runs.into_iter().flatten().collect();
            }
            fields.retain(|&(_, field)| self.needs_drop(field));
            self.drop_ladder(fields.len(), |this, at| {
                let (i, field) = fields[at];
                let offset = layout.fields.offset(i).bytes() as i64;
                let field_addr = this.builder.ins().iadd_imm(addr, offset);
                let field_addr = this.aligned_for_drop(field_addr, field, ty)?;
                path.push(i as u32);
                let dropped = this.drop_unmoved(field_addr, None, field, local, path, state);
                path.pop();
                dropped
            })?;
        } else {
            self.drop_value(addr, meta, ty)?;
        }
        self.builder.ins().jump(done, &[]);
        self.builder.switch_to_block(done);
        Ok(())
    }

    /// Field types can still be projections such as `<str as ToOwned>::Owned`.
    fn normalize(&self, ty: Ty<'db>) -> Ty<'db> {
        crate::next_solver::obligation_ctxt::ObligationCtxt::new(&self.infcx)
            .structurally_normalize_ty(
                &crate::next_solver::infer::traits::ObligationCause::dummy(),
                self.env.param_env,
                ty,
            )
            .unwrap_or(ty)
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

    /// The value `block` switches on, when it is a constant: a literal, or an associated
    /// constant such as `T::IS_ZST` that the block has just read. rustc does not compile the
    /// arms such a switch never takes either, and they may not even type check for this `T`.
    fn switch_bits(&mut self, block: &crate::mir::BasicBlock, discr: &Operand) -> Option<u128> {
        if let Some(bits) = self.const_bits(discr) {
            return Some(bits);
        }
        let (OperandKind::Copy(place) | OperandKind::Move(place)) = &discr.kind else {
            return None;
        };
        if self.field_path(place).is_none_or(|it| !it.is_empty()) {
            return None;
        }
        let assigned = block.statements.iter().rev().find_map(|it| match &it.kind {
            StatementKind::Assign(to, rvalue) => Some((to, rvalue)),
            _ => None,
        });
        let Some((to, Rvalue::Use(Operand { kind: OperandKind::Constant { konst, ty }, .. }))) =
            assigned
        else {
            return None;
        };
        if to != place || !matches!(ty.as_ref().kind(), TyKind::Bool) {
            return None;
        }
        let ConstKind::Unevaluated(unevaluated) = konst.as_ref().kind() else { return None };
        let allocation = self.eval_const(unevaluated).ok()?;
        Some(*allocation.memory.first()? as u128)
    }

    fn place(&mut self, place: &StoredPlace) -> Result<(Value, PlaceTy<'db>)> {
        let place = self.place_ref(place)?;
        Ok((place.addr, place.ty))
    }

    fn place_ref(&mut self, place: &StoredPlace) -> Result<PlaceRef<'db>> {
        let LocalSlot { mut addr, borrowed } = self.locals[place.local];
        let mut meta = None;
        if borrowed {
            meta = Some(self.builder.ins().load(PTR, MEM, addr, 8));
            addr = self.builder.ins().load(PTR, MEM, addr, 0);
        }
        let mut ty = PlaceTy::from_ty(self.body.expect("MIR body").locals[place.local].ty.as_ref());
        ty.ty = self.normalize(ty.ty);
        for proj in place.projection.as_slice() {
            let prev = ty;
            ty = ty.projection_ty(&self.infcx, proj, self.env.param_env);
            // The pointee or field can be `Self::Buffer`, which has to be an array to index.
            ty.ty = self.normalize(ty.ty);
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
                        // A variant without values has no layout of its own, and code
                        // reading its fields cannot be reached.
                        Variants::Single { .. } | Variants::Empty
                            if field.0 as usize >= layout.fields.count() =>
                        {
                            Size::ZERO
                        }
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
                    addr = self.unsized_field_addr(addr, meta, prev.ty, offset, ty.ty)?;
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
        self.invoke(callee, &[index, len, location]);
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
                    let flag = self.drop_flags[place.local];
                    let paths = self
                        .moved_out_paths(place)
                        .filter(|paths| paths.iter().all(|it| !it.is_empty()));
                    let mask = paths.and_then(|paths| {
                        paths.into_iter().try_fold(0i64, |mask, path| {
                            Some(mask | 1i64 << self.moved_bit(place.local, path)?)
                        })
                    });
                    let state = match mask {
                        Some(mask) => {
                            let state = self.builder.ins().load(types::I64, MEM, flag, 0);
                            self.builder.ins().bor_imm(state, mask)
                        }
                        // FIXME: a move out of an enum or a box forgets the rest of the local,
                        // which leaks it.
                        None => self.builder.ins().iconst(types::I64, 0),
                    };
                    self.builder.ins().store(MEM, state, flag, 0);
                }
                Ok((addr, ty.ty))
            }
            OperandKind::Constant { konst, ty } => {
                let ty = ty.as_ref();
                if let Some(MirSpan::ExprId(expr)) = operand.span
                    && let Some(addr) = self.builtin_location(expr, ty)?
                {
                    return Ok((addr, ty));
                }
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
                        self.allocation_blob(allocation, false)?
                    }
                    kind => unsupported!("constant {kind:?}"),
                };
                Ok((self.blob_addr(blob)?, ty))
            }
            OperandKind::Allocation { allocation } => {
                let allocation = allocation.as_ref();
                let ty = allocation.ty;
                if let Some(MirSpan::ExprId(expr)) = operand.span
                    && let Some(addr) = self.builtin_location(expr, ty)?
                {
                    return Ok((addr, ty));
                }
                let blob = self.allocation_blob(allocation, false)?;
                Ok((self.blob_addr(blob)?, ty))
            }
            OperandKind::Static(id) => {
                let thread_local =
                    AttrFlags::query(self.cg.db, (*id).into()).contains(AttrFlags::THREAD_LOCAL);
                let data = self.static_data(*id)?;
                let data = self.cg.module.declare_data_in_func(data, self.builder.func);
                let value = if thread_local {
                    self.builder.ins().tls_value(PTR, data)
                } else {
                    self.builder.ins().symbol_value(PTR, data)
                };
                let ty = self.static_ty(*id);
                let layout = self.layout(ty)?;
                let addr = self.stack_slot(&layout);
                self.builder.ins().store(MEM, value, addr, 0);
                Ok((addr, ty))
            }
        }
    }

    /// The data object of a static: rustc's when it exports one, else our own copy of its value.
    fn static_data(&mut self, id: StaticId) -> Result<DataId> {
        let db = self.cg.db;
        let thread_local = AttrFlags::query(db, id.into()).contains(AttrFlags::THREAD_LOCAL);
        if let Some(symbol) = self.cg.symbols.static_(id) {
            return self
                .cg
                .module
                .declare_data(&symbol, Linkage::Import, false, thread_local)
                .map_err(|e| CodegenError::Backend(e.to_string()));
        }
        if let Some(&data) = self.cg.statics.get(&id).or_else(|| self.cg.frozen_statics.get(&id)) {
            return Ok(data);
        }
        let flags = StaticSignature::of(db, id).flags;
        if flags.contains(StaticFlags::EXTERN) {
            unsupported!("extern static without a rustc symbol");
        }
        if thread_local {
            unsupported!("thread local without a rustc symbol");
        }
        let allocation = db
            .const_eval_static(id)
            .map_err(|e| CodegenError::Frontend(format!("const eval: {e:?}")))?;
        let writable = flags.contains(StaticFlags::MUTABLE) || !self.is_freeze(allocation.ty);
        if writable && self.cg.own_statics != Some(id.krate(db)) {
            unsupported!("writable static without a rustc symbol");
        }
        let blob = self.allocation_blob(allocation, true)?;
        // Not `define_const`: a static has an address of its own.
        let identity = if writable { DataIdentity::Writable } else { DataIdentity::Own };
        let data = self.cg.define_data_as(blob, identity)?;
        if writable {
            self.cg.statics.insert(id, data);
        } else {
            self.cg.frozen_statics.insert(id, data);
        }
        Ok(data)
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
                let mut target = ConstBlob {
                    bytes: Vec::new(),
                    relocs: Vec::new(),
                    fn_relocs: Vec::new(),
                    align: 1,
                };
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
        Ok(ConstBlob { bytes, relocs: vec![(0, target, 0)], fn_relocs: Vec::new(), align })
    }

    /// `in_static` says the value is a static's, where memory behind a mutable pointer can be
    /// written to.
    fn allocation_blob(
        &mut self,
        allocation: Allocation<'db>,
        in_static: bool,
    ) -> Result<ConstBlob> {
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
        let mut blob = ConstBlob {
            bytes,
            relocs: Vec::new(),
            fn_relocs: Vec::new(),
            align: layout.align.abi.bytes(),
        };
        let mut map = Relocations { map: memory_map, in_static, defined: FxHashMap::default() };
        self.relocate(&mut blob, 0, *ty, &mut map)?;
        Ok(blob)
    }

    /// Const eval gives pointers as keys into `map`. Emits each target as its own data object
    /// and records a relocation for the pointer, walking `ty` to find them.
    fn relocate(
        &mut self,
        blob: &mut ConstBlob,
        offset: usize,
        ty: Ty<'db>,
        map: &mut Relocations<'_, 'db>,
    ) -> Result<()> {
        if let MemoryMap::Empty = map.map {
            return Ok(());
        }
        let db = self.cg.db;
        match ty.kind() {
            TyKind::Pat(base, _) => self.relocate(blob, offset, base, map)?,
            TyKind::Ref(_, pointee, mutability) => {
                let Some(addr) = blob.usize_at(offset) else {
                    unsupported!("constant pointer out of bounds");
                };
                // Const eval gives a vtable as an id into its table of types.
                let pointee = match pointee.kind() {
                    TyKind::Dynamic(..) => {
                        let erased =
                            blob.usize_at(offset + 8).and_then(|id| map.map.vtable_ty(id).ok());
                        let Some(erased) = erased else {
                            unsupported!("`dyn` constant without a known type");
                        };
                        let trait_ref = self.principal(pointee, erased);
                        let vtable = self.vtable(erased, trait_ref)?;
                        blob.point_to(offset + 8, vtable, 0);
                        erased
                    }
                    _ => pointee,
                };
                if let Some((id, at)) = map.map.static_at(addr) {
                    let target = self.static_data(id)?;
                    blob.point_to(offset, target, at);
                    return Ok(());
                }
                let Some((start, memory)) = map.map.memory_at(addr) else { return Ok(()) };
                let elem = match pointee.kind() {
                    TyKind::Str => None,
                    TyKind::Slice(elem) => Some(elem),
                    _ => Some(pointee),
                };
                if memory.is_empty() {
                    // Nothing is behind the pointer, and rustc makes it the alignment.
                    let align = match elem {
                        Some(elem) => self.layout(elem)?.align.abi.bytes(),
                        None => 1,
                    };
                    blob.bytes[offset..offset + 8].copy_from_slice(&align.to_le_bytes());
                    return Ok(());
                }
                if let Some(&target) = map.defined.get(&start) {
                    blob.point_to(offset, target, addr - start);
                    return Ok(());
                }
                let mut target = ConstBlob {
                    bytes: memory.to_vec(),
                    relocs: Vec::new(),
                    fn_relocs: Vec::new(),
                    align: 16,
                };
                let at = addr - start;
                match pointee.kind() {
                    TyKind::Str => (),
                    TyKind::Slice(elem) => {
                        let Some(len) = blob.usize_at(offset + 8) else {
                            unsupported!("constant slice length out of bounds");
                        };
                        let size = self.layout(elem)?.size.bytes_usize();
                        for i in 0..len {
                            self.relocate(&mut target, at + i * size, elem, map)?;
                        }
                    }
                    _ => self.relocate(&mut target, at, pointee, map)?,
                }
                let writable = map.in_static && mutability == Mutability::Mut;
                let target = self.cg.define_data(target, writable)?;
                map.defined.insert(start, target);
                blob.point_to(offset, target, at);
            }
            // Const eval gives function pointers as ids into its table of fn item types. A
            // function cast to a raw pointer keeps that id, so a raw pointer holding one is
            // taken to be a function. FIXME: const eval should track provenance instead.
            TyKind::FnPtr(..) | TyKind::RawPtr(..) => {
                let Some(id) = blob.usize_at(offset) else {
                    unsupported!("constant pointer out of bounds");
                };
                let fn_ty = match map.map.vtable_ty(id) {
                    Ok(fn_ty)
                        if matches!(fn_ty.kind(), TyKind::FnDef(..) | TyKind::Closure(..)) =>
                    {
                        fn_ty
                    }
                    _ if let TyKind::RawPtr(_, mutability) = ty.kind() => {
                        self.relocate_data_pointer(blob, offset, id, mutability, map)?;
                        return Ok(());
                    }
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
                    // Const eval does not say which bytes of a union are a pointer, so every
                    // word that is the address of something it kept is taken to be one.
                    AdtId::UnionId(_) => {
                        let align = layout.align.abi.bytes_usize();
                        if align.is_multiple_of(8) {
                            for word in 0..layout.size.bytes_usize() / 8 {
                                let at = offset + word * 8;
                                if let Some(id) = blob.usize_at(at) {
                                    self.relocate_data_pointer(blob, at, id, Mutability::Not, map)?;
                                }
                            }
                        }
                        return Ok(());
                    }
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

    /// Points the word at `offset`, which const eval left holding address `addr`, at the static
    /// or the allocation that address is in.
    fn relocate_data_pointer(
        &mut self,
        blob: &mut ConstBlob,
        offset: usize,
        addr: usize,
        mutability: Mutability,
        map: &mut Relocations<'_, 'db>,
    ) -> Result<()> {
        if let Some((id, at)) = map.map.static_at(addr) {
            let target = self.static_data(id)?;
            blob.point_to(offset, target, at);
            return Ok(());
        }
        // Const eval keeps the whole allocation a raw pointer points into.
        // FIXME: pointers inside that allocation are not relocated.
        let Some((start, memory)) = map.map.memory_at(addr) else { return Ok(()) };
        let target = match map.defined.get(&start) {
            Some(&target) => target,
            None => {
                let target = ConstBlob {
                    bytes: memory.to_vec(),
                    relocs: Vec::new(),
                    fn_relocs: Vec::new(),
                    align: 16,
                };
                let writable = map.in_static && mutability == Mutability::Mut;
                let target = self.cg.define_data(target, writable)?;
                map.defined.insert(start, target);
                target
            }
        };
        blob.point_to(offset, target, addr - start);
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
        if self.locals[place.local].borrowed && place.projection.as_slice().is_empty() {
            let Rvalue::Use(operand) = rvalue else {
                unsupported!("unsized local assigned {rvalue:?}");
            };
            let (OperandKind::Copy(from) | OperandKind::Move(from)) = &operand.kind else {
                unsupported!("unsized local assigned a constant");
            };
            let PlaceRef { addr, meta: Some(meta), .. } = self.place_ref(from)? else {
                unsupported!("unsized local assigned a place without metadata");
            };
            // Marks the place as moved out of.
            self.operand(operand)?;
            let slot = self.locals[place.local].addr;
            self.builder.ins().store(MEM, addr, slot, 0);
            self.builder.ins().store(MEM, meta, slot, 8);
            return Ok(());
        }
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
                    if let Some(layout) = self.variant_layout(dst, *variant, args.as_ref())? {
                        self.write_fields(dst, &layout, operands)?;
                    }
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
                    if let TyKind::Coroutine(..) = ty.as_ref().kind() {
                        // The last field says where the body left off.
                        let state = layout.fields.offset(layout.fields.count() - 1).bytes();
                        let state = self.builder.ins().iadd_imm(dst, state as i64);
                        self.store_state(state, UNRESUMED);
                    }
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
            CastKind::PointerCoercion(
                PointerCast::ReifyFnPointer | PointerCast::ClosureFnPointer(_),
            ) => {
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
        if let TyKind::Closure(..) = fn_ty.kind() {
            return self.closure_fn_pointer(fn_ty);
        }
        let TyKind::FnDef(def, generic_args) = fn_ty.kind() else {
            unsupported!("fn pointer to {fn_ty:?}");
        };
        let CallableDefId::FunctionId(func) = def.0 else {
            return self.reified(fn_ty, false);
        };
        let db = self.cg.db;
        if FunctionSignature::is_intrinsic(db, func) {
            unsupported!("fn pointer to an intrinsic");
        }
        if is_dyn_method(self.interner, self.env.param_env, func, generic_args).is_some() {
            return self.reified(fn_ty, false);
        }
        let (callee, callee_args) = db.lookup_impl_method(self.env, func, generic_args);
        let either::Either::Left(callee) = callee else {
            unsupported!("fn pointer to a builtin derive method");
        };
        let sig = self.interner.instantiate_bound_regions_with_erased(fn_ty.fn_sig(self.interner));
        let abi = self.fn_abi(sig.abi(), sig.output(), sig.inputs().iter().copied())?;
        if track_caller(db, callee) {
            unsupported!("pointer to a `#[track_caller]` function");
        }
        let sig = self.signature(&abi.args, abi.ret);
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
            // There is no value to read it from, so this cannot be reached; rustc leaves the
            // result undefined.
            Variants::Empty => Ok(self.iconst(to.ty, 0)),
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
    /// `None` for a variant the layout has no room for, which is one without values: code
    /// building it cannot be reached.
    fn variant_layout(
        &mut self,
        dst: Value,
        variant: VariantId,
        args: GenericArgs<'db>,
    ) -> Result<Option<Arc<Layout>>> {
        let db = self.cg.db;
        let adt: AdtId = variant.adt_id(db);
        let layout = self.layout(Ty::new_adt(self.interner, adt, args))?;
        let (tag, tag_encoding, tag_field, variants) = match &layout.variants {
            Variants::Multiple { tag, tag_encoding, tag_field, variants } => {
                (tag, tag_encoding, tag_field, variants)
            }
            Variants::Single { index } => {
                let inhabited = match variant {
                    VariantId::EnumVariantId(variant) => variant.index(db) == index.0,
                    _ => true,
                };
                return Ok(inhabited.then_some(layout));
            }
            Variants::Empty => return Ok(None),
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
        Ok(Some(Arc::new(variants[RustcEnumVariantIdx(index)].clone())))
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
        // A constant pattern of a type that has no `==` of its own: rustc matches such a
        // constant field by field, never through `PartialEq`.
        if let BinOp::Eq | BinOp::Ne = op
            && matches!(
                ty.kind(),
                TyKind::Adt(..)
                    | TyKind::Tuple(_)
                    | TyKind::Array(..)
                    | TyKind::Slice(_)
                    | TyKind::Str
            )
        {
            let metas = match (l_ref, r_ref) {
                (Some(l_ref), Some(r_ref)) if self.is_unsized(ty) => Some((
                    self.builder.ins().load(PTR, MEM, l_ref, 8),
                    self.builder.ins().load(PTR, MEM, r_ref, 8),
                )),
                _ => None,
            };
            let (differ, done) = (self.builder.create_block(), self.builder.create_block());
            self.builder.append_block_param(done, types::I8);
            self.branch_unless_equal(l_addr, r_addr, metas, ty, differ)?;
            let equal = self.builder.ins().iconst(types::I8, matches!(op, BinOp::Eq) as i64);
            self.builder.ins().jump(done, &[equal.into()]);
            self.builder.switch_to_block(differ);
            let unequal = self.builder.ins().iconst(types::I8, matches!(op, BinOp::Ne) as i64);
            self.builder.ins().jump(done, &[unequal.into()]);
            self.builder.switch_to_block(done);
            return Ok(self.builder.block_params(done)[0]);
        }
        if matches!(ty.kind(), TyKind::RawPtr(..))
            && matches!(self.layout(ty)?.backend_repr, BackendRepr::ScalarPair(..))
        {
            // Wide pointers compare by address, then by metadata.
            let cc = match op {
                BinOp::Eq => IntCC::Equal,
                BinOp::Ne => IntCC::NotEqual,
                BinOp::Lt => IntCC::UnsignedLessThan,
                BinOp::Le => IntCC::UnsignedLessThanOrEqual,
                BinOp::Gt => IntCC::UnsignedGreaterThan,
                BinOp::Ge => IntCC::UnsignedGreaterThanOrEqual,
                _ => unsupported!("{op:?} on wide pointers"),
            };
            let [l_ptr, r_ptr] =
                [l_addr, r_addr].map(|it| self.builder.ins().load(PTR, MEM, it, 0));
            let [l_meta, r_meta] =
                [l_addr, r_addr].map(|it| self.builder.ins().load(PTR, MEM, it, 8));
            let same_ptr = self.builder.ins().icmp(IntCC::Equal, l_ptr, r_ptr);
            let by_meta = self.builder.ins().icmp(cc, l_meta, r_meta);
            let by_ptr = self.builder.ins().icmp(cc, l_ptr, r_ptr);
            return Ok(match op {
                BinOp::Eq => self.builder.ins().band(same_ptr, by_meta),
                BinOp::Ne => self.builder.ins().bor(by_ptr, by_meta),
                _ => self.builder.ins().select(same_ptr, by_meta, by_ptr),
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
                let div = matches!(op, BinOp::Div);
                let zero = self.builder.ins().icmp_imm(IntCC::Equal, r, 0);
                self.panic_if(
                    zero,
                    if div { OverflowOp::DivByZero } else { OverflowOp::RemByZero },
                )?;
                if signed {
                    let ty = scalar.ty;
                    let min = if ty == types::I128 {
                        let low = self.builder.ins().iconst(types::I64, 0);
                        let high = self.builder.ins().iconst(types::I64, i64::MIN);
                        self.builder.ins().iconcat(low, high)
                    } else {
                        self.builder.ins().iconst(ty, i64::MIN >> (64 - ty.bits()))
                    };
                    let is_min = self.builder.ins().icmp(IntCC::Equal, l, min);
                    let minus_one = self.builder.ins().icmp_imm(IntCC::Equal, r, -1);
                    let overflow = self.builder.ins().band(is_min, minus_one);
                    self.panic_if(overflow, if div { OverflowOp::Div } else { OverflowOp::Rem })?;
                }
                self.divide(l, r, div, signed)?
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
        Ok(self.invoke(callee, args)[0])
    }

    /// `as` between a float and an integer or another float.
    fn float_cast(&mut self, value: Value, from: AbiScalar, to: AbiScalar) -> Result<Value> {
        if from.ty == types::I128 || to.ty == types::I128 {
            // compiler-builtins' conversions, which saturate and turn NaN into zero as `as` does.
            let name = match (from.ty, from.signed, to.ty, to.signed) {
                (types::F32, _, _, true) => "__fixsfti",
                (types::F32, _, _, false) => "__fixunssfti",
                (types::F64, _, _, true) => "__fixdfti",
                (types::F64, _, _, false) => "__fixunsdfti",
                (_, true, types::F32, _) => "__floattisf",
                (_, false, types::F32, _) => "__floatuntisf",
                (_, true, types::F64, _) => "__floattidf",
                (_, false, types::F64, _) => "__floatuntidf",
                _ => unsupported!("cast from {} to {}", from.ty, to.ty),
            };
            let mut sig = Signature::new(CallConv::SystemV);
            sig.params.push(AbiParam::new(from.ty));
            sig.returns.push(AbiParam::new(to.ty));
            let callee = self.cg.import(name, &sig)?;
            let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
            return Ok(self.invoke(callee, &[value])[0]);
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
            OverflowOp::Add => "add_overflow",
            OverflowOp::Sub => "sub_overflow",
            OverflowOp::Mul => "mul_overflow",
            OverflowOp::Div => "div_overflow",
            OverflowOp::Rem => "rem_overflow",
            OverflowOp::Neg => "neg_overflow",
            OverflowOp::Shl => "shl_overflow",
            OverflowOp::Shr => "shr_overflow",
            OverflowOp::DivByZero => "div_by_zero",
            OverflowOp::RemByZero => "rem_by_zero",
        };
        let path = format!("core::panicking::panic_const::panic_const_{name}");
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
        self.invoke(callee, &[location]);
        self.builder.ins().trap(TrapCode::user(1).expect("non-zero trap code"));

        self.builder.switch_to_block(ok_block);
        Ok(())
    }

    /// File, line and column of the statement being compiled, as rustc reports them.
    fn source_location(&self) -> Option<(String, u32, u32)> {
        let db = self.cg.db;
        let InferBodyId::DefWithBodyId(owner) = self.body?.owner else { return None };
        let source_map = &Body::with_source_map(db, owner).1;
        let source = match self.span {
            MirSpan::ExprId(expr) => source_map.expr_syntax(expr).ok()?,
            MirSpan::PatId(pat) => source_map.pat_syntax(pat).ok()?,
            MirSpan::BindingId(_) | MirSpan::SelfParam | MirSpan::Unknown => return None,
        };
        let mut range = source.value.syntax_node_ptr().text_range();
        if source.value.syntax_node_ptr().kind() == SyntaxKind::METHOD_CALL_EXPR {
            // A method call is reported from the method's name on.
            let root = source.file_id.parse_or_expand(db);
            let call = ast::MethodCallExpr::cast(source.value.syntax_node_ptr().to_node(&root))?;
            range = TextRange::new(call.name_ref()?.syntax().text_range().start(), range.end());
        }
        if source.value.syntax_node_ptr().kind() == SyntaxKind::INDEX_EXPR {
            // An index is reported from its bracket on.
            let root = source.file_id.parse_or_expand(db);
            let index = ast::IndexExpr::cast(source.value.syntax_node_ptr().to_node(&root))?;
            range = TextRange::new(index.l_brack_token()?.text_range().start(), range.end());
        }
        self.location_of(source.file_id, range)
    }

    /// The macro call an expansion comes from that is itself written out, in the source or in
    /// what a macro was passed, and not by another macro.
    fn written_call(&self, file_id: HirFileId) -> Option<InFile<SyntaxNode>> {
        let db = self.cg.db;
        let mut call = file_id.macro_file()?.loc(db).to_node(db);
        while let Some(outer) = call.file_id.macro_file() {
            let start = TextRange::empty(call.value.text_range().start());
            if call.file_id.span_map(db).span_for_range(start).ctx.is_root() {
                break;
            }
            call = outer.loc(db).to_node(db);
        }
        Some(call)
    }

    fn location_of(&self, file_id: HirFileId, range: TextRange) -> Option<(String, u32, u32)> {
        let db = self.cg.db;
        let source = InFile::new(file_id, ());
        // What a macro writes itself is reported from where the outermost macro is called;
        // what it is passed keeps its own place.
        let own_tokens = source.file_id.macro_file().is_some()
            && !source
                .file_id
                .span_map(db)
                .span_for_range(TextRange::empty(range.start()))
                .ctx
                .is_root();
        let FileRange { file_id, range } = match self.written_call(source.file_id) {
            Some(call) if own_tokens => InFile::new(call.file_id, call.value.text_range())
                .original_node_file_range_rooted(db),
            _ => InFile::new(source.file_id, range).original_node_file_range_rooted(db),
        };
        let file_id = file_id.file_id(db);
        let text = db.file_text(file_id).text(db);
        let before = text.get(..usize::from(range.start()))?;
        let line_start = before.rfind('\n').map_or(0, |it| it + 1);
        let line = before.bytes().filter(|&it| it == b'\n').count() + 1;
        let col = before[line_start..].chars().count() + 1;
        Some((self.cg.symbols.source_path(file_id)?, line as u32, col as u32))
    }

    /// Whether `expr` is what `line!()`, `column!()` or `file!()` expanded to, and the
    /// expansion it is in.
    fn location_macro(
        &self,
        body: &MirBody<'db>,
        expr: ExprId,
    ) -> Option<(BuiltinFnLikeExpander, HirFileId)> {
        let db = self.cg.db;
        let InferBodyId::DefWithBodyId(owner) = body.owner else { return None };
        let (body, source_map) = Body::with_source_map(db, owner);
        if !matches!(body[expr], hir_def::hir::Expr::Literal(_)) {
            return None;
        }
        let file_id = source_map.expr_syntax(expr).ok()?.file_id;
        let MacroDefKind::BuiltIn(_, expander) = file_id.macro_file()?.loc(db).def.kind else {
            return None;
        };
        matches!(
            expander,
            BuiltinFnLikeExpander::Line
                | BuiltinFnLikeExpander::Column
                | BuiltinFnLikeExpander::File
        )
        .then_some((expander, file_id))
    }

    /// Whether a promoted expression takes its value from where it is in the source.
    fn mentions_location(&self, body: &MirBody<'db>, expr: ExprId) -> bool {
        let InferBodyId::DefWithBodyId(owner) = body.owner else { return false };
        if self.location_macro(body, expr).is_some() {
            return true;
        }
        let mut found = false;
        Body::of(self.cg.db, owner)
            .walk_child_exprs(expr, |child| found |= self.mentions_location(body, child));
        found
    }

    /// The value of a literal that `line!()`, `column!()` or `file!()` expanded to.
    /// rust-analyzer expands them to placeholders, since a real value would make every body
    /// below an edit change.
    fn builtin_location(&mut self, expr: ExprId, ty: Ty<'db>) -> Result<Option<Value>> {
        let db = self.cg.db;
        let Some((expander, file_id)) = self.body.and_then(|it| self.location_macro(it, expr))
        else {
            return Ok(None);
        };
        let Some(site) = self.written_call(file_id) else { return Ok(None) };
        let range = InFile::new(site.file_id, site.value.text_range());
        let FileRange { file_id, range } = range.original_node_file_range_rooted(db);
        let Some((file, line, col)) = self.location_of(file_id.into(), range) else {
            return Ok(None);
        };
        let number = match expander {
            BuiltinFnLikeExpander::Line => line,
            BuiltinFnLikeExpander::Column => col,
            _ => {
                let len = file.len();
                let name = self.cg.define_data(
                    ConstBlob {
                        bytes: file.into_bytes(),
                        relocs: Vec::new(),
                        fn_relocs: Vec::new(),
                        align: 1,
                    },
                    false,
                )?;
                let mut fat = ConstBlob {
                    bytes: vec![0; 16],
                    relocs: Vec::new(),
                    fn_relocs: Vec::new(),
                    align: 8,
                };
                fat.point_to(0, name, 0);
                fat.bytes[8..].copy_from_slice(&(len as u64).to_le_bytes());
                return self.blob_addr(fat).map(Some);
            }
        };
        if !matches!(ty.kind(), TyKind::Uint(UintTy::U32)) {
            return Ok(None);
        }
        let layout = self.layout(ty)?;
        let addr = self.stack_slot(&layout);
        let value = self.builder.ins().iconst(types::I32, number as i64);
        self.builder.ins().store(MEM, value, addr, 0);
        Ok(Some(addr))
    }

    /// A `core::panic::Location` (`{ file: &str, line: u32, col: u32 }`) for `#[track_caller]`.
    fn caller_location(&mut self) -> Result<Value> {
        if let Some(location) = self.location {
            return Ok(location);
        }
        // FIXME: the layout of `Location` should come from `layout_of_ty`. Locations also end up
        // in cached code, where they go stale when lines shift; they belong in a table of their
        // own.
        let backend = |e: cranelift_module::ModuleError| CodegenError::Backend(e.to_string());
        let n = self.cg.location_counter;
        self.cg.location_counter += 1;
        let (file, line, col) =
            self.source_location().unwrap_or_else(|| ("<rustd>".to_owned(), 0, 0));
        // The name is followed by a NUL that its length leaves out.
        let mut name = file.clone().into_bytes();
        name.push(0);
        let file_id = self.cg.define_data(
            ConstBlob { bytes: name, relocs: Vec::new(), fn_relocs: Vec::new(), align: 1 },
            false,
        )?;

        let loc_id = self
            .cg
            .module
            .declare_data(&format!(".Lrustd.loc.{n}"), Linkage::Local, false, false)
            .map_err(backend)?;
        let mut loc_data = DataDescription::new();
        let mut bytes = vec![0u8; 24];
        bytes[8..16].copy_from_slice(&(file.len() as u64).to_le_bytes());
        bytes[16..20].copy_from_slice(&line.to_le_bytes());
        bytes[20..24].copy_from_slice(&col.to_le_bytes());
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
                if let Some((state, _)) = self.suspended {
                    self.store_state(state, RETURNED);
                }
                self.return_()?;
            }
            TerminatorKind::Yield { resume, .. } => {
                let Some((state, resumes)) = &self.suspended else {
                    unsupported!("yield outside of an async body");
                };
                let Some(index) = resumes.iter().position(|it| it == resume) else {
                    unsupported!("yield to a block that is not resumed in");
                };
                self.store_state(*state, SUSPENDED + index as i64);
                self.return_()?;
            }
            TerminatorKind::Unreachable => {
                self.builder.ins().trap(TrapCode::user(2).expect("non-zero trap code"));
            }
            TerminatorKind::Drop { place, target, .. } => {
                let block = self.builder.create_block();
                self.builder.ins().jump(block, &[]);
                self.deferred_drops.push((block, place.clone(), *target));
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
            let abi = fn_ty.fn_sig(self.interner).abi();
            self.emit_call(Callee::Pointer(pointer, abi), &values, dst, dst_ty.ty)?;
            return self.finish_call(destination, target);
        }
        let TyKind::FnDef(def, generic_args) = fn_ty.kind() else {
            unsupported!("call of {fn_ty:?}");
        };
        let func = match def.0 {
            CallableDefId::FunctionId(func) => func,
            CallableDefId::StructId(id) => {
                if let Some(layout) = self.variant_layout(dst, id.into(), generic_args)? {
                    self.write_fields(dst, &layout, args)?;
                }
                return self.finish_call(destination, target);
            }
            CallableDefId::EnumVariantId(id) => {
                if let Some(layout) = self.variant_layout(dst, id.into(), generic_args)? {
                    self.write_fields(dst, &layout, args)?;
                }
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
            let meta = self.is_unsized(ty).then(|| self.builder.ins().load(PTR, MEM, src, 8));
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
                    // These have a body that only stands in for the const evaluator.
                    let compile_time = matches!(
                        signature.name.as_str(),
                        "const_allocate"
                            | "const_deallocate"
                            | "const_make_global"
                            | "size_of_type_id"
                            | "type_id_vtable"
                            | "type_of"
                    );
                    if overridden || compile_time || !signature.has_body() {
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

    /// `Fn::call`, `FnMut::call_mut` or `FnOnce::call_once`. For a call expression MIR passes
    /// the callee first and then the arguments one by one, not as a tuple.
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
        // A `dyn FnOnce` called by value, as `Box<dyn FnOnce>` does with what it holds: the
        // `call_once` slot takes a pointer to the callee and leaves it dropped.
        if let OperandKind::Copy(place) | OperandKind::Move(place) = &callee.kind {
            let place = self.place_ref(place)?;
            if let TyKind::Dynamic(..) = place.ty.ty.kind() {
                let dyn_ty = place.ty.ty;
                let Some(vtable) = place.meta else {
                    unsupported!("Fn-trait call on {dyn_ty:?} without a vtable");
                };
                // Marks the callee as moved out of.
                self.operand(callee)?;
                let Some(trait_ref) = self.principal(dyn_ty, dyn_ty) else {
                    unsupported!("vtable call on {dyn_ty:?}");
                };
                let Some(slot) = VtableLayout::of(self.interner, trait_ref).method(method) else {
                    unsupported!("method missing from the vtable of {dyn_ty:?}");
                };
                let pointer = self.builder.ins().load(PTR, MEM, vtable, slot as i32 * 8);
                let thin = self.scratch();
                self.builder.ins().store(MEM, place.addr, thin, 0);
                let thin_ty = Ty::new_imm_ptr(self.interner, Ty::new_unit(self.interner));
                let mut values = vec![(thin, thin_ty)];
                let tuple_ty = method_args.type_at(1);
                match (&self.operands(rest)?[..], tuple_ty.kind()) {
                    (&[(tuple, ty)], TyKind::Tuple(fields)) if ty == tuple_ty => {
                        let layout = self.layout(tuple_ty)?;
                        values.extend(fields.iter().enumerate().map(|(i, field)| {
                            let offset = layout.fields.offset(i).bytes() as i64;
                            (self.builder.ins().iadd_imm(tuple, offset), field)
                        }));
                    }
                    (rest, _) => values.extend_from_slice(rest),
                }
                let callee = Callee::Pointer(pointer, ExternAbi::Rust);
                return self.emit_call(callee, &values, dst, dst_ty);
            }
        }
        let (mut addr, mut ty) = self.operand(callee)?;
        let mut values = self.operands(rest)?;
        // A call written as `f.call_mut(args)` does pass the tuple, which the callee takes
        // apart all the same.
        let tuple_ty = method_args.type_at(1);
        if let (&[(tuple, ty)], TyKind::Tuple(fields)) = (&values[..], tuple_ty.kind())
            && ty == tuple_ty
        {
            let layout = self.layout(tuple_ty)?;
            values = fields
                .iter()
                .enumerate()
                .map(|(i, field)| {
                    let offset = layout.fields.offset(i).bytes() as i64;
                    (self.builder.ins().iadd_imm(tuple, offset), field)
                })
                .collect();
        }
        // The innermost reference to the callee and the address it is stored at.
        let mut by_ref = None;
        while let TyKind::Ref(_, inner, mutability) = ty.kind() {
            if self.implements_fn_itself(method, ty, tuple_ty) {
                break;
            }
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
                return self.call_virtual(method, inner, addr, ty, &values, dst, dst_ty);
            }
            addr = self.builder.ins().load(PTR, MEM, addr, 0);
            ty = inner;
        }
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
                            _ => Some(self.layout(dst_ty)?),
                        };
                        let Some(layout) = layout else {
                            return Ok(());
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
                let abi = ty.fn_sig(self.interner).abi();
                self.emit_call(Callee::Pointer(pointer, abi), &values, dst, dst_ty)
            }
            TyKind::Closure(id, generic_args) => {
                let parent_args =
                    GenericArgs::new_from_slice(generic_args.as_closure().parent_args());
                let body = self
                    .cg
                    .db
                    .monomorphized_mir_body_for_closure(id.0, parent_args.store(), self.env.store())
                    .map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
                if body.param_locals.len() != values.len() + 1 {
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
                let call = |this: &mut Self| {
                    this.emit_call(Callee::Instance(def, parent_args), &values, dst, dst_ty)
                };
                // A closure that is passed by value to a body that only borrows it is the
                // caller's to drop, whether the body returns or unwinds.
                if by_ref.is_none() && self_ty.as_reference().is_some() {
                    self.guarded(call, |this| this.drop_value(addr, None, ty))?;
                    self.drop_value(addr, None, ty)
                } else {
                    call(self)
                }
            }
            // A type with its own `Fn` impls, which take the arguments as a tuple.
            TyKind::Adt(..) | TyKind::Ref(..) => {
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

    /// Whether the reference type `ty` has an `Fn` impl written for it, as opposed to the ones
    /// every reference to a callable has, which call what it points to.
    fn implements_fn_itself(&self, method: FunctionId, ty: Ty<'db>, tuple_ty: Ty<'db>) -> bool {
        let db = self.cg.db;
        let args = GenericArgs::new_from_slice(&[ty.into(), tuple_ty.into()]);
        let (either::Either::Left(callee), _) = db.lookup_impl_method(self.env, method, args)
        else {
            return false;
        };
        let ItemContainerId::ImplId(impl_) = callee.lookup(db).container else {
            return false;
        };
        match db.impl_self_ty(impl_).skip_binder().kind() {
            TyKind::Ref(_, pointee, _) => !matches!(pointee.kind(), TyKind::Param(_)),
            _ => false,
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
        self.emit_call(Callee::Pointer(pointer, ExternAbi::Rust), &values, dst, dst_ty)
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
            (TyKind::Dynamic(..), TyKind::Dynamic(..)) => {
                let Some(vtable) = old else {
                    unsupported!("upcast of a thin pointer");
                };
                let (Some(trait_ref), Some(target)) =
                    (self.principal(from, from), self.principal(to, from))
                else {
                    // Dropping the principal: every vtable has the header.
                    return Ok(vtable);
                };
                if trait_ref == target {
                    // Only auto traits or lifetimes change.
                    return Ok(vtable);
                }
                match VtableLayout::of(self.interner, trait_ref).upcast(target) {
                    Some(Upcast::Same) => Ok(vtable),
                    Some(Upcast::Load(slot)) => {
                        Ok(self.builder.ins().load(PTR, MEM, vtable, slot as i32 * 8))
                    }
                    None => unsupported!("upcast of {from:?} to {to:?}"),
                }
            }
            (_, TyKind::Dynamic(..)) => {
                if self.is_unsized(from) {
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
        if Some(func) == lang_items.FuturePoll
            && let TyKind::Coroutine(id, args) = self_ty.kind()
        {
            let parent_args = GenericArgs::new_from_slice(args.as_coroutine().parent_args());
            let body = db
                .monomorphized_mir_body_for_coroutine(id.0, parent_args.store(), self.env.store())
                .map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
            let sig = self.body_signature(body, false)?;
            let def = InstanceDef::Coroutine(id.0.as_id());
            return self.cg.callee(def, parent_args, self.env, &sig);
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
        if track_caller(db, callee) {
            // FIXME: rustc puts a shim in the vtable that supplies the location.
            unsupported!("`#[track_caller]` method in a vtable");
        }
        let sig = self.body_signature(body, false)?;
        self.cg.callee(InstanceDef::Fn(callee), callee_args, self.env, &sig)
    }

    /// `call`, `call_mut` or `call_once` in the vtable of a closure.
    fn vtable_fn_method(&mut self, ty: Ty<'db>, once: bool) -> Result<FuncId> {
        let TyKind::Closure(id, generic_args) = ty.kind() else {
            return self.reified(ty, true);
        };
        let parent_args = GenericArgs::new_from_slice(generic_args.as_closure().parent_args());
        let body = self
            .cg
            .db
            .monomorphized_mir_body_for_closure(id.0, parent_args.store(), self.env.store())
            .map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
        let sig = self.body_signature(body, false)?;
        let Some(&receiver) = body.param_locals.first() else {
            unsupported!("closure without a receiver");
        };
        let receiver_ty = body.locals[receiver].ty.as_ref();
        let closure = InstanceDef::Closure(id.0.as_id());
        if receiver_ty.as_reference().is_none() {
            // The body takes the closure by value. Only when that means "through a pointer to
            // memory the body owns" is it what the `call_once` slot is called with.
            if !once {
                unsupported!("vtable shim for a closure taken by value");
            }
            let layout = self.layout(receiver_ty)?;
            let mode = self.pass_mode(&layout, false)?;
            if matches!(mode, PassMode::Indirect) {
                return self.cg.callee(closure, parent_args, self.env, &sig);
            }
            let sig = Self::by_pointer(&sig, mode);
            let args = GenericArgs::new_from_slice(&[ty.into()]);
            return self.cg.callee(InstanceDef::CallOnceShim, args, self.env, &sig);
        }
        if once {
            let args = GenericArgs::new_from_slice(&[ty.into()]);
            return self.cg.callee(InstanceDef::CallOnceShim, args, self.env, &sig);
        }
        self.cg.callee(closure, parent_args, self.env, &sig)
    }

    /// The `ReifyShim` of a fn item or fn pointer type.
    fn reified(&mut self, ty: Ty<'db>, receiver: bool) -> Result<FuncId> {
        let sig = self.reify_signature(ty, receiver)?.1;
        let args = GenericArgs::new_from_slice(&[ty.into()]);
        self.cg.callee(InstanceDef::ReifyShim { receiver }, args, self.env, &sig)
    }

    /// What a `ReifyShim` is called with and how.
    fn reify_signature(
        &mut self,
        ty: Ty<'db>,
        receiver: bool,
    ) -> Result<(rustc_type_ir::FnSig<DbInterner<'db>>, Signature, FnAbi)> {
        if !matches!(ty.kind(), TyKind::FnDef(..) | TyKind::FnPtr(..)) {
            unsupported!("Fn-trait vtable of {ty:?}");
        }
        let fn_sig = self.interner.instantiate_bound_regions_with_erased(ty.fn_sig(self.interner));
        if fn_sig.c_variadic() {
            unsupported!("shim for a variadic function");
        }
        // A vtable is called as a Rust function, whatever the function behind it is.
        let abi = if receiver { ExternAbi::Rust } else { fn_sig.abi() };
        let abi = self.fn_abi(abi, fn_sig.output(), fn_sig.inputs().iter().copied())?;
        let mut sig = self.signature(&abi.args, abi.ret);
        if receiver {
            let sret = matches!(abi.ret, PassMode::Indirect);
            sig.params.insert(sret as usize, AbiParam::new(PTR));
        }
        Ok((fn_sig, sig, abi))
    }

    /// The body of a `ReifyShim`: calls what the type names with the arguments it is handed.
    fn reify_shim(&mut self, ty: Ty<'db>, receiver: bool) -> Result<()> {
        let (fn_sig, sig, FnAbi { ret, args: modes }) = self.reify_signature(ty, receiver)?;
        self.builder.func.signature = sig;
        let entry = self.builder.create_block();
        self.builder.append_block_params_for_function_params(entry);
        self.builder.switch_to_block(entry);
        let mut incoming = self.builder.block_params(entry).to_vec().into_iter();
        let dst_ty = fn_sig.output();
        let dst_layout = self.layout(dst_ty)?;
        let dst = match ret {
            PassMode::Indirect => incoming.next().expect("sret param"),
            _ => self.stack_slot(&dst_layout),
        };
        let receiver = if receiver { incoming.next() } else { None };
        let mut values = Vec::new();
        for (&ty, mode) in fn_sig.inputs().iter().zip(modes) {
            let layout = self.layout(ty)?;
            let addr = match mode {
                PassMode::Indirect | PassMode::OnStack(_) => {
                    incoming.next().expect("indirect param")
                }
                _ => self.stack_slot(&layout),
            };
            self.receive(mode, &mut incoming, addr, &layout);
            values.push((addr, ty));
        }
        match ty.kind() {
            TyKind::FnDef(def, generic_args) => match def.0 {
                CallableDefId::FunctionId(func) => {
                    if FunctionSignature::is_intrinsic(self.cg.db, func) {
                        unsupported!("shim for an intrinsic");
                    }
                    if track_caller(self.cg.db, func) {
                        // FIXME: rustc's shim passes the location of the function itself.
                        unsupported!("pointer to a `#[track_caller]` function");
                    }
                    self.call_direct(func, generic_args, &values, dst, dst_ty)?;
                }
                CallableDefId::StructId(_) | CallableDefId::EnumVariantId(_) => {
                    let TyKind::Adt(_, adt_args) = dst_ty.kind() else {
                        unsupported!("constructor of {dst_ty:?}");
                    };
                    let layout = match def.0 {
                        CallableDefId::EnumVariantId(id) => {
                            self.variant_layout(dst, id.into(), adt_args)?
                        }
                        _ => Some(self.layout(dst_ty)?),
                    };
                    if let Some(layout) = layout {
                        for (i, &(src, ty)) in values.iter().enumerate() {
                            let size = self.layout(ty)?.size;
                            let offset = layout.fields.offset(i).bytes() as i64;
                            let field = self.builder.ins().iadd_imm(dst, offset);
                            self.copy(field, src, size);
                        }
                    }
                }
            },
            _ => {
                let receiver = receiver.expect("a fn pointer is only reified for a vtable");
                let pointer = self.builder.ins().load(PTR, MEM, receiver, 0);
                self.emit_call(Callee::Pointer(pointer, fn_sig.abi()), &values, dst, dst_ty)?;
            }
        }
        let mut results = Vec::new();
        if !matches!(ret, PassMode::Indirect) {
            self.pass(ret, dst, &dst_layout, &mut results);
        }
        self.builder.ins().return_(&results);
        Ok(())
    }

    /// The function a closure that captures nothing is as a function pointer.
    fn closure_fn_pointer(&mut self, ty: Ty<'db>) -> Result<FuncId> {
        let TyKind::Closure(id, generic_args) = ty.kind() else {
            unsupported!("fn pointer to {ty:?}");
        };
        let parent_args = GenericArgs::new_from_slice(generic_args.as_closure().parent_args());
        let body = self
            .cg
            .db
            .monomorphized_mir_body_for_closure(id.0, parent_args.store(), self.env.store())
            .map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
        let mut sig = self.body_signature(body, false)?;
        let Some(&receiver) = body.param_locals.first() else {
            unsupported!("closure without a receiver");
        };
        if body.locals[receiver].ty.as_ref().as_reference().is_none() {
            // Taken by value, a closure without captures is not passed at all.
            return self.cg.callee(InstanceDef::Closure(id.0.as_id()), parent_args, self.env, &sig);
        }
        let sret = sig.params.first().is_some_and(|it| it.purpose == ArgumentPurpose::StructReturn);
        sig.params.remove(sret as usize);
        let args = GenericArgs::new_from_slice(&[ty.into()]);
        self.cg.callee(InstanceDef::FnPointerShim, args, self.env, &sig)
    }

    /// The body of a `FnPointerShim`: the signature of the closure body less its receiver.
    fn fn_pointer_shim(&mut self, ty: Ty<'db>) -> Result<()> {
        let TyKind::Closure(id, generic_args) = ty.kind() else {
            unsupported!("fn pointer shim for {ty:?}");
        };
        let parent_args = GenericArgs::new_from_slice(generic_args.as_closure().parent_args());
        let body = self
            .cg
            .db
            .monomorphized_mir_body_for_closure(id.0, parent_args.store(), self.env.store())
            .map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
        let sig = self.body_signature(body, false)?;
        let sret = sig.params.first().is_some_and(|it| it.purpose == ArgumentPurpose::StructReturn);
        let mut shim = sig.clone();
        shim.params.remove(sret as usize);
        self.builder.func.signature = shim;
        let entry = self.builder.create_block();
        self.builder.append_block_params_for_function_params(entry);
        self.builder.switch_to_block(entry);
        let mut params = self.builder.block_params(entry).to_vec();
        let receiver = self.builder.ins().iconst(PTR, 1);
        params.insert(sret as usize, receiver);
        let callee =
            self.cg.callee(InstanceDef::Closure(id.0.as_id()), parent_args, self.env, &sig)?;
        let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
        let results = self.invoke(callee, &params);
        self.builder.ins().return_(&results);
        Ok(())
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
        let sig = self.body_signature(body, false)?;
        let Some(&receiver) = body.param_locals.first() else {
            unsupported!("closure without a receiver");
        };
        let receiver_ty = body.locals[receiver].ty.as_ref();
        let by_value = receiver_ty.as_reference().is_none();
        let layout = self.layout(receiver_ty)?;
        let mode = self.pass_mode(&layout, false)?;
        self.builder.func.signature =
            if by_value { Self::by_pointer(&sig, mode) } else { sig.clone() };
        let entry = self.builder.create_block();
        self.builder.append_block_params_for_function_params(entry);
        self.builder.switch_to_block(entry);
        let mut params = self.builder.block_params(entry).to_vec();
        let sret = sig.params.first().is_some_and(|it| it.purpose == ArgumentPurpose::StructReturn);
        let Some(&closure) = params.get(sret as usize) else {
            unsupported!("call_once shim for a closure that is not passed");
        };
        let callee =
            self.cg.callee(InstanceDef::Closure(id.0.as_id()), parent_args, self.env, &sig)?;
        let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
        if by_value {
            // The body owns the closure and takes it in registers.
            let mut values = Vec::new();
            self.pass(mode, closure, &layout, &mut values);
            params.splice(sret as usize..sret as usize + 1, values);
            let results = self.invoke(callee, &params);
            self.builder.ins().return_(&results);
            return Ok(());
        }
        let results = self.invoke(callee, &params);
        self.drop_value(closure, None, ty)?;
        self.builder.ins().return_(&results);
        Ok(())
    }

    /// `sig` with the receiver, which is passed as `mode` says, behind a pointer instead.
    fn by_pointer(sig: &Signature, mode: PassMode) -> Signature {
        let sret = sig.params.first().is_some_and(|it| it.purpose == ArgumentPurpose::StructReturn);
        let count = match mode {
            PassMode::Ignore => 0,
            PassMode::Pair(..) => 2,
            _ => 1,
        };
        let mut sig = sig.clone();
        sig.params.splice(sret as usize..sret as usize + count, [AbiParam::new(PTR)]);
        sig
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
        if Some(func) == self.interner.lang_items().FuturePoll
            && let Some(callee) = self.coroutine_callee(self.normalize(generic_args.type_at(0)))
        {
            return self.emit_call(callee, args, dst, dst_ty);
        }
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
        if callee == func
            && let ItemContainerId::TraitId(trait_) = func.lookup(db).container
            && Some(trait_) == self.interner.lang_items().Clone
            && FunctionSignature::of(db, func).name.as_str() == "clone"
            && let &[(receiver, _)] = args
        {
            let src = self.builder.ins().load(PTR, MEM, receiver, 0);
            return self.clone_builtin(func, src, dst, dst_ty);
        }
        // `FnPtr::addr` is the compiler's to supply as well: a function pointer is its address.
        if callee == func
            && let ItemContainerId::TraitId(trait_) = func.lookup(db).container
            && Some(trait_) == self.interner.lang_items().FnPtrTrait
            && let &[(receiver, _)] = args
        {
            let addr = self.builder.ins().load(PTR, MEM, receiver, 0);
            self.builder.ins().store(MEM, addr, dst, 0);
            return Ok(());
        }
        self.emit_call(Callee::Instance(InstanceDef::Fn(callee), callee_args), args, dst, dst_ty)
    }

    /// `Clone::clone` of a type whose impl the compiler supplies: tuples and arrays clone their
    /// parts, and what is left is `Copy`.
    fn clone_builtin(
        &mut self,
        clone: FunctionId,
        src: Value,
        dst: Value,
        ty: Ty<'db>,
    ) -> Result<()> {
        let layout = self.layout(ty)?;
        let clone_part = |this: &mut Self, src: Value, dst: Value, part: Ty<'db>| {
            let reference = Ty::new_ref(
                this.interner,
                Region::new_erased(this.interner),
                part,
                Mutability::Not,
            );
            let slot = this.scratch();
            this.builder.ins().store(MEM, src, slot, 0);
            let args = GenericArgs::new_from_slice(&[part.into()]);
            this.call_direct(clone, args, &[(slot, reference)], dst, part)
        };
        // A closure clones as the tuple of what it captured.
        let fields = match ty.kind() {
            TyKind::Tuple(fields) => Some(fields),
            TyKind::Closure(_, args) => match args.as_closure().tupled_upvars_ty().kind() {
                TyKind::Tuple(fields) => Some(fields),
                _ => unsupported!("builtin `Clone` of a closure without tupled upvars"),
            },
            _ => None,
        };
        match ty.kind() {
            _ if let Some(fields) = fields => {
                for (i, field) in fields.iter().enumerate() {
                    let offset = layout.fields.offset(i).bytes() as i64;
                    let src = self.builder.ins().iadd_imm(src, offset);
                    let part = self.builder.ins().iadd_imm(dst, offset);
                    self.guarded(
                        |this| clone_part(this, src, part, field),
                        |this| {
                            for (done, field) in fields.iter().enumerate().take(i) {
                                let offset = layout.fields.offset(done).bytes() as i64;
                                let done = this.builder.ins().iadd_imm(dst, offset);
                                this.drop_value(done, None, field)?;
                            }
                            Ok(())
                        },
                    )?;
                }
            }
            TyKind::Array(element, _) => {
                let size = self.layout(element)?.size.bytes() as i64;
                let len = self.builder.ins().iconst(PTR, layout.fields.count() as i64);
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
                let src = self.builder.ins().iadd(src, offset);
                let part = self.builder.ins().iadd(dst, offset);
                self.guarded(
                    |this| clone_part(this, src, part, element),
                    |this| this.drop_elements(dst, index, element),
                )?;
                let next = self.builder.ins().iadd_imm(index, 1);
                self.builder.ins().jump(header, &[next.into()]);
                self.builder.switch_to_block(done);
            }
            TyKind::FnDef(..)
            | TyKind::FnPtr(..)
            | TyKind::Never
            | TyKind::RawPtr(..)
            | TyKind::Ref(_, _, Mutability::Not) => self.copy(dst, src, layout.size),
            _ => unsupported!("builtin `Clone` of {ty:?}"),
        }
        Ok(())
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
        let abi = match callee {
            Callee::Instance(InstanceDef::Fn(func), _) => {
                FunctionSignature::of(self.cg.db, func).abi
            }
            Callee::Instance(..) => ExternAbi::Rust,
            Callee::Pointer(_, abi) => abi,
        };
        let FnAbi { ret: ret_mode, args: arg_modes } =
            self.fn_abi(abi, dst_ty, args.iter().map(|&(_, ty)| ty))?;
        let mut values = Vec::new();
        if let PassMode::Indirect = ret_mode {
            values.push(dst);
        }
        for (&(addr, ty), &mode) in args.iter().zip(&arg_modes) {
            let layout = self.layout(ty)?;
            self.pass(mode, addr, &layout, &mut values);
        }
        let mut sig = self.signature(&arg_modes, ret_mode);
        if let Callee::Instance(InstanceDef::Fn(func), _) = callee
            && track_caller(self.cg.db, func)
        {
            sig.params.push(AbiParam::new(PTR));
            values.push(self.caller_location()?);
        }
        let results = match callee {
            Callee::Instance(def, generic_args) => {
                let variadic = matches!(def, InstanceDef::Fn(func)
                    if FunctionSignature::of(self.cg.db, func).is_varargs());
                let callee = self.cg.callee(def, generic_args, self.env, &sig)?;
                let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
                if variadic {
                    // Each call of a variadic function has a signature of its own, and the
                    // function is declared with only one of them.
                    // FIXME: the callee is told how many vector registers hold arguments,
                    // which Cranelift has no way to say.
                    if sig.params.iter().any(|param| param.value_type.is_float()) {
                        unsupported!("float passed to a variadic function");
                    }
                    let sig = self.builder.import_signature(sig);
                    let pointer = self.builder.ins().func_addr(PTR, callee);
                    self.invoke_indirect(sig, pointer, &values)
                } else {
                    self.invoke(callee, &values)
                }
            }
            Callee::Pointer(pointer, _) => {
                let sig = self.builder.import_signature(sig);
                self.invoke_indirect(sig, pointer, &values)
            }
        };
        self.receive(ret_mode, &mut results.into_iter(), dst, &ret_layout);
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
                    "unchecked_div" | "exact_div" => self.divide(l, r, true, signed)?,
                    "unchecked_rem" => self.divide(l, r, false, signed)?,
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
            ("simd_shuffle", [first, second, indices]) => {
                let (first, ty) = self.operand(first)?;
                let (second, _) = self.operand(second)?;
                // `[u32; N]`, or a wrapper of one. Lanes of `second` come after `first`'s.
                let (indices, _) = self.operand(indices)?;
                let layout = self.layout(ty)?;
                let (lane, _) = self.simd_lanes(ty)?;
                let (_, count) = self.simd_lanes(dst_ty)?;
                let lane_size = lane.ty.bytes() as u64;
                let both = self.builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    (layout.size.bytes() * 2) as u32,
                    layout.align.abi.bytes().min(16).trailing_zeros() as u8,
                ));
                let both = self.builder.ins().stack_addr(PTR, both, 0);
                self.copy(both, first, layout.size);
                let upper = self.builder.ins().iadd_imm(both, layout.size.bytes() as i64);
                self.copy(upper, second, layout.size);
                let mut lanes = Vec::new();
                for i in 0..count {
                    let index = self.builder.ins().load(types::I32, MEM, indices, (i * 4) as i32);
                    let index = self.builder.ins().uextend(PTR, index);
                    let offset = self.builder.ins().imul_imm(index, lane_size as i64);
                    let addr = self.builder.ins().iadd(both, offset);
                    lanes.push(self.builder.ins().load(lane.ty, MEM, addr, 0));
                }
                for (i, value) in lanes.into_iter().enumerate() {
                    self.builder.ins().store(MEM, value, dst, (i as u64 * lane_size) as i32);
                }
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
                "fabs"
                | "fabsf32"
                | "fabsf64"
                | "sqrtf32"
                | "sqrtf64"
                | "floorf32"
                | "floorf64"
                | "ceilf32"
                | "ceilf64"
                | "truncf32"
                | "truncf64"
                | "round_ties_even_f32"
                | "round_ties_even_f64"
                | "roundf32"
                | "roundf64",
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
                    "minnumf32" | "minnumf64" => self.float_libcall(
                        if single { "fminf" } else { "fmin" },
                        scalar.ty,
                        &[l, r],
                    )?,
                    "maxnumf32" | "maxnumf64" => self.float_libcall(
                        if single { "fmaxf" } else { "fmax" },
                        scalar.ty,
                        &[l, r],
                    )?,
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
            ("vtable_size" | "vtable_align", [vtable]) => {
                let (vtable, _) = self.load_scalar(vtable)?;
                let offset = if name == "vtable_size" { 8 } else { 16 };
                let value = self.builder.ins().load(PTR, MEM, vtable, offset);
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("size_of_val" | "align_of_val", [arg]) => {
                let (src, _) = self.operand(arg)?;
                let ty = generic_args.type_at(0);
                let meta = self.is_unsized(ty).then(|| self.builder.ins().load(PTR, MEM, src, 8));
                let (size, align) = self.size_and_align_of_val(ty, meta)?;
                let value = if name == "size_of_val" { size } else { align };
                self.builder.ins().store(MEM, value, dst, 0);
            }
            ("catch_unwind", [try_fn, data, catch_fn]) => {
                let (try_fn, _) = self.load_scalar(try_fn)?;
                let (data, _) = self.load_scalar(data)?;
                let (catch_fn, _) = self.load_scalar(catch_fn)?;
                let mut sig = self.cg.module.make_signature();
                sig.params.push(AbiParam::new(PTR));
                let try_sig = self.builder.import_signature(sig.clone());
                sig.params.push(AbiParam::new(PTR));
                let catch_sig = self.builder.import_signature(sig);

                let returned = self.builder.create_block();
                let caught = self.builder.create_block();
                let done = self.builder.create_block();
                self.builder.append_block_param(caught, PTR);
                self.builder.append_block_param(done, types::I32);
                let normal = self.builder.func.dfg.block_call(returned, &[]);
                let unwound =
                    self.builder.func.dfg.block_call(caught, &[ir::BlockArg::TryCallExn(0)]);
                let tag = ir::ExceptionTag::with_number(unwind::CATCH).expect("a small tag");
                let table =
                    self.builder.func.dfg.exception_tables.push(ir::ExceptionTableData::new(
                        try_sig,
                        normal,
                        [ir::ExceptionTableItem::Tag(tag, unwound)],
                    ));
                self.builder.ins().try_call_indirect(try_fn, &[data], table);

                self.builder.switch_to_block(returned);
                let zero = self.builder.ins().iconst(types::I32, 0);
                self.builder.ins().jump(done, &[zero.into()]);

                self.builder.switch_to_block(caught);
                self.builder.set_cold_block(caught);
                let exception = self.builder.block_params(caught)[0];
                self.invoke_indirect(catch_sig, catch_fn, &[data, exception]);
                let one = self.builder.ins().iconst(types::I32, 1);
                self.builder.ins().jump(done, &[one.into()]);

                self.builder.switch_to_block(done);
                let result = self.builder.block_params(done)[0];
                self.builder.ins().store(MEM, result, dst, 0);
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

    /// The quotient of two integers, or the remainder. Cranelift has neither for 128 bits, so
    /// those call compiler-builtins.
    fn divide(&mut self, l: Value, r: Value, div: bool, signed: bool) -> Result<Value> {
        if self.builder.func.dfg.value_type(l) == types::I128 {
            let mut sig = Signature::new(CallConv::SystemV);
            sig.params.extend([AbiParam::new(types::I128), AbiParam::new(types::I128)]);
            sig.returns.push(AbiParam::new(types::I128));
            let name = match (div, signed) {
                (true, true) => "__divti3",
                (true, false) => "__udivti3",
                (false, true) => "__modti3",
                (false, false) => "__umodti3",
            };
            let callee = self.cg.import(name, &sig)?;
            let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
            return Ok(self.invoke(callee, &[l, r])[0]);
        }
        let ins = self.builder.ins();
        Ok(match (div, signed) {
            (true, true) => ins.sdiv(l, r),
            (true, false) => ins.udiv(l, r),
            (false, true) => ins.srem(l, r),
            (false, false) => ins.urem(l, r),
        })
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
        let value = self.invoke(callee, &[l, r, overflow])[0];
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

    /// Calls a function that may unwind. When this function has values to drop, the call gets a
    /// landing pad that drops them and resumes unwinding.
    fn invoke(&mut self, callee: ir::FuncRef, args: &[Value]) -> Vec<Value> {
        let Some(pad) = self.landing_pad_for_call() else {
            let call = self.builder.ins().call(callee, args);
            return self.builder.inst_results(call).to_vec();
        };
        let sig = self.builder.func.dfg.ext_funcs[callee].signature;
        let (table, after) = self.exception_table(sig, pad);
        self.builder.ins().try_call(callee, args, table);
        self.builder.switch_to_block(after);
        self.builder.block_params(after).to_vec()
    }

    fn invoke_indirect(&mut self, sig: ir::SigRef, pointer: Value, args: &[Value]) -> Vec<Value> {
        let Some(pad) = self.landing_pad_for_call() else {
            let call = self.builder.ins().call_indirect(sig, pointer, args);
            return self.builder.inst_results(call).to_vec();
        };
        let (table, after) = self.exception_table(sig, pad);
        self.builder.ins().try_call_indirect(pointer, args, table);
        self.builder.switch_to_block(after);
        self.builder.block_params(after).to_vec()
    }

    /// The block that cleans up after a call that unwinds, if there is anything to clean up.
    fn landing_pad_for_call(&mut self) -> Option<ir::Block> {
        if self.in_cleanup {
            return None;
        }
        if self.inner_pad.is_some() {
            return self.inner_pad;
        }
        if self.drop_flags.iter().next().is_none() {
            return None;
        }
        if self.cleanup.is_none() {
            let block = self.builder.create_block();
            self.builder.append_block_param(block, PTR);
            self.builder.set_cold_block(block);
            self.cleanup = Some(block);
        }
        self.cleanup
    }

    fn exception_table(
        &mut self,
        sig: ir::SigRef,
        cleanup: ir::Block,
    ) -> (ir::ExceptionTable, ir::Block) {
        let after = self.builder.create_block();
        let returns: Vec<_> =
            self.builder.func.dfg.signatures[sig].returns.iter().map(|it| it.value_type).collect();
        let mut results = Vec::new();
        for (i, &ty) in returns.iter().enumerate() {
            self.builder.append_block_param(after, ty);
            results.push(ir::BlockArg::TryCallRet(i as u32));
        }
        let normal = self.builder.func.dfg.block_call(after, &results);
        let unwound = self.builder.func.dfg.block_call(cleanup, &[ir::BlockArg::TryCallExn(0)]);
        let tag = ir::ExceptionTag::with_number(unwind::CLEANUP).expect("a small tag");
        let table = self.builder.func.dfg.exception_tables.push(ir::ExceptionTableData::new(
            sig,
            normal,
            [ir::ExceptionTableItem::Tag(tag, unwound)],
        ));
        (table, after)
    }

    /// Fills in the landing pad: drops every local that holds a value, last declared first, and
    /// carries on unwinding.
    fn emit_cleanup(&mut self) -> Result<()> {
        let Some(cleanup) = self.cleanup else { return Ok(()) };
        self.in_cleanup = true;
        self.builder.switch_to_block(cleanup);
        let exception = self.builder.block_params(cleanup)[0];
        self.drop_live_locals()?;
        if let Some((state, _)) = self.suspended {
            self.store_state(state, PANICKED);
        }
        self.resume_unwinding(exception)
    }

    /// Drops every local that holds a value.
    fn drop_live_locals(&mut self) -> Result<()> {
        // Locals the body never drops come first, last declared first, and of those the
        // operands of the expression that unwound before the temporaries it borrowed, which
        // live to the end of the statement. The rest go in the order the body drops them when
        // nothing unwinds, which is innermost scope first.
        let mut locals: Vec<LocalId> = self.drop_flags.iter().map(|(local, _)| local).collect();
        locals.reverse();
        let rank = |local: &LocalId| match self.drop_order.iter().position(|it| it == local) {
            Some(at) => at + 2,
            None => !self.operand_temps.contains(local) as usize,
        };
        locals.sort_by_key(rank);
        for local in locals {
            let next = self.builder.create_block();
            self.builder.set_cold_block(next);
            let place = crate::mir::Place::from(local).store();
            self.drop_place(&place, next)?;
            self.builder.switch_to_block(next);
        }
        Ok(())
    }

    fn store_state(&mut self, state: Value, value: i64) {
        let value = self.builder.ins().iconst(types::I64, value);
        self.builder.ins().store(MEM, value, state, 0);
    }

    /// Returns what is in the local that holds the function's result.
    fn return_(&mut self) -> Result<()> {
        let body = self.body.expect("MIR body");
        let local = returned_local(body);
        let mut values = Vec::new();
        if !matches!(self.ret_mode, PassMode::Indirect) {
            let layout = self.layout(body.locals[local].ty.as_ref())?;
            self.pass(self.ret_mode, self.locals[local].addr, &layout, &mut values);
        }
        self.builder.ins().return_(&values);
        Ok(())
    }

    /// The way into an `async` body. `poll` carries on from where the body last returned
    /// `Poll::Pending`. A null context instead asks for the future to be dropped: what it
    /// captured if it never ran, and what its locals hold if it is suspended.
    #[allow(clippy::too_many_arguments)]
    fn enter_coroutine(
        &mut self,
        body: &MirBody<'db>,
        state: Value,
        resumes: &[BasicBlockId],
        flags: &[Value],
        addr: Value,
        ty: Ty<'db>,
        layout: &Layout,
    ) -> Result<()> {
        let Some(crate::mir::CoroutineLocals { context, .. }) = body.coroutine else {
            unsupported!("async body without a context");
        };
        let context = self.builder.ins().load(PTR, MEM, self.locals[context].addr, 0);
        let at = self.builder.ins().load(types::I64, MEM, state, 0);
        let (poll, drop) = (self.builder.create_block(), self.builder.create_block());
        self.builder.ins().brif(context, poll, &[], drop, &[]);

        let (first, finished) = (self.builder.create_block(), self.builder.create_block());
        self.builder.switch_to_block(poll);
        let mut switch = Switch::new();
        switch.set_entry(UNRESUMED as u128, first);
        for (index, resume) in resumes.iter().enumerate() {
            switch.set_entry(SUSPENDED as u128 + index as u128, self.blocks[*resume]);
        }
        switch.emit(&mut self.builder, at, finished);
        self.builder.switch_to_block(first);
        let dead = self.builder.ins().iconst(types::I64, 0);
        for &flag in flags {
            self.builder.ins().store(MEM, dead, flag, 0);
        }
        self.builder.ins().jump(self.blocks[body.start_block], &[]);
        // FIXME: rustc panics with "`async fn` resumed after completion".
        self.builder.switch_to_block(finished);
        self.builder.ins().trap(TrapCode::user(3).expect("non-zero trap code"));

        let (unresumed, resumed, suspended, done) = (
            self.builder.create_block(),
            self.builder.create_block(),
            self.builder.create_block(),
            self.builder.create_block(),
        );
        self.builder.switch_to_block(drop);
        self.store_state(state, RETURNED);
        self.builder.ins().brif(at, resumed, &[], unresumed, &[]);
        self.builder.switch_to_block(resumed);
        let is_suspended =
            self.builder.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, at, SUSPENDED);
        self.builder.ins().brif(is_suspended, suspended, &[], done, &[]);
        self.in_cleanup = true;
        self.builder.switch_to_block(unresumed);
        let TyKind::Coroutine(_, args) = ty.kind() else {
            unsupported!("async body of {ty:?}");
        };
        for (i, field) in args.as_coroutine().tupled_upvars_ty().tuple_fields().iter().enumerate() {
            let field_addr =
                self.builder.ins().iadd_imm(addr, layout.fields.offset(i).bytes() as i64);
            self.drop_value(field_addr, None, field)?;
        }
        self.builder.ins().jump(done, &[]);
        self.in_cleanup = false;
        self.suspended_drop = Some((suspended, done));
        self.builder.switch_to_block(done);
        self.return_()
    }

    /// Calls the `poll` of the `async` block of type `ty`, which is what `Future::poll` is for
    /// it. `args` are the pinned reference and the context.
    fn coroutine_callee(&mut self, ty: Ty<'db>) -> Option<Callee<'db>> {
        let TyKind::Coroutine(id, args) = ty.kind() else { return None };
        let parent_args = GenericArgs::new_from_slice(args.as_coroutine().parent_args());
        Some(Callee::Instance(InstanceDef::Coroutine(id.0.as_id()), parent_args))
    }

    /// Drops the `async` block at `addr`, by polling it with no context.
    fn drop_coroutine(&mut self, addr: Value, ty: Ty<'db>) -> Result<()> {
        let TyKind::Coroutine(id, args) = ty.kind() else {
            unsupported!("drop glue for {ty:?}");
        };
        let parent_args = GenericArgs::new_from_slice(args.as_coroutine().parent_args());
        let body = self
            .cg
            .db
            .monomorphized_mir_body_for_coroutine(id.0, parent_args.store(), self.env.store())
            .map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
        let Some(crate::mir::CoroutineLocals { poll, .. }) = body.coroutine else {
            unsupported!("async block whose body is not one");
        };
        let mut values = Vec::new();
        for (&param, value) in body.param_locals.iter().zip([Some(addr), None]) {
            let slot = self.scratch();
            let value = value.unwrap_or_else(|| self.builder.ins().iconst(PTR, 0));
            self.builder.ins().store(MEM, value, slot, 0);
            values.push((slot, body.locals[param].ty.as_ref()));
        }
        let poll_ty = body.locals[poll].ty.as_ref();
        let layout = self.layout(poll_ty)?;
        let ignored = self.stack_slot(&layout);
        let callee = Callee::Instance(InstanceDef::Coroutine(id.0.as_id()), parent_args);
        self.emit_call(callee, &values, ignored, poll_ty)
    }

    fn resume_unwinding(&mut self, exception: Value) -> Result<()> {
        let mut sig = self.cg.module.make_signature();
        sig.params.push(AbiParam::new(PTR));
        let resume = self
            .cg
            .module
            .declare_function("_Unwind_Resume", Linkage::Import, &sig)
            .map_err(|e| CodegenError::Backend(e.to_string()))?;
        let resume = self.cg.module.declare_func_in_func(resume, self.builder.func);
        self.builder.ins().call(resume, &[exception]);
        self.builder.ins().trap(ir::TrapCode::user(1).expect("a nonzero trap code"));
        Ok(())
    }

    /// Runs a type's `Drop` impl. If that unwinds the fields are dropped all the same, on the
    /// way out.
    fn call_drop_impl_guarded(
        &mut self,
        addr: Value,
        meta: Option<Value>,
        ty: Ty<'db>,
    ) -> Result<()> {
        self.guarded(
            |this| this.call_drop_impl(addr, ty),
            |this| this.drop_adt_fields(addr, meta, ty),
        )
    }

    /// Runs `body`, and `cleanup` on the way out if a call that `body` makes unwinds.
    fn guarded(
        &mut self,
        body: impl FnOnce(&mut Self) -> Result<()>,
        cleanup: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        if self.in_cleanup {
            return body(self);
        }
        let outer = self.landing_pad_for_call();
        let pad = self.builder.create_block();
        self.builder.append_block_param(pad, PTR);
        self.builder.set_cold_block(pad);
        let enclosing = self.inner_pad.replace(pad);
        let done = body(self);
        self.inner_pad = enclosing;
        done?;
        let Some(after) = self.builder.current_block() else {
            unsupported!("guarded call without a block to return to");
        };
        self.builder.switch_to_block(pad);
        let exception = self.builder.block_params(pad)[0];
        self.in_cleanup = true;
        let cleaned = cleanup(self);
        self.in_cleanup = false;
        cleaned?;
        match outer {
            Some(outer) => {
                self.builder.ins().jump(outer, &[exception.into()]);
            }
            None => self.resume_unwinding(exception)?,
        }
        self.builder.switch_to_block(after);
        Ok(())
    }

    fn set_drop_flag(&mut self, place: &StoredPlace, live: bool) {
        if !place.projection.as_slice().is_empty() {
            if live {
                self.clear_moved(place);
            }
            return;
        }
        if let Some(&flag) = self.drop_flags.get(place.local) {
            let live = self.builder.ins().iconst(types::I64, live as i64);
            self.builder.ins().store(MEM, live, flag, 0);
        }
    }

    /// Drops the value at `addr` in place.
    fn drop_value(&mut self, addr: Value, meta: Option<Value>, ty: Ty<'db>) -> Result<()> {
        let ty = self.normalize(ty);
        if has_drop_glue(&self.infcx, ty, self.env.param_env) == DropGlue::None {
            return Ok(());
        }
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.push(AbiParam::new(PTR));
        let mut values = vec![addr];
        if self.is_unsized(ty) {
            let Some(meta) = meta else {
                unsupported!("drop of unsized {ty:?} without metadata");
            };
            sig.params.push(AbiParam::new(PTR));
            values.push(meta);
        }
        let args = GenericArgs::new_from_slice(&[ty.into()]);
        let callee = self.cg.callee(InstanceDef::DropGlue, args, self.env, &sig)?;
        let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
        self.invoke(callee, &values);
        Ok(())
    }

    /// The body of `drop_in_place::<ty>`.
    fn drop_glue(&mut self, ty: Ty<'db>) -> Result<()> {
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.push(AbiParam::new(PTR));
        if self.is_unsized(ty) {
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

    /// Drops the fields of a struct or of the variant an enum is in.
    /// Jumps to `differ` unless the values at `l` and `r` are the same value of `ty`, compared
    /// the way a constant pattern is: field by field and through references. `metas` are the
    /// lengths of the two when `ty` is unsized.
    fn branch_unless_equal(
        &mut self,
        l: Value,
        r: Value,
        metas: Option<(Value, Value)>,
        ty: Ty<'db>,
        differ: ir::Block,
    ) -> Result<()> {
        let db = self.cg.db;
        let ty = self.normalize(ty);
        match ty.kind() {
            TyKind::Ref(_, pointee, _) => {
                let metas = self.is_unsized(pointee).then(|| {
                    (
                        self.builder.ins().load(PTR, MEM, l, 8),
                        self.builder.ins().load(PTR, MEM, r, 8),
                    )
                });
                let l = self.builder.ins().load(PTR, MEM, l, 0);
                let r = self.builder.ins().load(PTR, MEM, r, 0);
                self.branch_unless_equal(l, r, metas, pointee, differ)
            }
            TyKind::Str | TyKind::Slice(_) | TyKind::Array(..) => {
                let element = match ty.kind() {
                    TyKind::Slice(element) | TyKind::Array(element, _) => element,
                    _ => Ty::new_uint(self.interner, rustc_type_ir::UintTy::U8),
                };
                let len = match (ty.kind(), metas) {
                    (TyKind::Array(_, len), _) => {
                        let Some(len) = try_const_usize(db, len) else {
                            unsupported!("array with unevaluated length");
                        };
                        self.builder.ins().iconst(PTR, len as i64)
                    }
                    (_, Some((l_len, r_len))) => {
                        let same = self.builder.ins().icmp(IntCC::Equal, l_len, r_len);
                        self.branch_unless(same, differ);
                        l_len
                    }
                    (_, None) => unsupported!("comparison of a slice without a length"),
                };
                let size = self.layout(element)?.size.bytes() as i64;
                // Elements without padding or references are equal exactly when their bytes
                // are.
                if matches!(
                    element.kind(),
                    TyKind::Int(_) | TyKind::Uint(_) | TyKind::Bool | TyKind::Char
                ) {
                    let bytes = self.builder.ins().imul_imm(len, size);
                    let order = self.memcmp(l, r, bytes)?;
                    let same = self.builder.ins().icmp_imm(IntCC::Equal, order, 0);
                    self.branch_unless(same, differ);
                    return Ok(());
                }
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
                let l = self.builder.ins().iadd(l, offset);
                let r = self.builder.ins().iadd(r, offset);
                self.branch_unless_equal(l, r, None, element, differ)?;
                let next = self.builder.ins().iadd_imm(index, 1);
                self.builder.ins().jump(header, &[next.into()]);
                self.builder.switch_to_block(done);
                Ok(())
            }
            TyKind::Tuple(tys) => {
                let layout = self.layout(ty)?;
                for (i, field) in tys.iter().enumerate() {
                    let offset = layout.fields.offset(i).bytes() as i64;
                    let l = self.builder.ins().iadd_imm(l, offset);
                    let r = self.builder.ins().iadd_imm(r, offset);
                    self.branch_unless_equal(l, r, None, field, differ)?;
                }
                Ok(())
            }
            TyKind::Adt(adt_def, subst) => {
                let layout = self.layout(ty)?;
                match adt_def.def_id() {
                    AdtId::UnionId(_) => unsupported!("comparison of a union"),
                    AdtId::StructId(id) => {
                        self.branch_unless_fields_equal(l, r, &layout, id.into(), subst, differ)
                    }
                    AdtId::EnumId(id) => {
                        let wide = match &layout.variants {
                            Variants::Multiple { tag, .. } => DiscriminantWidth::of(tag),
                            Variants::Empty => return Ok(()),
                            Variants::Single { index } => {
                                let variant = id
                                    .enum_variants(db)
                                    .variants
                                    .values()
                                    .map(|&(variant, _)| variant)
                                    .find(|variant| variant.index(db) == index.0);
                                let Some(variant) = variant else { return Ok(()) };
                                return self.branch_unless_fields_equal(
                                    l,
                                    r,
                                    &layout,
                                    variant.into(),
                                    subst,
                                    differ,
                                );
                            }
                        };
                        let Variants::Multiple { variants, .. } = &layout.variants else {
                            return Ok(());
                        };
                        let l_discr = self.discriminant(l, ty, wide.scalar())?;
                        let r_discr = self.discriminant(r, ty, wide.scalar())?;
                        let same = self.builder.ins().icmp(IntCC::Equal, l_discr, r_discr);
                        self.branch_unless(same, differ);
                        let done = self.builder.create_block();
                        let mut switch = Switch::new();
                        let mut arms = Vec::new();
                        for &(variant, _) in id.enum_variants(db).variants.values() {
                            if db.field_types(variant.into()).iter().next().is_none() {
                                continue;
                            }
                            let value = db.const_eval_discriminant(variant).map_err(|e| {
                                CodegenError::Frontend(format!("discriminant: {e:?}"))
                            })?;
                            let block = self.builder.create_block();
                            switch.set_entry(wide.entry(value), block);
                            arms.push((block, variant));
                        }
                        switch.emit(&mut self.builder, l_discr, done);
                        for (block, variant) in arms {
                            self.builder.switch_to_block(block);
                            let layout = &variants[RustcEnumVariantIdx(variant.index(db))];
                            self.branch_unless_fields_equal(
                                l,
                                r,
                                layout,
                                variant.into(),
                                subst,
                                differ,
                            )?;
                            self.builder.ins().jump(done, &[]);
                        }
                        self.builder.switch_to_block(done);
                        Ok(())
                    }
                }
            }
            _ => {
                let scalar = self.scalar_of(ty)?;
                let l = self.builder.ins().load(scalar.ty, MEM, l, 0);
                let r = self.builder.ins().load(scalar.ty, MEM, r, 0);
                let same = if scalar.ty.is_float() {
                    self.builder.ins().fcmp(FloatCC::Equal, l, r)
                } else {
                    self.builder.ins().icmp(IntCC::Equal, l, r)
                };
                self.branch_unless(same, differ);
                Ok(())
            }
        }
    }

    fn branch_unless_fields_equal(
        &mut self,
        l: Value,
        r: Value,
        layout: &Layout,
        variant: VariantId,
        subst: GenericArgs<'db>,
        differ: ir::Block,
    ) -> Result<()> {
        let field_types = self.cg.db.field_types(variant);
        for (field, ty) in field_types.iter() {
            let ty = ty.ty().instantiate(self.interner, subst).skip_norm_wip();
            let offset = layout.fields.offset(u32::from(field.into_raw()) as usize).bytes() as i64;
            let l = self.builder.ins().iadd_imm(l, offset);
            let r = self.builder.ins().iadd_imm(r, offset);
            self.branch_unless_equal(l, r, None, ty, differ)?;
        }
        Ok(())
    }

    /// Jumps to `otherwise` when `condition` is false, and carries on in a new block.
    fn branch_unless(&mut self, condition: Value, otherwise: ir::Block) {
        let next = self.builder.create_block();
        self.builder.ins().brif(condition, next, &[], otherwise, &[]);
        self.builder.switch_to_block(next);
    }

    fn drop_adt_fields(&mut self, addr: Value, meta: Option<Value>, ty: Ty<'db>) -> Result<()> {
        let db = self.cg.db;
        let TyKind::Adt(adt_def, subst) = ty.kind() else { return Ok(()) };
        {
            {
                let layout = self.layout(ty)?;
                match adt_def.def_id() {
                    AdtId::UnionId(_) => (),
                    AdtId::StructId(id) => {
                        let manually_drop = StructSignature::of(db, id)
                            .flags
                            .contains(StructFlags::IS_MANUALLY_DROP);
                        if !manually_drop {
                            self.drop_fields(addr, &layout, id.into(), subst, ty, meta)?;
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
                                self.drop_fields(addr, &layout, variant.into(), subst, ty, meta)?;
                            }
                        }
                        Variants::Multiple { variants, tag, .. } => {
                            let wide = DiscriminantWidth::of(tag);
                            let discr = self.discriminant(addr, ty, wide.scalar())?;
                            let done = self.builder.create_block();
                            let mut switch = Switch::new();
                            let mut arms = Vec::new();
                            for &(variant, _) in id.enum_variants(db).variants.values() {
                                let value = db.const_eval_discriminant(variant).map_err(|e| {
                                    CodegenError::Frontend(format!("discriminant: {e:?}"))
                                })?;
                                let block = self.builder.create_block();
                                switch.set_entry(wide.entry(value), block);
                                arms.push((block, variant));
                            }
                            switch.emit(&mut self.builder, discr, done);
                            for (block, variant) in arms {
                                self.builder.switch_to_block(block);
                                let layout = &variants[RustcEnumVariantIdx(variant.index(db))];
                                self.drop_fields(addr, layout, variant.into(), subst, ty, meta)?;
                                self.builder.ins().jump(done, &[]);
                            }
                            self.builder.switch_to_block(done);
                        }
                    },
                }
            }
        }
        Ok(())
    }

    fn drop_contents(&mut self, addr: Value, meta: Option<Value>, ty: Ty<'db>) -> Result<()> {
        let db = self.cg.db;
        // The future an `async fn` returns is still `impl Future` in its signature.
        let ty = self.normalize(ty);
        match ty.kind() {
            TyKind::Adt(adt_def, subst) => {
                if adt_def.is_box() {
                    // The compiler drops the pointee; `Box`'s `Drop` impl only frees the memory.
                    let pointee = subst.type_at(0);
                    let pointer = self.builder.ins().load(PTR, MEM, addr, 0);
                    let pointee_meta = self
                        .is_unsized(pointee)
                        .then(|| self.builder.ins().load(PTR, MEM, addr, 8));
                    // The memory is freed even if dropping what it holds unwinds.
                    self.guarded(
                        |this| this.drop_value(pointer, pointee_meta, pointee),
                        |this| {
                            this.call_drop_impl(addr, ty)?;
                            this.drop_adt_fields(addr, meta, ty)
                        },
                    )?;
                }
                if adt_def.destructor(self.interner).is_some() {
                    self.call_drop_impl_guarded(addr, meta, ty)?;
                }
                self.drop_adt_fields(addr, meta, ty)?;
            }
            TyKind::Tuple(tys) => self.drop_tuple_fields(addr, ty, tys.iter().collect())?,
            TyKind::Closure(_, args) => {
                let TyKind::Tuple(tys) = args.as_closure().tupled_upvars_ty().kind() else {
                    unsupported!("drop glue for a closure without tupled upvars");
                };
                self.drop_tuple_fields(addr, ty, tys.iter().collect())?
            }
            TyKind::Coroutine(..) => self.drop_coroutine(addr, ty)?,
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
                self.invoke_indirect(sig, glue, &[addr]);
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
        container: Ty<'db>,
        meta: Option<Value>,
    ) -> Result<()> {
        let field_types = self.cg.db.field_types(variant);
        let fields: Vec<_> = field_types
            .iter()
            .map(|(field, ty)| {
                let ty = ty.ty().instantiate(self.interner, subst).skip_norm_wip();
                (layout.fields.offset(u32::from(field.into_raw()) as usize), ty)
            })
            .filter(|&(_, ty)| self.needs_drop(ty))
            .collect();
        self.drop_ladder(fields.len(), |this, at| {
            let (offset, ty) = fields[at];
            if this.is_unsized(this.normalize(ty)) {
                let field_addr = this.unsized_field_addr(addr, meta, container, offset, ty)?;
                this.drop_value(field_addr, meta, ty)
            } else {
                let field_addr = this.builder.ins().iadd_imm(addr, offset.bytes() as i64);
                let field_addr = this.aligned_for_drop(field_addr, ty, container)?;
                this.drop_value(field_addr, None, ty)
            }
        })
    }

    fn drop_tuple_fields(&mut self, addr: Value, ty: Ty<'db>, fields: Vec<Ty<'db>>) -> Result<()> {
        let layout = self.layout(ty)?;
        let fields: Vec<_> =
            fields.into_iter().enumerate().filter(|&(_, field)| self.needs_drop(field)).collect();
        self.drop_ladder(fields.len(), |this, at| {
            let (index, field) = fields[at];
            let offset = layout.fields.offset(index).bytes() as i64;
            let field_addr = this.builder.ins().iadd_imm(addr, offset);
            this.drop_value(field_addr, None, field)
        })
    }

    fn needs_drop(&self, ty: Ty<'db>) -> bool {
        let ty = self.normalize(ty);
        has_drop_glue(&self.infcx, ty, self.env.param_env) != DropGlue::None
    }

    /// Runs `drop` for each of `0..count` in turn. If one unwinds the ones after it still run,
    /// on the way out.
    fn drop_ladder(
        &mut self,
        count: usize,
        mut drop: impl FnMut(&mut Self, usize) -> Result<()>,
    ) -> Result<()> {
        if self.in_cleanup || count < 2 {
            return (0..count).try_for_each(|at| drop(self, at));
        }
        let outer = self.landing_pad_for_call();
        let pads: Vec<ir::Block> = (1..count)
            .map(|_| {
                let pad = self.builder.create_block();
                self.builder.append_block_param(pad, PTR);
                self.builder.set_cold_block(pad);
                pad
            })
            .collect();
        let enclosing = self.inner_pad;
        let mut dropped = Ok(());
        for at in 0..count {
            self.inner_pad = pads.get(at).copied().or(enclosing);
            dropped = drop(self, at);
            if dropped.is_err() {
                break;
            }
        }
        self.inner_pad = enclosing;
        dropped?;
        let Some(after) = self.builder.current_block() else {
            unsupported!("drop without a block to return to");
        };
        self.in_cleanup = true;
        let mut cleaned = Ok(());
        for at in 1..count {
            self.builder.switch_to_block(pads[at - 1]);
            let exception = self.builder.block_params(pads[at - 1])[0];
            cleaned = drop(self, at);
            if cleaned.is_err() {
                break;
            }
            match (pads.get(at), outer) {
                (Some(&next), _) | (None, Some(next)) => {
                    self.builder.ins().jump(next, &[exception.into()]);
                }
                (None, None) => cleaned = self.resume_unwinding(exception),
            }
        }
        self.in_cleanup = false;
        cleaned?;
        self.builder.switch_to_block(after);
        Ok(())
    }

    fn drop_elements(&mut self, addr: Value, len: Value, element: Ty<'db>) -> Result<()> {
        let size = self.layout(element)?.size.bytes() as i64;
        let done = self.builder.create_block();
        let zero = self.builder.ins().iconst(PTR, 0);
        if self.in_cleanup {
            self.drop_elements_from(addr, len, element, size, zero, None, done)?;
        } else {
            // If an element's drop unwinds, the ones after it are still dropped on the way out.
            let outer = self.landing_pad_for_call();
            let pad = self.builder.create_block();
            self.builder.append_block_param(pad, PTR);
            self.builder.set_cold_block(pad);
            let index = self.drop_elements_from(addr, len, element, size, zero, Some(pad), done)?;
            self.builder.switch_to_block(pad);
            let exception = self.builder.block_params(pad)[0];
            let rest = self.builder.ins().iadd_imm(index, 1);
            let resume = self.builder.create_block();
            self.builder.set_cold_block(resume);
            self.in_cleanup = true;
            let cleaned = self.drop_elements_from(addr, len, element, size, rest, None, resume);
            self.in_cleanup = false;
            cleaned?;
            self.builder.switch_to_block(resume);
            match outer {
                Some(outer) => {
                    self.builder.ins().jump(outer, &[exception.into()]);
                }
                None => self.resume_unwinding(exception)?,
            }
        }
        self.builder.switch_to_block(done);
        Ok(())
    }

    /// Drops the elements from `start` on and jumps to `done`, unwinding to `pad` if there is
    /// one. Returns the index of the element being dropped.
    #[allow(clippy::too_many_arguments)]
    fn drop_elements_from(
        &mut self,
        addr: Value,
        len: Value,
        element: Ty<'db>,
        size: i64,
        start: Value,
        pad: Option<ir::Block>,
        done: ir::Block,
    ) -> Result<Value> {
        let header = self.builder.create_block();
        let body = self.builder.create_block();
        self.builder.append_block_param(header, PTR);
        self.builder.ins().jump(header, &[start.into()]);
        self.builder.switch_to_block(header);
        let index = self.builder.block_params(header)[0];
        let more = self.builder.ins().icmp(IntCC::UnsignedLessThan, index, len);
        self.builder.ins().brif(more, body, &[], done, &[]);
        self.builder.switch_to_block(body);
        let offset = self.builder.ins().imul_imm(index, size);
        let element_addr = self.builder.ins().iadd(addr, offset);
        let enclosing = self.inner_pad;
        if pad.is_some() {
            self.inner_pad = pad;
        }
        let dropped = self.drop_value(element_addr, None, element);
        self.inner_pad = enclosing;
        dropped?;
        let next = self.builder.ins().iadd_imm(index, 1);
        self.builder.ins().jump(header, &[next.into()]);
        Ok(index)
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

/// Prints a body one statement a line, for debugging with `RUSTD_DUMP_MIR`.
fn dump_mir(body: &MirBody<'_>) {
    let compact = |text: String| {
        let mut text = text
            .replace("Idx::<Local>(", "_(")
            .replace("Idx::<BasicBlock>(", "bb(")
            .replace(", projection: [] }", " }")
            .replace("StoredPlace { local: ", "{ ")
            .replace(", span: None }", " }")
            .replace("Operand { kind: ", "{ ");
        while let Some(start) = text.find(", span: Some(") {
            let end = text[start..].find("))").map_or(text.len(), |end| start + end + 2);
            text.replace_range(start..end, "");
        }
        text
    };
    for (local, data) in body.locals.iter() {
        eprintln!("  let {}: {:?}", compact(format!("{local:?}")), data.ty);
    }
    for (id, block) in body.basic_blocks.iter() {
        eprintln!("{}:", compact(format!("{id:?}")));
        for statement in &block.statements {
            eprintln!("    {}", compact(format!("{:?}", statement.kind)));
        }
        if let Some(terminator) = &block.terminator {
            eprintln!("    -> {}", compact(format!("{:?}", terminator.kind)));
        }
    }
}

/// The locals a body drops whole, in the order of the blocks that drop them when each block
/// comes after the blocks leading to it.
fn operand_temps(body: &MirBody<'_>) -> rustc_hash::FxHashSet<LocalId> {
    let mut temps: rustc_hash::FxHashSet<LocalId> = body.locals.iter().map(|(id, _)| id).collect();
    for &local in body.binding_locals.values().chain(&body.param_locals) {
        temps.remove(&local);
    }
    let mut held = |place: &StoredPlace| {
        temps.remove(&place.local);
    };
    let mut operand = |operand: &Operand, held: &mut dyn FnMut(&StoredPlace)| {
        if let OperandKind::Copy(place) | OperandKind::Move(place) = &operand.kind
            && !place.projection.as_slice().is_empty()
        {
            held(place);
        }
    };
    for (_, block) in body.basic_blocks.iter() {
        for statement in &block.statements {
            let StatementKind::Assign(to, rvalue) = &statement.kind else { continue };
            if !to.projection.as_slice().is_empty() {
                held(to);
            }
            match rvalue {
                Rvalue::Ref(_, place)
                | Rvalue::CopyForDeref(place)
                | Rvalue::Discriminant(place)
                | Rvalue::Len(place) => held(place),
                Rvalue::Use(it) | Rvalue::UnaryOp(_, it) | Rvalue::Cast(_, it, _) => {
                    operand(it, &mut held)
                }
                Rvalue::Aggregate(_, operands) => {
                    operands.iter().for_each(|it| operand(it, &mut held))
                }
                _ => (),
            }
        }
        if let Some(TerminatorKind::Call { args, .. }) =
            block.terminator.as_ref().map(|it| &it.kind)
        {
            args.iter().for_each(|it| operand(it, &mut held));
        }
    }
    temps
}

fn drop_order(body: &MirBody, reachable: &rustc_hash::FxHashSet<BasicBlockId>) -> Vec<LocalId> {
    let successors = |id: BasicBlockId| -> Vec<BasicBlockId> {
        match body.basic_blocks[id].terminator.as_ref().map(|it| &it.kind) {
            Some(TerminatorKind::SwitchInt { targets, .. }) => targets.all_targets().to_vec(),
            Some(
                TerminatorKind::Goto { target }
                | TerminatorKind::Drop { target, .. }
                | TerminatorKind::DropAndReplace { target, .. }
                | TerminatorKind::Assert { target, .. }
                | TerminatorKind::FalseEdge { real_target: target, .. }
                | TerminatorKind::FalseUnwind { real_target: target, .. },
            ) => vec![*target],
            Some(TerminatorKind::Call { target, .. }) => target.iter().copied().collect(),
            Some(TerminatorKind::Yield { resume, .. }) => vec![*resume],
            _ => Vec::new(),
        }
    };
    let mut seen = rustc_hash::FxHashSet::default();
    let mut post_order = Vec::new();
    let mut stack = vec![(body.start_block, successors(body.start_block))];
    seen.insert(body.start_block);
    while let Some((id, pending)) = stack.last_mut() {
        match pending.pop() {
            Some(next) if reachable.contains(&next) && seen.insert(next) => {
                stack.push((next, successors(next)));
            }
            Some(_) => (),
            None => {
                post_order.push(*id);
                stack.pop();
            }
        }
    }
    // `local = value` drops what the local held first, which says nothing about its scope, so
    // those drops only count for a local that has no other.
    let replaces = |place: &StoredPlace, target: BasicBlockId| {
        let assigned = body.basic_blocks[target].statements.iter().find_map(|it| match &it.kind {
            StatementKind::Assign(to, _) => Some(to),
            _ => None,
        });
        assigned == Some(place)
    };
    let mut order: Vec<(LocalId, BasicBlockId)> = Vec::new();
    for replaced in [false, true] {
        for &id in post_order.iter().rev() {
            if let Some(TerminatorKind::Drop { place, target, .. }) =
                body.basic_blocks[id].terminator.as_ref().map(|it| &it.kind)
                && place.projection.as_ref().as_slice().is_empty()
                && replaces(place, *target) == replaced
                && !order.iter().any(|&(local, _)| local == place.local)
            {
                order.push((place.local, id));
            }
        }
    }
    // A drop that another one is reached from, and not the other way around, is of an outer
    // scope. Reverse post-order alone can put the exit of a loop before its body.
    let reached: Vec<rustc_hash::FxHashSet<BasicBlockId>> = order
        .iter()
        .map(|&(_, from)| {
            let mut reached = rustc_hash::FxHashSet::default();
            let mut pending = successors(from);
            while let Some(id) = pending.pop() {
                if reachable.contains(&id) && reached.insert(id) {
                    pending.extend(successors(id));
                }
            }
            reached
        })
        .collect();
    let later = |at: usize| {
        let after = |a: usize, b: usize| reached[a].contains(&order[b].1);
        (0..order.len()).filter(|&other| after(at, other) && !after(other, at)).count()
    };
    let mut ranked: Vec<(usize, LocalId)> =
        order.iter().enumerate().map(|(at, &(local, _))| (later(at), local)).collect();
    ranked.sort_by_key(|&(later, _)| std::cmp::Reverse(later));
    ranked.into_iter().map(|(_, local)| local).collect()
}
