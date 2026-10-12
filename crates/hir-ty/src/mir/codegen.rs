//! rustd spike: lower RA MIR to native code with Cranelift.
//!
//! Every local lives in memory. Calls follow rustc's (unstable) Rust ABI so the
//! output can be linked against rustc-compiled objects.

use std::fmt;

use cranelift_codegen::{
    ir::{
        self, AbiParam, ArgumentPurpose, InstBuilder, MemFlagsData, Signature, StackSlotData,
        StackSlotKind, TrapCode, Type, Value, condcodes::IntCC, types,
    },
    isa::{self, CallConv},
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch};
use cranelift_module::{DataDescription, FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use hir_def::{
    AdtId, FunctionId, HasModule, VariantId,
    layout::{BackendRepr, Primitive, Scalar, TagEncoding, Variants},
};
use la_arena::ArenaMap;
use rustc_abi::{Integer, Size};
use rustc_hash::FxHashMap;
use rustc_type_ir::inherent::{IntoKind, SliceLike};
use triomphe::Arc;

use crate::{
    CallableDefId, InferBodyId, MemoryMap, ParamEnvAndCrate,
    db::HirDatabase,
    drop::{DropGlue, has_drop_glue},
    layout::{Layout, RustcEnumVariantIdx},
    next_solver::{
        AllocationData, ConstKind, DbInterner, GenericArgs, Ty, TyKind, TypingMode,
        infer::{DbInternerInferExt, InferCtxt},
    },
};

use super::{
    AggregateKind, BasicBlockId, BinOp, CastKind, LocalId, MirBody, Operand, OperandKind, PlaceTy,
    ProjectionElem, Rvalue, StatementKind, StoredPlace, TerminatorKind, UnOp, return_slot,
};

#[derive(Debug)]
pub enum CodegenError {
    /// The function uses something this backend can't compile yet; fall back to rustc.
    Unsupported(String),
    /// MIR lowering or layout failed in RA.
    Frontend(String),
    Backend(String),
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodegenError::Unsupported(it) => write!(f, "unsupported: {it}"),
            CodegenError::Frontend(it) => write!(f, "frontend: {it}"),
            CodegenError::Backend(it) => write!(f, "backend: {it}"),
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
    /// `core::panicking::panic_const::panic_const_{add,sub,mul,...}_overflow`
    fn overflow_panic(&self, op: OverflowOp) -> Option<String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverflowOp {
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

pub struct ObjectCodegen<'a, 'db> {
    db: &'db dyn HirDatabase,
    symbols: &'a dyn SymbolResolver,
    module: ObjectModule,
    imports: FxHashMap<String, FuncId>,
    builder_ctx: FunctionBuilderContext,
    location_counter: u32,
}

impl<'a, 'db> ObjectCodegen<'a, 'db> {
    pub fn new(db: &'db dyn HirDatabase, symbols: &'a dyn SymbolResolver) -> Result<Self> {
        let backend = |e: &dyn fmt::Display| CodegenError::Backend(e.to_string());
        let mut flags = settings::builder();
        flags.set("is_pic", "true").map_err(|e| backend(&e))?;
        flags.set("opt_level", "none").map_err(|e| backend(&e))?;
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
            builder_ctx: FunctionBuilderContext::new(),
            location_counter: 0,
        })
    }

    pub fn finish(self) -> Result<Vec<u8>> {
        self.module.finish().emit().map_err(|e| CodegenError::Backend(e.to_string()))
    }

    /// Compiles a non-generic function and exports it under its rustc symbol name.
    pub fn define_function(&mut self, func: FunctionId) -> Result<String> {
        let db = self.db;
        let owner: InferBodyId<'db> = func.into();
        let body =
            db.mir_body(owner).map_err(|e| CodegenError::Frontend(format!("mir: {e:?}")))?;
        let Some(symbol) = self.symbols.function(func) else {
            unsupported!("no rustc symbol for function");
        };
        let krate = owner.module(db).krate(db);
        let interner = DbInterner::new_with(db, krate);
        let env = ParamEnvAndCrate {
            param_env: db.trait_environment(owner.generic_def(db)),
            krate,
        };
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
                ret_mode: PassMode::Ignore,
            };
            fx.codegen().map(|()| {
                fx.builder.seal_all_blocks();
                let config = fx.cg.module.target_config();
                fx.builder.finalize(config);
            })
        };
        // A failed function leaves the builder context dirty.
        self.builder_ctx = if result.is_ok() { builder_ctx } else { FunctionBuilderContext::new() };
        result?;
        let id = self
            .module
            .declare_function(&symbol, Linkage::Export, &ctx.func.signature)
            .map_err(|e| CodegenError::Backend(e.to_string()))?;
        self.module
            .define_function(id, &mut ctx)
            .map_err(|e| CodegenError::Backend(format!("{e:?}")))?;
        Ok(symbol)
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

#[derive(Clone, Copy)]
struct LocalSlot {
    addr: Value,
}

struct FunctionCx<'cx, 'a, 'db> {
    cg: &'cx mut ObjectCodegen<'a, 'db>,
    body: &'db MirBody<'db>,
    env: ParamEnvAndCrate<'db>,
    infcx: InferCtxt<'db>,
    interner: DbInterner<'db>,
    builder: FunctionBuilder<'cx>,
    locals: ArenaMap<LocalId, LocalSlot>,
    blocks: ArenaMap<BasicBlockId, ir::Block>,
    ret_mode: PassMode,
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
            Primitive::Float(_) => unsupported!("float scalars"),
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
                PassMode::Pair(self.scalar(a)?, self.scalar(b)?, offset)
            }
            BackendRepr::Memory { sized: true } => {
                let max = if is_ret { 16 } else { 8 };
                match layout.size.bytes() {
                    size if size > max => PassMode::Indirect,
                    1 => PassMode::Cast(types::I8),
                    2 => PassMode::Cast(types::I16),
                    4 => PassMode::Cast(types::I32),
                    8 => PassMode::Cast(types::I64),
                    16 => PassMode::Cast(types::I128),
                    size => unsupported!("aggregate of {size} bytes passed in a register"),
                }
            }
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

    fn stack_slot(&mut self, layout: &Layout) -> Value {
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            layout.size.bytes() as u32,
            layout.align.abi.bytes().trailing_zeros() as u8,
        ));
        self.builder.ins().stack_addr(PTR, slot, 0)
    }

    fn codegen(&mut self) -> Result<()> {
        let body = self.body;
        let ret_layout = self.layout(body.locals[return_slot()].ty.as_ref())?;
        self.ret_mode = self.pass_mode(&ret_layout, true)?;
        let param_layouts = body
            .param_locals
            .iter()
            .map(|&l| self.layout(body.locals[l].ty.as_ref()))
            .collect::<Result<Vec<_>>>()?;
        let param_modes =
            param_layouts.iter().map(|l| self.pass_mode(l, false)).collect::<Result<Vec<_>>>()?;
        self.builder.func.signature = self.signature(&param_modes, self.ret_mode);

        let entry = self.builder.create_block();
        self.builder.append_block_params_for_function_params(entry);
        self.builder.switch_to_block(entry);
        let mut incoming = self.builder.block_params(entry).to_vec().into_iter();

        if let PassMode::Indirect = self.ret_mode {
            let addr = incoming.next().expect("sret param");
            self.locals.insert(return_slot(), LocalSlot { addr });
        }
        for ((&local, layout), mode) in body.param_locals.iter().zip(&param_layouts).zip(param_modes)
        {
            let addr = match mode {
                PassMode::Indirect => incoming.next().expect("indirect param"),
                _ => self.stack_slot(layout),
            };
            match mode {
                PassMode::Ignore | PassMode::Indirect => (),
                PassMode::Direct(_) | PassMode::Cast(_) => {
                    let value = incoming.next().expect("direct param");
                    self.builder.ins().store(MEM, value, addr, 0);
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
        for (id, _) in body.basic_blocks.iter() {
            let block = self.builder.create_block();
            self.blocks.insert(id, block);
        }
        self.builder.ins().jump(self.blocks[body.start_block], &[]);

        for (id, block) in body.basic_blocks.iter() {
            self.builder.switch_to_block(self.blocks[id]);
            for statement in &block.statements {
                match &statement.kind {
                    StatementKind::Assign(place, rvalue) => self.assign(place, rvalue)?,
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

    fn place(&mut self, place: &StoredPlace) -> Result<(Value, PlaceTy<'db>)> {
        let mut addr = self.locals[place.local].addr;
        let mut ty = PlaceTy::from_ty(self.body.locals[place.local].ty.as_ref());
        for proj in place.projection.as_slice() {
            let prev = ty;
            ty = ty.projection_ty(&self.infcx, proj, self.env.param_env);
            match proj {
                ProjectionElem::Deref => {
                    if !matches!(self.layout(prev.ty)?.backend_repr, BackendRepr::Scalar(_)) {
                        unsupported!("deref of fat pointer {:?}", prev.ty);
                    }
                    addr = self.builder.ins().load(PTR, MEM, addr, 0);
                }
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
                ProjectionElem::Index(_)
                | ProjectionElem::ConstantIndex { .. }
                | ProjectionElem::Subslice { .. } => unsupported!("projection {proj:?}"),
            }
        }
        Ok((addr, ty))
    }

    fn operand_ty(&mut self, operand: &Operand) -> Result<Ty<'db>> {
        Ok(match &operand.kind {
            OperandKind::Copy(place) | OperandKind::Move(place) => self.place(place)?.1.ty,
            OperandKind::Constant { ty, .. } => ty.as_ref(),
            OperandKind::Allocation { allocation } => allocation.as_ref().ty,
            OperandKind::Static(_) => unsupported!("statics"),
        })
    }

    /// Address of memory holding the operand's value.
    fn operand(&mut self, operand: &Operand) -> Result<(Value, Ty<'db>)> {
        match &operand.kind {
            OperandKind::Copy(place) | OperandKind::Move(place) => {
                let (addr, ty) = self.place(place)?;
                Ok((addr, ty.ty))
            }
            OperandKind::Constant { konst, ty } => {
                let ty = ty.as_ref();
                let ConstKind::Value(value) = konst.as_ref().kind() else {
                    unsupported!("unevaluated const");
                };
                let layout = self.layout(ty)?;
                let scalar = self.scalar_of(ty)?;
                let bits = match ty.kind() {
                    TyKind::Bool | TyKind::Char | TyKind::Int(_) | TyKind::Uint(_) => {
                        value.value.inner().to_leaf().to_bits(layout.size)
                    }
                    _ => unsupported!("constant of type {ty:?}"),
                };
                let addr = self.stack_slot(&layout);
                let value = self.iconst(scalar.ty, bits);
                self.builder.ins().store(MEM, value, addr, 0);
                Ok((addr, ty))
            }
            OperandKind::Allocation { allocation } => {
                let allocation = allocation.as_ref();
                let AllocationData { ty, memory, memory_map } = &*allocation;
                let ty = *ty;
                if !matches!(memory_map, MemoryMap::Empty) {
                    unsupported!("constant with relocations");
                }
                let layout = self.layout(ty)?;
                if layout.size.bytes_usize() != memory.len() {
                    unsupported!("constant whose size differs from its layout");
                }
                let addr = self.stack_slot(&layout);
                for (offset, byte) in memory.iter().enumerate() {
                    let byte = self.builder.ins().iconst(types::I8, i64::from(*byte));
                    self.builder.ins().store(MEM, byte, addr, offset as i32);
                }
                Ok((addr, ty))
            }
            OperandKind::Static(_) => unsupported!("statics"),
        }
    }

    fn iconst(&mut self, ty: Type, bits: u128) -> Value {
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
            Rvalue::Ref(_, place) => {
                let (addr, ty) = self.place(place)?;
                if self.layout(ty.ty)?.is_unsized() {
                    unsupported!("reference to unsized place");
                }
                store(self, addr);
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
                    UnOp::Neg => self.builder.ins().ineg(value),
                };
                store(self, value);
            }
            Rvalue::Cast(kind, operand, target) => {
                let to = self.scalar_of(target.as_ref())?;
                let (value, from) = self.load_scalar(operand)?;
                let value = match kind {
                    CastKind::IntToInt
                    | CastKind::PtrToPtr
                    | CastKind::PointerExposeAddress
                    | CastKind::PointerFromExposedAddress => {
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
                    _ => unsupported!("cast {kind:?}"),
                };
                store(self, value);
            }
            Rvalue::Aggregate(kind, operands) => {
                let layout = match kind {
                    AggregateKind::Tuple(ty) => self.layout(ty.as_ref())?,
                    AggregateKind::Adt(variant, args) => {
                        self.variant_layout(dst, *variant, args.as_ref())?
                    }
                    _ => unsupported!("aggregate {kind:?}"),
                };
                self.write_fields(dst, &layout, operands)?;
            }
            _ => unsupported!("rvalue {rvalue:?}"),
        }
        let _ = dst_ty;
        Ok(())
    }

    /// Layout whose `fields` describe `variant`. Writes the tag for multi-variant enums.
    fn variant_layout(
        &mut self,
        dst: Value,
        variant: VariantId,
        args: GenericArgs<'db>,
    ) -> Result<Arc<Layout>> {
        let adt: AdtId = variant.adt_id(self.cg.db);
        let layout = self.layout(Ty::new_adt(self.interner, adt, args))?;
        match &layout.variants {
            Variants::Single { .. } | Variants::Empty => Ok(layout),
            Variants::Multiple { tag, tag_encoding: TagEncoding::Direct, variants, .. } => {
                let VariantId::EnumVariantId(variant) = variant else {
                    unsupported!("multi-variant non-enum");
                };
                let index = variant.index(self.cg.db);
                // FIXME: explicit discriminants need const eval; the spike assumes `index == discr`.
                let tag_ty = self.scalar(*tag)?.ty;
                let value = self.iconst(tag_ty, index as u128);
                let offset = layout.fields.offset(0).bytes() as i32;
                self.builder.ins().store(MEM, value, dst, offset);
                Ok(Arc::new(variants[RustcEnumVariantIdx(index)].clone()))
            }
            Variants::Multiple { .. } => unsupported!("niche-encoded enum construction"),
        }
    }

    fn write_fields(&mut self, dst: Value, layout: &Layout, operands: &[Operand]) -> Result<()> {
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
        while let TyKind::Ref(_, inner, _) = ty.kind() {
            ty = inner;
            l_addr = self.builder.ins().load(PTR, MEM, l_addr, 0);
        }
        while let TyKind::Ref(_, inner, _) = r_ty.kind() {
            r_ty = inner;
            r_addr = self.builder.ins().load(PTR, MEM, r_addr, 0);
        }
        let scalar = self.scalar_of(ty)?;
        let r_scalar = self.scalar_of(r_ty)?;
        let l = self.builder.ins().load(scalar.ty, MEM, l_addr, 0);
        let r = self.builder.ins().load(r_scalar.ty, MEM, r_addr, 0);
        let signed = scalar.signed;
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
                let overflow =
                    self.builder.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, r, bits);
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

    /// Branches to a call of rustc's `panic_const_*` when `cond` is set.
    fn panic_if(&mut self, cond: Value, op: OverflowOp) -> Result<()> {
        let Some(symbol) = self.cg.symbols.overflow_panic(op) else {
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
                let (value, _) = self.load_scalar(discr)?;
                let mut switch = Switch::new();
                for (case, target) in targets.iter() {
                    switch.set_entry(case, self.blocks[target]);
                }
                switch.emit(&mut self.builder, value, self.blocks[targets.otherwise()]);
            }
            TerminatorKind::Return => {
                let addr = self.locals[return_slot()].addr;
                let mut load =
                    |ty, offset| self.builder.ins().load(ty, MEM, addr, offset);
                let values = match self.ret_mode {
                    PassMode::Ignore | PassMode::Indirect => vec![],
                    PassMode::Direct(s) => vec![load(s.ty, 0)],
                    PassMode::Cast(ty) => vec![load(ty, 0)],
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
                let ty = self.place(place)?.1.ty;
                if has_drop_glue(&self.infcx, ty, self.env.param_env) != DropGlue::None {
                    unsupported!("drop glue for {ty:?}");
                }
                self.builder.ins().jump(self.blocks[*target], &[]);
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
        let TyKind::FnDef(def, generic_args) = self.operand_ty(func)?.kind() else {
            unsupported!("indirect call");
        };
        let func = match def.0 {
            CallableDefId::FunctionId(func) => func,
            CallableDefId::StructId(id) => {
                let layout = self.variant_layout(dst, id.into(), generic_args)?;
                self.write_fields(dst, &layout, args)?;
                return self.jump_after_call(target);
            }
            CallableDefId::EnumVariantId(id) => {
                let layout = self.variant_layout(dst, id.into(), generic_args)?;
                self.write_fields(dst, &layout, args)?;
                return self.jump_after_call(target);
            }
        };
        let (callee, callee_args) = self.cg.db.lookup_impl_method(self.env, func, generic_args);
        let either::Either::Left(callee) = callee else {
            unsupported!("call to builtin derive method");
        };
        if !callee_args.is_empty() {
            // Needs a mono collector and v0 mangling of generic instances.
            unsupported!("call to generic function");
        }
        let Some(symbol) = self.cg.symbols.function(callee) else {
            unsupported!("call to function without a rustc symbol");
        };

        let ret_layout = self.layout(dst_ty.ty)?;
        let ret_mode = self.pass_mode(&ret_layout, true)?;
        let mut arg_modes = Vec::with_capacity(args.len());
        let mut values = Vec::new();
        if let PassMode::Indirect = ret_mode {
            values.push(dst);
        }
        for arg in args {
            let (addr, ty) = self.operand(arg)?;
            let layout = self.layout(ty)?;
            let mode = self.pass_mode(&layout, false)?;
            arg_modes.push(mode);
            let mut load =
                |ty, offset| self.builder.ins().load(ty, MEM, addr, offset);
            match mode {
                PassMode::Ignore => (),
                PassMode::Direct(s) => values.push(load(s.ty, 0)),
                PassMode::Cast(ty) => values.push(load(ty, 0)),
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
        let callee = self.cg.import(&symbol, &sig)?;
        let callee = self.cg.module.declare_func_in_func(callee, self.builder.func);
        let call = self.builder.ins().call(callee, &values);
        let results = self.builder.inst_results(call).to_vec();
        match ret_mode {
            PassMode::Ignore | PassMode::Indirect => (),
            PassMode::Direct(_) | PassMode::Cast(_) => {
                self.builder.ins().store(MEM, results[0], dst, 0);
            }
            PassMode::Pair(_, _, offset) => {
                self.builder.ins().store(MEM, results[0], dst, 0);
                self.builder.ins().store(
                    MEM,
                    results[1],
                    dst,
                    offset.bytes() as i32,
                );
            }
        }
        self.jump_after_call(target)
    }

    fn jump_after_call(&mut self, target: Option<BasicBlockId>) -> Result<()> {
        match target {
            Some(target) => self.builder.ins().jump(self.blocks[target], &[]),
            None => self.builder.ins().trap(TrapCode::user(2).expect("non-zero trap code")),
        };
        Ok(())
    }
}
