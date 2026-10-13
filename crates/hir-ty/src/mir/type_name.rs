//! `type_name` as rustc spells it: full paths from the crate root, no lifetimes, and no
//! generic arguments that only repeat a default.

use std::fmt::Write;

use hir_def::{
    AdtId, HasModule, ModuleIdLt,
    signatures::{EnumSignature, StructSignature, TraitSignature, UnionSignature},
};
use rustc_ast_ir::Mutability;
use rustc_type_ir::inherent::{BoundExistentialPredicates as _, IntoKind};

use crate::{
    consteval::try_const_usize,
    db::HirDatabase,
    next_solver::{DbInterner, Ty, TyKind},
};

/// `None` is a type whose rustc spelling this does not reproduce.
pub(crate) fn rustc_type_name<'db>(db: &'db dyn HirDatabase, ty: Ty<'db>) -> Option<String> {
    let mut name = String::new();
    write_ty(db, &mut name, ty)?;
    Some(name)
}

fn write_module<'db>(
    db: &'db dyn HirDatabase,
    out: &mut String,
    module: ModuleIdLt<'db>,
) -> Option<()> {
    // rustc names an item in a body after the function, which a block module does not say.
    if module.is_block_module(db) {
        return None;
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
    out: &mut String,
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

fn write_ty<'db>(db: &'db dyn HirDatabase, out: &mut String, ty: Ty<'db>) -> Option<()> {
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
            let interner = DbInterner::new_no_crate(db);
            let defaults = db.generic_defaults(adt.into());
            let mut args = args.as_slice();
            while let Some((last, rest)) = args.split_last()
                && let Some(default) = defaults.get(rest.len())
                && default.instantiate(interner, rest).skip_norm_wip() == *last
            {
                args = rest;
            }
            let mut first = true;
            for arg in args {
                if arg.region().is_some() {
                    continue;
                }
                out.push_str(if first { "<" } else { ", " });
                first = false;
                match (arg.ty(), arg.konst()) {
                    (Some(ty), _) => write_ty(db, out, ty)?,
                    (_, Some(konst)) => write!(out, "{}", try_const_usize(db, konst)?).ok()?,
                    _ => return None,
                }
            }
            if !first {
                out.push('>');
            }
        }
        TyKind::Dynamic(bounds, _) => {
            let principal = bounds.principal()?.skip_binder();
            if !principal.args.is_empty() || bounds.projection_bounds().into_iter().count() != 0 {
                return None;
            }
            out.push_str("dyn ");
            let trait_ = principal.def_id.0;
            write_module(db, out, trait_.module(db))?;
            write!(out, "::{}", TraitSignature::of(db, trait_).name.as_str()).ok()?;
            for auto in bounds.auto_traits() {
                out.push_str(" + ");
                write_module(db, out, auto.0.module(db))?;
                write!(out, "::{}", TraitSignature::of(db, auto.0).name.as_str()).ok()?;
            }
        }
        _ => return None,
    }
    Some(())
}
