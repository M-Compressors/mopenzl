//! Hand-built SDDL descriptions (CBOR) and SDDL2 bytecode for the tests.
#![allow(dead_code, reason = "each test uses a different subset")]

/// A small CBOR encoder.
pub mod cbor {
    pub fn head(major: u8, n: u64) -> Vec<u8> {
        let major = major << 5;
        match n {
            0..=23 => vec![major | n as u8],
            24..=0xFF => vec![major | 24, n as u8],
            0x100..=0xFFFF => [vec![major | 25], (n as u16).to_be_bytes().to_vec()].concat(),
            0x1_0000..=0xFFFF_FFFF => {
                [vec![major | 26], (n as u32).to_be_bytes().to_vec()].concat()
            }
            _ => [vec![major | 27], n.to_be_bytes().to_vec()].concat(),
        }
    }

    pub fn int(n: i64) -> Vec<u8> {
        if n >= 0 {
            head(0, n as u64)
        } else {
            head(1, !n as u64)
        }
    }

    pub fn text(s: &str) -> Vec<u8> {
        [head(3, s.len() as u64), s.as_bytes().to_vec()].concat()
    }

    pub fn null() -> Vec<u8> {
        vec![0xF6]
    }

    pub fn arr(items: Vec<Vec<u8>>) -> Vec<u8> {
        [head(4, items.len() as u64), items.concat()].concat()
    }

    pub fn map(pairs: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
        let count = pairs.len() as u64;
        let body: Vec<u8> = pairs
            .into_iter()
            .flat_map(|(k, v)| [k, v].concat())
            .collect();
        [head(5, count), body].concat()
    }
}

use cbor::{arr, int, map, null, text};

/// `{kind: value}`.
pub fn expr(kind: &str, value: Vec<u8>) -> Vec<u8> {
    map(vec![(text(kind), value)])
}

/// An expression with a `dbg` location.
pub fn with_loc(expression: Vec<u8>, start: i64, size: i64) -> Vec<u8> {
    // Re-encode as a two-pair map: the first pair of `expression` plus dbg.
    assert_eq!(expression[0], 0xA1, "a single-pair map");
    let mut out = vec![0xA2];
    out.extend_from_slice(&expression[1..]);
    out.extend(text("dbg"));
    out.extend(map(vec![(text("loc"), arr(vec![int(start), int(size)]))]));
    out
}

pub fn atom(name: &str) -> Vec<u8> {
    expr("atom", text(name))
}
pub fn dyn_atom(width: Vec<u8>) -> Vec<u8> {
    expr("atom", width)
}
pub fn dest() -> Vec<u8> {
    expr("dest", null())
}
pub fn send(field: Vec<u8>, dest: Vec<u8>) -> Vec<u8> {
    expr("send", arr(vec![field, dest]))
}
pub fn consume(x: Vec<u8>) -> Vec<u8> {
    expr("consume", x)
}
pub fn record(items: Vec<Vec<u8>>) -> Vec<u8> {
    expr("record", arr(items))
}
pub fn array(element: Vec<u8>, len: Vec<u8>) -> Vec<u8> {
    expr("array", arr(vec![element, len]))
}
pub fn num(n: i64) -> Vec<u8> {
    expr("int", int(n))
}
pub fn var(name: &str) -> Vec<u8> {
    expr("var", text(name))
}
pub fn assign(name: &str, x: Vec<u8>) -> Vec<u8> {
    expr("assign", arr(vec![var(name), x]))
}
pub fn op2(name: &str, a: Vec<u8>, b: Vec<u8>) -> Vec<u8> {
    expr(name, arr(vec![a, b]))
}
pub fn op1(name: &str, a: Vec<u8>) -> Vec<u8> {
    expr(name, a)
}
pub fn member(scope: Vec<u8>, name: &str) -> Vec<u8> {
    expr("member", arr(vec![scope, var(name)]))
}
pub fn tuple(items: Vec<Vec<u8>>) -> Vec<u8> {
    expr("tuple", arr(items))
}
pub fn func(args: &[&str], body: Vec<Vec<u8>>) -> Vec<u8> {
    expr(
        "func",
        arr(vec![arr(args.iter().map(|a| var(a)).collect()), arr(body)]),
    )
}
pub fn bind(f: Vec<u8>, args: Vec<Vec<u8>>) -> Vec<u8> {
    expr("bind", arr(vec![f, tuple(args)]))
}
pub fn die() -> Vec<u8> {
    expr("die", null())
}
pub fn poison() -> Vec<u8> {
    expr("poison", text("no"))
}

/// A compiled program: `{"exprs": [...]}`.
pub fn program(exprs: Vec<Vec<u8>>) -> Vec<u8> {
    map(vec![(text("exprs"), arr(exprs))])
}

/// `d<i> = dest` for `i` in `0..n`: shared destinations.
pub fn dests(n: usize) -> Vec<Vec<u8>> {
    (0..n).map(|i| assign(&format!("d{i}"), dest())).collect()
}

/// `send(atom(name), d<i>)`.
pub fn field(name: &str, d: usize) -> Vec<u8> {
    send(atom(name), var(&format!("d{d}")))
}

/// `consume(field(name, d))`.
pub fn eat(name: &str, d: usize) -> Vec<u8> {
    consume(field(name, d))
}

// ----- SDDL2 assembler -----

/// Opcode words of `sddl2_opcodes.def` (family << 16 | opcode).
const OPCODES: &[(&str, u32)] = &[
    ("push.zero", 0x0001_0001),
    ("push.u32", 0x0001_0002),
    ("push.i32", 0x0001_0003),
    ("push.i64", 0x0001_0004),
    ("push.tag", 0x0001_0005),
    ("push.current_pos", 0x0001_0080),
    ("push.remaining", 0x0001_0081),
    ("push.stack_depth", 0x0001_0082),
    ("push.type.bytes", 0x0001_0100),
    ("push.type.u8", 0x0001_0110),
    ("push.type.i8", 0x0001_0111),
    ("push.type.u16le", 0x0001_0112),
    ("push.type.u16be", 0x0001_0113),
    ("push.type.i16le", 0x0001_0114),
    ("push.type.i16be", 0x0001_0115),
    ("push.type.u32le", 0x0001_0116),
    ("push.type.u32be", 0x0001_0117),
    ("push.type.i32le", 0x0001_0118),
    ("push.type.i32be", 0x0001_0119),
    ("push.type.u64le", 0x0001_011A),
    ("push.type.u64be", 0x0001_011B),
    ("push.type.i64le", 0x0001_011C),
    ("push.type.i64be", 0x0001_011D),
    ("push.type.f8", 0x0001_0130),
    ("push.type.f16le", 0x0001_0131),
    ("push.type.f16be", 0x0001_0132),
    ("push.type.bf16le", 0x0001_0133),
    ("push.type.bf16be", 0x0001_0134),
    ("push.type.f32le", 0x0001_0135),
    ("push.type.f32be", 0x0001_0136),
    ("push.type.f64le", 0x0001_0137),
    ("push.type.f64be", 0x0001_0138),
    ("math.add", 0x0002_0001),
    ("math.sub", 0x0002_0002),
    ("math.mul", 0x0002_0003),
    ("math.div", 0x0002_0004),
    ("math.mod", 0x0002_0005),
    ("math.abs", 0x0002_0006),
    ("math.neg", 0x0002_0007),
    ("math.bit_and", 0x0002_0008),
    ("math.bit_or", 0x0002_0009),
    ("math.bit_xor", 0x0002_000A),
    ("math.bit_not", 0x0002_000B),
    ("cmp.eq", 0x0003_0001),
    ("cmp.ne", 0x0003_0002),
    ("cmp.lt", 0x0003_0003),
    ("cmp.le", 0x0003_0004),
    ("cmp.gt", 0x0003_0005),
    ("cmp.ge", 0x0003_0006),
    ("logic.and", 0x0004_0001),
    ("logic.or", 0x0004_0002),
    ("logic.xor", 0x0004_0003),
    ("logic.not", 0x0004_0004),
    ("halt", 0x0005_0001),
    ("expect_true", 0x0005_0002),
    ("jump_if", 0x0005_0003),
    ("trace.start", 0x0005_0004),
    ("load.u8", 0x0006_0001),
    ("load.i8", 0x0006_0002),
    ("load.u16le", 0x0006_0010),
    ("load.i16le", 0x0006_0011),
    ("load.u32le", 0x0006_0020),
    ("load.i32le", 0x0006_0021),
    ("load.i64le", 0x0006_0030),
    ("load.u16be", 0x0006_0110),
    ("load.i16be", 0x0006_0111),
    ("load.u32be", 0x0006_0120),
    ("load.i32be", 0x0006_0121),
    ("load.i64be", 0x0006_0130),
    ("stack.dup", 0x0007_0001),
    ("stack.over", 0x0007_0002),
    ("stack.drop", 0x0007_0003),
    ("stack.swap", 0x0007_0004),
    ("stack.rot", 0x0007_0005),
    ("stack.drop_if", 0x0007_0010),
    ("type.fixed_array", 0x0008_0001),
    ("type.structure", 0x0008_0002),
    ("type.sizeof", 0x0008_0010),
    ("var.store", 0x0009_0001),
    ("var.load", 0x0009_0002),
    ("call", 0x000B_0001),
    ("segment.create_unspecified", 0x000C_0001),
    ("segment.create_tagged", 0x000C_0002),
];

/// Assembles `source`: one instruction per line (`#` comments), with the
/// immediate of `push.u32`, `push.i32`, `push.i64` and `push.tag` after the
/// mnemonic. `word 0x...` emits a raw word.
pub fn assemble(source: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for line in source.lines() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let name = parts.next().unwrap();
        if name == "word" {
            let value = parts.next().unwrap();
            let value = u32::from_str_radix(value.trim_start_matches("0x"), 16).unwrap();
            out.extend(value.to_le_bytes());
            continue;
        }
        let word = OPCODES
            .iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("unknown mnemonic {name}"))
            .1;
        out.extend(word.to_le_bytes());
        match name {
            "push.u32" | "push.tag" => {
                let v: u32 = parts.next().unwrap().parse().unwrap();
                out.extend(v.to_le_bytes());
            }
            "push.i32" => {
                let v: i32 = parts.next().unwrap().parse().unwrap();
                out.extend(v.to_le_bytes());
            }
            "push.i64" => {
                let v: i64 = parts.next().unwrap().parse().unwrap();
                out.extend(v.to_le_bytes());
            }
            _ => {}
        }
    }
    out
}
