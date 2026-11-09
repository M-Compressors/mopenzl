//! The compiled SDDL program: expression types and their deserialization
//! (`ZL_SDDL_Program_load` and the `ZL_SDDL_Program_decodeExpr*` family of
//! `compress/graphs/sddl/simple_data_description_language.c`).
//!
//! A program is a CBOR map with an `exprs` array (and an optional `src`
//! string, kept for the reference's error messages and ignored here). Every
//! expression is a map whose first pair names its kind; an optional second
//! pair `dbg` carries a source location, validated and then ignored.
//!
//! Destinations (`dest`) are numbered in the order the decoder meets them,
//! which is the order the CBOR is laid out in.

use alloc::rc::Rc;
use alloc::vec::Vec;

use super::error::SddlError;
use crate::shared::cbor::{DecoderConfig, Item, decode};

/// A variable or atom name, compared bytewise.
pub(super) type Name = Rc<[u8]>;

/// An SDDL operation (`ZL_SDDL_OpCode`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum OpCode {
    Die,
    Expect,
    Log,
    Consume,
    Sizeof,
    Send,
    Assign,
    Member,
    Bind,
    Neg,
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    BitNot,
    LogAnd,
    LogOr,
    LogNot,
}

impl OpCode {
    /// The name the compiled form uses for the operation.
    fn from_name(name: &[u8]) -> Option<Self> {
        Some(match name {
            b"die" => Self::Die,
            b"expect" => Self::Expect,
            b"log" => Self::Log,
            b"consume" => Self::Consume,
            b"sizeof" => Self::Sizeof,
            b"send" => Self::Send,
            b"assign" => Self::Assign,
            b"member" => Self::Member,
            b"bind" => Self::Bind,
            b"neg" => Self::Neg,
            b"eq" => Self::Eq,
            b"ne" => Self::Ne,
            b"gt" => Self::Gt,
            b"ge" => Self::Ge,
            b"lt" => Self::Lt,
            b"le" => Self::Le,
            b"add" => Self::Add,
            b"sub" => Self::Sub,
            b"mul" => Self::Mul,
            b"div" => Self::Div,
            b"mod" => Self::Mod,
            b"bit_and" => Self::BitAnd,
            b"bit_or" => Self::BitOr,
            b"bit_xor" => Self::BitXor,
            b"bit_not" => Self::BitNot,
            b"log_and" => Self::LogAnd,
            b"log_or" => Self::LogOr,
            b"log_not" => Self::LogNot,
            _ => return None,
        })
    }

    /// `ZL_SDDL_OpCode_numArgs`.
    pub(super) const fn num_args(self) -> usize {
        match self {
            Self::Die => 0,
            Self::Expect
            | Self::Log
            | Self::Consume
            | Self::Sizeof
            | Self::Neg
            | Self::BitNot
            | Self::LogNot => 1,
            _ => 2,
        }
    }
}

/// An operation and its arguments.
#[derive(Clone, Debug)]
pub(super) struct Op {
    pub(super) op: OpCode,
    pub(super) args: [Option<Rc<Expr>>; 2],
}

/// The kind of a built-in atom's data.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum AtomType {
    /// `ZL_Type_serial`.
    Serial,
    /// `ZL_Type_numeric`.
    Numeric,
}

/// `ZL_SDDL_Field_Atom`.
#[derive(Clone, Debug)]
pub(super) struct Atom {
    /// A width still to evaluate, for atoms declared with an expression.
    pub(super) width_expr: Option<Rc<Expr>>,
    pub(super) width: usize,
    pub(super) ty: AtomType,
    pub(super) is_integer: bool,
    pub(super) is_signed: bool,
    pub(super) is_big_endian: bool,
    /// The output stream the atom is sent to.
    pub(super) dest: u32,
}

/// The expressions a record or array resolved to at execution time
/// (`ZL_SDDL_DynExprSet`), shared by every copy of the field.
#[derive(Debug)]
pub(super) struct DynSet {
    pub(super) exprs: Vec<Expr>,
    /// For records: the variable each member's value is assigned to.
    pub(super) names: Vec<Option<Name>>,
    /// The instructions replaying one consumption of a record.
    pub(super) instrs: core::cell::RefCell<Option<super::state::CachedInstructions>>,
}

/// `ZL_SDDL_Field_Record`.
#[derive(Clone, Debug)]
pub(super) struct Record {
    pub(super) exprs: Rc<[Rc<Expr>]>,
    pub(super) dyn_set: Option<Rc<DynSet>>,
}

/// `ZL_SDDL_Field_Array`.
#[derive(Clone, Debug)]
pub(super) struct Array {
    pub(super) expr: Rc<Expr>,
    pub(super) len: Rc<Expr>,
    pub(super) dyn_set: Option<Rc<DynSet>>,
}

/// `ZL_SDDL_Field`.
#[derive(Clone, Debug)]
pub(super) enum Field {
    /// Fails the parse when consumed.
    Poison,
    Atom(Atom),
    Record(Record),
    Array(Array),
}

/// A scope: the variables of a record, a function call or the program.
#[derive(Debug, Default)]
pub(super) struct Scope {
    pub(super) vars: alloc::collections::BTreeMap<Name, Expr>,
}

/// A shared scope.
pub(super) type ScopeRef = Rc<core::cell::RefCell<Scope>>;

/// `ZL_SDDL_Func`: a function, possibly with some arguments bound.
#[derive(Clone, Debug)]
pub(super) struct Func {
    pub(super) exprs: Rc<[Expr]>,
    pub(super) args: Rc<[Name]>,
    /// How many of `args` are bound already.
    pub(super) bound: usize,
    pub(super) scope: Option<ScopeRef>,
}

/// `ZL_SDDL_Expr`.
#[derive(Clone, Debug)]
pub(super) enum Expr {
    Null,
    Op(Op),
    Num(i64),
    Field(Field),
    Dest(u32),
    Var(Name),
    Scope(ScopeRef),
    Tuple(Rc<[Expr]>),
    Func(Func),
}

/// A loaded program.
#[derive(Debug)]
pub(super) struct Program {
    pub(super) exprs: Vec<Expr>,
}

/// `ZL_SDDL_Program_load`.
///
/// # Errors
///
/// A description that is not valid CBOR, lacks the `exprs` array or holds a
/// malformed expression.
pub(super) fn load(source: &[u8]) -> Result<Program, SddlError> {
    let root = decode(source, &DecoderConfig::STRICT)?;
    if !matches!(root, Item::Map(_)) {
        return Err(SddlError::Corruption("the description is not a map"));
    }
    if let Some(src) = root.map_get_str("src")
        && !matches!(src, Item::Text(_))
    {
        return Err(SddlError::Corruption("the source text is not a string"));
    }
    let Some(Item::Array(items)) = root.map_get_str("exprs") else {
        return Err(SddlError::Corruption("the description has no expressions"));
    };
    let mut decoder = Decoder { num_dests: 0 };
    let exprs = items
        .iter()
        .map(|item| decoder.expr(item))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Program { exprs })
}

struct Decoder {
    num_dests: u32,
}

impl Decoder {
    /// `ZL_SDDL_Program_decodeExpr`.
    fn boxed(&mut self, item: &Item<'_>) -> Result<Rc<Expr>, SddlError> {
        self.expr(item).map(Rc::new)
    }

    /// `ZL_SDDL_Program_decodeExpr_inner`.
    fn expr(&mut self, item: &Item<'_>) -> Result<Expr, SddlError> {
        let Item::Map(pairs) = item else {
            return Err(SddlError::Corruption("an expression is not a map"));
        };
        if pairs.is_empty() || pairs.len() > 2 {
            return Err(SddlError::Corruption("an expression map has a bad size"));
        }
        let (Item::Text(kind), value) = (&pairs[0].0, &pairs[0].1) else {
            return Err(SddlError::Corruption("an expression kind is not a string"));
        };
        let expr = match kind.as_ref() {
            b"int" => match value {
                Item::Int(number) => Expr::Num(*number),
                _ => return Err(SddlError::Corruption("a number is not an integer")),
            },
            b"poison" => self.poison(value)?,
            b"atom" => Expr::Field(Field::Atom(self.atom(value)?)),
            b"record" => {
                let Item::Array(items) = value else {
                    return Err(SddlError::Corruption("a record is not an array"));
                };
                let exprs = items
                    .iter()
                    .map(|item| self.boxed(item))
                    .collect::<Result<Rc<[_]>, _>>()?;
                Expr::Field(Field::Record(Record {
                    exprs,
                    dyn_set: None,
                }))
            }
            b"array" => {
                let Item::Array(items) = value else {
                    return Err(SddlError::Corruption("an array is not an array"));
                };
                if items.len() != 2 {
                    return Err(SddlError::Corruption("an array needs a type and a length"));
                }
                let expr = self.boxed(&items[0])?;
                let len = self.boxed(&items[1])?;
                Expr::Field(Field::Array(Array {
                    expr,
                    len,
                    dyn_set: None,
                }))
            }
            b"dest" => {
                if !matches!(value, Item::Null) {
                    return Err(SddlError::Corruption("a destination must be null"));
                }
                let dest = self.num_dests;
                self.num_dests += 1;
                Expr::Dest(dest)
            }
            b"var" => Expr::Var(name(value)?),
            b"tuple" => {
                let Item::Array(items) = value else {
                    return Err(SddlError::Corruption("a tuple is not an array"));
                };
                let exprs = items
                    .iter()
                    .map(|item| self.expr(item))
                    .collect::<Result<Rc<[_]>, _>>()?;
                Expr::Tuple(exprs)
            }
            b"func" => self.func(value)?,
            other => match OpCode::from_name(other) {
                Some(op) => Expr::Op(self.op(op, value)?),
                None => return Err(SddlError::Corruption("unknown expression kind")),
            },
        };
        if pairs.len() == 2 {
            debug_info(&pairs[1])?;
        }
        Ok(expr)
    }

    /// `ZL_SDDL_Program_decodeExpr_op`.
    fn op(&mut self, op: OpCode, desc: &Item<'_>) -> Result<Op, SddlError> {
        let expected = op.num_args();
        let mut args = [None, None];
        match desc {
            Item::Null if expected == 0 => {}
            Item::Map(_) if expected == 1 => args[0] = Some(self.boxed(desc)?),
            Item::Array(items) => {
                if items.len() != expected {
                    return Err(SddlError::Corruption("an operation has the wrong arity"));
                }
                for (slot, item) in args.iter_mut().zip(items) {
                    *slot = Some(self.boxed(item)?);
                }
            }
            _ => return Err(SddlError::Corruption("an operation is not an array")),
        }
        Ok(Op { op, args })
    }

    /// `ZL_SDDL_Program_decodeExpr_field_poison`.
    fn poison(&mut self, desc: &Item<'_>) -> Result<Expr, SddlError> {
        match desc {
            Item::Text(_) | Item::Null => Ok(Expr::Field(Field::Poison)),
            _ => Err(SddlError::Corruption("unsupported poison description")),
        }
    }

    /// `ZL_SDDL_Program_decodeExpr_field_atom`.
    fn atom(&mut self, desc: &Item<'_>) -> Result<Atom, SddlError> {
        let Item::Text(type_name) = desc else {
            let width_expr = self.boxed(desc)?;
            return Ok(Atom {
                width_expr: Some(width_expr),
                width: 0,
                ty: AtomType::Serial,
                is_integer: false,
                is_signed: false,
                is_big_endian: false,
                dest: 0,
            });
        };
        let (ty, width, is_integer, is_signed, is_big_endian) =
            builtin(type_name).ok_or(SddlError::Corruption("unrecognized built-in type name"))?;
        Ok(Atom {
            width_expr: None,
            width,
            ty,
            is_integer,
            is_signed,
            is_big_endian,
            dest: 0,
        })
    }

    /// `ZL_SDDL_Program_decodeExpr_func`.
    fn func(&mut self, desc: &Item<'_>) -> Result<Expr, SddlError> {
        let Item::Array(parts) = desc else {
            return Err(SddlError::Corruption("a function is not an array"));
        };
        let [Item::Array(arg_items), Item::Array(expr_items)] = parts.as_slice() else {
            return Err(SddlError::Corruption(
                "a function needs arguments and a body",
            ));
        };
        let mut args = Vec::with_capacity(arg_items.len());
        for item in arg_items {
            match self.expr(item)? {
                Expr::Var(name) => args.push(name),
                _ => {
                    return Err(SddlError::Corruption(
                        "a function argument is not a variable",
                    ));
                }
            }
        }
        let exprs = expr_items
            .iter()
            .map(|item| self.expr(item))
            .collect::<Result<Rc<[_]>, _>>()?;
        Ok(Expr::Func(Func {
            exprs,
            args: args.into(),
            bound: 0,
            scope: None,
        }))
    }
}

/// A variable name.
fn name(desc: &Item<'_>) -> Result<Name, SddlError> {
    match desc {
        Item::Text(text) => Ok(Rc::from(text.as_ref())),
        _ => Err(SddlError::Corruption("a variable name is not a string")),
    }
}

/// `ZL_SDDL_Program_decodeExpr_addDebugInfo` and the `dbg` key check.
fn debug_info(pair: &(Item<'_>, Item<'_>)) -> Result<(), SddlError> {
    if !matches!(&pair.0, Item::Text(key) if key.as_ref() == b"dbg") {
        return Err(SddlError::Corruption("the second pair is not debug info"));
    }
    if !matches!(pair.1, Item::Map(_)) {
        return Err(SddlError::Corruption("debug info is not a map"));
    }
    if let Some(loc) = pair.1.map_get_str("loc") {
        let Item::Array(parts) = loc else {
            return Err(SddlError::Corruption("a location is not an array"));
        };
        let [Item::Int(start), Item::Int(size)] = parts.as_slice() else {
            return Err(SddlError::Corruption(
                "a location is not a pair of integers",
            ));
        };
        if *start < 0 || *size < 0 {
            return Err(SddlError::Corruption("a location is negative"));
        }
    }
    Ok(())
}

/// The built-in atom named `name`: type, width, integer, signed, big endian.
fn builtin(name: &[u8]) -> Option<(AtomType, usize, bool, bool, bool)> {
    use AtomType::{Numeric, Serial};
    if name == b"byte" {
        return Some((Serial, 1, true, false, false));
    }
    // `[bf|f|i|u]<width>[l|b]` with an implicit endianness for one byte.
    let (prefix, rest) = if let Some(rest) = name.strip_prefix(b"bf") {
        (b'B', rest)
    } else {
        let (&first, rest) = name.split_first()?;
        (first, rest)
    };
    let (width, big_endian) = match rest {
        b"1" => (1, false),
        [digit @ (b'2' | b'4' | b'8'), endian @ (b'l' | b'b')] => {
            (usize::from(digit - b'0'), *endian == b'b')
        }
        _ => return None,
    };
    let (is_integer, is_signed) = match prefix {
        b'i' => (true, true),
        b'u' => (true, false),
        b'f' | b'B' => (false, true),
        _ => return None,
    };
    Some((Numeric, width, is_integer, is_signed, big_endian))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_names_map_to_the_reference_table() {
        use AtomType::{Numeric, Serial};
        assert_eq!(builtin(b"byte"), Some((Serial, 1, true, false, false)));
        assert_eq!(builtin(b"i1"), Some((Numeric, 1, true, true, false)));
        assert_eq!(builtin(b"u1"), Some((Numeric, 1, true, false, false)));
        assert_eq!(builtin(b"u2b"), Some((Numeric, 2, true, false, true)));
        assert_eq!(builtin(b"i4l"), Some((Numeric, 4, true, true, false)));
        assert_eq!(builtin(b"u8b"), Some((Numeric, 8, true, false, true)));
        assert_eq!(builtin(b"f1"), Some((Numeric, 1, false, true, false)));
        assert_eq!(builtin(b"f8b"), Some((Numeric, 8, false, true, true)));
        assert_eq!(builtin(b"bf1"), Some((Numeric, 1, false, true, false)));
        assert_eq!(builtin(b"bf2l"), Some((Numeric, 2, false, true, false)));
        for bad in [
            &b""[..],
            b"i3l",
            b"i2",
            b"u1l",
            b"x4l",
            b"bf",
            b"i4x",
            b"i8lx",
        ] {
            assert_eq!(builtin(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn every_opcode_name_and_arity_matches_the_reference() {
        let unary = [
            "expect", "log", "consume", "sizeof", "neg", "bit_not", "log_not",
        ];
        let binary = [
            "send", "assign", "member", "bind", "eq", "ne", "gt", "ge", "lt", "le", "add", "sub",
            "mul", "div", "mod", "bit_and", "bit_or", "bit_xor", "log_and", "log_or",
        ];
        assert_eq!(OpCode::from_name(b"die").map(OpCode::num_args), Some(0));
        for name in unary {
            assert_eq!(
                OpCode::from_name(name.as_bytes()).map(OpCode::num_args),
                Some(1),
                "{name}"
            );
        }
        for name in binary {
            assert_eq!(
                OpCode::from_name(name.as_bytes()).map(OpCode::num_args),
                Some(2),
                "{name}"
            );
        }
        assert_eq!(OpCode::from_name(b"nope"), None);
    }
}
