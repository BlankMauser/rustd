//! The names rustc has for types and functions: full paths from the crate root, spelled
//! either as `type_name` gives them or as its symbols demangle.

use std::fmt::Write;

use hir_def::{
    AdtId, CallableDefId, DefWithBodyId, FunctionId, GenericDefId, HasModule, ItemContainerId,
    Lookup, ModuleDefId, ModuleIdLt,
    expr_store::{Body, BodySourceMap},
    hir::{ClosureKind, CoroutineKind, CoroutineSource, Expr, ExprId},
    signatures::{
        EnumSignature, FunctionSignature, StructSignature, TraitSignature, TypeAliasSignature,
        UnionSignature,
    },
};
use rustc_abi::ExternAbi;
use rustc_ast_ir::Mutability;
use rustc_type_ir::inherent::{
    BoundExistentialPredicates as _, GenericArgs as _, IntoKind, Safety as _, Term as _,
};

use crate::{
    InferBodyId,
    consteval::try_const_usize,
    db::{HirDatabase, InternedClosure},
    generics::generics,
    next_solver::{DbInterner, GenericArg, TraitRef, Ty, TyKind},
};

/// `None` is a type whose rustc spelling this does not reproduce.
pub(crate) fn rustc_type_name<'db>(db: &'db dyn HirDatabase, ty: Ty<'db>) -> Option<String> {
    let mut out = Spelled::new(Spelling::TypeName);
    write_ty(db, &mut out, ty)?;
    Some(out.text)
}

/// What the symbol of `func` with `args` demangles to, which is how a backtrace names it.
pub(crate) fn rustc_function_name<'db>(
    db: &'db dyn HirDatabase,
    func: FunctionId,
    args: &[GenericArg<'db>],
) -> Option<String> {
    let mut out = Spelled::new(Spelling::Symbol);
    write_function(db, &mut out, func, args)?;
    Some(out.text)
}

/// The same of the closure or `async` block written at `expr` of `func`.
pub(crate) fn rustc_closure_name<'db>(
    db: &'db dyn HirDatabase,
    func: FunctionId,
    args: &[GenericArg<'db>],
    expr: ExprId,
) -> Option<String> {
    let mut out = Spelled::new(Spelling::Symbol);
    write_closure(db, &mut out, func, args, expr)?;
    Some(out.text)
}

/// The same of a type, as it stands among the generic arguments of a symbol.
pub(crate) fn rustc_symbol_type<'db>(db: &'db dyn HirDatabase, ty: Ty<'db>) -> Option<String> {
    let mut out = Spelled::new(Spelling::Symbol);
    write_ty(db, &mut out, ty)?;
    Some(out.text)
}

/// What rustc calls a type where it tells a debugger of it: every generic argument that is
/// not a lifetime, a closure `{closure_env#0}` and an `async fn` `{async_fn_env#0}` after
/// what they are written in.
pub(crate) fn rustc_debug_name<'db>(db: &'db dyn HirDatabase, ty: Ty<'db>) -> Option<String> {
    let mut out = Spelled::new(Spelling::Debug);
    write_ty(db, &mut out, ty)?;
    Some(out.text)
}

/// Which of rustc's ways to write a name is wanted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// `core::any::type_name`: no lifetimes but `'_`, every closure `{{closure}}`.
    TypeName,
    /// A demangled symbol: no lifetimes, closures numbered.
    Symbol,
    /// The name of a type in debug info: no lifetimes, defaults written out, closures
    /// numbered and called after what they are.
    Debug,
}

/// A name being written.
struct Spelled {
    text: String,
    spelling: Spelling,
}

impl Spelled {
    fn new(spelling: Spelling) -> Self {
        Self { text: String::new(), spelling }
    }

    fn push_str(&mut self, text: &str) {
        self.text.push_str(text);
    }

    fn push(&mut self, text: char) {
        self.text.push(text);
    }

    /// The closures something is written in, outermost first, by their places among the
    /// closures of what they are written in.
    fn closures(&mut self, places: &[(usize, ClosureKind)]) {
        for (place, kind) in places {
            match self.spelling {
                Spelling::TypeName => self.text.push_str("::{{closure}}"),
                Spelling::Symbol => {
                    let _ = write!(self.text, "::{{closure#{place}}}");
                }
                Spelling::Debug => {
                    let _ = write!(self.text, "::{{{}#{place}}}", kind_label(*kind));
                }
            }
        }
    }

    /// The same where the last of them is what is named, which a debugger is told of as
    /// what the closure keeps: its environment.
    fn closure_env(&mut self, places: &[(usize, ClosureKind)]) -> Option<()> {
        let ((place, kind), around) = places.split_last()?;
        self.closures(around);
        write!(self.text, "::{{{}_env#{place}}}", kind_label(*kind)).ok()
    }
}

/// What rustc calls a closure of a kind in debug info.
fn kind_label(kind: ClosureKind) -> &'static str {
    use CoroutineKind::{Async, AsyncGen, Gen};
    use CoroutineSource::{Block, Closure, Fn};
    match kind {
        ClosureKind::Closure | ClosureKind::CoroutineClosure(_) => "closure",
        ClosureKind::OldCoroutine(_) => "coroutine",
        ClosureKind::Coroutine { kind: Async, source: Block } => "async_block",
        ClosureKind::Coroutine { kind: Async, source: Closure } => "async_closure",
        ClosureKind::Coroutine { kind: Async, source: Fn } => "async_fn",
        ClosureKind::Coroutine { kind: Gen, source: Block } => "gen_block",
        ClosureKind::Coroutine { kind: Gen, source: Closure } => "gen_closure",
        ClosureKind::Coroutine { kind: Gen, source: Fn } => "gen_fn",
        ClosureKind::Coroutine { kind: AsyncGen, source: Block } => "async_gen_block",
        ClosureKind::Coroutine { kind: AsyncGen, source: Closure } => "async_gen_closure",
        ClosureKind::Coroutine { kind: AsyncGen, source: Fn } => "async_gen_fn",
    }
}

impl Write for Spelled {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.text.push_str(text);
        Ok(())
    }
}

fn write_module<'db>(
    db: &'db dyn HirDatabase,
    out: &mut Spelled,
    module: ModuleIdLt<'db>,
) -> Option<()> {
    // rustc names an item in a body after the function the body is of, and the closures of
    // it the item is written in.
    if module.is_block_module(db) {
        let block = module.block(db)?;
        let mut around = block.module(db);
        // FIXME: a method's body is named after the `impl`, which is not looked through here.
        let owner = loop {
            let scope = around.def_map(db).module(db, around).scope(db);
            let owner = scope.declarations().find_map(|def| match def {
                ModuleDefId::FunctionId(it) => {
                    Body::of(db, it.into()).blocks(db).any(|(id, _)| id == block).then_some(it)
                }
                _ => None,
            });
            match owner {
                Some(owner) => break owner,
                // A block in a block, of the same body.
                None => around = around.block(db)?.module(db),
            }
        };
        let body = Body::of(db, owner.into());
        let (at, _) = body
            .exprs()
            .find(|(_, expr)| matches!(expr, Expr::Block { id: Some(id), .. } if *id == block))?;
        write_module(db, out, around)?;
        write!(out, "::{}", FunctionSignature::of(db, owner).name.as_str()).ok()?;
        out.closures(&closures_around(db, owner.into(), at)?);
        return Some(());
    }
    match module.containing_module(db) {
        Some(parent) => {
            write_module(db, out, parent)?;
            write!(out, "::{}", module.name(db)?.as_str()).ok()
        }
        None => {
            let krate = module.krate(db);
            let name = krate.extra_data(db).display_name.as_ref()?;
            out.push_str(name.crate_name().as_str());
            Some(())
        }
    }
}

fn write_list<'db>(
    db: &'db dyn HirDatabase,
    out: &mut Spelled,
    tys: impl IntoIterator<Item = Ty<'db>>,
) -> Option<()> {
    for (i, ty) in tys.into_iter().enumerate() {
        if i != 0 {
            out.push_str(", ");
        }
        write_ty(db, out, ty)?;
    }
    Some(())
}

fn write_ty<'db>(db: &'db dyn HirDatabase, out: &mut Spelled, ty: Ty<'db>) -> Option<()> {
    match ty.kind() {
        TyKind::Bool => out.push_str("bool"),
        TyKind::Char => out.push_str("char"),
        TyKind::Str => out.push_str("str"),
        TyKind::Never => out.push('!'),
        TyKind::Int(it) => out.push_str(it.name_str()),
        TyKind::Uint(it) => out.push_str(it.name_str()),
        TyKind::Float(it) => out.push_str(it.name_str()),
        TyKind::Ref(_, pointee, mutability) => {
            out.push_str(match mutability {
                Mutability::Not => "&",
                Mutability::Mut => "&mut ",
            });
            write_ty(db, out, pointee)?;
        }
        TyKind::RawPtr(pointee, mutability) => {
            out.push_str(match mutability {
                Mutability::Not => "*const ",
                Mutability::Mut => "*mut ",
            });
            write_ty(db, out, pointee)?;
        }
        TyKind::Slice(element) => {
            out.push('[');
            write_ty(db, out, element)?;
            out.push(']');
        }
        TyKind::Array(element, len) => {
            out.push('[');
            write_ty(db, out, element)?;
            write!(out, "; {}]", try_const_usize(db, len)?).ok()?;
        }
        TyKind::Tuple(tys) => {
            out.push('(');
            write_list(db, out, tys.iter())?;
            out.push_str(if tys.len() == 1 { ",)" } else { ")" });
        }
        TyKind::Adt(adt, args) => {
            let adt = adt.def_id();
            let name = match adt {
                AdtId::StructId(it) => &StructSignature::of(db, it).name,
                AdtId::UnionId(it) => &UnionSignature::of(db, it).name,
                AdtId::EnumId(it) => &EnumSignature::of(db, it).name,
            };
            write_module(db, out, adt.module(db))?;
            write!(out, "::{}", name.as_str()).ok()?;
            let args = match out.spelling {
                Spelling::Debug => args.as_slice(),
                _ => without_defaults(db, adt.into(), args.as_slice()),
            };
            write_args(db, out, args)?;
        }
        TyKind::Dynamic(bounds, _) if out.spelling == Spelling::Debug => {
            write_dyn_debug(db, out, bounds)?;
        }
        TyKind::Dynamic(bounds, _) => {
            let principal = bounds.principal()?.skip_binder();
            if bounds.projection_bounds().into_iter().count() != 0 {
                return None;
            }
            if !principal.args.is_empty() && out.spelling == Spelling::TypeName {
                return None;
            }
            out.push_str("dyn ");
            let trait_ = principal.def_id.0;
            write_module(db, out, trait_.module(db))?;
            write!(out, "::{}", TraitSignature::of(db, trait_).name.as_str()).ok()?;
            write_args(db, out, principal.args.as_slice())?;
            for auto in bounds.auto_traits() {
                out.push_str(" + ");
                write_module(db, out, auto.0.module(db))?;
                write!(out, "::{}", TraitSignature::of(db, auto.0).name.as_str()).ok()?;
            }
        }
        TyKind::FnDef(def, args) => {
            let CallableDefId::FunctionId(func) = def.0 else { return None };
            write_function(db, out, func, args.as_slice())?;
        }
        TyKind::Closure(id, args) => {
            let func = written_in(id.0.loc(db))?;
            write_closure(db, out, func.0, args.as_closure().parent_args(), func.1)?;
        }
        TyKind::Coroutine(id, args) if out.spelling != Spelling::TypeName => {
            let func = written_in(id.0.loc(db))?;
            write_closure(db, out, func.0, args.as_coroutine().parent_args(), func.1)?;
        }
        TyKind::CoroutineClosure(id, args) if out.spelling != Spelling::TypeName => {
            let func = written_in(id.0.loc(db))?;
            write_closure(db, out, func.0, args.as_coroutine_closure().parent_args(), func.1)?;
        }
        TyKind::FnPtr(sig, header) if out.spelling != Spelling::TypeName => {
            let sig = sig.skip_binder();
            if !header.safety().is_safe() {
                out.push_str("unsafe ");
            }
            if header.abi() != ExternAbi::Rust {
                write!(out, "extern \"{}\" ", header.abi().as_str()).ok()?;
            }
            out.push_str("fn(");
            write_list(db, out, sig.inputs().iter().copied())?;
            out.push(')');
            if !sig.output().is_unit() {
                out.push_str(" -> ");
                write_ty(db, out, sig.output())?;
            }
        }
        _ => return None,
    }
    Some(())
}

/// The function a closure is written in and the expression that is the closure.
fn written_in(closure: InternedClosure<'_>) -> Option<(FunctionId, ExprId)> {
    match closure.owner {
        InferBodyId::DefWithBodyId(DefWithBodyId::FunctionId(func)) => Some((func, closure.expr)),
        // FIXME: one of a constant or a static is named after that.
        _ => None,
    }
}

/// `<A, B>`, or nothing for no arguments. `type_name` writes every lifetime as `'_`, and a
/// symbol has none.
fn write_args<'db>(
    db: &'db dyn HirDatabase,
    out: &mut Spelled,
    args: &[GenericArg<'db>],
) -> Option<()> {
    let mut written = 0;
    for arg in args {
        if out.spelling != Spelling::TypeName && arg.region().is_some() {
            continue;
        }
        out.push_str(if written == 0 { "<" } else { ", " });
        written += 1;
        match (arg.ty(), arg.konst()) {
            (Some(ty), _) => write_ty(db, out, ty)?,
            (_, Some(konst)) => write!(out, "{}", try_const_usize(db, konst)?).ok()?,
            _ => out.push_str("'_"),
        }
    }
    if written != 0 {
        out.push('>');
    }
    Some(())
}

/// The closure or `async` block written at `expr` of `func`.
fn write_closure<'db>(
    db: &'db dyn HirDatabase,
    out: &mut Spelled,
    func: FunctionId,
    args: &[GenericArg<'db>],
    expr: ExprId,
) -> Option<()> {
    let around = closures_around(db, func.into(), expr)?;
    if out.spelling != Spelling::Debug {
        write_function(db, out, func, args)?;
        out.closures(&around);
        return Some(());
    }
    // A debugger is told of what the function is written in by its place there, then of the
    // closure, and last of what the function is generic over.
    match func.lookup(db).container {
        ItemContainerId::ModuleId(module) => write_module(db, out, module)?,
        ItemContainerId::ImplId(impl_) => {
            let module = impl_.lookup(db).container;
            let scope = module.def_map(db).module(db, module).scope(db);
            let place = scope.impls().position(|it| it == impl_)?;
            write_module(db, out, module)?;
            write!(out, "::{{impl#{place}}}").ok()?;
        }
        ItemContainerId::TraitId(trait_) => {
            write_module(db, out, trait_.module(db))?;
            write!(out, "::{}", TraitSignature::of(db, trait_).name.as_str()).ok()?;
        }
        ItemContainerId::ExternBlockId(_) => return None,
    }
    write!(out, "::{}", FunctionSignature::of(db, func).name.as_str()).ok()?;
    out.closure_env(&around)?;
    write_args(db, out, args)
}

/// `dyn Trait<A, Item=B> + Auto` as a debugger is told of it, in brackets after `dyn` when
/// there is more than the trait.
fn write_dyn_debug<'db>(
    db: &'db dyn HirDatabase,
    out: &mut Spelled,
    bounds: crate::next_solver::BoundExistentialPredicates<'db>,
) -> Option<()> {
    let mut autos = Vec::new();
    for auto in bounds.auto_traits() {
        let mut name = Spelled::new(Spelling::Debug);
        write_module(db, &mut name, auto.0.module(db))?;
        write!(name, "::{}", TraitSignature::of(db, auto.0).name.as_str()).ok()?;
        autos.push(name.text);
    }
    autos.sort_unstable();
    let principal = bounds.principal();
    let bracketed = principal.is_some() && !autos.is_empty();
    out.push_str(if bracketed { "dyn (" } else { "dyn " });
    if let Some(principal) = principal {
        let principal = principal.skip_binder();
        let trait_ = principal.def_id.0;
        write_module(db, out, trait_.module(db))?;
        write!(out, "::{}", TraitSignature::of(db, trait_).name.as_str()).ok()?;
        let mut written = Spelled::new(Spelling::Debug);
        write_args(db, &mut written, principal.args.as_slice())?;
        let mut inner =
            written.text.strip_prefix('<').and_then(|it| it.strip_suffix('>')).map(str::to_owned);
        for bound in bounds.projection_bounds() {
            let bound = bound.skip_binder();
            let crate::next_solver::TermId::TypeAliasId(alias) = bound.def_id.0 else {
                return None;
            };
            let mut part = Spelled::new(Spelling::Debug);
            write!(part, "{}=", TypeAliasSignature::of(db, alias).name.as_str()).ok()?;
            write_ty(db, &mut part, bound.term.as_type()?)?;
            inner = Some(match inner {
                Some(inner) => format!("{inner}, {}", part.text),
                None => part.text,
            });
        }
        if let Some(inner) = inner {
            write!(out, "<{inner}>").ok()?;
        }
        if !autos.is_empty() {
            out.push_str(" + ");
        }
    }
    out.push_str(&autos.join(" + "));
    if bracketed {
        out.push(')');
    }
    Some(())
}

/// The module of the item rustc takes a type to be about, when it has one.
fn characteristic_module<'db>(db: &'db dyn HirDatabase, ty: Ty<'db>) -> Option<ModuleIdLt<'db>> {
    match ty.kind() {
        TyKind::Adt(adt, _) => Some(adt.def_id().module(db)),
        TyKind::Dynamic(bounds, _) => Some(bounds.principal()?.skip_binder().def_id.0.module(db)),
        TyKind::Ref(_, inner, _)
        | TyKind::RawPtr(inner, _)
        | TyKind::Slice(inner)
        | TyKind::Array(inner, _) => characteristic_module(db, inner),
        TyKind::Tuple(tys) => tys.iter().find_map(|ty| characteristic_module(db, ty)),
        // FIXME: a function or a closure is about itself, in what it is written in.
        _ => None,
    }
}

fn write_trait<'db>(
    db: &'db dyn HirDatabase,
    out: &mut Spelled,
    trait_ref: TraitRef<'db>,
) -> Option<()> {
    let trait_ = trait_ref.def_id.0;
    write_module(db, out, trait_.module(db))?;
    write!(out, "::{}", TraitSignature::of(db, trait_).name.as_str()).ok()?;
    write_args(db, out, without_defaults(db, trait_.into(), trait_ref.args.as_slice()).get(1..)?)
}

/// The arguments of `def` that rustc writes: not the last ones that only repeat a default.
fn without_defaults<'a, 'db>(
    db: &'db dyn HirDatabase,
    def: GenericDefId,
    mut args: &'a [GenericArg<'db>],
) -> &'a [GenericArg<'db>] {
    let interner = DbInterner::new_no_crate(db);
    let defaults = db.generic_defaults(def);
    while let Some((last, rest)) = args.split_last()
        && let Some(default) = defaults.get(rest.len())
        && default.instantiate(interner, rest).skip_norm_wip() == *last
    {
        args = rest;
    }
    args
}

/// A function with its generic arguments. To `type_name` that is `path::f<A>`, and for one
/// of an `impl` `path::Type<T>::f`, `<T as Trait>::f`, or `path::<impl Trait for T>::f` when
/// the `impl` is in the module of neither the type nor the trait. A symbol has `path::f::<A>`,
/// `<path::Type<T>>::f` and `<T as Trait>::f`.
fn write_function<'db>(
    db: &'db dyn HirDatabase,
    out: &mut Spelled,
    func: FunctionId,
    args: &[GenericArg<'db>],
) -> Option<()> {
    let (parent_args, own_args) = args.split_at_checked(generics(db, func.into()).len_parent())?;
    // A lifetime of a function is mostly late bound, and then no argument to rustc.
    if out.spelling == Spelling::TypeName && own_args.iter().any(|arg| arg.region().is_some()) {
        return None;
    }
    match func.lookup(db).container {
        ItemContainerId::ModuleId(module) => write_module(db, out, module)?,
        ItemContainerId::ImplId(impl_) if out.spelling == Spelling::Symbol => {
            let interner = DbInterner::new_no_crate(db);
            let self_ty = db.impl_self_ty(impl_).instantiate(interner, parent_args).skip_norm_wip();
            out.push('<');
            write_ty(db, out, self_ty)?;
            if let Some(trait_ref) = db.impl_trait(impl_) {
                out.push_str(" as ");
                write_trait(db, out, trait_ref.instantiate(interner, parent_args).skip_norm_wip())?;
            }
            out.push('>');
        }
        ItemContainerId::TraitId(trait_) if out.spelling == Spelling::Symbol => {
            let self_ty = parent_args.first()?;
            out.push('<');
            write_ty(db, out, self_ty.ty()?)?;
            out.push_str(" as ");
            write_module(db, out, trait_.module(db))?;
            write!(out, "::{}", TraitSignature::of(db, trait_).name.as_str()).ok()?;
            write_args(db, out, without_defaults(db, trait_.into(), parent_args).get(1..)?)?;
            out.push('>');
        }
        ItemContainerId::ImplId(impl_) => {
            let interner = DbInterner::new_no_crate(db);
            let self_ty = db.impl_self_ty(impl_).instantiate(interner, parent_args).skip_norm_wip();
            let trait_ref = db
                .impl_trait(impl_)
                .map(|it| it.instantiate(interner, parent_args).skip_norm_wip());
            let module = impl_.lookup(db).container;
            let in_self_module = characteristic_module(db, self_ty) == Some(module);
            let in_trait_module = trait_ref.is_some_and(|it| it.def_id.0.module(db) == module);
            if !in_self_module && !in_trait_module {
                write_module(db, out, module)?;
                out.push_str("::<impl ");
                if let Some(trait_ref) = trait_ref {
                    write_trait(db, out, trait_ref)?;
                    out.push_str(" for ");
                }
                write_ty(db, out, self_ty)?;
                out.push('>');
            } else if let Some(trait_ref) = trait_ref {
                out.push('<');
                write_ty(db, out, self_ty)?;
                out.push_str(" as ");
                write_trait(db, out, trait_ref)?;
                out.push('>');
            } else if matches!(
                self_ty.kind(),
                TyKind::Adt(..)
                    | TyKind::Bool
                    | TyKind::Char
                    | TyKind::Str
                    | TyKind::Int(_)
                    | TyKind::Uint(_)
                    | TyKind::Float(_)
            ) {
                write_ty(db, out, self_ty)?;
            } else {
                out.push('<');
                write_ty(db, out, self_ty)?;
                out.push('>');
            }
        }
        // FIXME: a provided method is `<T as Trait>::f`.
        ItemContainerId::TraitId(_) | ItemContainerId::ExternBlockId(_) => return None,
    }
    write!(out, "::{}", FunctionSignature::of(db, func).name.as_str()).ok()?;
    if out.spelling == Spelling::Symbol && own_args.iter().any(|arg| arg.region().is_none()) {
        out.push_str("::");
    }
    write_args(db, out, own_args)
}

/// The closures and `async` blocks `target` is written in, outermost first and with `target`
/// last when it is one: each by its place among those written in the one before it.
fn closures_around(
    db: &dyn HirDatabase,
    owner: DefWithBodyId,
    target: ExprId,
) -> Option<Vec<(usize, ClosureKind)>> {
    let (body, source_map) = Body::with_source_map(db, owner);
    let mut around = Vec::new();
    if !find_closures(body, body.root_expr(), target, &mut around) {
        return None;
    }
    let order = |closure: ExprId| MacroCalls::around(db, source_map, closure);
    let mut inside = None;
    let places = around.iter().map(|&closure| {
        let mut beside = Vec::new();
        match inside.replace(closure) {
            Some(it) => body.walk_child_exprs(it, |child| closures_in(body, child, &mut beside)),
            None => closures_in(body, body.root_expr(), &mut beside),
        }
        // Stable, so those of one expansion stay in the order they are written in.
        beside.sort_by_cached_key(|&it| order(it));
        let Expr::Closure { closure_kind, .. } = body[closure] else { return None };
        Some((beside.iter().position(|&it| it == closure)?, closure_kind))
    });
    places.collect()
}

/// The closures `target` is written in, looking for it from `at` down.
fn find_closures(body: &Body, at: ExprId, target: ExprId, around: &mut Vec<ExprId>) -> bool {
    let is_closure = matches!(body[at], Expr::Closure { .. });
    if is_closure {
        around.push(at);
    }
    let mut found = at == target;
    if !found {
        body.walk_child_exprs(at, |child| {
            found = found || find_closures(body, child, target, around);
        });
    }
    if is_closure && !found {
        around.pop();
    }
    found
}

/// The closures from `at` down that are written in no other closure on the way.
fn closures_in(body: &Body, at: ExprId, found: &mut Vec<ExprId>) {
    match body[at] {
        Expr::Closure { .. } => found.push(at),
        _ => body.walk_child_exprs(at, |child| closures_in(body, child, found)),
    }
}

/// The macro calls something is in the expansion of, outermost first: each by where in the
/// text it is expanded from the call starts.
///
/// rustc numbers what it finds in a body before it expands the macros of it, and then what
/// each expansion holds as it gets to it, the macros of an expansion first. That is the
/// order these sort in.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct MacroCalls(Vec<u32>);

impl MacroCalls {
    fn around(db: &dyn HirDatabase, source_map: &BodySourceMap, expr: ExprId) -> Self {
        let mut calls = Vec::new();
        let mut file = source_map.expr_syntax(expr).ok().map(|it| it.file_id);
        while let Some(call) = file.and_then(|it| it.macro_file()) {
            let node = call.call_node(db);
            calls.push(node.value.text_range().start().into());
            file = Some(node.file_id);
        }
        calls.reverse();
        MacroCalls(calls)
    }
}
