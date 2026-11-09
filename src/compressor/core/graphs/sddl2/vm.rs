//! The SDDL2 virtual machine (`sddl2_vm.c`, `sddl2_interpreter.c`).
//!
//! A stack machine runs 32-bit instruction words over the input and emits a
//! list of *segments*: runs of input bytes with a tag and a type. The
//! disassembler of the reference is debug-only and is not ported, and neither
//! is the trace buffer: it only feeds log messages.
//!
//! Instruction word: family in the high 16 bits, opcode in the low 16 bits,
//! little endian. `push.u32`, `push.i32` and `push.tag` are followed by a
//! 4-byte immediate, `push.i64` by an 8-byte one.
//!
//! Reference quirks kept on purpose:
//!
//! * a family the interpreter does not know is a silent no-op (the dispatch
//!   `switch` has no default);
//! * the stack holds 256 values, the segment list 524288 segments, the tag
//!   registry 32768 tags, and the 16 value registers of `var.*` are 256;
//! * `type.fixed_array` truncates its count to 32 bits before multiplying;
//! * `stack.over` and `stack.rot` are in the opcode table but not
//!   implemented by the interpreter, and are invalid bytecode.

use alloc::sync::Arc;
use alloc::vec::Vec;

use thiserror::Error;

/// The most values the stack holds (`stack_storage[256]`).
const STACK_CAPACITY: usize = 256;
/// `SDDL2_VAR_REGISTER_COUNT`.
const REGISTERS: usize = 256;
/// `SDDL2_SEGMENT_MAX_CAPACITY`.
const SEGMENT_MAX: usize = 524_288;
/// `SDDL2_TAG_MAX_CAPACITY`.
const TAG_MAX: usize = 32_768;

/// Why the machine stopped (`SDDL2_Error`).
#[derive(Error, Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum VmError {
    /// `SDDL2_STACK_OVERFLOW`.
    #[error("stack overflow")]
    StackOverflow,
    /// `SDDL2_STACK_UNDERFLOW`.
    #[error("stack underflow")]
    StackUnderflow,
    /// `SDDL2_MATH_OVERFLOW`.
    #[error("arithmetic overflow")]
    MathOverflow,
    /// `SDDL2_TYPE_MISMATCH`.
    #[error("operand of the wrong type")]
    TypeMismatch,
    /// `SDDL2_LOAD_BOUNDS`.
    #[error("load outside the input")]
    LoadBounds,
    /// `SDDL2_SEGMENT_BOUNDS`.
    #[error("segment beyond the input")]
    SegmentBounds,
    /// `SDDL2_LIMIT_EXCEEDED`.
    #[error("too many segments")]
    LimitExceeded,
    /// `SDDL2_DIV_ZERO`.
    #[error("division by zero")]
    DivZero,
    /// `SDDL2_INVALID_BYTECODE`.
    #[error("invalid bytecode")]
    InvalidBytecode,
    /// `SDDL2_VALIDATION_FAILED`.
    #[error("expect_true failed")]
    ValidationFailed,
}

/// `SDDL2_Type_kind`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Bytes,
    U8,
    I8,
    U16Le,
    U16Be,
    I16Le,
    I16Be,
    U32Le,
    U32Be,
    I32Le,
    I32Be,
    U64Le,
    U64Be,
    I64Le,
    I64Be,
    F8,
    F16Le,
    F16Be,
    Bf16Le,
    Bf16Be,
    F32Le,
    F32Be,
    F64Le,
    F64Be,
    Structure,
}

impl Kind {
    /// `SDDL2_kind_size`: bytes of one element, or `None` for a structure.
    pub(crate) const fn size(self) -> Option<usize> {
        Some(match self {
            Self::Bytes | Self::U8 | Self::I8 | Self::F8 => 1,
            Self::U16Le
            | Self::U16Be
            | Self::I16Le
            | Self::I16Be
            | Self::F16Le
            | Self::F16Be
            | Self::Bf16Le
            | Self::Bf16Be => 2,
            Self::U32Le | Self::U32Be | Self::I32Le | Self::I32Be | Self::F32Le | Self::F32Be => 4,
            Self::U64Le | Self::U64Be | Self::I64Le | Self::I64Be | Self::F64Le | Self::F64Be => 8,
            Self::Structure => return None,
        })
    }

    /// The kind pushed by `push.type.*` opcode `opcode`.
    const fn from_push_opcode(opcode: u16) -> Option<Self> {
        Some(match opcode {
            0x0100 => Self::Bytes,
            0x0110 => Self::U8,
            0x0111 => Self::I8,
            0x0112 => Self::U16Le,
            0x0113 => Self::U16Be,
            0x0114 => Self::I16Le,
            0x0115 => Self::I16Be,
            0x0116 => Self::U32Le,
            0x0117 => Self::U32Be,
            0x0118 => Self::I32Le,
            0x0119 => Self::I32Be,
            0x011A => Self::U64Le,
            0x011B => Self::U64Be,
            0x011C => Self::I64Le,
            0x011D => Self::I64Be,
            0x0130 => Self::F8,
            0x0131 => Self::F16Le,
            0x0132 => Self::F16Be,
            0x0133 => Self::Bf16Le,
            0x0134 => Self::Bf16Be,
            0x0135 => Self::F32Le,
            0x0136 => Self::F32Be,
            0x0137 => Self::F64Le,
            0x0138 => Self::F64Be,
            _ => return None,
        })
    }
}

/// `SDDL2_Struct_data`: the members of a structure type.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct StructData {
    pub(crate) members: Vec<Type>,
    /// The sum of the members' sizes.
    pub(crate) total_size_bytes: usize,
}

/// `SDDL2_Type`: `width` elements of a primitive kind, or `width`
/// repetitions of a structure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Type {
    pub(crate) kind: Kind,
    pub(crate) width: u32,
    pub(crate) struct_data: Option<Arc<StructData>>,
}

impl Type {
    /// A single element of a primitive `kind`.
    pub(crate) const fn primitive(kind: Kind) -> Self {
        Self {
            kind,
            width: 1,
            struct_data: None,
        }
    }

    /// `SDDL2_Type_size`: the bytes of the whole type, `width` included.
    pub(crate) fn size(&self) -> Result<usize, VmError> {
        let unit = match (&self.struct_data, self.kind.size()) {
            (Some(data), _) => data.total_size_bytes,
            (None, Some(size)) => size,
            (None, None) => return Err(VmError::TypeMismatch),
        };
        Ok(unit.wrapping_mul(self.width as usize))
    }

    /// `types_equal`: structures are equal only when they share their data.
    fn same_as(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.width == other.width
            && match (&self.struct_data, &other.struct_data) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

/// `SDDL2_Value`.
#[derive(Clone, Debug)]
enum Value {
    I64(i64),
    Tag(u32),
    Type(Type),
}

/// `SDDL2_Segment`: `size_bytes` of input from `start_pos`, of one tag and
/// type.
#[derive(Clone, Debug)]
pub(crate) struct Segment {
    /// 0 for unspecified segments.
    pub(crate) tag: u32,
    pub(crate) start_pos: usize,
    pub(crate) size_bytes: usize,
    pub(crate) ty: Type,
}

/// Family IDs (`sddl2_opcodes.h`).
mod family {
    pub(super) const PUSH: u16 = 0x0001;
    pub(super) const MATH: u16 = 0x0002;
    pub(super) const CMP: u16 = 0x0003;
    pub(super) const LOGIC: u16 = 0x0004;
    pub(super) const CONTROL: u16 = 0x0005;
    pub(super) const LOAD: u16 = 0x0006;
    pub(super) const STACK: u16 = 0x0007;
    pub(super) const TYPE: u16 = 0x0008;
    pub(super) const VAR: u16 = 0x0009;
    pub(super) const CALL: u16 = 0x000B;
    pub(super) const SEGMENT: u16 = 0x000C;
}

struct Machine<'a> {
    input: &'a [u8],
    /// `SDDL2_Input_cursor::current_pos`.
    pos: usize,
    stack: Vec<Value>,
    registers: [Option<Value>; REGISTERS],
    registry: Vec<(u32, Type)>,
    segments: Vec<Segment>,
}

/// `SDDL2_execute_bytecode`: runs `bytecode` over `input`, returning the
/// segments it created.
///
/// # Errors
///
/// Malformed bytecode, a failed check, or a limit.
pub(crate) fn execute(bytecode: &[u8], input: &[u8]) -> Result<Vec<Segment>, VmError> {
    if !bytecode.len().is_multiple_of(4) {
        return Err(VmError::InvalidBytecode);
    }
    let mut machine = Machine {
        input,
        pos: 0,
        stack: Vec::new(),
        registers: core::array::from_fn(|_| None),
        registry: Vec::new(),
        segments: Vec::new(),
    };
    machine.run(bytecode)?;
    Ok(machine.segments)
}

/// A little-endian immediate at `*pc`.
fn immediate<const N: usize>(bytecode: &[u8], pc: &mut usize) -> Result<[u8; N], VmError> {
    let end = pc.checked_add(N).ok_or(VmError::InvalidBytecode)?;
    let bytes = bytecode
        .get(*pc..end)
        .and_then(|slice| <[u8; N]>::try_from(slice).ok())
        .ok_or(VmError::InvalidBytecode)?;
    *pc = end;
    Ok(bytes)
}

/// `SDDL2_skip`: advances `pc` over `count` instructions and their
/// immediates.
fn skip(bytecode: &[u8], pc: &mut usize, count: usize) -> Result<(), VmError> {
    for _ in 0..count {
        let word = u32::from_le_bytes(immediate(bytecode, pc)?);
        let (family, opcode) = ((word >> 16) as u16, word as u16);
        if family == family::PUSH {
            match opcode {
                0x0002 | 0x0003 | 0x0005 => {
                    immediate::<4>(bytecode, pc)?;
                }
                0x0004 => {
                    immediate::<8>(bytecode, pc)?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// `add_would_overflow` and friends are `checked_*`.
fn checked(result: Option<i64>) -> Result<i64, VmError> {
    result.ok_or(VmError::MathOverflow)
}

impl Machine<'_> {
    fn run(&mut self, bytecode: &[u8]) -> Result<(), VmError> {
        let mut pc = 0;
        while pc < bytecode.len() {
            let word = u32::from_le_bytes(immediate(bytecode, &mut pc)?);
            let (family, opcode) = ((word >> 16) as u16, word as u16);
            match family {
                family::CONTROL => match opcode {
                    0x0001 => break,
                    0x0002 => self.expect_true()?,
                    0x0003 => {
                        let condition = self.pop_i64()?;
                        let count = self.pop_positive()?;
                        if condition != 0 {
                            skip(bytecode, &mut pc, count)?;
                        }
                    }
                    0x0004 => {}
                    _ => return Err(VmError::InvalidBytecode),
                },
                family::PUSH => self.push_family(opcode, bytecode, &mut pc)?,
                family::SEGMENT => match opcode {
                    0x0001 => self.segment_unspecified()?,
                    0x0002 => self.segment_tagged()?,
                    _ => return Err(VmError::InvalidBytecode),
                },
                family::MATH => self.math(opcode)?,
                family::CMP => self.compare(opcode)?,
                family::LOGIC => self.logic(opcode)?,
                family::STACK => self.stack_op(opcode)?,
                family::TYPE => match opcode {
                    0x0001 => self.type_fixed_array()?,
                    0x0002 => self.type_structure()?,
                    0x0010 => {
                        let ty = self.pop_type()?;
                        self.push(Value::I64(ty.size()? as i64))?;
                    }
                    _ => return Err(VmError::InvalidBytecode),
                },
                family::LOAD => self.load(opcode)?,
                family::VAR => match opcode {
                    0x0001 => self.var_store()?,
                    0x0002 => self.var_load()?,
                    _ => return Err(VmError::InvalidBytecode),
                },
                family::CALL => return Err(VmError::InvalidBytecode),
                // The reference's `switch` has no default: nothing happens.
                _ => {}
            }
        }
        Ok(())
    }

    fn push(&mut self, value: Value) -> Result<(), VmError> {
        if self.stack.len() >= STACK_CAPACITY {
            return Err(VmError::StackOverflow);
        }
        self.stack.push(value);
        Ok(())
    }

    fn pop(&mut self) -> Result<Value, VmError> {
        self.stack.pop().ok_or(VmError::StackUnderflow)
    }

    fn push_i64(&mut self, value: i64) -> Result<(), VmError> {
        self.push(Value::I64(value))
    }

    fn pop_i64(&mut self) -> Result<i64, VmError> {
        match self.pop()? {
            Value::I64(value) => Ok(value),
            _ => Err(VmError::TypeMismatch),
        }
    }

    /// `pop_positive_i64`.
    fn pop_positive(&mut self) -> Result<usize, VmError> {
        let value = self.pop_i64()?;
        usize::try_from(value).map_err(|_| VmError::TypeMismatch)
    }

    fn pop_tag(&mut self) -> Result<u32, VmError> {
        match self.pop()? {
            Value::Tag(tag) => Ok(tag),
            _ => Err(VmError::TypeMismatch),
        }
    }

    fn pop_type(&mut self) -> Result<Type, VmError> {
        match self.pop()? {
            Value::Type(ty) => Ok(ty),
            _ => Err(VmError::TypeMismatch),
        }
    }

    /// `handle_push_family`.
    fn push_family(&mut self, opcode: u16, bytecode: &[u8], pc: &mut usize) -> Result<(), VmError> {
        match opcode {
            0x0001 => self.push_i64(0),
            0x0002 => self.push_i64(i64::from(u32::from_le_bytes(immediate(bytecode, pc)?))),
            0x0003 => self.push_i64(i64::from(i32::from_le_bytes(immediate(bytecode, pc)?))),
            0x0004 => self.push_i64(i64::from_le_bytes(immediate(bytecode, pc)?)),
            0x0005 => self.push(Value::Tag(u32::from_le_bytes(immediate(bytecode, pc)?))),
            0x0080 => self.push_i64(self.pos as i64),
            0x0081 => self.push_i64(self.input.len().wrapping_sub(self.pos) as i64),
            0x0082 => self.push_i64(self.stack.len() as i64),
            other => match Kind::from_push_opcode(other) {
                Some(kind) => self.push(Value::Type(Type::primitive(kind))),
                None => Err(VmError::InvalidBytecode),
            },
        }
    }

    /// Pops `b` then `a`.
    fn pop_pair(&mut self) -> Result<(i64, i64), VmError> {
        let b = self.pop_i64()?;
        let a = self.pop_i64()?;
        Ok((a, b))
    }

    fn math(&mut self, opcode: u16) -> Result<(), VmError> {
        let result = match opcode {
            0x0001 => {
                let (a, b) = self.pop_pair()?;
                checked(a.checked_add(b))?
            }
            0x0002 => {
                let (a, b) = self.pop_pair()?;
                checked(a.checked_sub(b))?
            }
            0x0003 => {
                let (a, b) = self.pop_pair()?;
                checked(a.checked_mul(b))?
            }
            0x0004 => {
                let (a, b) = self.pop_pair()?;
                if b == 0 {
                    return Err(VmError::DivZero);
                }
                checked(a.checked_div(b))?
            }
            0x0005 => {
                let (a, b) = self.pop_pair()?;
                if b == 0 {
                    return Err(VmError::DivZero);
                }
                // `INT64_MIN % -1` traps in C; the remainder is 0.
                a.wrapping_rem(b)
            }
            0x0006 => {
                let a = self.pop_i64()?;
                checked(a.checked_abs())?
            }
            0x0007 => {
                let a = self.pop_i64()?;
                checked(a.checked_neg())?
            }
            0x0008 => {
                let (a, b) = self.pop_pair()?;
                a & b
            }
            0x0009 => {
                let (a, b) = self.pop_pair()?;
                a | b
            }
            0x000A => {
                let (a, b) = self.pop_pair()?;
                a ^ b
            }
            0x000B => !self.pop_i64()?,
            _ => return Err(VmError::InvalidBytecode),
        };
        self.push_i64(result)
    }

    fn compare(&mut self, opcode: u16) -> Result<(), VmError> {
        if !(0x0001..=0x0006).contains(&opcode) {
            return Err(VmError::InvalidBytecode);
        }
        let (a, b) = self.pop_pair()?;
        self.push_i64(i64::from(match opcode {
            0x0001 => a == b,
            0x0002 => a != b,
            0x0003 => a < b,
            0x0004 => a <= b,
            0x0005 => a > b,
            _ => a >= b,
        }))
    }

    fn logic(&mut self, opcode: u16) -> Result<(), VmError> {
        let result = match opcode {
            0x0001 => {
                let (a, b) = self.pop_pair()?;
                a != 0 && b != 0
            }
            0x0002 => {
                let (a, b) = self.pop_pair()?;
                a != 0 || b != 0
            }
            0x0003 => {
                let (a, b) = self.pop_pair()?;
                (a != 0) != (b != 0)
            }
            0x0004 => self.pop_i64()? == 0,
            _ => return Err(VmError::InvalidBytecode),
        };
        self.push_i64(i64::from(result))
    }

    /// `stack.dup`, `stack.drop`, `stack.swap` and `stack.drop_if`.
    fn stack_op(&mut self, opcode: u16) -> Result<(), VmError> {
        match opcode {
            0x0001 => {
                let top = self.stack.last().cloned().ok_or(VmError::StackUnderflow)?;
                self.push(top)
            }
            0x0003 => self.pop().map(drop),
            0x0004 => {
                let len = self.stack.len();
                if len < 2 {
                    return Err(VmError::StackUnderflow);
                }
                self.stack.swap(len - 1, len - 2);
                Ok(())
            }
            0x0010 => {
                if self.pop_i64()? != 0 {
                    self.pop()?;
                }
                Ok(())
            }
            _ => Err(VmError::InvalidBytecode),
        }
    }

    fn expect_true(&mut self) -> Result<(), VmError> {
        if self.pop_i64()? == 0 {
            return Err(VmError::ValidationFailed);
        }
        Ok(())
    }

    /// `SDDL2_op_type_fixed_array`.
    fn type_fixed_array(&mut self) -> Result<(), VmError> {
        let count = self.pop_positive()?;
        let mut ty = self.pop_type()?;
        // The count is truncated to 32 bits before the product.
        ty.width = ty
            .width
            .checked_mul(count as u32)
            .ok_or(VmError::MathOverflow)?;
        self.push(Value::Type(ty))
    }

    /// `SDDL2_op_type_structure`.
    fn type_structure(&mut self) -> Result<(), VmError> {
        let count = self.pop_positive()?;
        if count > self.stack.len() {
            return Err(VmError::StackUnderflow);
        }
        let mut members = Vec::with_capacity(count);
        for _ in 0..count {
            members.push(self.pop_type()?);
        }
        members.reverse();
        let mut total = 0usize;
        for member in &members {
            total = total
                .checked_add(member.size()?)
                .ok_or(VmError::MathOverflow)?;
        }
        self.push(Value::Type(Type {
            kind: Kind::Structure,
            width: 1,
            struct_data: Some(Arc::new(StructData {
                members,
                total_size_bytes: total,
            })),
        }))
    }

    /// The `load.*` family: reads `N` bytes at the popped address.
    fn load(&mut self, opcode: u16) -> Result<(), VmError> {
        let (size, big) = match opcode {
            0x0001 | 0x0002 => (1, false),
            0x0010 | 0x0011 => (2, false),
            0x0020 | 0x0021 => (4, false),
            0x0030 => (8, false),
            0x0110 | 0x0111 => (2, true),
            0x0120 | 0x0121 => (4, true),
            0x0130 => (8, true),
            _ => return Err(VmError::InvalidBytecode),
        };
        let signed = matches!(opcode, 0x0002 | 0x0011 | 0x0021 | 0x0111 | 0x0121);
        let addr = usize::try_from(self.pop_i64()?).map_err(|_| VmError::LoadBounds)?;
        let end = addr.checked_add(size).ok_or(VmError::LoadBounds)?;
        let bytes = self.input.get(addr..end).ok_or(VmError::LoadBounds)?;
        let mut raw = [0u8; 8];
        raw[..size].copy_from_slice(bytes);
        let unsigned = if big {
            raw[..size].reverse();
            u64::from_le_bytes(raw)
        } else {
            u64::from_le_bytes(raw)
        };
        let value = match (size, signed) {
            (1, true) => i64::from(unsigned as u8 as i8),
            (2, true) => i64::from(unsigned as u16 as i16),
            (4, true) => i64::from(unsigned as u32 as i32),
            _ => unsigned as i64,
        };
        self.push_i64(value)
    }

    fn register_index(&mut self) -> Result<usize, VmError> {
        let index = self.pop_i64()?;
        usize::try_from(index)
            .ok()
            .filter(|&index| index < REGISTERS)
            .ok_or(VmError::LoadBounds)
    }

    /// `SDDL2_op_var_store`.
    fn var_store(&mut self) -> Result<(), VmError> {
        let index = self.register_index()?;
        self.registers[index] = Some(self.pop()?);
        Ok(())
    }

    /// `SDDL2_op_var_load`.
    fn var_load(&mut self) -> Result<(), VmError> {
        let index = self.register_index()?;
        let value = self.registers[index].clone().ok_or(VmError::LoadBounds)?;
        self.push(value)
    }

    fn segment_unspecified(&mut self) -> Result<(), VmError> {
        let size = self.pop_i64()?;
        let size = usize::try_from(size).map_err(|_| VmError::TypeMismatch)?;
        self.create_segment(0, Type::primitive(Kind::Bytes), size)
    }

    fn segment_tagged(&mut self) -> Result<(), VmError> {
        let size = self.pop_i64()?;
        let ty = self.pop_type()?;
        let tag = self.pop_tag()?;
        let size = usize::try_from(size).map_err(|_| VmError::TypeMismatch)?;
        self.create_segment(tag, ty, size)
    }

    /// `tag_registry_register`: a tag keeps its first type.
    fn register_tag(&mut self, tag: u32, ty: &Type) -> Result<(), VmError> {
        if let Some((_, known)) = self.registry.iter().find(|(known, _)| *known == tag) {
            return if known.same_as(ty) {
                Ok(())
            } else {
                Err(VmError::TypeMismatch)
            };
        }
        if self.registry.len() >= TAG_MAX {
            return Err(VmError::TypeMismatch);
        }
        self.registry.push((tag, ty.clone()));
        Ok(())
    }

    /// `segment_create_internal`.
    fn create_segment(&mut self, tag: u32, ty: Type, count: usize) -> Result<(), VmError> {
        let size_bytes = count.checked_mul(ty.size()?).ok_or(VmError::MathOverflow)?;
        if self.pos.wrapping_add(size_bytes) > self.input.len() {
            return Err(VmError::SegmentBounds);
        }
        if tag != 0 {
            self.register_tag(tag, &ty)?;
        }
        if let Some(last) = self.segments.last_mut()
            && last.tag == tag
            && last.start_pos.wrapping_add(last.size_bytes) == self.pos
        {
            last.size_bytes = last.size_bytes.wrapping_add(size_bytes);
            self.pos = self.pos.wrapping_add(size_bytes);
            return Ok(());
        }
        if self.segments.len() >= SEGMENT_MAX {
            return Err(VmError::LimitExceeded);
        }
        self.segments.push(Segment {
            tag,
            start_pos: self.pos,
            size_bytes,
            ty,
        });
        self.pos = self.pos.wrapping_add(size_bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
