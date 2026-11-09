//! The SDDL interpreter (`ZL_SDDL_State_exec` and the `ZL_SDDL_State_*`
//! functions of `simple_data_description_language.c`).
//!
//! The interpreter evaluates the program's root expressions against the
//! input. Consuming an atom appends to a list of dispatch instructions
//! (`segment_sizes`, `segment_tags`): consecutive atoms sent to the same
//! destination are merged into one segment. Records consumed repeatedly by an
//! array replay a cached copy of their instructions, exactly as the
//! reference does, including the way a cached first instruction is
//! reconstructed (see [`State::consume_record_no_scope`]), because the
//! segment list is part of the frame.
//!
//! Reference counting is replaced by `Rc`: scopes and resolved field
//! expression sets have value semantics in the reference (a scope shared by
//! several holders is copied before it is modified), which `Rc` clones
//! reproduce.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::error::SddlError;
use super::program::{
    Array, Atom, AtomType, Expr, Field, Func, Name, Op, OpCode, Program, Record, Scope, ScopeRef,
};

/// `ZL_SDDL_SEGMENT_LIMIT`: the most segments a run may emit.
const SEGMENT_LIMIT: usize = 100_000_000;
/// `ZL_SDDL_VARIABLE_LIMIT`: the most variables a scope may hold.
const VARIABLE_LIMIT: usize = 1024;
/// `ZL_SDDL_DEST_LIMIT`: the most destinations a run may use.
const DEST_LIMIT: usize = 1024;

/// `ZL_SDDL_OutputInfo`: what a destination receives.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct OutputInfo {
    pub(super) ty: AtomType,
    /// 0 while nothing was sent to the destination.
    pub(super) width: usize,
    pub(super) big_endian: bool,
}

const UNUSED: OutputInfo = OutputInfo {
    ty: AtomType::Serial,
    width: 0,
    big_endian: false,
};

/// `ZL_SDDL_Instructions`: the dispatch plan a run produced.
#[derive(Debug)]
pub(super) struct Instructions {
    /// The size in bytes of every segment.
    pub(super) segment_sizes: Vec<usize>,
    /// The destination of every segment.
    pub(super) tags: Vec<u32>,
    /// One entry per destination, `0..nb_tags`.
    pub(super) outputs: Vec<OutputInfo>,
}

/// `ZL_SDDL_CachedInstructions`: the instructions one consumption of a
/// record appended.
#[derive(Debug)]
pub(super) struct CachedInstructions {
    lens: Vec<usize>,
    tags: Vec<u32>,
    total_size: usize,
}

/// `ZL_SDDL_State_exec`: runs `program` over `src`.
///
/// # Errors
///
/// Any failure of the description on this data: a violated `expect`, a read
/// past the end, an inconsistent destination, unconsumed input, ...
pub(super) fn run(program: &Program, src: &[u8]) -> Result<Instructions, SddlError> {
    let mut state = State {
        src,
        pos: 0,
        dests: Vec::new(),
        segment_sizes: Vec::new(),
        segment_tags: Vec::new(),
    };
    let globals = new_scope();
    for root in &program.exprs {
        state.exec_expr(&globals, root)?;
    }
    if state.pos != src.len() {
        return Err(SddlError::SrcTooLarge);
    }
    Ok(Instructions {
        segment_sizes: state.segment_sizes,
        tags: state.segment_tags,
        outputs: state.dests,
    })
}

struct State<'a> {
    src: &'a [u8],
    pos: usize,
    dests: Vec<OutputInfo>,
    segment_sizes: Vec<usize>,
    segment_tags: Vec<u32>,
}

fn new_scope() -> ScopeRef {
    Rc::new(RefCell::new(Scope {
        vars: BTreeMap::new(),
    }))
}

/// `ZL_SDDL_Scope_createCopy`.
fn copy_scope(scope: Option<&ScopeRef>) -> ScopeRef {
    Rc::new(RefCell::new(Scope {
        vars: scope.map_or_else(BTreeMap::new, |scope| scope.borrow().vars.clone()),
    }))
}

/// `ZL_SDDL_Scope_get`.
fn scope_get(scope: &ScopeRef, name: &Name) -> Result<Expr, SddlError> {
    scope
        .borrow()
        .vars
        .get(name)
        .cloned()
        .ok_or(SddlError::Corruption(
            "a variable was read without ever having been written",
        ))
}

/// `ZL_SDDL_Scope_set`.
fn scope_set(scope: &ScopeRef, name: &Name, value: &Expr) -> Result<Expr, SddlError> {
    if matches!(value, Expr::Var(_)) {
        return Err(SddlError::Corruption(
            "an unresolved variable cannot be assigned to a variable",
        ));
    }
    let mut scope = scope.borrow_mut();
    // The reference's map refuses every insertion once it is full, even to
    // overwrite an existing key.
    if scope.vars.len() >= VARIABLE_LIMIT {
        return Err(SddlError::Limit("too many variables in a scope"));
    }
    scope.vars.insert(Rc::clone(name), value.clone());
    Ok(value.clone())
}

/// The two numbers an arithmetic operation takes.
fn numbers(args: &[Expr; 2]) -> Result<(i64, i64), SddlError> {
    match args {
        [Expr::Num(a), Expr::Num(b)] => Ok((*a, *b)),
        _ => Err(SddlError::Corruption("an operation expects numbers")),
    }
}

/// The one number a unary operation takes.
fn number(args: &[Expr; 2]) -> Result<i64, SddlError> {
    match args[0] {
        Expr::Num(a) => Ok(a),
        _ => Err(SddlError::Corruption("an operation expects a number")),
    }
}

impl State<'_> {
    /// `ZL_SDDL_State_execExpr`.
    fn exec_expr(&mut self, scope: &ScopeRef, expr: &Expr) -> Result<Expr, SddlError> {
        match expr {
            Expr::Op(op) => self.exec_op(scope, op),
            Expr::Field(field) => self.exec_field(scope, field),
            Expr::Var(name) => match name.as_ref() {
                b"_rem" => Ok(Expr::Num(self.src.len() as i64 - self.pos as i64)),
                b"_pos" => Ok(Expr::Num(self.pos as i64)),
                _ => scope_get(scope, name),
            },
            Expr::Null
            | Expr::Num(_)
            | Expr::Dest(_)
            | Expr::Scope(_)
            | Expr::Tuple(_)
            | Expr::Func(_) => Ok(expr.clone()),
        }
    }

    /// `ZL_SDDL_State_execExpr_op`.
    fn exec_op(&mut self, scope: &ScopeRef, op: &Op) -> Result<Expr, SddlError> {
        let mut args = [Expr::Null, Expr::Null];
        for (index, slot) in args.iter_mut().enumerate().take(op.op.num_args()) {
            let arg = op.args[index]
                .as_deref()
                .ok_or(SddlError::Corruption("an operation lacks an argument"))?;
            *slot = match (op.op, index, arg) {
                (OpCode::Assign, 0, Expr::Var(_)) | (OpCode::Member, 1, Expr::Var(_)) => {
                    arg.clone()
                }
                _ => self.exec_expr(scope, arg)?,
            };
        }
        self.exec_op_inner(scope, op.op, &args)
    }

    /// `ZL_SDDL_State_execExpr_op_inner`.
    fn exec_op_inner(
        &mut self,
        scope: &ScopeRef,
        op: OpCode,
        args: &[Expr; 2],
    ) -> Result<Expr, SddlError> {
        let boolean = |value: bool| Ok(Expr::Num(i64::from(value)));
        match op {
            OpCode::Die => Err(SddlError::Generic("reached a die operation")),
            OpCode::Expect => {
                if number(args)? == 0 {
                    return Err(SddlError::Corruption(
                        "an expect operation got 0; failing the parse",
                    ));
                }
                Ok(Expr::Null)
            }
            OpCode::Log => Ok(args[0].clone()),
            OpCode::Consume => self.consume(&args[0]),
            OpCode::Sizeof => Ok(Expr::Num(self.sizeof_field(&args[0])?)),
            OpCode::Send => send(&args[0], &args[1]),
            OpCode::Assign => {
                let Expr::Var(name) = &args[0] else {
                    return Err(SddlError::Corruption("only a variable can be assigned to"));
                };
                scope_set(scope, name, &args[1])
            }
            OpCode::Member => match (&args[0], &args[1]) {
                (Expr::Scope(inner), Expr::Var(name)) => scope_get(inner, name),
                _ => Err(SddlError::Corruption(
                    "a member access needs a scope and a name",
                )),
            },
            OpCode::Bind => self.bind(scope, &args[0], &args[1]),
            OpCode::Neg => Ok(Expr::Num(number(args)?.wrapping_neg())),
            OpCode::Eq => numbers(args).and_then(|(a, b)| boolean(a == b)),
            OpCode::Ne => numbers(args).and_then(|(a, b)| boolean(a != b)),
            OpCode::Gt => numbers(args).and_then(|(a, b)| boolean(a > b)),
            OpCode::Ge => numbers(args).and_then(|(a, b)| boolean(a >= b)),
            OpCode::Lt => numbers(args).and_then(|(a, b)| boolean(a < b)),
            OpCode::Le => numbers(args).and_then(|(a, b)| boolean(a <= b)),
            OpCode::Add => numbers(args).map(|(a, b)| Expr::Num(a.wrapping_add(b))),
            OpCode::Sub => numbers(args).map(|(a, b)| Expr::Num(a.wrapping_sub(b))),
            OpCode::Mul => numbers(args).map(|(a, b)| Expr::Num(a.wrapping_mul(b))),
            OpCode::Div => {
                let (a, b) = numbers(args)?;
                if b == 0 {
                    return Err(SddlError::Corruption("cannot divide by zero"));
                }
                Ok(Expr::Num(a.wrapping_div(b)))
            }
            OpCode::Mod => {
                let (a, b) = numbers(args)?;
                if b == 0 {
                    return Err(SddlError::Corruption("the modulus cannot be zero"));
                }
                Ok(Expr::Num(a.wrapping_rem(b)))
            }
            OpCode::BitAnd => numbers(args).map(|(a, b)| Expr::Num(a & b)),
            OpCode::BitOr => numbers(args).map(|(a, b)| Expr::Num(a | b)),
            OpCode::BitXor => numbers(args).map(|(a, b)| Expr::Num(a ^ b)),
            OpCode::BitNot => Ok(Expr::Num(!number(args)?)),
            OpCode::LogAnd => numbers(args).and_then(|(a, b)| boolean(a != 0 && b != 0)),
            OpCode::LogOr => numbers(args).and_then(|(a, b)| boolean(a != 0 || b != 0)),
            OpCode::LogNot => boolean(number(args)? == 0),
        }
    }

    /// `ZL_SDDL_State_execExpr_bind`: applies arguments to a function.
    fn bind(&mut self, scope: &ScopeRef, func: &Expr, args: &Expr) -> Result<Expr, SddlError> {
        let (Expr::Func(func), Expr::Tuple(tuple)) = (func, args) else {
            return Err(SddlError::Corruption("a bind needs a function and a tuple"));
        };
        if tuple.len() > func.args.len() - func.bound {
            return Err(SddlError::Corruption(
                "a function received more arguments than it takes",
            ));
        }
        let bound_scope = copy_scope(func.scope.as_ref());
        for (index, arg) in tuple.iter().enumerate() {
            let value = self.exec_expr(scope, arg)?;
            scope_set(&bound_scope, &func.args[func.bound + index], &value)?;
        }
        Ok(Expr::Func(Func {
            exprs: Rc::clone(&func.exprs),
            args: Rc::clone(&func.args),
            bound: func.bound + tuple.len(),
            scope: Some(bound_scope),
        }))
    }

    /// `ZL_SDDL_State_execExpr_field`: resolves the expressions a field
    /// holds.
    fn exec_field(&mut self, scope: &ScopeRef, field: &Field) -> Result<Expr, SddlError> {
        let resolved = match field {
            Field::Poison => Field::Poison,
            Field::Atom(atom) => {
                let mut atom = atom.clone();
                if let Some(width_expr) = atom.width_expr.take() {
                    match self.exec_expr(scope, &width_expr)? {
                        Expr::Num(width) if width >= 0 => atom.width = width as usize,
                        _ => {
                            return Err(SddlError::Corruption(
                                "an atom width is not a non-negative number",
                            ));
                        }
                    }
                }
                Field::Atom(atom)
            }
            Field::Record(record) if record.dyn_set.is_some() => Field::Record(record.clone()),
            Field::Record(record) => {
                let mut exprs = Vec::with_capacity(record.exprs.len());
                let mut names = Vec::with_capacity(record.exprs.len());
                for expr in record.exprs.iter() {
                    let (name, field_expr) = record_member(expr);
                    names.push(name);
                    exprs.push(self.exec_expr(scope, field_expr)?);
                }
                Field::Record(Record {
                    exprs: Rc::clone(&record.exprs),
                    dyn_set: Some(Rc::new(super::program::DynSet {
                        exprs,
                        names,
                        instrs: RefCell::new(None),
                    })),
                })
            }
            Field::Array(array) if array.dyn_set.is_some() => Field::Array(array.clone()),
            Field::Array(array) => {
                let inner = self.exec_expr(scope, &array.expr)?;
                let len = self.exec_expr(scope, &array.len)?;
                Field::Array(Array {
                    expr: Rc::clone(&array.expr),
                    len: Rc::clone(&array.len),
                    dyn_set: Some(Rc::new(super::program::DynSet {
                        exprs: alloc::vec![inner, len],
                        names: Vec::new(),
                        instrs: RefCell::new(None),
                    })),
                })
            }
        };
        Ok(Expr::Field(resolved))
    }

    // ----- consuming -----

    /// `ZL_SDDL_State_consume`.
    fn consume(&mut self, expr: &Expr) -> Result<Expr, SddlError> {
        match expr {
            Expr::Field(field) => self.consume_field(field),
            Expr::Func(func) => self.consume_func(func),
            _ => Err(SddlError::Corruption(
                "only a field or function can be consumed",
            )),
        }
    }

    /// `ZL_SDDL_State_consumeField`.
    fn consume_field(&mut self, field: &Field) -> Result<Expr, SddlError> {
        match field {
            Field::Poison => Err(SddlError::Corruption("tried to consume a poison field")),
            Field::Atom(atom) => self.consume_atom(atom),
            Field::Record(record) => {
                let scope = new_scope();
                self.consume_record_with_scope(record, Some(&scope))?;
                Ok(Expr::Scope(scope))
            }
            Field::Array(array) => self.consume_array(array),
        }
    }

    /// `ZL_SDDL_State_consumeFunc`.
    fn consume_func(&mut self, func: &Func) -> Result<Expr, SddlError> {
        if func.bound != func.args.len() {
            return Err(SddlError::Corruption(
                "a function was consumed before receiving all its arguments",
            ));
        }
        let scope = copy_scope(func.scope.as_ref());
        for expr in func.exprs.iter() {
            self.exec_expr(&scope, expr)?;
        }
        Ok(Expr::Scope(scope))
    }

    /// `ZL_SDDL_State_updateDest`.
    fn update_dest(&mut self, tag: u32, atom: &Atom) -> Result<(), SddlError> {
        let tag = tag as usize;
        if tag >= self.dests.len() {
            if tag + 1 > DEST_LIMIT {
                return Err(SddlError::Limit("too many destinations"));
            }
            self.dests.resize(tag + 1, UNUSED);
        }
        let info = &mut self.dests[tag];
        if info.width == 0 {
            *info = OutputInfo {
                ty: atom.ty,
                width: atom.width,
                big_endian: atom.is_big_endian,
            };
            return Ok(());
        }
        if info.ty != atom.ty {
            return Err(SddlError::Generic(
                "cannot send different types to the same destination",
            ));
        }
        if atom.ty != AtomType::Serial {
            if info.width != atom.width {
                return Err(SddlError::Generic(
                    "cannot mix fields of different widths in the same destination",
                ));
            }
            if info.big_endian != atom.is_big_endian {
                return Err(SddlError::Generic(
                    "cannot mix fields of different endianness in the same destination",
                ));
            }
        }
        Ok(())
    }

    /// Adds `width` bytes for `tag`: onto the last instruction when it has
    /// the same tag, else as a new one.
    fn push_segment(&mut self, tag: u32, width: usize) -> Result<(), SddlError> {
        match (self.segment_tags.last(), self.segment_sizes.last_mut()) {
            (Some(&last), Some(size)) if last == tag => *size += width,
            _ => {
                if self.segment_sizes.len() >= SEGMENT_LIMIT {
                    return Err(SddlError::Limit("too many segments"));
                }
                self.segment_sizes.push(width);
                self.segment_tags.push(tag);
            }
        }
        Ok(())
    }

    /// `ZL_SDDL_State_readAtom`.
    fn read_atom(&self, atom: &Atom) -> Expr {
        let bytes = &self.src[self.pos..];
        match atom.ty {
            AtomType::Serial => Expr::Num(i64::from(bytes[0])),
            AtomType::Numeric if !atom.is_integer => Expr::Null,
            AtomType::Numeric => {
                let big = atom.is_big_endian;
                let signed = atom.is_signed;
                Expr::Num(match atom.width {
                    1 if signed => i64::from(bytes[0] as i8),
                    1 => i64::from(bytes[0]),
                    2 => {
                        let raw = [bytes[0], bytes[1]];
                        let value = if big {
                            u16::from_be_bytes(raw)
                        } else {
                            u16::from_le_bytes(raw)
                        };
                        if signed {
                            i64::from(value as i16)
                        } else {
                            i64::from(value)
                        }
                    }
                    4 => {
                        let raw = [bytes[0], bytes[1], bytes[2], bytes[3]];
                        let value = if big {
                            u32::from_be_bytes(raw)
                        } else {
                            u32::from_le_bytes(raw)
                        };
                        if signed {
                            i64::from(value as i32)
                        } else {
                            i64::from(value)
                        }
                    }
                    _ => {
                        let raw = [
                            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                            bytes[7],
                        ];
                        (if big {
                            u64::from_be_bytes(raw)
                        } else {
                            u64::from_le_bytes(raw)
                        }) as i64
                    }
                })
            }
        }
    }

    /// `ZL_SDDL_State_consumeAtom`.
    fn consume_atom(&mut self, atom: &Atom) -> Result<Expr, SddlError> {
        let width = atom.width;
        if width == 0 {
            return Ok(Expr::Null);
        }
        if self.pos.wrapping_add(width) > self.src.len() {
            return Err(SddlError::SrcTooSmall);
        }
        self.update_dest(atom.dest, atom)?;
        self.push_segment(atom.dest, width)?;
        let result = self.read_atom(atom);
        self.pos += width;
        Ok(result)
    }

    /// `ZL_SDDL_State_consumeArray_ofAtoms`.
    fn consume_array_of_atoms(&mut self, atom: &Atom, len: usize) -> Result<Expr, SddlError> {
        // The reference does not check this product for overflow.
        let width = atom.width.wrapping_mul(len);
        if width == 0 {
            return Ok(Expr::Null);
        }
        if self.pos.wrapping_add(width) > self.src.len() {
            return Err(SddlError::SrcTooSmall);
        }
        self.update_dest(atom.dest, atom)?;
        self.push_segment(atom.dest, width)?;
        self.pos += width;
        Ok(Expr::Null)
    }

    /// `ZL_SDDL_State_consumeRecord_withScope`: consumes every member,
    /// assigning named ones into `scope` when given.
    fn consume_record_with_scope(
        &mut self,
        record: &Record,
        scope: Option<&ScopeRef>,
    ) -> Result<(), SddlError> {
        let dyn_set = record
            .dyn_set
            .as_ref()
            .ok_or(SddlError::Generic("a record was consumed unresolved"))?;
        for (expr, name) in dyn_set.exprs.iter().zip(&dyn_set.names) {
            let Expr::Field(field) = expr else {
                return Err(SddlError::Corruption("a record member is not a field"));
            };
            let result = self.consume_field(field)?;
            if let (Some(scope), Some(name)) = (scope, name) {
                scope_set(scope, name, &result)?;
            }
        }
        Ok(())
    }

    /// `ZL_SDDL_State_applyCachedInstructions`.
    fn apply_cached(&mut self, instrs: &CachedInstructions) -> Result<(), SddlError> {
        if self.pos + instrs.total_size > self.src.len() {
            return Err(SddlError::SrcTooSmall);
        }
        if self.segment_tags.len() + instrs.tags.len() > SEGMENT_LIMIT {
            return Err(SddlError::Limit("too many segments"));
        }
        self.segment_sizes.extend_from_slice(&instrs.lens);
        self.segment_tags.extend_from_slice(&instrs.tags);
        self.pos += instrs.total_size;
        Ok(())
    }

    /// `ZL_SDDL_State_consumeRecord_noScope`.
    ///
    /// With `should_cache` the instructions the consumption appends are
    /// recorded in the record's expression set for later replays.
    ///
    /// When the record's first atom merged into the instruction before it,
    /// that instruction is part of the recorded copy and its length is
    /// reduced by the amount the record added, in the reference's own
    /// arithmetic: it subtracts the added amount from the merged length, so
    /// the replayed first instruction carries the length it had *before*
    /// the merge. This is reproduced faithfully; it is observable in the
    /// frame.
    fn consume_record_no_scope(
        &mut self,
        record: &Record,
        should_cache: bool,
    ) -> Result<(), SddlError> {
        if let Some(dyn_set) = &record.dyn_set
            && let Some(instrs) = dyn_set.instrs.borrow().as_ref()
        {
            return self.apply_cached(instrs);
        }
        if !should_cache {
            return self.consume_record_with_scope(record, None);
        }
        let start_pos = self.pos;
        let first_new = self.segment_tags.len();
        let last_len_before = first_new
            .checked_sub(1)
            .map_or(0, |last| self.segment_sizes[last]);
        self.consume_record_with_scope(record, None)?;
        let last_len_after = first_new
            .checked_sub(1)
            .map_or(0, |last| self.segment_sizes[last]);
        let first_offset = last_len_after - last_len_before;
        let mut start = first_new;
        if first_offset != 0 {
            start -= 1;
        }
        let mut lens = self.segment_sizes[start..].to_vec();
        let tags = self.segment_tags[start..].to_vec();
        if first_offset != 0 {
            lens[0] -= first_offset;
        }
        let Some(dyn_set) = &record.dyn_set else {
            return Err(SddlError::Generic("a record was consumed unresolved"));
        };
        *dyn_set.instrs.borrow_mut() = Some(CachedInstructions {
            lens,
            tags,
            total_size: self.pos - start_pos,
        });
        Ok(())
    }

    /// `ZL_SDDL_State_consumeArray`.
    fn consume_array(&mut self, array: &Array) -> Result<Expr, SddlError> {
        let dyn_set = array
            .dyn_set
            .as_ref()
            .ok_or(SddlError::Generic("an array was consumed unresolved"))?;
        let (inner, len) = (&dyn_set.exprs[0], &dyn_set.exprs[1]);
        if !matches!(inner, Expr::Field(_) | Expr::Func(_)) {
            return Err(SddlError::Corruption("an array element is not a field"));
        }
        let len = match len {
            Expr::Num(len) if *len >= 0 => *len as usize,
            _ => {
                return Err(SddlError::Corruption(
                    "an array length is not a non-negative number",
                ));
            }
        };
        if let Expr::Field(field) = inner {
            match field {
                Field::Atom(atom) => return self.consume_array_of_atoms(atom, len),
                Field::Record(record) if len > 1 => {
                    self.consume_record_no_scope(record, true)?;
                    for _ in 1..len {
                        self.consume_record_no_scope(record, false)?;
                    }
                    return Ok(Expr::Null);
                }
                _ => {}
            }
        }
        for _ in 0..len {
            self.consume(inner)?;
        }
        Ok(Expr::Null)
    }

    // ----- sizeof -----

    /// `ZL_SDDL_State_sizeofField`.
    fn sizeof_field(&self, expr: &Expr) -> Result<i64, SddlError> {
        let Expr::Field(field) = expr else {
            return Err(SddlError::Corruption("sizeof needs a field"));
        };
        match field {
            Field::Poison => Ok(0),
            Field::Atom(atom) => Ok(atom.width as i64),
            Field::Record(record) => {
                let dyn_set = record
                    .dyn_set
                    .as_ref()
                    .ok_or(SddlError::Generic("sizeof of an unresolved record"))?;
                let mut total = 0i64;
                for member in &dyn_set.exprs {
                    if !matches!(member, Expr::Field(_)) {
                        return Err(SddlError::Corruption("a record member is not a field"));
                    }
                    total = total.wrapping_add(self.sizeof_field(member)?);
                }
                Ok(total)
            }
            Field::Array(array) => {
                let dyn_set = array
                    .dyn_set
                    .as_ref()
                    .ok_or(SddlError::Generic("sizeof of an unresolved array"))?;
                let (inner, len) = (&dyn_set.exprs[0], &dyn_set.exprs[1]);
                if !matches!(inner, Expr::Field(_)) {
                    return Err(SddlError::Corruption("an array element is not a field"));
                }
                let Expr::Num(len) = len else {
                    return Err(SddlError::Corruption("an array length is not a number"));
                };
                if *len < 0 {
                    return Err(SddlError::Corruption("an array length is negative"));
                }
                Ok(self.sizeof_field(inner)?.wrapping_mul(*len))
            }
        }
    }
}

/// How a record member is evaluated: `(name, expression)`. A member that
/// assigns a consumption, `x = consume(f)`, names its field; a bare
/// `consume(f)` evaluates `f`; anything else is evaluated as written.
fn record_member(expr: &Expr) -> (Option<Name>, &Expr) {
    if let Expr::Op(op) = expr {
        match (op.op, &op.args) {
            (OpCode::Assign, [Some(lhs), Some(rhs)]) => {
                if let (Expr::Var(name), Expr::Op(inner)) = (&**lhs, &**rhs)
                    && inner.op == OpCode::Consume
                    && let Some(field_expr) = inner.args[0].as_deref()
                {
                    return (Some(Rc::clone(name)), field_expr);
                }
            }
            (OpCode::Consume, [Some(field_expr), _]) => return (None, field_expr),
            _ => {}
        }
    }
    (None, expr)
}

/// `ZL_SDDL_State_execExpr_send`.
fn send(field: &Expr, dest: &Expr) -> Result<Expr, SddlError> {
    let (Expr::Field(field), Expr::Dest(dest)) = (field, dest) else {
        return Err(SddlError::Corruption(
            "a send needs a field and a destination",
        ));
    };
    match field {
        Field::Atom(atom) => Ok(Expr::Field(Field::Atom(Atom {
            dest: *dest,
            ..atom.clone()
        }))),
        _ => Err(SddlError::Corruption("cannot send a non-atom field")),
    }
}
