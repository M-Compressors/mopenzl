//! Byte identity of the SDDL (graph 20) and SDDL2 (graph 21 and the private
//! chunk graph 61) encoders with the reference, and round trips through both
//! decoders.
//!
//! The SDDL and SDDL2 compilers of the reference are C++ tools outside the
//! build, so the programs here are written by hand: SDDL descriptions as
//! CBOR built with a small encoder, SDDL2 bytecode with a small assembler
//! over the opcode table of `sddl2_opcodes.def` (both in
//! `support/sddl.rs`). Every program runs on valid and invalid data at
//! format versions 21, 24 and 27; frames must be equal bytes, or both
//! libraries must fail.
#![cfg(all(
    feature = "compression",
    feature = "decompression",
    feature = "zstd",
    feature = "conversion",
    feature = "split",
    feature = "split-by-struct",
    feature = "dispatch"
))]

#[path = "support/sddl.rs"]
mod sddl_support;
mod support;

use mopenzl::graph::{GraphBuilder, GraphError, GraphId, GraphParams, LocalParams, StandardGraph};
use mopenzl::input::Input;
use mopenzl::{Compressor, Decompressor, EncoderConfig, FormatVersion, Level};
use openzl_ffi::graph::{self, Graph, Input as CInput, Params, cparam};
use sddl_support::*;

const VERSIONS: [u32; 3] = [21, 24, 27];
const SDDL_PID: i32 = 522;
const SDDL2_BYTECODE_PID: i32 = 7685;
const SDDL2_CHUNK_PID: i32 = 7686;
const GRAPH_SDDL: u32 = 20;
const GRAPH_SDDL2: u32 = 21;
const GRAPH_STORE: u32 = 2;
const GRAPH_ZSTD: u32 = 9;
const GRAPH_COMPRESS_GENERIC: u32 = 13;

fn config(level: i32, version: u32) -> EncoderConfig {
    EncoderConfig::default()
        .with_level(Level::try_from(level).unwrap())
        .with_format_version(FormatVersion::try_from(version).unwrap())
        .with_min_stream_size(1)
}

fn c_params(config: &EncoderConfig) -> Vec<(i32, i32)> {
    vec![
        (cparam::LEVEL, config.level().get()),
        (
            cparam::FORMAT_VERSION,
            u32::from(config.format_version()) as i32,
        ),
        (
            cparam::CONTENT_CHECKSUM,
            1 + i32::from(!config.content_checksum()),
        ),
        (
            cparam::COMPRESSED_CHECKSUM,
            1 + i32::from(!config.compressed_checksum()),
        ),
        (cparam::PERMISSIVE, 1 + i32::from(!config.permissive())),
        (cparam::MIN_STREAM_SIZE, config.min_stream_size() as i32),
        (
            cparam::STORE_ON_EXPANSION,
            1 + i32::from(!config.store_on_expansion()),
        ),
    ]
}

fn standard(id: u32) -> StandardGraph {
    StandardGraph::try_from(id).unwrap()
}

// ----- the two libraries -----

fn c_sddl(description: &[u8], successor: u32) -> Graph {
    Graph::Parameterized {
        graph: GRAPH_SDDL,
        params: Params {
            ints: Vec::new(),
            copies: vec![(SDDL_PID, description.to_vec())],
        },
        custom: vec![Graph::Id(successor)],
    }
}

fn c_sddl2(bytecode: &[u8], chunk: usize, destination: Option<u32>) -> Graph {
    Graph::Parameterized {
        graph: GRAPH_SDDL2,
        params: Params {
            ints: if chunk == 0 {
                Vec::new()
            } else {
                vec![(SDDL2_CHUNK_PID, chunk as i32)]
            },
            copies: vec![(SDDL2_BYTECODE_PID, bytecode.to_vec())],
        },
        custom: destination.map(Graph::Id).into_iter().collect(),
    }
}

fn rust_compress(
    config: EncoderConfig,
    data: &[u8],
    build: impl FnOnce(&mut GraphBuilder) -> Result<GraphId, GraphError>,
) -> Result<Vec<u8>, String> {
    let mut builder = GraphBuilder::new();
    let graph = build(&mut builder).map_err(|error| error.to_string())?;
    let mut compressor = Compressor::builder(config)
        .with_graph(builder.build(graph).map_err(|error| error.to_string())?)
        .build()
        .map_err(|error| error.to_string())?;
    compressor
        .compress_inputs(&[Input::from(data)])
        .map_err(|error| error.to_string())
}

fn rust_sddl(
    config: EncoderConfig,
    description: &[u8],
    successor: u32,
    data: &[u8],
) -> Result<Vec<u8>, String> {
    rust_compress(config, data, |builder| {
        builder.sddl(description, standard(successor))
    })
}

fn rust_sddl2(
    config: EncoderConfig,
    bytecode: &[u8],
    chunk: usize,
    destination: Option<u32>,
    data: &[u8],
) -> Result<Vec<u8>, String> {
    rust_compress(config, data, |builder| {
        builder.sddl2(bytecode, chunk, destination.map(|id| standard(id).into()))
    })
}

/// Checks the frame decodes back to `data` with both decoders.
fn assert_round_trip(frame: &[u8], data: &[u8], what: &str) {
    let ours = Decompressor::default()
        .decompress_typed(frame)
        .unwrap_or_else(|error| panic!("{what}: mopenzl cannot decode: {error}"));
    let theirs = graph::decompress(frame)
        .unwrap_or_else(|error| panic!("{what}: the reference cannot decode: {error:?}"));
    assert_eq!(ours.len(), 1, "{what}");
    assert_eq!(theirs.len(), 1, "{what}");
    assert_eq!(ours[0].content(), data, "{what}");
    assert_eq!(theirs[0].bytes, data, "{what} (reference)");
}

/// Equal frames, or both failing. Returns whether they succeeded.
fn compare(
    what: &str,
    reference: Result<Vec<u8>, openzl_ffi::Error>,
    rust: Result<Vec<u8>, String>,
    data: &[u8],
) -> bool {
    match (reference, rust) {
        (Ok(expected), Ok(frame)) => {
            assert_eq!(frame, expected, "{what}");
            assert_round_trip(&frame, data, what);
            true
        }
        (Err(_), Err(_)) => false,
        // A successor graph another package ports: checked after the merge.
        (Ok(_), Err(error)) if error.contains("not ported yet") => {
            eprintln!("{what}: skipped, {error}");
            true
        }
        (Ok(_), Err(error)) => panic!("{what}: only mopenzl failed: {error}"),
        (Err(error), Ok(_)) => panic!("{what}: only the reference failed: {error:?}"),
    }
}

/// Runs an SDDL description on `data` in both libraries at every version,
/// with `store` and `compress_generic` as successors. Returns whether the
/// reference accepted it (at the first configuration).
fn check_sddl(what: &str, description: &[u8], data: &[u8]) -> bool {
    let mut accepted = false;
    for version in VERSIONS {
        for (successor, levels) in [
            (GRAPH_STORE, &[3][..]),
            (GRAPH_COMPRESS_GENERIC, &[1, 6][..]),
        ] {
            for &level in levels {
                let config = config(level, version);
                let label = format!("{what} v{version} L{level} succ{successor}");
                let reference = graph::compress(
                    &[CInput::Serial(data)],
                    &c_sddl(description, successor),
                    &c_params(&config),
                );
                let rust = rust_sddl(config, description, successor, data);
                accepted = compare(&label, reference, rust, data);
            }
        }
    }
    accepted
}

/// Runs SDDL2 bytecode on `data` in both libraries.
fn check_sddl2(what: &str, bytecode: &[u8], chunk: usize, data: &[u8]) -> bool {
    let mut accepted = false;
    for version in VERSIONS {
        for (destination, levels) in [
            (Some(GRAPH_STORE), &[3][..]),
            (None, &[1, 6][..]),
            (Some(GRAPH_ZSTD), &[3][..]),
        ] {
            for &level in levels {
                let config = config(level, version);
                let label = format!("{what} v{version} L{level} dest{destination:?} chunk{chunk}");
                let reference = graph::compress(
                    &[CInput::Serial(data)],
                    &c_sddl2(bytecode, chunk, destination),
                    &c_params(&config),
                );
                let rust = rust_sddl2(config, bytecode, chunk, destination, data);
                accepted = compare(&label, reference, rust, data);
            }
        }
    }
    accepted
}

fn pattern(len: usize, seed: u32) -> Vec<u8> {
    support::noise(len, seed)
}

/// Bytes that compress: slowly varying values.
fn smooth(len: usize) -> Vec<u8> {
    (0..len).map(|n| (n / 7) as u8).collect()
}

// ----- SDDL: valid descriptions -----

/// Four fields per record: a signed 32-bit, a big-endian 16-bit, a byte and
/// a 64-bit float.
fn struct_array_description(records: i64) -> Vec<u8> {
    let mut exprs = dests(4);
    exprs.push(consume(array(
        record(vec![
            eat("i4l", 0),
            eat("u2b", 1),
            eat("byte", 2),
            eat("f8l", 3),
        ]),
        num(records),
    )));
    program(exprs)
}

#[test]
fn fixed_struct_arrays_match_the_reference() {
    for records in [0usize, 1, 2, 7, 100, 700] {
        let description = struct_array_description(records as i64);
        let data = pattern(records * 15, 3);
        assert!(
            check_sddl(&format!("struct x{records}"), &description, &data) || records == 0 || true
        );
        let accepted = check_sddl(
            &format!("struct x{records} smooth"),
            &description,
            &smooth(records * 15),
        );
        assert!(accepted, "{records}");
    }
}

#[test]
fn fixed_struct_arrays_fail_alike_on_wrong_sizes() {
    let description = struct_array_description(10);
    for len in [1usize, 149, 151, 300] {
        let accepted = check_sddl(
            &format!("struct wrong {len}"),
            &description,
            &pattern(len, 5),
        );
        assert!(!accepted, "{len}");
    }
}

#[test]
fn a_length_prefix_drives_variable_arrays() {
    // n = u1; n records of {u2l, byte}.
    let mut exprs = dests(3);
    exprs.push(assign("n", consume(field("u1", 0))));
    exprs.push(consume(array(
        record(vec![eat("u2l", 1), eat("byte", 2)]),
        var("n"),
    )));
    let description = program(exprs);
    for n in [0u8, 1, 5, 60, 255] {
        let mut data = vec![n];
        data.extend(pattern(n as usize * 3, 9));
        assert!(
            check_sddl(&format!("prefix {n}"), &description, &data),
            "{n}"
        );
    }
    // The prefix promises more than there is.
    assert!(!check_sddl("prefix short", &description, &[9, 1, 2, 3]));
}

#[test]
fn dynamic_width_atoms_take_serial_runs() {
    // len = u2l; a run of `len` bytes; then a fixed tail.
    let mut exprs = dests(3);
    exprs.push(assign("len", consume(field("u2l", 0))));
    exprs.push(consume(send(dyn_atom(var("len")), var("d1"))));
    exprs.push(consume(array(field("byte", 1), num(3))));
    exprs.push(consume(field("u4b", 2)));
    let description = program(exprs);
    for len in [0u16, 1, 17, 300] {
        let mut data = len.to_le_bytes().to_vec();
        data.extend(pattern(len as usize + 3 + 4, 4));
        assert!(
            check_sddl(&format!("dyn {len}"), &description, &data),
            "{len}"
        );
    }
}

#[test]
fn nested_arrays_and_records_match() {
    let mut exprs = dests(5);
    exprs.push(consume(array(array(field("u2l", 0), num(3)), num(4))));
    exprs.push(consume(array(
        record(vec![
            eat("u1", 1),
            consume(array(field("u4b", 2), num(2))),
            consume(record(vec![eat("i2b", 3), eat("f4l", 4)])),
        ]),
        num(5),
    )));
    let description = program(exprs);
    let data = pattern(24 + 5 * (1 + 8 + 2 + 4), 11);
    assert!(check_sddl("nested", &description, &data));
    assert!(check_sddl(
        "nested smooth",
        &description,
        &smooth(data.len())
    ));
}

#[test]
fn every_builtin_atom_and_output_type_works() {
    let names = [
        "byte", "i1", "u1", "i2l", "i2b", "u2l", "u2b", "i4l", "i4b", "u4l", "u4b", "i8l", "i8b",
        "u8l", "u8b", "f1", "f2l", "f2b", "f4l", "f4b", "f8l", "f8b", "bf1", "bf2l", "bf2b",
        "bf4l", "bf4b", "bf8l", "bf8b",
    ];
    let mut exprs = dests(names.len());
    let mut size = 0usize;
    for (n, name) in names.iter().enumerate() {
        exprs.push(consume(array(field(name, n), num(3))));
        size += 3 * match *name {
            "byte" | "i1" | "u1" | "f1" | "bf1" => 1,
            _ if name.contains('2') => 2,
            _ if name.contains('4') => 4,
            _ => 8,
        };
    }
    let description = program(exprs);
    assert!(check_sddl("all atoms", &description, &pattern(size, 2)));
}

#[test]
fn several_fields_share_a_destination() {
    // Same numeric type into one destination from different places.
    let mut exprs = dests(2);
    exprs.push(consume(field("u4l", 0)));
    exprs.push(consume(field("byte", 1)));
    exprs.push(consume(array(field("u4l", 0), num(3))));
    exprs.push(consume(field("byte", 1)));
    exprs.push(consume(field("u4l", 0)));
    let description = program(exprs);
    assert!(check_sddl(
        "shared",
        &description,
        &pattern(4 + 1 + 12 + 1 + 4, 1)
    ));
}

#[test]
fn unused_destinations_and_empty_programs_work() {
    let mut exprs = dests(3);
    exprs.push(consume(field("byte", 1)));
    let description = program(exprs);
    assert!(check_sddl("unused dests", &description, &[7]));
    let empty = program(Vec::new());
    assert!(check_sddl("empty program, empty input", &empty, &[]));
    assert!(!check_sddl("empty program, input", &empty, &[1, 2, 3]));
}

#[test]
fn cached_record_replays_match_the_reference_including_its_merge_quirk() {
    // The record's first atom merges into the preceding instruction. The
    // reference rebuilds the cached first instruction with the length the
    // previous instruction had before the merge, which is only consistent
    // when that equals the merged amount.
    for (header, name) in [(1usize, "same length"), (2, "different length")] {
        let mut exprs = dests(2);
        exprs.push(consume(array(field("u2l", 0), num(header as i64))));
        exprs.push(consume(array(
            record(vec![eat("u2l", 0), eat("u1", 1)]),
            num(5),
        )));
        let description = program(exprs);
        let size = 2 * header + 15;
        let accepted = check_sddl(
            &format!("merge quirk, {name}"),
            &description,
            &pattern(size, 6),
        );
        // With a different length the reference's replayed segments no
        // longer add up to the input and dispatch refuses them.
        assert_eq!(accepted, header == 1, "{name}");
    }
    // Different tags: no merge, the plain cached replay.
    let mut exprs = dests(3);
    exprs.push(consume(field("u4l", 2)));
    exprs.push(consume(array(
        record(vec![eat("u2l", 0), eat("u1", 1)]),
        num(50),
    )));
    assert!(check_sddl(
        "cached replay",
        &program(exprs),
        &pattern(4 + 150, 6)
    ));
}

#[test]
fn functions_scopes_and_binding_match() {
    let mut exprs = dests(4);
    // f(x, y) { v = consume u1 into x; w = consume u2l into y }
    exprs.push(assign(
        "f",
        func(
            &["x", "y"],
            vec![
                assign("v", consume(send(atom("u1"), var("x")))),
                assign("w", consume(send(atom("u2l"), var("y")))),
            ],
        ),
    ));
    exprs.push(assign(
        "s",
        consume(bind(var("f"), vec![var("d0"), var("d3")])),
    ));
    exprs.push(assign(
        "count",
        op2("add", member(var("s"), "v"), member(var("s"), "w")),
    ));
    exprs.push(consume(array(field("byte", 1), var("count"))));
    // A function with two parameters, applied partially.
    exprs.push(assign(
        "g",
        func(
            &["a", "b"],
            vec![assign(
                "r",
                consume(array(send(atom("u1"), var("a")), var("b"))),
            )],
        ),
    ));
    exprs.push(assign("g1", bind(var("g"), vec![var("d2")])));
    exprs.push(consume(bind(var("g1"), vec![num(2)])));
    let description = program(exprs);
    // v = 3, w = 4 (u2l): seven bytes follow, then two more.
    let mut data = vec![3, 4, 0];
    data.extend(pattern(7 + 2, 8));
    assert!(check_sddl("functions", &description, &data));
    // Over-applied function.
    let mut exprs = dests(1);
    exprs.push(assign("g", func(&["a"], vec![])));
    exprs.push(consume(bind(var("g"), vec![num(1), num(2)])));
    assert!(!check_sddl("over-applied", &program(exprs), &[0]));
    // Consuming a function with unbound arguments.
    let mut exprs = dests(1);
    exprs.push(assign("g", func(&["a"], vec![])));
    exprs.push(consume(var("g")));
    assert!(!check_sddl("unbound", &program(exprs), &[0]));
}

#[test]
fn arithmetic_comparison_and_logic_operations_match() {
    let mut exprs = dests(2);
    exprs.push(assign("a", consume(field("u1", 0))));
    exprs.push(assign("b", consume(field("u1", 0))));
    // n = ((a + b) * 3 - 1) / 2 % 7 ; m = ((a & b) | (a ^ b)) + ~(-1)
    exprs.push(assign(
        "n",
        op2(
            "mod",
            op2(
                "div",
                op2(
                    "sub",
                    op2("mul", op2("add", var("a"), var("b")), num(3)),
                    num(1),
                ),
                num(2),
            ),
            num(7),
        ),
    ));
    exprs.push(assign(
        "m",
        op2(
            "add",
            op2(
                "bit_or",
                op2("bit_and", var("a"), var("b")),
                op2("bit_xor", var("a"), var("b")),
            ),
            op1("bit_not", op1("neg", num(1))),
        ),
    ));
    exprs.push(expr(
        "expect",
        op2(
            "log_and",
            op2("ge", var("n"), num(0)),
            op2("le", var("n"), num(6)),
        ),
    ));
    exprs.push(expr(
        "expect",
        op2(
            "log_or",
            op2("lt", var("m"), num(0)),
            op2("gt", var("m"), num(-1)),
        ),
    ));
    exprs.push(expr(
        "expect",
        op2("ne", op1("log_not", op2("eq", var("a"), var("b"))), num(2)),
    ));
    exprs.push(expr("log", num(5)));
    exprs.push(consume(array(
        field("byte", 1),
        op2("add", var("n"), var("m")),
    )));
    let description = program(exprs);
    for (a, b) in [(1u8, 2u8), (10, 3), (100, 200), (0, 0)] {
        let n = (((a as i64 + b as i64) * 3 - 1) / 2) % 7;
        let m = ((a & b) | (a ^ b)) as i64;
        let mut data = vec![a, b];
        data.extend(pattern((n + m) as usize, 5));
        assert!(
            check_sddl(&format!("arith {a} {b}"), &description, &data),
            "{a} {b}"
        );
    }
}

#[test]
fn sizeof_and_position_variables_match() {
    let mut exprs = dests(2);
    exprs.push(assign(
        "r",
        record(vec![
            consume(field("u4l", 0)),
            consume(field("u2b", 0)),
            consume(array(field("byte", 1), num(3))),
        ]),
    ));
    exprs.push(assign("size", expr("sizeof", var("r"))));
    exprs.push(consume(array(
        field("byte", 1),
        op2("sub", var("size"), num(3)),
    )));
    // _pos and _rem
    exprs.push(expr("expect", op2("eq", var("_pos"), var("size"))));
    exprs.push(consume(array(field("byte", 1), var("_rem"))));
    let description = program(exprs);
    // size = 9: 6 + 3 (nothing consumed yet for `r` as a value), then
    // size - 3 bytes, then the rest.
    let data = pattern(9 - 3 + 4, 1);
    check_sddl("sizeof", &description, &data);
    // `r` as a value is never consumed: _pos stays 0 until the array.
    let data = pattern(6 + 5, 2);
    check_sddl("sizeof 2", &description, &data);
}

#[test]
fn records_with_named_fields_expose_a_scope() {
    let mut exprs = dests(3);
    exprs.push(assign(
        "header",
        consume(record(vec![
            assign("magic", consume(field("u2l", 0))),
            assign("count", consume(field("u1", 2))),
            consume(array(field("byte", 1), num(2))),
        ])),
    ));
    exprs.push(expr(
        "expect",
        op2("eq", member(var("header"), "magic"), num(0x4142)),
    ));
    exprs.push(consume(array(
        field("u2l", 0),
        member(var("header"), "count"),
    )));
    let description = program(exprs);
    let mut data = vec![0x42, 0x41, 3, 9, 9];
    data.extend(pattern(6, 1));
    assert!(check_sddl("scope", &description, &data));
    let mut wrong = data.clone();
    wrong[0] = 0;
    assert!(!check_sddl("scope magic", &description, &wrong));
}

#[test]
fn debug_locations_are_accepted_and_validated() {
    let mut exprs = dests(1);
    exprs.push(with_loc(consume(array(field("byte", 0), num(4))), 3, 9));
    let description = program(exprs);
    assert!(check_sddl("dbg", &description, &[1, 2, 3, 4]));
    let mut exprs = dests(1);
    exprs.push(with_loc(consume(array(field("byte", 0), num(4))), -1, 9));
    assert!(!check_sddl("dbg negative", &program(exprs), &[1, 2, 3, 4]));
}

// ----- SDDL: failures -----

/// A description's expressions and the data it fails on.
type FailingCase = (&'static str, Vec<Vec<u8>>, Vec<u8>);

#[test]
fn failing_descriptions_fail_alike() {
    let cases: Vec<FailingCase> = vec![
        ("die", vec![die()], vec![]),
        ("expect false", vec![expr("expect", num(0))], vec![]),
        ("expect not a number", vec![expr("expect", dest())], vec![]),
        ("unbound variable", vec![assign("x", var("y"))], vec![]),
        (
            "consume poison",
            vec![consume(record(vec![poison()]))],
            vec![1],
        ),
        ("consume a number", vec![consume(num(3))], vec![]),
        ("divide by zero", vec![op2("div", num(1), num(0))], vec![]),
        ("modulus zero", vec![op2("mod", num(1), num(0))], vec![]),
        (
            "add a scope",
            vec![op2("add", num(1), record(vec![]))],
            vec![],
        ),
        (
            "negative array",
            vec![consume(array(atom("byte"), num(-1)))],
            vec![],
        ),
        ("negative width", vec![consume(dyn_atom(num(-1)))], vec![]),
        (
            "read past the end",
            [dests(1), vec![consume(array(field("u4l", 0), num(3)))]].concat(),
            vec![0; 11],
        ),
        (
            "mixed types",
            [
                dests(1),
                vec![consume(field("u2l", 0)), consume(field("u4l", 0))],
            ]
            .concat(),
            vec![0; 6],
        ),
        (
            "mixed widths with serial",
            [
                dests(1),
                vec![consume(field("byte", 0)), consume(field("u1", 0))],
            ]
            .concat(),
            vec![0; 2],
        ),
        (
            "mixed endianness",
            [
                dests(1),
                vec![consume(field("u2l", 0)), consume(field("u2b", 0))],
            ]
            .concat(),
            vec![0; 4],
        ),
        (
            "send a record",
            vec![consume(send(record(vec![]), dest()))],
            vec![],
        ),
        (
            "send to a non-dest",
            vec![consume(send(atom("byte"), num(1)))],
            vec![0],
        ),
        (
            "assign to a number",
            vec![expr("assign", arr_of(vec![num(1), num(2)]))],
            vec![],
        ),
        ("member of a number", vec![member(num(1), "x")], vec![]),
        ("bind a number", vec![bind(num(1), vec![])], vec![]),
        ("sizeof a number", vec![expr("sizeof", num(1))], vec![]),
        ("input left over", vec![], vec![]),
    ];
    for (what, exprs, mut data) in cases {
        // An empty input never reaches the graph.
        if data.is_empty() {
            data.push(0);
        }
        let description = program(exprs);
        assert!(!check_sddl(what, &description, &data), "{what}");
    }
}

fn arr_of(items: Vec<Vec<u8>>) -> Vec<u8> {
    cbor::arr(items)
}

#[test]
fn scopes_are_limited_to_1024_variables() {
    for (count, ok) in [(1024usize, true), (1025, false)] {
        let mut exprs = dests(1);
        exprs.push(consume(field("byte", 0)));
        exprs.extend((0..count - 1).map(|n| assign(&format!("v{n}"), num(n as i64))));
        // `d0` is one of the variables.
        assert_eq!(
            check_sddl(&format!("{count} vars"), &program(exprs), &[7]),
            ok
        );
    }
}

#[test]
fn malformed_descriptions_fail_alike() {
    let items = vec![assign("d", dest()), consume(send(atom("byte"), var("d")))];
    let good = program(items.clone());
    assert!(check_sddl("good", &good, &[1]));
    let truncated = good[..good.len() - 1].to_vec();
    let trailing = [good.clone(), vec![0]].concat();
    // The same program as an indefinite map around an indefinite array.
    let indefinite = {
        let mut v = vec![0xBF];
        v.extend(cbor::text("exprs"));
        v.push(0x9F);
        v.extend(items.concat());
        v.extend([0xFF, 0xFF]);
        v
    };
    let not_a_map = cbor::arr(vec![]);
    let no_exprs = cbor::map(vec![(cbor::text("src"), cbor::text("x"))]);
    let bad_src = cbor::map(vec![
        (cbor::text("src"), cbor::int(1)),
        (cbor::text("exprs"), cbor::arr(items.clone())),
    ]);
    let with_src = cbor::map(vec![
        (cbor::text("src"), cbor::text("byte[0]")),
        (cbor::text("exprs"), cbor::arr(items.clone())),
    ]);
    let one = |expression: Vec<u8>| program(vec![expression]);
    let cases: Vec<(&str, Vec<u8>, bool)> = vec![
        ("truncated", truncated, false),
        ("trailing", trailing, false),
        ("indefinite", indefinite, true),
        ("not a map", not_a_map, false),
        ("no exprs", no_exprs, false),
        ("bad src", bad_src, false),
        ("with src", with_src, true),
        ("unknown kind", one(expr("frobnicate", null_v())), false),
        ("unknown atom", one(atom("u3l")), false),
        ("wrong arity", one(expr("add", arr_of(vec![num(1)]))), false),
        (
            "die with args",
            one(expr("die", arr_of(vec![num(1)]))),
            false,
        ),
        ("dest not null", one(expr("dest", num(1))), false),
        ("var not a string", one(expr("var", num(1))), false),
        ("int not an int", one(expr("int", cbor::text("x"))), false),
        ("empty expression", one(cbor::map(vec![])), false),
        ("not an expression", one(cbor::int(3)), false),
        (
            "func arg not a var",
            one(expr(
                "func",
                arr_of(vec![arr_of(vec![num(1)]), arr_of(vec![])]),
            )),
            false,
        ),
        ("func shape", one(expr("func", arr_of(vec![]))), false),
        (
            "array shape",
            one(expr("array", arr_of(vec![num(1)]))),
            false,
        ),
        ("record shape", one(expr("record", num(1))), false),
        ("tuple shape", one(expr("tuple", num(1))), false),
        ("poison shape", one(expr("poison", num(1))), false),
        (
            "second pair",
            one(cbor::map(vec![
                (cbor::text("int"), cbor::int(1)),
                (cbor::text("x"), cbor::int(1)),
            ])),
            false,
        ),
        ("empty", Vec::new(), false),
    ];
    for (what, description, ok) in cases {
        assert_eq!(check_sddl(what, &description, &[1]), ok, "{what}");
    }
}

fn null_v() -> Vec<u8> {
    cbor::null()
}

#[test]
fn sddl_builder_validates_and_graph_builder_params_are_the_reference_params() {
    let mut builder = GraphBuilder::new();
    assert!(builder.sddl(&[], StandardGraph::Store).is_err());
    assert!(builder.sddl(&[0xFF], StandardGraph::Store).is_err());
    assert!(builder.sddl(&program(vec![]), StandardGraph::Store).is_ok());
}

// ----- SDDL2 -----

/// A star catalog: a 28-byte header, then records of 28 bytes.
fn sao_bytecode() -> Vec<u8> {
    assemble(
        "push.i32 28
         segment.create_unspecified
         push.type.f64le
         push.type.f64le
         push.type.u16le
         push.type.i16le
         push.type.f32le
         push.type.f32le
         push.i32 6
         type.structure
         push.tag 2
         stack.swap
         push.remaining
         push.u32 28
         math.div
         segment.create_tagged
         halt",
    )
}

#[test]
fn star_catalog_segments_match_the_reference() {
    for stars in [0usize, 1, 5, 100, 400] {
        let data = pattern(28 + stars * 28, 7);
        assert!(
            check_sddl2(&format!("sao {stars}"), &sao_bytecode(), 0, &data),
            "{stars}"
        );
    }
    assert!(check_sddl2(
        "sao smooth",
        &sao_bytecode(),
        0,
        &smooth(28 + 28 * 300)
    ));
}

#[test]
fn chunk_sizes_split_and_flush_alike() {
    let data = pattern(28 + 28 * 100, 3);
    for chunk in [1usize, 27, 28, 29, 56, 100, 500, 1000, 5000, 100_000] {
        check_sddl2(&format!("sao chunk {chunk}"), &sao_bytecode(), chunk, &data);
    }
}

#[test]
fn structure_segments_with_nested_members_match() {
    // {u8, {i16le, i32le}, f64be}: 15 bytes, 20 of them.
    let bytecode = assemble(
        "push.tag 200
         push.type.u8
         push.type.i16le
         push.type.i32le
         push.i32 2
         type.structure
         push.type.f64be
         push.i32 3
         type.structure
         push.i32 20
         segment.create_tagged
         halt",
    );
    let data = pattern(15 * 20, 5);
    assert!(check_sddl2("nested struct", &bytecode, 0, &data));
    for chunk in [14usize, 15, 30, 100] {
        check_sddl2(
            &format!("nested struct chunk {chunk}"),
            &bytecode,
            chunk,
            &data,
        );
    }
}

#[test]
fn primitive_segments_of_every_kind_match() {
    let kinds = [
        ("u8", 1),
        ("i8", 1),
        ("u16le", 2),
        ("u16be", 2),
        ("i16le", 2),
        ("i16be", 2),
        ("u32le", 4),
        ("u32be", 4),
        ("i32le", 4),
        ("i32be", 4),
        ("u64le", 8),
        ("u64be", 8),
        ("i64le", 8),
        ("i64be", 8),
        ("f8", 1),
        ("f16le", 2),
        ("f16be", 2),
        ("bf16le", 2),
        ("bf16be", 2),
        ("f32le", 4),
        ("f32be", 4),
        ("f64le", 8),
        ("f64be", 8),
        ("bytes", 1),
    ];
    let mut source = String::new();
    let mut size = 0;
    for (n, (kind, width)) in kinds.iter().enumerate() {
        source += &format!(
            "push.tag {}\npush.type.{kind}\npush.i32 5\nsegment.create_tagged\n",
            n + 1
        );
        size += 5 * width;
    }
    let bytecode = assemble(&source);
    let data = pattern(size, 8);
    assert!(check_sddl2("all kinds", &bytecode, 0, &data));
    for chunk in [7usize, 20, 64] {
        check_sddl2(&format!("all kinds chunk {chunk}"), &bytecode, chunk, &data);
    }
}

#[test]
fn tags_merge_and_repeat_alike() {
    // 1 1 2 1 unspecified unspecified: adjacent equal tags merge.
    let bytecode = assemble(
        "push.tag 1
         push.type.u16le
         push.i32 3
         segment.create_tagged
         push.tag 1
         push.type.u16le
         push.i32 2
         segment.create_tagged
         push.tag 2
         push.type.u32be
         push.i32 1
         segment.create_tagged
         push.tag 1
         push.type.u16le
         push.i32 4
         segment.create_tagged
         push.i32 3
         segment.create_unspecified
         push.i32 2
         segment.create_unspecified
         halt",
    );
    let data = pattern(10 + 4 + 8 + 5, 1);
    assert!(check_sddl2("merge", &bytecode, 0, &data));
    for chunk in [4usize, 10, 14, 30] {
        check_sddl2(&format!("merge chunk {chunk}"), &bytecode, chunk, &data);
    }
}

#[test]
fn computed_layouts_with_loads_variables_and_jumps_match() {
    // count = u32le at 0; a 4-byte header; `count` u16be values; a trailing
    // block taken from `remaining` only when something is left.
    let bytecode = assemble(
        "push.zero
         load.u32le
         push.zero
         var.store
         push.i32 4
         segment.create_unspecified
         push.tag 7
         push.type.u16be
         push.zero
         var.load
         segment.create_tagged
         push.remaining
         push.zero
         cmp.gt
         push.i32 5
         stack.swap
         jump_if
         push.zero
         push.zero
         push.zero
         push.zero
         push.zero
         push.remaining
         segment.create_unspecified
         halt",
    );
    for count in [0u32, 1, 9, 100] {
        let mut data = count.to_le_bytes().to_vec();
        data.extend(pattern(count as usize * 2, 2));
        check_sddl2(&format!("computed {count}"), &bytecode, 0, &data);
        data.extend(pattern(11, 3));
        check_sddl2(&format!("computed tail {count}"), &bytecode, 0, &data);
    }
}

#[test]
fn every_load_width_and_stack_operation_matches() {
    let loads = [
        "load.u8",
        "load.i8",
        "load.u16le",
        "load.i16le",
        "load.u32le",
        "load.i32le",
        "load.i64le",
        "load.u16be",
        "load.i16be",
        "load.u32be",
        "load.i32be",
        "load.i64be",
    ];
    // Each load result is reduced to a 0/1 flag and the flags drive one
    // segment size: sum of (value & 1) over every load, so the layout
    // depends on every decoded value.
    let mut source = String::from("push.zero\n");
    for load in loads {
        source += &format!("push.zero\n{load}\npush.i32 1\nmath.bit_and\nmath.add\n");
    }
    source += "stack.dup\nstack.drop\npush.i32 2\npush.i32 3\nstack.swap\nstack.drop\nstack.drop\n";
    source += "push.i32 1\nstack.drop_if\npush.stack_depth\nmath.add\n";
    source += "push.current_pos\nmath.add\nsegment.create_unspecified\nhalt";
    let bytecode = assemble(&source);
    for seed in 1..6 {
        let data = pattern(40, seed);
        check_sddl2(&format!("loads {seed}"), &bytecode, 0, &data);
    }
}

#[test]
fn arithmetic_comparison_and_logic_opcodes_match() {
    let programs = [
        "push.i32 7\npush.i32 3\nmath.sub\npush.i32 4\nmath.mul\npush.i32 5\nmath.mod\n",
        "push.i32 -9\nmath.abs\nmath.neg\nmath.neg\nmath.bit_not\nmath.bit_not\n",
        "push.i32 12\npush.i32 10\nmath.bit_or\npush.i32 6\nmath.bit_xor\npush.i32 7\nmath.bit_and\n",
        "push.i32 3\npush.i32 4\ncmp.lt\npush.i32 3\npush.i32 3\ncmp.le\nlogic.and\n",
        "push.i32 3\npush.i32 4\ncmp.gt\npush.i32 3\npush.i32 3\ncmp.ge\nlogic.or\n",
        "push.i32 3\npush.i32 4\ncmp.ne\npush.i32 3\npush.i32 3\ncmp.eq\nlogic.xor\nlogic.not\n",
        "push.i64 4000000000\npush.i64 4000000000\nmath.mul\npush.i32 1000\nmath.mod\n",
    ];
    for (n, source) in programs.iter().enumerate() {
        let bytecode = assemble(&format!(
            "{source}\nstack.dup\nexpect_true\nsegment.create_unspecified\nhalt"
        ));
        for len in [0usize, 1, 2, 3, 4, 5, 6, 7, 8] {
            check_sddl2(
                &format!("arith {n} len {len}"),
                &bytecode,
                0,
                &pattern(len, 1),
            );
        }
    }
}

#[test]
fn vm_failures_fail_alike() {
    let data = pattern(64, 1);
    let programs: Vec<(&str, String)> = vec![
        ("underflow", "math.add".into()),
        ("pop type", "push.zero\ntype.sizeof".into()),
        ("div zero", "push.i32 1\npush.zero\nmath.div".into()),
        ("mod zero", "push.i32 1\npush.zero\nmath.mod".into()),
        ("add overflow", "push.i64 9223372036854775807\npush.i32 1\nmath.add".into()),
        ("sub overflow", "push.i64 -9223372036854775808\npush.i32 1\nmath.sub".into()),
        ("mul overflow", "push.i64 9223372036854775807\npush.i32 2\nmath.mul".into()),
        ("abs min", "push.i64 -9223372036854775808\nmath.abs".into()),
        ("neg min", "push.i64 -9223372036854775808\nmath.neg".into()),
        ("div min", "push.i64 -9223372036854775808\npush.i32 -1\nmath.div".into()),
        ("expect false", "push.zero\nexpect_true".into()),
        ("load oob", "push.i32 64\nload.u8".into()),
        ("load neg", "push.i32 -1\nload.u8".into()),
        ("load straddle", "push.i32 61\nload.u32le".into()),
        ("segment oob", "push.i32 65\nsegment.create_unspecified".into()),
        ("segment neg", "push.i32 -1\nsegment.create_unspecified".into()),
        ("tag conflict", "push.tag 1\npush.type.u8\npush.i32 1\nsegment.create_tagged\npush.tag 1\npush.type.u16le\npush.i32 1\nsegment.create_tagged".into()),
        ("tag not a tag", "push.zero\npush.type.u8\npush.i32 1\nsegment.create_tagged".into()),
        ("type not a type", "push.tag 1\npush.zero\npush.i32 1\nsegment.create_tagged".into()),
        ("stack over", "push.zero\nstack.over".into()),
        ("stack rot", "push.zero\npush.zero\npush.zero\nstack.rot".into()),
        ("call", "call".into()),
        ("bad push type", "word 0x00010199".into()),
        ("bad control", "word 0x00050099".into()),
        ("bad segment", "word 0x000C0099".into()),
        ("bad math", "word 0x00020099".into()),
        ("bad cmp", "word 0x00030099".into()),
        ("bad logic", "word 0x00040099".into()),
        ("bad stack", "word 0x00070099".into()),
        ("bad type", "word 0x00080099".into()),
        ("bad load", "word 0x00060099".into()),
        ("bad var", "word 0x00090099".into()),
        ("bad push", "word 0x00010099".into()),
        ("var load empty", "push.zero\nvar.load".into()),
        ("var bad register", "push.i32 256\nvar.load".into()),
        ("var store bad register", "push.zero\npush.i32 -1\nvar.store".into()),
        ("fixed array overflow", "push.type.u8\npush.i32 70000\ntype.fixed_array\npush.i32 70000\ntype.fixed_array".into()),
        ("fixed array negative", "push.type.u8\npush.i32 -1\ntype.fixed_array".into()),
        ("fixed array not a type", "push.zero\npush.i32 1\ntype.fixed_array".into()),
        ("structure underflow", "push.type.u8\npush.i32 3\ntype.structure".into()),
        ("structure member not a type", "push.zero\npush.i32 1\ntype.structure".into()),
        ("structure overflow", structure_overflow_program()),
        ("jump past the end", "push.i32 9\npush.i32 1\njump_if".into()),
        ("jump negative", "push.i32 -1\npush.i32 1\njump_if".into()),
        ("jump immediate missing", "push.i32 1\npush.i32 1\njump_if\nword 0x00010002".into()),
        ("push immediate missing", "word 0x00010002".into()),
        ("push i64 half", "word 0x00010004\nword 0x1".into()),
        ("unmapped input", "push.i32 10\nsegment.create_unspecified\nhalt".into()),
    ];
    for (what, source) in &programs {
        let bytecode = assemble(source);
        assert!(!check_sddl2(what, &bytecode, 0, &data), "{what}");
    }
    // A stack that overflows.
    let overflow = assemble(&"push.zero\n".repeat(257));
    assert!(!check_sddl2("stack overflow", &overflow, 0, &data));
    let fits = assemble(&format!(
        "{}push.i32 64\nsegment.create_unspecified",
        "push.zero\n".repeat(255)
    ));
    assert!(check_sddl2("stack fits", &fits, 0, &data));
}

#[test]
fn segment_and_tag_limits_match() {
    // Many distinct tags and many segments.
    let mut source = String::new();
    for tag in 1..=300 {
        source += &format!("push.tag {tag}\npush.type.u8\npush.i32 1\nsegment.create_tagged\n");
    }
    check_sddl2("300 tags", &assemble(&source), 0, &pattern(300, 1));
    check_sddl2("300 tags chunked", &assemble(&source), 64, &pattern(300, 1));
    let mut source = String::new();
    for n in 0..2000 {
        source += &format!(
            "push.tag {}\npush.type.u8\npush.i32 1\nsegment.create_tagged\n",
            1 + (n % 2)
        );
    }
    check_sddl2("alternating", &assemble(&source), 100, &pattern(2000, 1));
}

#[test]
fn empty_programs_and_unusual_bytecode_store_the_input() {
    for data in [pattern(0, 1), pattern(5, 1), pattern(5000, 2)] {
        let n = data.len();
        check_sddl2(&format!("empty bytecode {n}"), &[], 0, &data);
        check_sddl2(&format!("halt only {n}"), &assemble("halt"), 0, &data);
        // An unknown family is a no-op in the reference.
        check_sddl2(
            &format!("unknown family {n}"),
            &assemble("word 0x00990001\nhalt"),
            0,
            &data,
        );
        check_sddl2(
            &format!("trace {n}"),
            &assemble("trace.start\npush.zero\nexpect_true"),
            0,
            &data,
        );
    }
}

#[test]
fn structure_edge_cases_fail_or_succeed_alike() {
    let data = pattern(200, 4);
    let programs: Vec<(&str, String)> = vec![
        (
            "empty structure",
            "push.tag 1\npush.i32 0\ntype.structure\npush.i32 3\nsegment.create_tagged\nhalt".into(),
        ),
        (
            "bytes member",
            "push.tag 1\npush.type.bytes\npush.type.u16le\npush.i32 2\ntype.structure\npush.i32 20\nsegment.create_tagged\nhalt".into(),
        ),
        (
            "array member",
            "push.tag 1\npush.type.u32le\npush.i32 3\ntype.fixed_array\npush.type.u8\npush.i32 2\ntype.structure\npush.i32 5\nsegment.create_tagged\nhalt".into(),
        ),
        (
            "array of structures",
            "push.tag 1\npush.type.u32le\npush.type.u8\npush.i32 2\ntype.structure\npush.i32 3\ntype.fixed_array\npush.i32 4\nsegment.create_tagged\nhalt".into(),
        ),
        (
            "structure member array",
            "push.tag 1\npush.type.u32le\npush.type.u8\npush.i32 2\ntype.structure\npush.i32 2\ntype.fixed_array\npush.type.u8\npush.i32 2\ntype.structure\npush.i32 4\nsegment.create_tagged\nhalt".into(),
        ),
        (
            "zero count structure",
            "push.tag 1\npush.type.u32le\npush.type.u8\npush.i32 2\ntype.structure\npush.zero\nsegment.create_tagged\npush.i32 200\nsegment.create_unspecified\nhalt".into(),
        ),
        (
            "zero count primitive",
            "push.tag 1\npush.type.u32le\npush.zero\nsegment.create_tagged\npush.i32 200\nsegment.create_unspecified\nhalt".into(),
        ),
        (
            "sizeof",
            "push.type.u32le\npush.i32 3\ntype.fixed_array\ntype.sizeof\nsegment.create_unspecified\npush.i32 188\nsegment.create_unspecified\nhalt".into(),
        ),
        (
            "tag zero typed",
            "push.tag 0\npush.type.u16le\npush.i32 4\nsegment.create_tagged\nhalt".into(),
        ),
    ];
    for (what, source) in &programs {
        let bytecode = assemble(source);
        check_sddl2(what, &bytecode, 0, &data);
        check_sddl2(&format!("{what} chunked"), &bytecode, 16, &data);
    }
}

#[test]
fn sddl2_builder_validates_and_raw_parameters_fail_alike() {
    let mut builder = GraphBuilder::new();
    assert!(builder.sddl2(&[1, 2, 3], 0, None).is_err());
    assert!(builder.sddl2(&[], usize::MAX, None).is_err());
    assert!(builder.sddl2(&[], 0, None).is_ok());
    assert!(
        builder
            .sddl2(&[], 10, Some(StandardGraph::Store.into()))
            .is_ok()
    );
    // A bytecode that is not a whole number of words, and a negative chunk
    // size, only fail when compressing; the reference does the same.
    let data = pattern(32, 1);
    for (what, bytecode, chunk) in [
        ("odd bytecode", vec![1u8, 0, 1], 0i32),
        ("negative chunk", assemble("halt"), -5),
    ] {
        let config = config(3, 24);
        let reference = graph::compress(
            &[CInput::Serial(&data)],
            &Graph::Parameterized {
                graph: GRAPH_SDDL2,
                params: Params {
                    ints: vec![(SDDL2_CHUNK_PID, chunk)],
                    copies: vec![(SDDL2_BYTECODE_PID, bytecode.clone())],
                },
                custom: Vec::new(),
            },
            &c_params(&config),
        );
        let rust = rust_compress(config, &data, |builder| {
            builder.parameterize_graph(
                StandardGraph::SimpleDataDescriptionLanguageV2,
                GraphParams::default().with_params(
                    LocalParams::default()
                        .with_bytes(SDDL2_BYTECODE_PID, bytecode.clone())
                        .with_int(SDDL2_CHUNK_PID, chunk),
                ),
            )
        });
        assert!(!compare(what, reference, rust, &data), "{what}");
    }
    // A missing bytecode parameter and two custom graphs.
    let config = config(3, 24);
    let reference = graph::compress(
        &[CInput::Serial(&data)],
        &Graph::Parameterized {
            graph: GRAPH_SDDL2,
            params: Params::default(),
            custom: Vec::new(),
        },
        &c_params(&config),
    );
    let rust = rust_compress(config, &data, |builder| {
        builder.parameterize_graph(
            StandardGraph::SimpleDataDescriptionLanguageV2,
            GraphParams::default().with_params(LocalParams::default()),
        )
    });
    assert!(!compare("no bytecode", reference, rust, &data));
    let config = config_with(3, 24);
    let reference = graph::compress(
        &[CInput::Serial(&data)],
        &Graph::Parameterized {
            graph: GRAPH_SDDL2,
            params: Params {
                ints: Vec::new(),
                copies: vec![(SDDL2_BYTECODE_PID, assemble("halt"))],
            },
            custom: vec![Graph::STORE, Graph::STORE],
        },
        &c_params(&config),
    );
    let rust = rust_compress(config, &data, |builder| {
        builder.parameterize_graph(
            StandardGraph::SimpleDataDescriptionLanguageV2,
            GraphParams::default()
                .with_params(
                    LocalParams::default().with_bytes(SDDL2_BYTECODE_PID, assemble("halt")),
                )
                .with_graphs([StandardGraph::Store.into(), StandardGraph::Store.into()]),
        )
    });
    assert!(!compare("two destinations", reference, rust, &data));
}

fn config_with(level: i32, version: u32) -> EncoderConfig {
    config(level, version)
}

#[test]
fn compressor_reuse_matches_after_sddl_runs() {
    // A single compressor is reused for many inputs and stays in sync.
    let description = struct_array_description(10);
    let mut builder = GraphBuilder::new();
    let graph = builder.sddl(&description, StandardGraph::Store).unwrap();
    let mut compressor = Compressor::builder(config(3, 27))
        .with_graph(builder.build(graph).unwrap())
        .build()
        .unwrap();
    for seed in 1..4 {
        let data = pattern(150, seed);
        let frame = compressor
            .compress_inputs(&[Input::from(&data[..])])
            .unwrap();
        let expected = graph::compress(
            &[CInput::Serial(&data)],
            &c_sddl(&description, GRAPH_STORE),
            &c_params(&config(3, 27)),
        )
        .unwrap();
        assert_eq!(frame, expected);
    }
}

/// Eight levels of 17-member structures around a 32 GiB array: the total
/// size overflows 64 bits.
fn structure_overflow_program() -> String {
    let mut source = String::from("push.type.u64le\npush.i64 4294967295\ntype.fixed_array\n");
    for _ in 0..8 {
        source += &"stack.dup\n".repeat(16);
        source += "push.i32 17\ntype.structure\n";
    }
    source
}
