//! The crate an instance is compiled from the point of view of.
//!
//! An instance is asked for by a function of some crate, but what it compiles to depends only
//! on what its own name mentions: the function it instantiates and the types it is
//! instantiated with. Compiling it as the crate that asked would make `Vec<u8>::push` depend
//! on every file of the workspace crate that happened to use it, and give it one cache entry
//! per crate that does.

use base_db::Crate;
use hir_def::{CallableDefId, DefWithBodyId, HasModule};
use rustc_hash::{FxHashMap, FxHashSet};
use rustc_type_ir::{
    TypeSuperVisitable, TypeVisitable, TypeVisitor,
    inherent::{BoundExistentialPredicates as _, IntoKind},
};
use salsa::plumbing::FromId as _;

use crate::{
    InferBodyId,
    db::{HirDatabase, InternedClosureId, InternedCoroutineId},
    next_solver::{DbInterner, Ty, TyKind},
    traits::{ParamEnvAndCrate, StoredParamEnvAndCrate},
};

use super::{Instance, InstanceDef};

/// The crates the name of an instance mentions.
struct Mentions<'db> {
    db: &'db dyn HirDatabase,
    crates: FxHashSet<Crate>,
    /// It mentions something whose crate we can't tell.
    opaque: bool,
}

impl<'db> Mentions<'db> {
    fn of(db: &'db dyn HirDatabase, instance: &Instance) -> Mentions<'db> {
        let mut mentions = Mentions { db, crates: FxHashSet::default(), opaque: false };
        match instance.def {
            InstanceDef::Fn(func) => {
                mentions.crates.insert(func.krate(db));
            }
            InstanceDef::Closure(id) => {
                mentions.owner(InternedClosureId::from_id(id).loc(db).owner)
            }
            InstanceDef::Coroutine(id) => {
                mentions.owner(InternedCoroutineId::from_id(id).loc(db).owner)
            }
            InstanceDef::DropGlue
            | InstanceDef::CallOnceShim
            | InstanceDef::FnPointerShim
            | InstanceDef::ReifyShim { .. } => {}
        }
        for arg in instance.args.as_ref().iter() {
            arg.visit_with(&mut mentions);
        }
        mentions
    }

    fn owner(&mut self, owner: InferBodyId<'_>) {
        let InferBodyId::DefWithBodyId(owner) = owner else {
            self.opaque = true;
            return;
        };
        let krate = match owner {
            DefWithBodyId::FunctionId(it) => it.krate(self.db),
            DefWithBodyId::ConstId(it) => it.krate(self.db),
            DefWithBodyId::StaticId(it) => it.krate(self.db),
            DefWithBodyId::VariantId(it) => it.krate(self.db),
        };
        self.crates.insert(krate);
    }
}

impl<'db> TypeVisitor<DbInterner<'db>> for Mentions<'db> {
    type Result = ();

    fn visit_ty(&mut self, ty: Ty<'db>) {
        match ty.kind() {
            TyKind::Adt(adt, _) => {
                self.crates.insert(adt.def_id().module(self.db).krate(self.db));
            }
            TyKind::FnDef(def, _) => {
                let krate = match def.0 {
                    CallableDefId::FunctionId(it) => it.krate(self.db),
                    CallableDefId::StructId(it) => it.krate(self.db),
                    CallableDefId::EnumVariantId(it) => it.krate(self.db),
                };
                self.crates.insert(krate);
            }
            TyKind::Closure(id, _) => self.owner(id.0.loc(self.db).owner),
            TyKind::Coroutine(id, _) => self.owner(id.0.loc(self.db).owner),
            TyKind::Dynamic(bounds, _) => {
                if let Some(principal) = bounds.principal() {
                    self.crates.insert(principal.skip_binder().def_id.0.krate(self.db));
                }
            }
            TyKind::Alias(..) | TyKind::Foreign(..) => self.opaque = true,
            _ => {}
        }
        ty.super_visit_with(self)
    }
}

/// Picks the crate each instance is compiled as.
#[derive(Default)]
pub(super) struct Homes {
    /// What each crate can see: itself and everything it depends on.
    sees: FxHashMap<Crate, FxHashSet<Crate>>,
}

impl Homes {
    /// The environment to compile `instance` in when a function compiled in `asked` asks for
    /// it: that of the one crate it mentions that sees all the others, which every crate
    /// asking for the instance agrees on. Without such a crate, the asking one's.
    pub(super) fn home(
        &mut self,
        db: &dyn HirDatabase,
        instance: &Instance,
        asked: &StoredParamEnvAndCrate,
    ) -> StoredParamEnvAndCrate {
        if std::env::var_os("RUSTD_NO_HOME").is_some() || !asked.param_env(db).clauses.is_empty() {
            return asked.clone();
        }
        let mentions = Mentions::of(db, instance);
        if mentions.opaque {
            return asked.clone();
        }
        let home = mentions.crates.iter().copied().find(|&krate| {
            let sees = self
                .sees
                .entry(krate)
                .or_insert_with(|| krate.transitive_deps(db).into_iter().collect());
            mentions.crates.iter().all(|it| sees.contains(it))
        });
        match home {
            Some(krate) => ParamEnvAndCrate { krate, ..asked.as_ref(db) }.store(),
            None => asked.clone(),
        }
    }
}
