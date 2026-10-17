//! What a debugger is told of the type of a variable: [`DebugType`]s from the types of a
//! function's locals.
//!
//! A number, a `bool`, a `char`, a pointer, an array, a tuple, a struct, a union and an enum
//! are told in full, an enum the way rustc tells of one, so that a debugger's printers for
//! `Option`, `Vec` and `String` find what they look for. A closure, a `dyn` pointer and
//! whatever else there is are told by name and size alone, which a debugger shows as a value
//! without fields.

use hir_def::{
    AdtId, EnumId, GenericDefId, VariantId, hir::generics::GenericParamDataRef,
    item_tree::FieldsShape as Written,
};
use rustc_abi::{TagEncoding, Variants};
use rustc_hash::FxHashMap;
use rustc_type_ir::{UintTy, inherent::IntoKind};

use super::{Aggregate, DebugType, Field, Tag, TypeIndex, Variant};
use crate::{
    ParamEnvAndCrate,
    consteval::try_const_usize,
    db::HirDatabase,
    generics::generics,
    layout::{Layout, RustcFieldIdx},
    mir::type_name::rustc_type_name,
    next_solver::{
        DbInterner, GenericArgs, Ty, TyKind, TypingMode,
        infer::{DbInternerInferExt, InferCtxt, traits::ObligationCause},
        obligation_ctxt::ObligationCtxt,
    },
};

/// `DW_ATE_*`: how the bytes of a [`DebugType::Base`] are read.
mod encoding {
    pub(super) const BOOLEAN: u8 = 0x02;
    pub(super) const FLOAT: u8 = 0x04;
    pub(super) const SIGNED: u8 = 0x05;
    pub(super) const UNSIGNED: u8 = 0x07;
    pub(super) const UTF: u8 = 0x10;
}

/// How many pointers are followed from a variable before what one points to is left untold,
/// unless it is told of anyway: a list is as long as it likes.
const POINTERS: u32 = 2;

/// The types of one function's variables, which are of its instance.
pub(in super::super) struct Types<'db> {
    db: &'db dyn HirDatabase,
    env: ParamEnvAndCrate<'db>,
    interner: DbInterner<'db>,
    infcx: InferCtxt<'db>,
    told: Vec<DebugType>,
    /// Where each type that is told of, or is being told of, is among `told`.
    known: FxHashMap<Ty<'db>, TypeIndex>,
}

impl<'db> Types<'db> {
    pub(in super::super) fn new(db: &'db dyn HirDatabase, env: ParamEnvAndCrate<'db>) -> Self {
        let interner = DbInterner::new_with(db, env.krate);
        let infcx = interner.infer_ctxt().build(TypingMode::PostAnalysis);
        Types { db, env, interner, infcx, told: Vec::new(), known: FxHashMap::default() }
    }

    /// `None` is a type that has no layout or no name rustc would spell.
    pub(in super::super) fn describe(&mut self, ty: Ty<'db>) -> Option<TypeIndex> {
        self.ty(ty, POINTERS)
    }

    /// Every type told of, each where its [`TypeIndex`] says.
    pub(in super::super) fn finish(self) -> Vec<DebugType> {
        self.told
    }

    fn ty(&mut self, ty: Ty<'db>, pointers: u32) -> Option<TypeIndex> {
        // A type narrowed to some of its values, as what a `NonNull` keeps is, is told as
        // the type itself.
        let ty = match self.normalize(ty).kind() {
            TyKind::Pat(of, _) => self.normalize(of),
            _ => self.normalize(ty),
        };
        if let Some(&index) = self.known.get(&ty) {
            return Some(index);
        }
        let layout = self.db.layout_of_ty(ty.store(), self.env.store()).ok()?;
        let name = rustc_type_name(self.db, ty)?;
        // A type that is made of itself finds its place here. What stands there until the
        // type is told is what a type nothing more is known of is told as.
        let index = self.told.len() as TypeIndex;
        self.told.push(DebugType::Struct(Aggregate::opaque(name.clone(), layout.size.bytes())));
        self.known.insert(ty, index);
        match self.told_as(ty, name, &layout, pointers) {
            Some(told) => self.told[index as usize] = told,
            // What was told since is of no use to anything.
            None => {
                self.told.truncate(index as usize);
                self.known.retain(|_, known| *known < index);
            }
        }
        self.known.get(&ty).copied()
    }

    fn told_as(
        &mut self,
        ty: Ty<'db>,
        name: String,
        layout: &Layout,
        pointers: u32,
    ) -> Option<DebugType> {
        let size = layout.size.bytes();
        let base = |name, encoding| Some(DebugType::Base { name, size, encoding });
        let mut of = Aggregate::opaque(name, size);
        match ty.kind() {
            TyKind::Bool => return base(of.name, encoding::BOOLEAN),
            TyKind::Char => return base(of.name, encoding::UTF),
            TyKind::Int(_) => return base(of.name, encoding::SIGNED),
            TyKind::Uint(_) => return base(of.name, encoding::UNSIGNED),
            TyKind::Float(_) => return base(of.name, encoding::FLOAT),
            TyKind::Ref(_, to, _) | TyKind::RawPtr(to, _) => {
                return self.pointer(of, to, pointers);
            }
            TyKind::Array(element, len) => {
                let len = try_const_usize(self.db, len)? as u64;
                return Some(DebugType::Array { of: self.ty(element, pointers)?, len });
            }
            TyKind::Tuple(tys) => {
                for (index, ty) in tys.iter().enumerate() {
                    let offset = layout.fields.offset(index).bytes();
                    // A field of a type that is not told of is left out.
                    if let Some(ty) = self.ty(ty, pointers) {
                        of.fields.push(Field { name: format!("__{index}"), offset, ty });
                    }
                }
            }
            TyKind::Adt(def, args) => {
                of.parameters = self.parameters(def.def_id().into(), args, pointers);
                match def.def_id() {
                    AdtId::StructId(id) => {
                        of.fields = self.fields(
                            id.into(),
                            args,
                            |it| layout.fields.offset(it).bytes(),
                            pointers,
                        )?;
                    }
                    AdtId::UnionId(id) => {
                        of.fields = self.fields(
                            id.into(),
                            args,
                            |it| layout.fields.offset(it).bytes(),
                            pointers,
                        )?;
                        return Some(DebugType::Union(of));
                    }
                    AdtId::EnumId(id) => return self.enumeration(of, id, args, layout, pointers),
                }
            }
            _ => {}
        }
        Some(DebugType::Struct(of))
    }

    /// The fields of a struct, a union or a variant of an enum, which `layout` is of.
    fn fields(
        &mut self,
        variant: VariantId,
        args: GenericArgs<'db>,
        offset_of: impl Fn(usize) -> u64,
        pointers: u32,
    ) -> Option<Vec<Field>> {
        let written = variant.fields(self.db);
        let tuple = written.shape == Written::Tuple;
        let mut fields = Vec::new();
        for (id, ty) in self.db.field_types(variant).iter() {
            let index = u32::from(id.into_raw()) as usize;
            let name = written.fields()[id].name.as_str();
            let name = if tuple { format!("__{name}") } else { name.to_owned() };
            let ty = ty.ty().instantiate(self.interner, args).skip_norm_wip();
            let offset = offset_of(index);
            if let Some(ty) = self.ty(ty, pointers) {
                fields.push(Field { name, offset, ty });
            }
        }
        Some(fields)
    }

    /// The types the generic parameters of `def` stand for in `args`, by the names the
    /// parameters are written with.
    fn parameters(
        &mut self,
        def: GenericDefId,
        args: GenericArgs<'db>,
        pointers: u32,
    ) -> Vec<(String, TypeIndex)> {
        let names: Vec<Option<String>> = generics(self.db, def)
            .iter_self()
            .map(|(_, parameter)| match parameter {
                GenericParamDataRef::TypeParamData(it) => {
                    it.name.as_ref().map(|name| name.as_str().to_owned())
                }
                _ => None,
            })
            .collect();
        let mut parameters = Vec::new();
        for (name, arg) in names.into_iter().zip(args.iter()) {
            if let Some(name) = name
                && let Some(ty) = arg.ty()
                && let Some(ty) = self.ty(ty, pointers)
            {
                parameters.push((name, ty));
            }
        }
        parameters
    }

    /// An enum: the names of its values when no variant has a field, and otherwise which
    /// variant a value is of by its tag and the fields of each.
    fn enumeration(
        &mut self,
        of: Aggregate,
        id: EnumId,
        args: GenericArgs<'db>,
        layout: &Layout,
        pointers: u32,
    ) -> Option<DebugType> {
        let written = &id.enum_variants(self.db).variants;
        let (tag, encoding, layouts) = match &layout.variants {
            Variants::Empty => return Some(DebugType::Struct(of)),
            Variants::Single { index } => {
                let (name, (variant, _)) = written.get_index(index.0)?;
                let offset_of = |it| layout.fields.offset(it).bytes();
                let fields = self.fields((*variant).into(), args, offset_of, pointers)?;
                let variant = Variant { name: name.as_str().to_owned(), tag: None, fields };
                return Some(DebugType::Enum { of, tag: None, variants: vec![variant] });
            }
            Variants::Multiple { tag, tag_encoding, tag_field, variants } => {
                let size = tag.size(self.target()?).bytes();
                let offset = layout.fields.offset(rustc_index::Idx::index(*tag_field)).bytes();
                (Tag { offset, size }, tag_encoding, variants)
            }
        };
        // A tag is told as a number of a machine word at most.
        let mask = match tag.size {
            8 => u64::MAX,
            size @ 1..8 => (1 << (size * 8)) - 1,
            _ => return Some(DebugType::Struct(of)),
        };
        let mut variants = Vec::new();
        let mut fieldless = true;
        for (index, layout) in layouts.iter_enumerated() {
            let (name, (variant, _)) = written.get_index(index.0)?;
            let value = match encoding {
                TagEncoding::Direct => {
                    Some(self.db.const_eval_discriminant(*variant).ok()? as u64 & mask)
                }
                TagEncoding::Niche { untagged_variant, .. } if *untagged_variant == index => None,
                TagEncoding::Niche { niche_variants, niche_start, .. } => {
                    let nth = (index.0 - niche_variants.start.0) as u128;
                    Some(nth.wrapping_add(*niche_start) as u64 & mask)
                }
            };
            let offset_of = |it| layout.field_offsets[RustcFieldIdx::new(it)].bytes();
            let fields = self.fields((*variant).into(), args, offset_of, pointers)?;
            fieldless &= fields.is_empty();
            variants.push(Variant { name: name.as_str().to_owned(), tag: value, fields });
        }
        if fieldless && matches!(encoding, TagEncoding::Direct) {
            let names = variants.into_iter().filter_map(|it| Some((it.name, it.tag?))).collect();
            return Some(DebugType::Names { name: of.name, size: of.size, names });
        }
        Some(DebugType::Enum { of, tag: Some(tag), variants })
    }

    /// A pointer of one word, or of two as the struct rustc has for it when it is to a slice
    /// or a `str`: where the first of them is and how many there are.
    fn pointer(&mut self, of: Aggregate, to: Ty<'db>, pointers: u32) -> Option<DebugType> {
        let to = self.normalize(to);
        let word = self.target()?.pointer_size().bytes();
        if of.size == word {
            return Some(DebugType::Pointer { name: of.name, to: self.followed(to, pointers) });
        }
        let element = match to.kind() {
            TyKind::Str => Ty::new_uint(self.interner, UintTy::U8),
            TyKind::Slice(element) => element,
            _ => return Some(DebugType::Struct(of)),
        };
        let first = self.ty(Ty::new_imm_ptr(self.interner, element), pointers)?;
        let count = self.ty(Ty::new_uint(self.interner, UintTy::Usize), pointers)?;
        let fields = vec![
            Field { name: "data_ptr".to_owned(), offset: 0, ty: first },
            Field { name: "length".to_owned(), offset: word, ty: count },
        ];
        Some(DebugType::Struct(Aggregate { fields, ..of }))
    }

    /// What a pointer points to, where that is told of.
    fn followed(&mut self, to: Ty<'db>, pointers: u32) -> Option<TypeIndex> {
        match pointers.checked_sub(1) {
            Some(pointers) => self.ty(to, pointers),
            None => self.known.get(&to).copied(),
        }
    }

    fn target(&self) -> Option<&'db rustc_abi::TargetDataLayout> {
        self.db.target_data_layout(self.env.krate).ok()
    }

    fn normalize(&self, ty: Ty<'db>) -> Ty<'db> {
        ObligationCtxt::new(&self.infcx)
            .structurally_normalize_ty(&ObligationCause::dummy(), self.env.param_env, ty)
            .unwrap_or(ty)
    }
}

impl Aggregate {
    /// A type nothing is told of but its name and its size.
    fn opaque(name: String, size: u64) -> Aggregate {
        Aggregate { name, size, fields: Vec::new(), parameters: Vec::new() }
    }
}
