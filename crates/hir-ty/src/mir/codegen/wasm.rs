//! Lowers one function of Cranelift IR to a WebAssembly function body, for a build of the
//! compiler that runs where native code cannot: in a browser.
//!
//! Cranelift has no WebAssembly backend, so this takes the IR the rest of codegen builds and
//! writes wasm from it directly. The target is `wasm64` (the memory64 proposal): pointers are
//! 64 bits wide there, as everything that builds the IR assumes.
//!
//! What comes out is not a module. A [`WasmFunction`] names the functions and data it uses
//! through relocations, like machine code does, and a linker gives them places:
//!
//! - every function has a slot in one function table, and every call goes through the table,
//!   so a function is replaced by writing its slot;
//! - the address of a function is its slot, the address of data is where the linker put it
//!   in linear memory;
//! - stack slots live on a stack in linear memory, whose pointer is global 0.
//!
//! Unwinding is not lowered: the exceptional edges of `try_call` are dropped, so a panic that
//! would unwind has to abort.

use cranelift_codegen::{
    entity::{EntityRef, SecondaryMap},
    ir::{
        self, AtomicRmwOp, Block, BlockArg, BlockCall, ExternalName, Function, GlobalValueData,
        InstructionData, LibCall, Opcode, Signature, Type, Value,
        condcodes::{FloatCC, IntCC},
        types,
    },
};
use cranelift_module::{ModuleReloc, ModuleRelocTarget};
use serde_derive::{Deserialize, Serialize};
use wasm_encoder::{BlockType, Encode, Instruction as I, MemArg, ValType};

/// The index of the global that holds the stack pointer in every module the linker writes.
pub const STACK_POINTER: u32 = 0;

/// How many bytes a relocated `i64.const` operand takes: a LEB128 padded to full length.
pub const ADDRESS_LEN: usize = 10;
/// How many bytes a type index takes: a LEB128 padded to full length.
pub const TYPE_INDEX_LEN: usize = 5;

/// A value type, as its byte in the binary format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum WasmType {
    I32 = 0x7f,
    I64 = 0x7e,
    F32 = 0x7d,
    F64 = 0x7c,
}

impl From<WasmType> for ValType {
    fn from(ty: WasmType) -> ValType {
        match ty {
            WasmType::I32 => ValType::I32,
            WasmType::I64 => ValType::I64,
            WasmType::F32 => ValType::F32,
            WasmType::F64 => ValType::F64,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WasmSignature {
    pub params: Vec<WasmType>,
    pub results: Vec<WasmType>,
}

impl WasmSignature {
    pub fn of(sig: &Signature) -> Result<WasmSignature, String> {
        let flatten = |params: &[ir::AbiParam]| {
            let mut flat = Vec::new();
            for param in params {
                match Class::of(param.value_type)? {
                    Class::Pair => flat.extend([WasmType::I64, WasmType::I64]),
                    class => flat.push(class.wasm()),
                }
            }
            Ok::<_, String>(flat)
        };
        Ok(WasmSignature { params: flatten(&sig.params)?, results: flatten(&sig.returns)? })
    }
}

/// A function body with what a linker has to fill in.
#[derive(Debug, Serialize, Deserialize)]
pub struct WasmFunction {
    pub signature: WasmSignature,
    /// The signatures of the indirect calls in the body.
    pub types: Vec<WasmSignature>,
    /// Where the body has a type index to fill in, and which of `types` it is. Each place is
    /// [`TYPE_INDEX_LEN`] bytes.
    pub type_fixups: Vec<(u32, u32)>,
    /// Local declarations and instructions, as the code section has them after the size.
    pub body: Vec<u8>,
}

impl WasmFunction {
    /// The bytes that stand for the function where machine code would, and where in them
    /// `body` starts: relocations are counted from the start of these.
    pub fn encode(&self) -> Result<(Vec<u8>, u32), String> {
        let header = WasmFunction { body: Vec::new(), ..self.clone_header() };
        let header = postcard::to_allocvec(&header).map_err(|e| e.to_string())?;
        let mut bytes = (header.len() as u32).to_le_bytes().to_vec();
        bytes.extend(header);
        let start = bytes.len() as u32;
        bytes.extend(&self.body);
        Ok((bytes, start))
    }

    /// Reads what [`Self::encode`] wrote. The second value is where the body starts in
    /// `bytes`.
    pub fn decode(bytes: &[u8]) -> Result<(WasmFunction, usize), String> {
        let len = bytes.first_chunk::<4>().ok_or("truncated function")?;
        let start = 4 + u32::from_le_bytes(*len) as usize;
        let header = bytes.get(4..start).ok_or("truncated function header")?;
        let mut function: WasmFunction = postcard::from_bytes(header).map_err(|e| e.to_string())?;
        function.body = bytes[start..].to_vec();
        Ok((function, start))
    }

    fn clone_header(&self) -> WasmFunction {
        WasmFunction {
            signature: self.signature.clone(),
            types: self.types.clone(),
            type_fixups: self.type_fixups.clone(),
            body: Vec::new(),
        }
    }
}

/// Lowers `func`. The relocations are relative to the start of the body.
pub(super) fn lower(func: &Function) -> Result<(WasmFunction, Vec<ModuleReloc>), String> {
    Lower::new(func)?.run()
}

/// How a Cranelift type is held in wasm locals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// `i8`, `i16` and `i32` in an `i32`, the narrow ones zero-extended.
    Word(u8),
    Long,
    /// `i128` in two `i64`, low half first.
    Pair,
    F32,
    F64,
}

impl Class {
    fn of(ty: Type) -> Result<Class, String> {
        Ok(match ty {
            types::I8 => Class::Word(8),
            types::I16 => Class::Word(16),
            types::I32 => Class::Word(32),
            types::I64 => Class::Long,
            types::I128 => Class::Pair,
            types::F32 => Class::F32,
            types::F64 => Class::F64,
            _ => return Err(format!("wasm: type {ty}")),
        })
    }

    fn wasm(self) -> WasmType {
        match self {
            Class::Word(_) => WasmType::I32,
            Class::Long | Class::Pair => WasmType::I64,
            Class::F32 => WasmType::F32,
            Class::F64 => WasmType::F64,
        }
    }

    fn narrow(self) -> Option<u8> {
        match self {
            Class::Word(bits @ (8 | 16)) => Some(bits),
            _ => None,
        }
    }

    fn is_long(self) -> bool {
        self == Class::Long
    }
}

/// An integer operation that `i32` and `i64` both have.
#[derive(Clone, Copy)]
enum Op {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Shl,
    ShrU,
    ShrS,
    Rotl,
    Rotr,
    DivU,
    DivS,
    RemU,
    RemS,
    Eq,
    Ne,
    LtU,
    LtS,
    GtU,
    GtS,
    LeU,
    LeS,
    GeU,
    GeS,
    Popcnt,
}

impl Op {
    fn compare(cond: IntCC) -> Op {
        match cond {
            IntCC::Equal => Op::Eq,
            IntCC::NotEqual => Op::Ne,
            IntCC::SignedLessThan => Op::LtS,
            IntCC::SignedGreaterThanOrEqual => Op::GeS,
            IntCC::SignedGreaterThan => Op::GtS,
            IntCC::SignedLessThanOrEqual => Op::LeS,
            IntCC::UnsignedLessThan => Op::LtU,
            IntCC::UnsignedGreaterThanOrEqual => Op::GeU,
            IntCC::UnsignedGreaterThan => Op::GtU,
            IntCC::UnsignedLessThanOrEqual => Op::LeU,
        }
    }

    fn is_signed(self) -> bool {
        matches!(self, Op::ShrS | Op::DivS | Op::RemS | Op::LtS | Op::GtS | Op::LeS | Op::GeS)
    }

    /// The comparison without its `or equal`, and unsigned: what decides between two wide
    /// integers whose high halves are equal.
    fn strict_unsigned(self) -> Op {
        match self {
            Op::LtS | Op::LtU | Op::LeS | Op::LeU => Op::LtU,
            _ => Op::GtU,
        }
    }

    fn strict(self) -> Op {
        match self {
            Op::LeS => Op::LtS,
            Op::LeU => Op::LtU,
            Op::GeS => Op::GtS,
            Op::GeU => Op::GtU,
            other => other,
        }
    }

    fn or_equal(self) -> bool {
        matches!(self, Op::LeS | Op::LeU | Op::GeS | Op::GeU)
    }

    fn instruction(self, long: bool) -> I<'static> {
        match (self, long) {
            (Op::Add, false) => I::I32Add,
            (Op::Add, true) => I::I64Add,
            (Op::Sub, false) => I::I32Sub,
            (Op::Sub, true) => I::I64Sub,
            (Op::Mul, false) => I::I32Mul,
            (Op::Mul, true) => I::I64Mul,
            (Op::And, false) => I::I32And,
            (Op::And, true) => I::I64And,
            (Op::Or, false) => I::I32Or,
            (Op::Or, true) => I::I64Or,
            (Op::Xor, false) => I::I32Xor,
            (Op::Xor, true) => I::I64Xor,
            (Op::Shl, false) => I::I32Shl,
            (Op::Shl, true) => I::I64Shl,
            (Op::ShrU, false) => I::I32ShrU,
            (Op::ShrU, true) => I::I64ShrU,
            (Op::ShrS, false) => I::I32ShrS,
            (Op::ShrS, true) => I::I64ShrS,
            (Op::Rotl, false) => I::I32Rotl,
            (Op::Rotl, true) => I::I64Rotl,
            (Op::Rotr, false) => I::I32Rotr,
            (Op::Rotr, true) => I::I64Rotr,
            (Op::DivU, false) => I::I32DivU,
            (Op::DivU, true) => I::I64DivU,
            (Op::DivS, false) => I::I32DivS,
            (Op::DivS, true) => I::I64DivS,
            (Op::RemU, false) => I::I32RemU,
            (Op::RemU, true) => I::I64RemU,
            (Op::RemS, false) => I::I32RemS,
            (Op::RemS, true) => I::I64RemS,
            (Op::Eq, false) => I::I32Eq,
            (Op::Eq, true) => I::I64Eq,
            (Op::Ne, false) => I::I32Ne,
            (Op::Ne, true) => I::I64Ne,
            (Op::LtU, false) => I::I32LtU,
            (Op::LtU, true) => I::I64LtU,
            (Op::LtS, false) => I::I32LtS,
            (Op::LtS, true) => I::I64LtS,
            (Op::GtU, false) => I::I32GtU,
            (Op::GtU, true) => I::I64GtU,
            (Op::GtS, false) => I::I32GtS,
            (Op::GtS, true) => I::I64GtS,
            (Op::LeU, false) => I::I32LeU,
            (Op::LeU, true) => I::I64LeU,
            (Op::LeS, false) => I::I32LeS,
            (Op::LeS, true) => I::I64LeS,
            (Op::GeU, false) => I::I32GeU,
            (Op::GeU, true) => I::I64GeU,
            (Op::GeS, false) => I::I32GeS,
            (Op::GeS, true) => I::I64GeS,
            (Op::Popcnt, false) => I::I32Popcnt,
            (Op::Popcnt, true) => I::I64Popcnt,
        }
    }
}

struct Lower<'a> {
    func: &'a Function,
    signature: WasmSignature,
    code: Vec<u8>,
    /// The locals after the parameters.
    locals: Vec<ValType>,
    param_count: u32,
    /// The first local of each value, plus one.
    values: SecondaryMap<Value, u32>,
    /// The place of each block in the layout, plus one.
    order: SecondaryMap<Block, u32>,
    block_count: u32,
    /// The local that says which block runs next, when a block jumps backwards.
    next_block: Option<u32>,
    /// The local that holds the address of the stack slots, and their size together.
    frame: Option<(u32, u64)>,
    slot_offsets: Vec<u64>,
    relocs: Vec<ModuleReloc>,
    types: Vec<WasmSignature>,
    type_fixups: Vec<(u32, u32)>,
}

const MEMORY: u32 = 0;

fn mem(offset: u64) -> MemArg {
    MemArg { offset, align: 0, memory_index: MEMORY }
}

impl<'a> Lower<'a> {
    fn new(func: &'a Function) -> Result<Self, String> {
        let signature = WasmSignature::of(&func.signature)?;
        let mut order = SecondaryMap::new();
        let mut block_count = 0;
        for block in func.layout.blocks() {
            block_count += 1;
            order[block] = block_count;
        }
        let mut slot_offsets = Vec::new();
        let mut size = 0u64;
        for (_, slot) in func.sized_stack_slots.iter() {
            let align = 1u64 << slot.align_shift;
            size = size.next_multiple_of(align);
            slot_offsets.push(size);
            size += u64::from(slot.size);
        }
        if func.dynamic_stack_slots.iter().next().is_some() {
            return Err("wasm: dynamic stack slot".to_owned());
        }
        let mut this = Lower {
            func,
            param_count: signature.params.len() as u32,
            signature,
            code: Vec::new(),
            locals: Vec::new(),
            values: SecondaryMap::new(),
            order,
            block_count,
            next_block: None,
            frame: None,
            slot_offsets,
            relocs: Vec::new(),
            types: Vec::new(),
            type_fixups: Vec::new(),
        };
        if !this.slot_offsets.is_empty() {
            let frame = this.temp(ValType::I64);
            this.frame = Some((frame, size.next_multiple_of(16)));
        }
        Ok(this)
    }

    fn run(mut self) -> Result<(WasmFunction, Vec<ModuleReloc>), String> {
        let func = self.func;
        let Some(entry) = func.layout.entry_block() else {
            return Err("wasm: function without blocks".to_owned());
        };
        // The parameters of the entry block are the parameters of the function.
        let mut local = 0;
        for &param in func.dfg.block_params(entry) {
            self.values[param] = local + 1;
            let class = self.class(param)?;
            if let Some(bits) = class.narrow() {
                // Whoever calls from outside may leave anything in the upper bits.
                self.i(I::LocalGet(local));
                self.mask(bits);
                self.i(I::LocalSet(local));
            }
            local += if class == Class::Pair { 2 } else { 1 };
        }
        if local != self.param_count {
            return Err("wasm: entry block and signature disagree".to_owned());
        }
        if let Some((frame, size)) = self.frame {
            self.i(I::GlobalGet(STACK_POINTER));
            self.i(I::I64Const(size as i64));
            self.i(I::I64Sub);
            self.i(I::LocalTee(frame));
            self.i(I::GlobalSet(STACK_POINTER));
        }

        // Blocks are laid out one after the other, each inside a wasm block that ends where
        // it starts, so a jump forwards is a `br` out of the blocks in between. A jump
        // backwards sets which block is next and goes around a loop that holds all of them,
        // where a `br_table` finds the block. That is right for any control flow graph,
        // reducible or not, and costs only on the edges that go backwards.
        let backwards = func.layout.blocks().any(|block| {
            let from = self.order[block];
            func.layout.last_inst(block).is_some_and(|last| {
                func.dfg.insts[last]
                    .branch_destination(&func.dfg.jump_tables, &func.dfg.exception_tables)
                    .iter()
                    .any(|call| self.order[call.block(&func.dfg.value_lists)] <= from)
            })
        });
        if backwards {
            self.next_block = Some(self.temp(ValType::I32));
            self.i(I::Loop(BlockType::Empty));
        }
        for _ in 0..self.block_count {
            self.i(I::Block(BlockType::Empty));
        }
        if let Some(next) = self.next_block {
            self.i(I::LocalGet(next));
            let targets: Vec<u32> = (0..self.block_count).collect();
            self.i(I::BrTable(targets.into(), 0));
        }
        for block in func.layout.blocks() {
            self.i(I::End);
            for inst in func.layout.block_insts(block) {
                self.inst(block, inst)?;
            }
        }
        if backwards {
            self.i(I::End);
        }
        self.i(I::Unreachable);
        self.i(I::End);

        let mut body = Vec::new();
        // Runs of the same type, as the format wants them.
        let mut runs: Vec<(u32, ValType)> = Vec::new();
        for &ty in &self.locals {
            match runs.last_mut() {
                Some((count, last)) if *last == ty => *count += 1,
                _ => runs.push((1, ty)),
            }
        }
        (runs.len() as u32).encode(&mut body);
        for (count, ty) in runs {
            count.encode(&mut body);
            ty.encode(&mut body);
        }
        let start = body.len() as u32;
        body.extend(self.code);
        for reloc in &mut self.relocs {
            reloc.offset += start;
        }
        for (offset, _) in &mut self.type_fixups {
            *offset += start;
        }
        let function = WasmFunction {
            signature: self.signature,
            types: self.types,
            type_fixups: self.type_fixups,
            body,
        };
        Ok((function, self.relocs))
    }

    fn i(&mut self, instruction: I<'_>) {
        instruction.encode(&mut self.code);
    }

    fn temp(&mut self, ty: ValType) -> u32 {
        self.locals.push(ty);
        self.param_count + self.locals.len() as u32 - 1
    }

    fn ty(&self, value: Value) -> Type {
        self.func.dfg.value_type(value)
    }

    fn class(&self, value: Value) -> Result<Class, String> {
        Class::of(self.ty(value))
    }

    /// The first local of a value.
    fn local(&mut self, value: Value) -> Result<u32, String> {
        let value = self.func.dfg.resolve_aliases(value);
        if self.values[value] != 0 {
            return Ok(self.values[value] - 1);
        }
        let class = self.class(value)?;
        let local = self.temp(class.wasm().into());
        if class == Class::Pair {
            self.temp(ValType::I64);
        }
        self.values[value] = local + 1;
        Ok(local)
    }

    /// Pushes a value: both halves of a wide integer, the low one first.
    fn get(&mut self, value: Value) -> Result<(), String> {
        let local = self.local(value)?;
        self.i(I::LocalGet(local));
        if self.class(value)? == Class::Pair {
            self.i(I::LocalGet(local + 1));
        }
        Ok(())
    }

    /// Pops a value pushed the way [`Self::get`] pushes.
    fn set(&mut self, value: Value) -> Result<(), String> {
        let local = self.local(value)?;
        if self.class(value)? == Class::Pair {
            self.i(I::LocalSet(local + 1));
        }
        self.i(I::LocalSet(local));
        Ok(())
    }

    fn get_low(&mut self, value: Value) -> Result<(), String> {
        let local = self.local(value)?;
        self.i(I::LocalGet(local));
        Ok(())
    }

    fn get_high(&mut self, value: Value) -> Result<(), String> {
        let local = self.local(value)? + 1;
        self.i(I::LocalGet(local));
        Ok(())
    }

    /// Pushes a narrow value sign-extended to the `i32` it is held in.
    fn get_signed(&mut self, value: Value) -> Result<(), String> {
        self.get(value)?;
        match self.class(value)?.narrow() {
            Some(8) => self.i(I::I32Extend8S),
            Some(_) => self.i(I::I32Extend16S),
            None => {}
        }
        Ok(())
    }

    fn get_for(&mut self, value: Value, signed: bool) -> Result<(), String> {
        if signed { self.get_signed(value) } else { self.get(value) }
    }

    /// Clears the bits of the `i32` on the stack that a narrow integer has no use for.
    fn mask(&mut self, bits: u8) {
        self.i(I::I32Const(((1u32 << bits) - 1) as i32));
        self.i(I::I32And);
    }

    fn mask_for(&mut self, class: Class) {
        if let Some(bits) = class.narrow() {
            self.mask(bits);
        }
    }

    fn op(&mut self, op: Op, long: bool) {
        self.i(op.instruction(long));
    }

    fn constant(&mut self, value: i64, long: bool) {
        if long {
            self.i(I::I64Const(value));
        } else {
            self.i(I::I32Const(value as i32));
        }
    }

    /// Pushes whether a value is anything but zero.
    fn truthy(&mut self, value: Value) -> Result<(), String> {
        match self.class(value)? {
            Class::Word(_) => self.get(value)?,
            Class::Long => {
                self.get(value)?;
                self.i(I::I64Const(0));
                self.i(I::I64Ne);
            }
            Class::Pair => {
                self.get(value)?;
                self.i(I::I64Or);
                self.i(I::I64Const(0));
                self.i(I::I64Ne);
            }
            Class::F32 | Class::F64 => return Err("wasm: float as a condition".to_owned()),
        }
        Ok(())
    }

    /// Pushes an integer as a shift amount for an operand that is an `i64` or is not.
    fn amount(&mut self, value: Value, long: bool) -> Result<(), String> {
        match (self.class(value)?, long) {
            (Class::Word(_), false) | (Class::Long, true) => self.get(value)?,
            (Class::Word(_), true) => {
                self.get(value)?;
                self.i(I::I64ExtendI32U);
            }
            (Class::Long, false) => {
                self.get(value)?;
                self.i(I::I32WrapI64);
            }
            (Class::Pair, _) => {
                self.get_low(value)?;
                if !long {
                    self.i(I::I32WrapI64);
                }
            }
            _ => return Err("wasm: float as a shift amount".to_owned()),
        }
        Ok(())
    }

    /// Pushes an address that a linker fills in.
    fn address(&mut self, name: &ExternalName, addend: i64) -> Result<(), String> {
        let ExternalName::User(name) = name else {
            return Err(format!("wasm: address of {name:?}"));
        };
        let name = &self.func.params.user_named_funcs()[*name];
        // `i64.const`, then the operand.
        self.code.push(0x42);
        self.relocs.push(ModuleReloc {
            offset: self.code.len() as u32,
            kind: cranelift_codegen::binemit::Reloc::Abs8,
            name: ModuleRelocTarget::user(name.namespace, name.index),
            addend,
        });
        self.code.extend([0x80; ADDRESS_LEN - 1]);
        self.code.push(0);
        Ok(())
    }

    /// Calls through the table the function whose slot is on the stack as an `i64`.
    fn call_slot(&mut self, signature: &Signature) -> Result<(), String> {
        let signature = WasmSignature::of(signature)?;
        let index = match self.types.iter().position(|it| *it == signature) {
            Some(index) => index,
            None => {
                self.types.push(signature);
                self.types.len() - 1
            }
        };
        self.i(I::I32WrapI64);
        // `call_indirect`, the type, the table.
        self.code.push(0x11);
        self.type_fixups.push((self.code.len() as u32, index as u32));
        self.code.extend([0x80; TYPE_INDEX_LEN - 1]);
        self.code.push(0);
        self.code.push(0);
        Ok(())
    }

    fn slot_address(&mut self, slot: ir::StackSlot, offset: i64) -> Result<(), String> {
        let Some((frame, _)) = self.frame else {
            return Err("wasm: stack slot without a frame".to_owned());
        };
        let offset = self.slot_offsets[slot.index()] as i64 + offset;
        self.i(I::LocalGet(frame));
        if offset != 0 {
            self.i(I::I64Const(offset));
            self.i(I::I64Add);
        }
        Ok(())
    }

    /// Pushes the address `base + offset` and gives what is left of the offset to put in the
    /// instruction.
    fn pointer(&mut self, base: Value, offset: i64) -> Result<u64, String> {
        self.get(base)?;
        if offset >= 0 {
            return Ok(offset as u64);
        }
        self.i(I::I64Const(offset));
        self.i(I::I64Add);
        Ok(0)
    }

    /// Loads `bits` of an integer held as `class` from the address on the stack.
    fn load_int(&mut self, class: Class, bits: u8, signed: bool, offset: u64) {
        let at = mem(offset);
        self.i(match (class.is_long() || class == Class::Pair, bits, signed) {
            (false, 8, false) => I::I32Load8U(at),
            (false, 8, true) => I::I32Load8S(at),
            (false, 16, false) => I::I32Load16U(at),
            (false, 16, true) => I::I32Load16S(at),
            (false, ..) => I::I32Load(at),
            (true, 8, false) => I::I64Load8U(at),
            (true, 8, true) => I::I64Load8S(at),
            (true, 16, false) => I::I64Load16U(at),
            (true, 16, true) => I::I64Load16S(at),
            (true, 32, false) => I::I64Load32U(at),
            (true, 32, true) => I::I64Load32S(at),
            (true, ..) => I::I64Load(at),
        });
    }

    /// Stores the low `bits` of the integer on the stack, held as `class`, to the address
    /// under it.
    fn store_int(&mut self, class: Class, bits: u8, offset: u64) {
        let at = mem(offset);
        self.i(match (class.is_long() || class == Class::Pair, bits) {
            (false, 8) => I::I32Store8(at),
            (false, 16) => I::I32Store16(at),
            (false, _) => I::I32Store(at),
            (true, 8) => I::I64Store8(at),
            (true, 16) => I::I64Store16(at),
            (true, 32) => I::I64Store32(at),
            (true, _) => I::I64Store(at),
        });
    }

    fn load(&mut self, result: Value, base: Value, offset: i64) -> Result<(), String> {
        let class = self.class(result)?;
        let offset = self.pointer(base, offset)?;
        match class {
            Class::Word(bits) => self.load_int(class, bits, false, offset),
            Class::Long => self.load_int(class, 64, false, offset),
            Class::Pair => {
                self.i(I::I64Load(mem(offset)));
                self.get(base)?;
                self.i(I::I64Load(mem(offset + 8)));
            }
            Class::F32 => self.i(I::F32Load(mem(offset))),
            Class::F64 => self.i(I::F64Load(mem(offset))),
        }
        self.set(result)
    }

    fn store(&mut self, value: Value, base: Value, offset: i64) -> Result<(), String> {
        let class = self.class(value)?;
        let offset = self.pointer(base, offset)?;
        match class {
            Class::Word(bits) => {
                self.get(value)?;
                self.store_int(class, bits, offset);
            }
            Class::Long => {
                self.get(value)?;
                self.i(I::I64Store(mem(offset)));
            }
            Class::Pair => {
                self.get_low(value)?;
                self.i(I::I64Store(mem(offset)));
                self.get(base)?;
                self.get_high(value)?;
                self.i(I::I64Store(mem(offset + 8)));
            }
            Class::F32 => {
                self.get(value)?;
                self.i(I::F32Store(mem(offset)));
            }
            Class::F64 => {
                self.get(value)?;
                self.i(I::F64Store(mem(offset)));
            }
        }
        Ok(())
    }

    /// Leaves the block for another: `depth` is how many wasm blocks opened inside the
    /// block's own code the jump is in.
    fn jump(
        &mut self,
        from: Block,
        call: BlockCall,
        depth: u32,
        returned: &[u32],
    ) -> Result<(), String> {
        let func = self.func;
        let target = call.block(&func.dfg.value_lists);
        let params = func.dfg.block_params(target);
        let mut passed = 0;
        for (arg, &param) in call.args(&func.dfg.value_lists).zip(params) {
            match arg {
                BlockArg::Value(value) => self.get(value)?,
                BlockArg::TryCallRet(index) => {
                    let local = *returned.get(index as usize).ok_or("wasm: no such result")?;
                    self.i(I::LocalGet(local));
                    if self.class(param)? == Class::Pair {
                        self.i(I::LocalGet(local + 1));
                    }
                }
                BlockArg::TryCallExn(_) => return Err("wasm: exception payload".to_owned()),
            }
            passed += 1;
        }
        // All of them are on the stack before any is written: a block may pass its own
        // parameters on in another order.
        for &param in params[..passed].iter().rev() {
            self.set(param)?;
        }
        let (from, target) = (self.order[from] - 1, self.order[target] - 1);
        if target > from {
            self.i(I::Br(target - from - 1 + depth));
            return Ok(());
        }
        let Some(next) = self.next_block else {
            return Err("wasm: jump backwards without a loop".to_owned());
        };
        self.i(I::I32Const(target as i32));
        self.i(I::LocalSet(next));
        self.i(I::Br(self.block_count - 1 - from + depth));
        Ok(())
    }

    fn return_(&mut self, values: &[Value]) -> Result<(), String> {
        for &value in values {
            self.get(value)?;
        }
        if let Some((frame, size)) = self.frame {
            self.i(I::LocalGet(frame));
            self.i(I::I64Const(size as i64));
            self.i(I::I64Add);
            self.i(I::GlobalSet(STACK_POINTER));
        }
        self.i(I::Return);
        Ok(())
    }

    /// Calls a function and gives the first local of each result. `callee` is `None` when
    /// the slot to call is the first of `args`.
    fn call(
        &mut self,
        callee: Option<&ExternalName>,
        signature: &Signature,
        args: &[Value],
        results: &[Value],
    ) -> Result<(), String> {
        match callee {
            Some(ExternalName::LibCall(libcall)) => return self.libcall(*libcall, args, results),
            Some(name) => {
                for &arg in args {
                    self.get(arg)?;
                }
                self.address(name, 0)?;
            }
            None => {
                let [slot, args @ ..] = args else {
                    return Err("wasm: indirect call of nothing".to_owned());
                };
                for &arg in args {
                    self.get(arg)?;
                }
                self.get(*slot)?;
            }
        }
        self.call_slot(signature)?;
        for &result in results.iter().rev() {
            self.mask_for(self.class(result)?);
            self.set(result)?;
        }
        Ok(())
    }

    /// The memory routines Cranelift's frontend calls by name are instructions here.
    fn libcall(
        &mut self,
        libcall: LibCall,
        args: &[Value],
        results: &[Value],
    ) -> Result<(), String> {
        match (libcall, args) {
            (LibCall::Memcpy | LibCall::Memmove, &[to, from, len]) => {
                self.get(to)?;
                self.get(from)?;
                self.get(len)?;
                self.i(I::MemoryCopy { src_mem: MEMORY, dst_mem: MEMORY });
            }
            (LibCall::Memset, &[to, byte, len]) => {
                self.get(to)?;
                self.get(byte)?;
                self.get(len)?;
                self.i(I::MemoryFill(MEMORY));
            }
            _ => return Err(format!("wasm: libcall {libcall}")),
        }
        if let (&[to, ..], &[result]) = (args, results) {
            self.get(to)?;
            self.set(result)?;
        }
        Ok(())
    }

    /// Calls as [`Self::call`] does and goes on to where the call returns to. Nothing here
    /// unwinds, so the handlers are never reached.
    fn try_call(
        &mut self,
        block: Block,
        callee: Option<&ExternalName>,
        table: ir::ExceptionTable,
        args: &[Value],
    ) -> Result<(), String> {
        let func = self.func;
        let table = &func.dfg.exception_tables[table];
        let signature = &func.dfg.signatures[table.signature()];
        match callee {
            Some(name) => {
                for &arg in args {
                    self.get(arg)?;
                }
                self.address(name, 0)?;
            }
            None => {
                let [slot, args @ ..] = args else {
                    return Err("wasm: indirect call of nothing".to_owned());
                };
                for &arg in args {
                    self.get(arg)?;
                }
                self.get(*slot)?;
            }
        }
        self.call_slot(signature)?;
        let mut returned = Vec::new();
        for param in &signature.returns {
            let class = Class::of(param.value_type)?;
            let local = self.temp(class.wasm().into());
            if class == Class::Pair {
                self.temp(ValType::I64);
            }
            returned.push((local, class));
        }
        for &(local, class) in returned.iter().rev() {
            if class == Class::Pair {
                self.i(I::LocalSet(local + 1));
            }
            self.mask_for(class);
            self.i(I::LocalSet(local));
        }
        let returned: Vec<u32> = returned.into_iter().map(|it| it.0).collect();
        self.jump(block, *table.normal_return(), 0, &returned)
    }

    fn inst(&mut self, block: Block, inst: ir::Inst) -> Result<(), String> {
        let func = self.func;
        let dfg = &func.dfg;
        let results = dfg.inst_results(inst);
        let data = dfg.insts[inst];
        let opcode = data.opcode();
        match data {
            InstructionData::Jump { destination, .. } => self.jump(block, destination, 0, &[]),
            InstructionData::Brif { arg, blocks: [then, otherwise], .. } => {
                self.truthy(arg)?;
                self.i(I::If(BlockType::Empty));
                self.jump(block, then, 1, &[])?;
                self.i(I::End);
                self.jump(block, otherwise, 0, &[])
            }
            InstructionData::BranchTable { arg, table, .. } => {
                let table = &dfg.jump_tables[table];
                let cases = table.as_slice();
                // One wasm block per case to land in, where the jump proper is: a case
                // passes arguments of its own.
                for _ in 0..=cases.len() {
                    self.i(I::Block(BlockType::Empty));
                }
                match self.class(arg)? {
                    Class::Word(_) => self.get(arg)?,
                    Class::Long => {
                        // An index that does not fit goes to the default like any other
                        // that is out of range.
                        let count = cases.len() as i32;
                        self.i(I::I32Const(count));
                        self.get(arg)?;
                        self.i(I::I32WrapI64);
                        self.get(arg)?;
                        self.i(I::I64Const(i64::from(count)));
                        self.i(I::I64GeU);
                        self.i(I::Select);
                    }
                    _ => return Err("wasm: br_table on a wide value".to_owned()),
                }
                let targets: Vec<u32> = (0..cases.len() as u32).collect();
                self.i(I::BrTable(targets.into(), cases.len() as u32));
                let count = cases.len() as u32;
                for (at, &case) in cases.iter().enumerate() {
                    self.i(I::End);
                    self.jump(block, case, count - at as u32, &[])?;
                }
                self.i(I::End);
                self.jump(block, table.default_block(), 0, &[])
            }
            InstructionData::MultiAry { opcode: Opcode::Return, args } => {
                let args = args.as_slice(&dfg.value_lists);
                self.return_(args)
            }
            InstructionData::Trap { .. } => {
                self.i(I::Unreachable);
                Ok(())
            }
            InstructionData::CondTrap { arg, .. } => {
                self.truthy(arg)?;
                if opcode == Opcode::Trapz {
                    self.i(I::I32Eqz);
                }
                self.i(I::If(BlockType::Empty));
                self.i(I::Unreachable);
                self.i(I::End);
                Ok(())
            }
            InstructionData::Call { args, func_ref, .. } => {
                let callee = &dfg.ext_funcs[func_ref];
                let signature = &dfg.signatures[callee.signature];
                let args = args.as_slice(&dfg.value_lists);
                self.call(Some(&callee.name), signature, args, results)
            }
            InstructionData::CallIndirect { args, sig_ref, .. } => {
                let args = args.as_slice(&dfg.value_lists);
                self.call(None, &dfg.signatures[sig_ref], args, results)
            }
            InstructionData::TryCall { args, func_ref, exception, .. } => {
                let callee = &dfg.ext_funcs[func_ref];
                let args = args.as_slice(&dfg.value_lists);
                self.try_call(block, Some(&callee.name), exception, args)
            }
            InstructionData::TryCallIndirect { args, exception, .. } => {
                let args = args.as_slice(&dfg.value_lists);
                self.try_call(block, None, exception, args)
            }
            InstructionData::FuncAddr { func_ref, .. } => {
                self.address(&dfg.ext_funcs[func_ref].name, 0)?;
                self.set(results[0])
            }
            InstructionData::UnaryGlobalValue { global_value, .. } => {
                let GlobalValueData::Symbol { name, offset, .. } =
                    &func.global_values[global_value]
                else {
                    return Err("wasm: global value that is not a symbol".to_owned());
                };
                self.address(name, offset.bits())?;
                self.set(results[0])
            }
            InstructionData::StackAddr { stack_slot, offset, .. } => {
                self.slot_address(stack_slot, offset.into())?;
                self.set(results[0])
            }
            InstructionData::UnaryImm { imm, .. } => {
                let class = self.class(results[0])?;
                match class {
                    Class::Word(bits) => {
                        let mask = if bits == 32 { u32::MAX } else { (1 << bits) - 1 };
                        self.i(I::I32Const((imm.bits() as u32 & mask) as i32));
                    }
                    Class::Long => self.i(I::I64Const(imm.bits())),
                    _ => return Err("wasm: iconst of a wide type".to_owned()),
                }
                self.set(results[0])
            }
            InstructionData::UnaryIeee32 { imm, .. } => {
                self.i(I::F32Const(f32::from_bits(imm.bits()).into()));
                self.set(results[0])
            }
            InstructionData::UnaryIeee64 { imm, .. } => {
                self.i(I::F64Const(f64::from_bits(imm.bits()).into()));
                self.set(results[0])
            }
            InstructionData::Load { arg, offset, .. } => match opcode {
                Opcode::Load => self.load(results[0], arg, offset.into()),
                _ => {
                    let (bits, signed) = match opcode {
                        Opcode::Uload8 => (8, false),
                        Opcode::Sload8 => (8, true),
                        Opcode::Uload16 => (16, false),
                        Opcode::Sload16 => (16, true),
                        Opcode::Uload32 => (32, false),
                        Opcode::Sload32 => (32, true),
                        _ => return Err(format!("wasm: {opcode}")),
                    };
                    let class = self.class(results[0])?;
                    let offset = self.pointer(arg, offset.into())?;
                    self.load_int(class, bits, signed, offset);
                    self.mask_for(class);
                    self.set(results[0])
                }
            },
            InstructionData::Store { args: [value, base], offset, .. } => match opcode {
                Opcode::Store => self.store(value, base, offset.into()),
                _ => {
                    let bits = match opcode {
                        Opcode::Istore8 => 8,
                        Opcode::Istore16 => 16,
                        Opcode::Istore32 => 32,
                        _ => return Err(format!("wasm: {opcode}")),
                    };
                    let class = self.class(value)?;
                    let offset = self.pointer(base, offset.into())?;
                    self.get_low(value)?;
                    self.store_int(class, bits, offset);
                    Ok(())
                }
            },
            InstructionData::LoadNoOffset { arg, .. } => match opcode {
                Opcode::Bitcast => self.bitcast(results[0], arg),
                // One thread runs the code: an atomic load is a load.
                Opcode::AtomicLoad => self.load(results[0], arg, 0),
                _ => Err(format!("wasm: {opcode}")),
            },
            InstructionData::StoreNoOffset { args: [value, base], .. } => match opcode {
                Opcode::AtomicStore => self.store(value, base, 0),
                _ => Err(format!("wasm: {opcode}")),
            },
            InstructionData::AtomicRmw { args: [base, value], op, .. } => {
                self.atomic_rmw(results[0], base, value, op)
            }
            InstructionData::AtomicCas { args: [base, expected, replacement], .. } => {
                let old = results[0];
                self.load(old, base, 0)?;
                match self.class(old)? {
                    Class::Word(_) => {
                        self.get(old)?;
                        self.get(expected)?;
                        self.i(I::I32Eq);
                    }
                    Class::Long => {
                        self.get(old)?;
                        self.get(expected)?;
                        self.i(I::I64Eq);
                    }
                    _ => return Err("wasm: atomic_cas of a wide type".to_owned()),
                }
                self.i(I::If(BlockType::Empty));
                self.store(replacement, base, 0)?;
                self.i(I::End);
                Ok(())
            }
            InstructionData::IntCompare { args: [a, b], cond, .. } => {
                self.compare(Op::compare(cond), a, b)?;
                self.set(results[0])
            }
            InstructionData::FloatCompare { args: [a, b], cond, .. } => {
                self.float_compare(cond, a, b)?;
                self.set(results[0])
            }
            InstructionData::Ternary { args: [a, b, c], .. } => match opcode {
                Opcode::Select | Opcode::SelectSpectreGuard => self.select(results[0], a, b, c),
                Opcode::Bitselect => {
                    let long = match self.class(b)? {
                        Class::Word(_) => false,
                        Class::Long => true,
                        _ => return Err("wasm: bitselect of this type".to_owned()),
                    };
                    self.get(b)?;
                    self.get(a)?;
                    self.op(Op::And, long);
                    self.get(c)?;
                    self.get(a)?;
                    self.constant(-1, long);
                    self.op(Op::Xor, long);
                    self.op(Op::And, long);
                    self.op(Op::Or, long);
                    self.mask_for(self.class(b)?);
                    self.set(results[0])
                }
                _ => Err(format!("wasm: {opcode}")),
            },
            InstructionData::Unary { arg, .. } => self.unary(opcode, results, arg),
            InstructionData::Binary { args: [a, b], .. } => self.binary(opcode, results, a, b),
            InstructionData::NullAry { .. } => match opcode {
                Opcode::Nop | Opcode::Fence => Ok(()),
                _ => Err(format!("wasm: {opcode}")),
            },
            _ => Err(format!("wasm: {opcode}")),
        }
    }

    fn select(
        &mut self,
        result: Value,
        cond: Value,
        then: Value,
        otherwise: Value,
    ) -> Result<(), String> {
        if self.class(then)? == Class::Pair {
            let local = self.local(result)?;
            self.get_low(then)?;
            self.get_low(otherwise)?;
            self.truthy(cond)?;
            self.i(I::Select);
            self.get_high(then)?;
            self.get_high(otherwise)?;
            self.truthy(cond)?;
            self.i(I::Select);
            self.i(I::LocalSet(local + 1));
            self.i(I::LocalSet(local));
            return Ok(());
        }
        self.get(then)?;
        self.get(otherwise)?;
        self.truthy(cond)?;
        self.i(I::Select);
        self.set(result)
    }

    /// Pushes the `i32` that is one when the comparison holds.
    fn compare(&mut self, op: Op, a: Value, b: Value) -> Result<(), String> {
        match self.class(a)? {
            class @ (Class::Word(_) | Class::Long) => {
                self.get_for(a, op.is_signed())?;
                self.get_for(b, op.is_signed())?;
                self.op(op, class.is_long());
            }
            Class::Pair => match op {
                Op::Eq | Op::Ne => {
                    self.get_low(a)?;
                    self.get_low(b)?;
                    self.i(I::I64Xor);
                    self.get_high(a)?;
                    self.get_high(b)?;
                    self.i(I::I64Xor);
                    self.i(I::I64Or);
                    self.i(I::I64Eqz);
                    if matches!(op, Op::Ne) {
                        self.i(I::I32Eqz);
                    }
                }
                _ => {
                    // The high halves decide unless they are equal, and then the low ones
                    // do, unsigned.
                    self.get_low(a)?;
                    self.get_low(b)?;
                    self.op(
                        if op.or_equal() {
                            op.strict_unsigned_or_equal()
                        } else {
                            op.strict_unsigned()
                        },
                        true,
                    );
                    self.get_high(a)?;
                    self.get_high(b)?;
                    self.op(op.strict(), true);
                    self.get_high(a)?;
                    self.get_high(b)?;
                    self.i(I::I64Eq);
                    self.i(I::Select);
                }
            },
            _ => return Err("wasm: icmp of floats".to_owned()),
        }
        Ok(())
    }

    fn float_compare(&mut self, cond: FloatCC, a: Value, b: Value) -> Result<(), String> {
        let single = self.class(a)? == Class::F32;
        let pick = |single: bool, f32: I<'static>, f64: I<'static>| if single { f32 } else { f64 };
        let eq = pick(single, I::F32Eq, I::F64Eq);
        let ne = pick(single, I::F32Ne, I::F64Ne);
        let lt = pick(single, I::F32Lt, I::F64Lt);
        let le = pick(single, I::F32Le, I::F64Le);
        let gt = pick(single, I::F32Gt, I::F64Gt);
        let ge = pick(single, I::F32Ge, I::F64Ge);
        // Each is one comparison, or the opposite of one: an unordered pair fails every
        // comparison but `ne`.
        let (first, negate) = match cond {
            FloatCC::Equal => (eq, false),
            FloatCC::NotEqual => (ne, false),
            FloatCC::LessThan => (lt, false),
            FloatCC::LessThanOrEqual => (le, false),
            FloatCC::GreaterThan => (gt, false),
            FloatCC::GreaterThanOrEqual => (ge, false),
            FloatCC::UnorderedOrLessThan => (ge, true),
            FloatCC::UnorderedOrLessThanOrEqual => (gt, true),
            FloatCC::UnorderedOrGreaterThan => (le, true),
            FloatCC::UnorderedOrGreaterThanOrEqual => (lt, true),
            FloatCC::Ordered | FloatCC::Unordered => {
                self.get(a)?;
                self.get(a)?;
                self.i(eq.clone());
                self.get(b)?;
                self.get(b)?;
                self.i(eq);
                self.i(I::I32And);
                if cond == FloatCC::Unordered {
                    self.i(I::I32Eqz);
                }
                return Ok(());
            }
            FloatCC::OrderedNotEqual | FloatCC::UnorderedOrEqual => {
                self.get(a)?;
                self.get(b)?;
                self.i(lt);
                self.get(a)?;
                self.get(b)?;
                self.i(gt);
                self.i(I::I32Or);
                if cond == FloatCC::UnorderedOrEqual {
                    self.i(I::I32Eqz);
                }
                return Ok(());
            }
        };
        self.get(a)?;
        self.get(b)?;
        self.i(first);
        if negate {
            self.i(I::I32Eqz);
        }
        Ok(())
    }

    fn bitcast(&mut self, result: Value, arg: Value) -> Result<(), String> {
        self.get(arg)?;
        match (self.class(arg)?, self.class(result)?) {
            (Class::Word(32), Class::F32) => self.i(I::F32ReinterpretI32),
            (Class::F32, Class::Word(32)) => self.i(I::I32ReinterpretF32),
            (Class::Long, Class::F64) => self.i(I::F64ReinterpretI64),
            (Class::F64, Class::Long) => self.i(I::I64ReinterpretF64),
            (from, to) if from == to => {}
            (from, to) => return Err(format!("wasm: bitcast from {from:?} to {to:?}")),
        }
        self.set(result)
    }

    fn atomic_rmw(
        &mut self,
        old: Value,
        base: Value,
        value: Value,
        op: AtomicRmwOp,
    ) -> Result<(), String> {
        let class = self.class(old)?;
        let long = match class {
            Class::Word(_) => false,
            Class::Long => true,
            _ => return Err("wasm: atomic_rmw of a wide type".to_owned()),
        };
        self.load(old, base, 0)?;
        self.get(base)?;
        match op {
            AtomicRmwOp::Xchg => self.get(value)?,
            AtomicRmwOp::Add
            | AtomicRmwOp::Sub
            | AtomicRmwOp::And
            | AtomicRmwOp::Or
            | AtomicRmwOp::Xor => {
                self.get(old)?;
                self.get(value)?;
                self.op(
                    match op {
                        AtomicRmwOp::Add => Op::Add,
                        AtomicRmwOp::Sub => Op::Sub,
                        AtomicRmwOp::And => Op::And,
                        AtomicRmwOp::Or => Op::Or,
                        _ => Op::Xor,
                    },
                    long,
                );
            }
            AtomicRmwOp::Nand => {
                self.get(old)?;
                self.get(value)?;
                self.op(Op::And, long);
                self.constant(-1, long);
                self.op(Op::Xor, long);
            }
            AtomicRmwOp::Umin | AtomicRmwOp::Umax | AtomicRmwOp::Smin | AtomicRmwOp::Smax => {
                let pick = match op {
                    AtomicRmwOp::Umin => Op::LtU,
                    AtomicRmwOp::Umax => Op::GtU,
                    AtomicRmwOp::Smin => Op::LtS,
                    _ => Op::GtS,
                };
                self.get(old)?;
                self.get(value)?;
                self.compare(pick, old, value)?;
                self.i(I::Select);
            }
        }
        // The store keeps the low bits alone, so nothing has to be cleared.
        match class {
            Class::Word(bits) => self.store_int(class, bits, 0),
            _ => self.i(I::I64Store(mem(0))),
        }
        Ok(())
    }

    fn unary(&mut self, opcode: Opcode, results: &[Value], arg: Value) -> Result<(), String> {
        let from = self.class(arg)?;
        if opcode == Opcode::Isplit {
            self.get_low(arg)?;
            self.set(results[0])?;
            self.get_high(arg)?;
            return self.set(results[1]);
        }
        let result = results[0];
        let to = self.class(result)?;
        let single = from == Class::F32;
        let float = |single: bool, f32: I<'static>, f64: I<'static>| if single { f32 } else { f64 };
        match opcode {
            Opcode::Uextend | Opcode::Sextend => {
                let signed = opcode == Opcode::Sextend;
                match (from, to) {
                    (Class::Word(_), Class::Word(_)) => {
                        self.get_for(arg, signed)?;
                        self.mask_for(to);
                    }
                    (Class::Word(_), Class::Long | Class::Pair) => {
                        self.get_for(arg, signed)?;
                        self.i(if signed { I::I64ExtendI32S } else { I::I64ExtendI32U });
                    }
                    (Class::Long, Class::Pair) => self.get(arg)?,
                    _ => return Err(format!("wasm: extend from {from:?} to {to:?}")),
                }
                if to == Class::Pair {
                    // The low half is on the stack; the high one is its sign, or nothing.
                    let local = self.local(result)?;
                    self.i(I::LocalTee(local));
                    self.i(I::I64Const(if signed { 63 } else { 64 }));
                    self.i(if signed { I::I64ShrS } else { I::I64ShrU });
                    if !signed {
                        self.i(I::Drop);
                        self.i(I::I64Const(0));
                    }
                    self.i(I::LocalSet(local + 1));
                    return Ok(());
                }
            }
            Opcode::Ireduce => {
                self.get_low(arg)?;
                if matches!(from, Class::Long | Class::Pair) && matches!(to, Class::Word(_)) {
                    self.i(I::I32WrapI64);
                }
                self.mask_for(to);
            }
            Opcode::Ineg => match to {
                Class::Word(_) | Class::Long => {
                    self.constant(0, to.is_long());
                    self.get(arg)?;
                    self.op(Op::Sub, to.is_long());
                    self.mask_for(to);
                }
                _ => return Err("wasm: ineg of a wide type".to_owned()),
            },
            Opcode::Bnot => match to {
                Class::Word(_) | Class::Long => {
                    self.get(arg)?;
                    self.constant(-1, to.is_long());
                    self.op(Op::Xor, to.is_long());
                    self.mask_for(to);
                }
                Class::Pair => {
                    self.get_low(arg)?;
                    self.i(I::I64Const(-1));
                    self.i(I::I64Xor);
                    self.get_high(arg)?;
                    self.i(I::I64Const(-1));
                    self.i(I::I64Xor);
                }
                _ => return Err("wasm: bnot of a float".to_owned()),
            },
            Opcode::Popcnt => match to {
                Class::Word(_) | Class::Long => {
                    self.get(arg)?;
                    self.op(Op::Popcnt, to.is_long());
                }
                Class::Pair => {
                    self.get_low(arg)?;
                    self.i(I::I64Popcnt);
                    self.get_high(arg)?;
                    self.i(I::I64Popcnt);
                    self.i(I::I64Add);
                    self.i(I::I64Const(0));
                }
                _ => return Err("wasm: popcnt of a float".to_owned()),
            },
            Opcode::Clz => match to {
                Class::Word(bits) => {
                    self.get(arg)?;
                    self.i(I::I32Clz);
                    if bits < 32 {
                        self.i(I::I32Const(32 - i32::from(bits)));
                        self.i(I::I32Sub);
                    }
                }
                Class::Long => {
                    self.get(arg)?;
                    self.i(I::I64Clz);
                }
                Class::Pair => {
                    self.get_high(arg)?;
                    self.i(I::I64Clz);
                    self.get_low(arg)?;
                    self.i(I::I64Clz);
                    self.i(I::I64Const(64));
                    self.i(I::I64Add);
                    self.get_high(arg)?;
                    self.i(I::I64Const(0));
                    self.i(I::I64Ne);
                    self.i(I::Select);
                    self.i(I::I64Const(0));
                }
                _ => return Err("wasm: clz of a float".to_owned()),
            },
            Opcode::Ctz => match to {
                Class::Word(bits) => {
                    self.get(arg)?;
                    if bits < 32 {
                        // A bit past the end stops the count there.
                        self.i(I::I32Const(1 << bits));
                        self.i(I::I32Or);
                    }
                    self.i(I::I32Ctz);
                }
                Class::Long => {
                    self.get(arg)?;
                    self.i(I::I64Ctz);
                }
                Class::Pair => {
                    self.get_low(arg)?;
                    self.i(I::I64Ctz);
                    self.get_high(arg)?;
                    self.i(I::I64Ctz);
                    self.i(I::I64Const(64));
                    self.i(I::I64Add);
                    self.get_low(arg)?;
                    self.i(I::I64Const(0));
                    self.i(I::I64Ne);
                    self.i(I::Select);
                    self.i(I::I64Const(0));
                }
                _ => return Err("wasm: ctz of a float".to_owned()),
            },
            Opcode::Bswap => match to {
                Class::Word(16) => {
                    self.get(arg)?;
                    self.swap_bytes(false, 16);
                }
                Class::Word(32) => {
                    self.get(arg)?;
                    self.swap_bytes(false, 32);
                }
                Class::Long => {
                    self.get(arg)?;
                    self.swap_bytes(true, 64);
                }
                Class::Pair => {
                    self.get_high(arg)?;
                    self.swap_bytes(true, 64);
                    self.get_low(arg)?;
                    self.swap_bytes(true, 64);
                }
                _ => return Err("wasm: bswap of this type".to_owned()),
            },
            Opcode::Bitrev => match to {
                Class::Word(bits) => {
                    self.get(arg)?;
                    self.reverse_bits(false, bits);
                }
                Class::Long => {
                    self.get(arg)?;
                    self.reverse_bits(true, 64);
                }
                Class::Pair => {
                    self.get_high(arg)?;
                    self.reverse_bits(true, 64);
                    self.get_low(arg)?;
                    self.reverse_bits(true, 64);
                }
                _ => return Err("wasm: bitrev of a float".to_owned()),
            },
            Opcode::Fneg => {
                self.get(arg)?;
                self.i(float(single, I::F32Neg, I::F64Neg));
            }
            Opcode::Fabs => {
                self.get(arg)?;
                self.i(float(single, I::F32Abs, I::F64Abs));
            }
            Opcode::Sqrt => {
                self.get(arg)?;
                self.i(float(single, I::F32Sqrt, I::F64Sqrt));
            }
            Opcode::Ceil => {
                self.get(arg)?;
                self.i(float(single, I::F32Ceil, I::F64Ceil));
            }
            Opcode::Floor => {
                self.get(arg)?;
                self.i(float(single, I::F32Floor, I::F64Floor));
            }
            Opcode::Trunc => {
                self.get(arg)?;
                self.i(float(single, I::F32Trunc, I::F64Trunc));
            }
            Opcode::Nearest => {
                self.get(arg)?;
                self.i(float(single, I::F32Nearest, I::F64Nearest));
            }
            Opcode::Fpromote => {
                self.get(arg)?;
                self.i(I::F64PromoteF32);
            }
            Opcode::Fdemote => {
                self.get(arg)?;
                self.i(I::F32DemoteF64);
            }
            Opcode::FcvtFromUint | Opcode::FcvtFromSint => {
                let signed = opcode == Opcode::FcvtFromSint;
                self.get_for(arg, signed)?;
                self.i(match (from, to, signed) {
                    (Class::Word(_), Class::F32, false) => I::F32ConvertI32U,
                    (Class::Word(_), Class::F32, true) => I::F32ConvertI32S,
                    (Class::Word(_), Class::F64, false) => I::F64ConvertI32U,
                    (Class::Word(_), Class::F64, true) => I::F64ConvertI32S,
                    (Class::Long, Class::F32, false) => I::F32ConvertI64U,
                    (Class::Long, Class::F32, true) => I::F32ConvertI64S,
                    (Class::Long, Class::F64, false) => I::F64ConvertI64U,
                    (Class::Long, Class::F64, true) => I::F64ConvertI64S,
                    _ => return Err(format!("wasm: {opcode} from {from:?}")),
                });
            }
            Opcode::FcvtToUintSat | Opcode::FcvtToSintSat => {
                let signed = opcode == Opcode::FcvtToSintSat;
                self.get(arg)?;
                self.i(match (single, to, signed) {
                    (true, Class::Word(_), false) => I::I32TruncSatF32U,
                    (true, Class::Word(_), true) => I::I32TruncSatF32S,
                    (false, Class::Word(_), false) => I::I32TruncSatF64U,
                    (false, Class::Word(_), true) => I::I32TruncSatF64S,
                    (true, Class::Long, false) => I::I64TruncSatF32U,
                    (true, Class::Long, true) => I::I64TruncSatF32S,
                    (false, Class::Long, false) => I::I64TruncSatF64U,
                    (false, Class::Long, true) => I::I64TruncSatF64S,
                    _ => return Err(format!("wasm: {opcode} to {to:?}")),
                });
                if let Some(bits) = to.narrow() {
                    // Saturated to 32 bits so far: the narrow type has bounds of its own.
                    let (low, high) = match signed {
                        true => (-(1i32 << (bits - 1)), (1i32 << (bits - 1)) - 1),
                        false => (0, (1i32 << bits) - 1),
                    };
                    let value = self.temp(ValType::I32);
                    for (bound, keep) in [(high, Op::LtS), (low, Op::GtS)] {
                        let keep = if signed { keep } else { keep.strict_unsigned() };
                        self.i(I::LocalTee(value));
                        self.i(I::I32Const(bound));
                        self.i(I::LocalGet(value));
                        self.i(I::I32Const(bound));
                        self.op(keep, false);
                        self.i(I::Select);
                    }
                    self.mask(bits);
                }
            }
            _ => return Err(format!("wasm: {opcode}")),
        }
        self.set(result)
    }

    /// Reverses the bytes of the integer on the stack.
    fn swap_bytes(&mut self, long: bool, bits: u8) {
        let value = self.temp(if long { ValType::I64 } else { ValType::I32 });
        // Neighbouring bytes change places, then neighbouring pairs of them, and so on.
        let steps: &[(i64, u64)] = match bits {
            16 => &[(8, 0x00ff)],
            32 => &[(8, 0x00ff_00ff), (16, 0x0000_ffff)],
            _ => &[(8, 0x00ff_00ff_00ff_00ff), (16, 0x0000_ffff_0000_ffff), (32, 0xffff_ffff)],
        };
        self.swap_groups(value, long, steps);
    }

    /// Reverses the bits of the integer on the stack, which is `bits` wide.
    fn reverse_bits(&mut self, long: bool, bits: u8) {
        let value = self.temp(if long { ValType::I64 } else { ValType::I32 });
        let ones = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
        let steps = [
            (1, 0x5555_5555_5555_5555 & ones),
            (2, 0x3333_3333_3333_3333 & ones),
            (4, 0x0f0f_0f0f_0f0f_0f0f & ones),
        ];
        self.swap_groups(value, long, &steps);
        if bits > 8 {
            self.swap_bytes(long, bits);
        }
    }

    /// For each step, swaps every group of `shift` bits with the one beside it: `mask` has
    /// the lower group of each pair set.
    fn swap_groups(&mut self, value: u32, long: bool, steps: &[(i64, u64)]) {
        for &(shift, mask) in steps {
            self.i(I::LocalTee(value));
            self.constant(mask as i64, long);
            self.op(Op::And, long);
            self.constant(shift, long);
            self.op(Op::Shl, long);
            self.i(I::LocalGet(value));
            self.constant(shift, long);
            self.op(Op::ShrU, long);
            self.constant(mask as i64, long);
            self.op(Op::And, long);
            self.op(Op::Or, long);
        }
    }

    fn binary(
        &mut self,
        opcode: Opcode,
        results: &[Value],
        a: Value,
        b: Value,
    ) -> Result<(), String> {
        let result = results[0];
        let class = self.class(a)?;
        if opcode == Opcode::Iconcat {
            self.get(a)?;
            self.get(b)?;
            return self.set(result);
        }
        match class {
            Class::Word(_) | Class::Long => self.binary_int(opcode, results, a, b, class),
            Class::Pair => self.binary_wide(opcode, results, a, b),
            Class::F32 | Class::F64 => {
                let single = class == Class::F32;
                let float = |f32: I<'static>, f64: I<'static>| if single { f32 } else { f64 };
                let instruction = match opcode {
                    Opcode::Fadd => float(I::F32Add, I::F64Add),
                    Opcode::Fsub => float(I::F32Sub, I::F64Sub),
                    Opcode::Fmul => float(I::F32Mul, I::F64Mul),
                    Opcode::Fdiv => float(I::F32Div, I::F64Div),
                    Opcode::Fmin => float(I::F32Min, I::F64Min),
                    Opcode::Fmax => float(I::F32Max, I::F64Max),
                    Opcode::Fcopysign => float(I::F32Copysign, I::F64Copysign),
                    _ => return Err(format!("wasm: {opcode}")),
                };
                self.get(a)?;
                self.get(b)?;
                self.i(instruction);
                self.set(result)
            }
        }
    }

    fn binary_int(
        &mut self,
        opcode: Opcode,
        results: &[Value],
        a: Value,
        b: Value,
        class: Class,
    ) -> Result<(), String> {
        let result = results[0];
        let long = class.is_long();
        let bits = match class {
            Class::Word(bits) => bits,
            _ => 64,
        };
        let simple = match opcode {
            Opcode::Iadd => Some(Op::Add),
            Opcode::Isub => Some(Op::Sub),
            Opcode::Imul => Some(Op::Mul),
            Opcode::Band => Some(Op::And),
            Opcode::Bor => Some(Op::Or),
            Opcode::Bxor => Some(Op::Xor),
            Opcode::Udiv => Some(Op::DivU),
            Opcode::Urem => Some(Op::RemU),
            Opcode::Sdiv => Some(Op::DivS),
            Opcode::Srem => Some(Op::RemS),
            _ => None,
        };
        if let Some(op) = simple {
            self.get_for(a, op.is_signed())?;
            self.get_for(b, op.is_signed())?;
            self.op(op, long);
            self.mask_for(class);
            return self.set(result);
        }
        match opcode {
            Opcode::Ishl | Opcode::Ushr | Opcode::Sshr => {
                let op = match opcode {
                    Opcode::Ishl => Op::Shl,
                    Opcode::Ushr => Op::ShrU,
                    _ => Op::ShrS,
                };
                self.get_for(a, op.is_signed())?;
                self.amount(b, long)?;
                if bits < 32 {
                    // Wasm takes the amount modulo 32, the narrow type modulo its width.
                    self.i(I::I32Const(i32::from(bits) - 1));
                    self.i(I::I32And);
                }
                self.op(op, long);
                self.mask_for(class);
            }
            Opcode::Rotl | Opcode::Rotr => {
                let left = opcode == Opcode::Rotl;
                if bits >= 32 {
                    self.get(a)?;
                    self.amount(b, long)?;
                    self.op(if left { Op::Rotl } else { Op::Rotr }, long);
                } else {
                    // Two shifts that meet: the second amount is what is left of the width.
                    let amount = self.temp(ValType::I32);
                    let (there, back) =
                        if left { (Op::Shl, Op::ShrU) } else { (Op::ShrU, Op::Shl) };
                    self.amount(b, false)?;
                    self.i(I::I32Const(i32::from(bits) - 1));
                    self.i(I::I32And);
                    self.i(I::LocalSet(amount));
                    self.get(a)?;
                    self.i(I::LocalGet(amount));
                    self.op(there, false);
                    self.get(a)?;
                    self.i(I::I32Const(i32::from(bits)));
                    self.i(I::LocalGet(amount));
                    self.i(I::I32Sub);
                    self.i(I::I32Const(i32::from(bits) - 1));
                    self.i(I::I32And);
                    self.op(back, false);
                    self.i(I::I32Or);
                    self.mask(bits);
                }
            }
            Opcode::Umin | Opcode::Umax | Opcode::Smin | Opcode::Smax => {
                let pick = match opcode {
                    Opcode::Umin => Op::LtU,
                    Opcode::Umax => Op::GtU,
                    Opcode::Smin => Op::LtS,
                    _ => Op::GtS,
                };
                self.get(a)?;
                self.get(b)?;
                self.compare(pick, a, b)?;
                self.i(I::Select);
            }
            Opcode::Umulhi | Opcode::Smulhi if long => {
                let (x, y) = (self.local(a)?, self.local(b)?);
                self.multiply_high(x, y, opcode == Opcode::Smulhi);
            }
            Opcode::UaddOverflow | Opcode::UsubOverflow => {
                let add = opcode == Opcode::UaddOverflow;
                self.get(a)?;
                self.get(b)?;
                self.op(if add { Op::Add } else { Op::Sub }, long);
                self.mask_for(class);
                self.set(result)?;
                // A sum that wrapped is below what went in, and a difference wraps when
                // more is taken than there is.
                self.get(if add { result } else { a })?;
                self.get(if add { a } else { b })?;
                self.op(Op::LtU, long);
                return self.set(results[1]);
            }
            Opcode::SaddOverflow | Opcode::SsubOverflow if long => {
                let add = opcode == Opcode::SaddOverflow;
                self.get(a)?;
                self.get(b)?;
                self.op(if add { Op::Add } else { Op::Sub }, true);
                self.set(result)?;
                // The sign is wrong when both operands differ from the sum in it, and for a
                // difference when the operands differ and the result is not on `a`'s side.
                self.get(a)?;
                self.get(if add { result } else { b })?;
                self.i(I::I64Xor);
                self.get(if add { b } else { a })?;
                self.get(result)?;
                self.i(I::I64Xor);
                self.i(I::I64And);
                self.i(I::I64Const(0));
                self.i(I::I64LtS);
                return self.set(results[1]);
            }
            Opcode::SaddOverflow | Opcode::SsubOverflow | Opcode::SmulOverflow if !long => {
                // In 64 bits nothing overflows: the result fits the type when extending its
                // low bits gives it back.
                let exact = self.temp(ValType::I64);
                for value in [a, b] {
                    self.get_signed(value)?;
                    self.i(I::I64ExtendI32S);
                }
                self.i(match opcode {
                    Opcode::SaddOverflow => I::I64Add,
                    Opcode::SsubOverflow => I::I64Sub,
                    _ => I::I64Mul,
                });
                self.i(I::LocalTee(exact));
                self.i(I::I32WrapI64);
                self.mask_for(class);
                self.set(result)?;
                self.i(I::LocalGet(exact));
                self.i(I::LocalGet(exact));
                self.i(match bits {
                    8 => I::I64Extend8S,
                    16 => I::I64Extend16S,
                    _ => I::I64Extend32S,
                });
                self.i(I::I64Ne);
                return self.set(results[1]);
            }
            Opcode::UmulOverflow if !long => {
                let exact = self.temp(ValType::I64);
                for value in [a, b] {
                    self.get(value)?;
                    self.i(I::I64ExtendI32U);
                }
                self.i(I::I64Mul);
                self.i(I::LocalTee(exact));
                self.i(I::I32WrapI64);
                self.mask_for(class);
                self.set(result)?;
                self.i(I::LocalGet(exact));
                self.i(I::I64Const(i64::from(bits)));
                self.i(I::I64ShrU);
                self.i(I::I64Const(0));
                self.i(I::I64Ne);
                return self.set(results[1]);
            }
            Opcode::UmulOverflow | Opcode::SmulOverflow => {
                let signed = opcode == Opcode::SmulOverflow;
                let (x, y) = (self.local(a)?, self.local(b)?);
                self.get(a)?;
                self.get(b)?;
                self.i(I::I64Mul);
                self.set(result)?;
                // The upper half of the full product has to be nothing but the sign of the
                // lower one.
                self.multiply_high(x, y, signed);
                if signed {
                    self.get(result)?;
                    self.i(I::I64Const(63));
                    self.i(I::I64ShrS);
                } else {
                    self.i(I::I64Const(0));
                }
                self.i(I::I64Ne);
                return self.set(results[1]);
            }
            _ => return Err(format!("wasm: {opcode}")),
        }
        self.set(result)
    }

    /// Pushes the upper 64 bits of the product of two `i64` locals.
    fn multiply_high(&mut self, a: u32, b: u32, signed: bool) {
        let [low, cross] = [(); 2].map(|()| self.temp(ValType::I64));
        let half = |this: &mut Self, local: u32, upper: bool| {
            this.i(I::LocalGet(local));
            if upper {
                this.i(I::I64Const(32));
                this.i(I::I64ShrU);
            } else {
                this.i(I::I64Const(0xffff_ffff));
                this.i(I::I64And);
            }
        };
        // Schoolbook multiplication on 32-bit halves, carrying through the middle.
        half(self, a, true);
        half(self, b, false);
        self.i(I::I64Mul);
        half(self, a, false);
        half(self, b, false);
        self.i(I::I64Mul);
        self.i(I::I64Const(32));
        self.i(I::I64ShrU);
        self.i(I::I64Add);
        self.i(I::LocalSet(cross));
        half(self, a, false);
        half(self, b, true);
        self.i(I::I64Mul);
        half(self, cross, false);
        self.i(I::I64Add);
        self.i(I::LocalSet(low));
        half(self, a, true);
        half(self, b, true);
        self.i(I::I64Mul);
        half(self, cross, true);
        self.i(I::I64Add);
        half(self, low, true);
        self.i(I::I64Add);
        if signed {
            // A negative operand counted as 2^64 more than it is, times the other.
            for (negative, other) in [(a, b), (b, a)] {
                self.i(I::LocalGet(other));
                self.i(I::I64Const(0));
                self.i(I::LocalGet(negative));
                self.i(I::I64Const(0));
                self.i(I::I64LtS);
                self.i(I::Select);
                self.i(I::I64Sub);
            }
        }
    }

    fn binary_wide(
        &mut self,
        opcode: Opcode,
        results: &[Value],
        a: Value,
        b: Value,
    ) -> Result<(), String> {
        let result = results[0];
        let out = self.local(result)?;
        match opcode {
            Opcode::Band | Opcode::Bor | Opcode::Bxor => {
                let op = match opcode {
                    Opcode::Band => Op::And,
                    Opcode::Bor => Op::Or,
                    _ => Op::Xor,
                };
                self.get_low(a)?;
                self.get_low(b)?;
                self.op(op, true);
                self.get_high(a)?;
                self.get_high(b)?;
                self.op(op, true);
                self.set(result)
            }
            Opcode::Iadd
            | Opcode::Isub
            | Opcode::UaddOverflow
            | Opcode::UsubOverflow
            | Opcode::SaddOverflow
            | Opcode::SsubOverflow => {
                let add =
                    matches!(opcode, Opcode::Iadd | Opcode::UaddOverflow | Opcode::SaddOverflow);
                let op = if add { Op::Add } else { Op::Sub };
                let [low, high] = [(); 2].map(|()| self.temp(ValType::I64));
                self.get_low(a)?;
                self.get_low(b)?;
                self.op(op, true);
                self.i(I::LocalSet(low));
                self.get_high(a)?;
                self.get_high(b)?;
                self.op(op, true);
                // What the low halves carry or borrow.
                if add {
                    self.i(I::LocalGet(low));
                    self.get_low(a)?;
                } else {
                    self.get_low(a)?;
                    self.get_low(b)?;
                }
                self.i(I::I64LtU);
                self.i(I::I64ExtendI32U);
                self.op(op, true);
                self.i(I::LocalSet(high));
                match opcode {
                    Opcode::UaddOverflow | Opcode::UsubOverflow => {
                        // As for one word, over both: the high halves decide unless equal.
                        let (x_low, x_high, y) = if add { (low, high, a) } else { (0, 0, b) };
                        if add {
                            self.i(I::LocalGet(x_low));
                            self.get_low(y)?;
                            self.i(I::I64LtU);
                            self.i(I::LocalGet(x_high));
                            self.get_high(y)?;
                            self.i(I::I64LtU);
                            self.i(I::LocalGet(x_high));
                            self.get_high(y)?;
                            self.i(I::I64Eq);
                            self.i(I::Select);
                        } else {
                            self.compare(Op::LtU, a, y)?;
                        }
                        self.set(results[1])?;
                    }
                    Opcode::SaddOverflow | Opcode::SsubOverflow => {
                        self.get_high(a)?;
                        if add {
                            self.i(I::LocalGet(high));
                        } else {
                            self.get_high(b)?;
                        }
                        self.i(I::I64Xor);
                        if add {
                            self.get_high(b)?;
                        } else {
                            self.get_high(a)?;
                        }
                        self.i(I::LocalGet(high));
                        self.i(I::I64Xor);
                        self.i(I::I64And);
                        self.i(I::I64Const(0));
                        self.i(I::I64LtS);
                        self.set(results[1])?;
                    }
                    _ => {}
                }
                self.i(I::LocalGet(low));
                self.i(I::LocalSet(out));
                self.i(I::LocalGet(high));
                self.i(I::LocalSet(out + 1));
                Ok(())
            }
            Opcode::Imul => {
                let (x, y) = (self.local(a)?, self.local(b)?);
                let [low, high] = [(); 2].map(|()| self.temp(ValType::I64));
                self.i(I::LocalGet(x));
                self.i(I::LocalGet(y));
                self.i(I::I64Mul);
                self.i(I::LocalSet(low));
                self.multiply_high(x, y, false);
                self.i(I::LocalGet(x));
                self.i(I::LocalGet(y + 1));
                self.i(I::I64Mul);
                self.i(I::I64Add);
                self.i(I::LocalGet(x + 1));
                self.i(I::LocalGet(y));
                self.i(I::I64Mul);
                self.i(I::I64Add);
                self.i(I::LocalSet(high));
                self.i(I::LocalGet(low));
                self.i(I::LocalSet(out));
                self.i(I::LocalGet(high));
                self.i(I::LocalSet(out + 1));
                Ok(())
            }
            Opcode::Ishl | Opcode::Ushr | Opcode::Sshr => {
                let x = self.local(a)?;
                let amount = self.temp(ValType::I64);
                let [low, high] = [(); 2].map(|()| self.temp(ValType::I64));
                self.amount(b, true)?;
                self.i(I::I64Const(127));
                self.i(I::I64And);
                self.i(I::LocalSet(amount));
                // `near` is the half the bits leave and `far` the one they arrive in.
                let left = opcode == Opcode::Ishl;
                let signed = opcode == Opcode::Sshr;
                let (near, far, near_out, far_out) =
                    if left { (x, x + 1, low, high) } else { (x + 1, x, high, low) };
                let (there, back) = if left { (Op::Shl, Op::ShrU) } else { (Op::ShrU, Op::Shl) };
                let near_shift = if signed { Op::ShrS } else { there };
                self.i(I::LocalGet(amount));
                self.i(I::I64Const(64));
                self.i(I::I64GeU);
                self.i(I::If(BlockType::Empty));
                {
                    // Everything crosses over: wasm takes the amount modulo 64 itself.
                    self.i(I::LocalGet(near));
                    self.i(I::LocalGet(amount));
                    self.op(near_shift, true);
                    self.i(I::LocalSet(far_out));
                    if signed {
                        self.i(I::LocalGet(near));
                        self.i(I::I64Const(63));
                        self.i(I::I64ShrS);
                    } else {
                        self.i(I::I64Const(0));
                    }
                    self.i(I::LocalSet(near_out));
                }
                self.i(I::Else);
                {
                    self.i(I::LocalGet(near));
                    self.i(I::LocalGet(amount));
                    self.op(near_shift, true);
                    self.i(I::LocalSet(near_out));
                    self.i(I::LocalGet(far));
                    self.i(I::LocalGet(amount));
                    self.op(there, true);
                    // What crosses over, in two steps so that an amount of zero moves
                    // nothing rather than everything.
                    self.i(I::LocalGet(near));
                    self.i(I::I64Const(1));
                    self.op(back, true);
                    self.i(I::I64Const(63));
                    self.i(I::LocalGet(amount));
                    self.i(I::I64Sub);
                    self.op(back, true);
                    self.i(I::I64Or);
                    self.i(I::LocalSet(far_out));
                }
                self.i(I::End);
                self.i(I::LocalGet(low));
                self.i(I::LocalSet(out));
                self.i(I::LocalGet(high));
                self.i(I::LocalSet(out + 1));
                Ok(())
            }
            _ => Err(format!("wasm: {opcode} of a wide integer")),
        }
    }
}

impl Op {
    fn strict_unsigned_or_equal(self) -> Op {
        match self {
            Op::LeS | Op::LeU => Op::LeU,
            _ => Op::GeU,
        }
    }
}
