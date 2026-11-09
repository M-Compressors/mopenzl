//! Unit tests of the SDDL interpreter: descriptions are built as CBOR with a
//! tiny encoder and run directly, checking the dispatch instructions.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::program::{self, AtomType};
use super::state::{self, OutputInfo};
use super::{conversion_node, validate};

fn text(s: &str) -> Vec<u8> {
    assert!(s.len() < 24);
    [vec![0x60 | s.len() as u8], s.as_bytes().to_vec()].concat()
}

fn int(n: u8) -> Vec<u8> {
    assert!(n < 24);
    vec![n]
}

fn arr(items: Vec<Vec<u8>>) -> Vec<u8> {
    assert!(items.len() < 24);
    [vec![0x80 | items.len() as u8], items.concat()].concat()
}

fn expr(kind: &str, value: Vec<u8>) -> Vec<u8> {
    [vec![0xA1], text(kind), value].concat()
}

fn var(name: &str) -> Vec<u8> {
    expr("var", text(name))
}

fn assign(name: &str, value: Vec<u8>) -> Vec<u8> {
    expr("assign", arr(vec![var(name), value]))
}

fn field(atom: &str, dest: usize) -> Vec<u8> {
    expr(
        "consume",
        expr(
            "send",
            arr(vec![
                expr("atom", text(atom)),
                var(&alloc::format!("d{dest}")),
            ]),
        ),
    )
}

fn program(exprs: Vec<Vec<u8>>) -> Vec<u8> {
    let dests = (0..3).map(|n| assign(&alloc::format!("d{n}"), expr("dest", vec![0xF6])));
    let all: Vec<Vec<u8>> = dests.chain(exprs).collect();
    [
        vec![0xA1],
        text("exprs"),
        [vec![0x98, all.len() as u8], all.concat()].concat(),
    ]
    .concat()
}

fn run(description: &[u8], data: &[u8]) -> Result<state::Instructions, String> {
    let loaded = program::load(description).map_err(|e| alloc::format!("{e}"))?;
    state::run(&loaded, data).map_err(|e| alloc::format!("{e}"))
}

#[test]
fn consecutive_atoms_of_one_destination_merge_into_one_segment() {
    let description = program(vec![
        field("u2l", 0),
        field("u2l", 0),
        field("byte", 1),
        field("u2l", 0),
    ]);
    let out = run(&description, &[1, 2, 3, 4, 5, 6, 7]).unwrap();
    assert_eq!(out.segment_sizes, vec![4, 1, 2]);
    assert_eq!(out.tags, vec![0, 1, 0]);
    assert_eq!(
        out.outputs,
        vec![
            OutputInfo {
                ty: AtomType::Numeric,
                width: 2,
                big_endian: false
            },
            OutputInfo {
                ty: AtomType::Serial,
                width: 1,
                big_endian: false
            },
        ]
    );
}

#[test]
fn arrays_of_records_replay_cached_instructions() {
    let record = expr("record", arr(vec![field("u2b", 0), field("byte", 1)]));
    let description = program(vec![expr(
        "consume",
        expr("array", arr(vec![record, expr("int", int(3))])),
    )]);
    let out = run(&description, &[0; 9]).unwrap();
    assert_eq!(out.segment_sizes, vec![2, 1, 2, 1, 2, 1]);
    assert_eq!(out.tags, vec![0, 1, 0, 1, 0, 1]);
    assert!(out.outputs[0].big_endian);
}

#[test]
fn a_cached_first_instruction_keeps_the_pre_merge_length_like_the_reference() {
    // A 4-byte header, then a record whose first atom merges into it.
    let record = expr("record", arr(vec![field("u2l", 0)]));
    let header = expr(
        "consume",
        expr(
            "array",
            arr(vec![
                expr("send", arr(vec![expr("atom", text("u2l")), var("d0")])),
                expr("int", int(2)),
            ]),
        ),
    );
    let description = program(vec![
        header,
        expr(
            "consume",
            expr("array", arr(vec![record, expr("int", int(2))])),
        ),
    ]);
    // The first record merges (6 bytes at tag 0); the replay pushes the
    // reference's cached length of 4 instead of 2.
    let out = run(&description, &[0; 8]).unwrap();
    assert_eq!(out.segment_sizes, vec![6, 4]);
    assert_eq!(out.tags, vec![0, 0]);
}

#[test]
fn failures_report_their_cause() {
    let short = run(&program(vec![field("u4l", 0)]), &[1, 2]).unwrap_err();
    assert!(short.contains("past the end"), "{short}");
    let left = run(&program(vec![field("byte", 0)]), &[1, 2]).unwrap_err();
    assert!(left.contains("did not consume"), "{left}");
    let mixed = run(&program(vec![field("u2l", 0), field("u4l", 0)]), &[0; 6]).unwrap_err();
    assert!(mixed.contains("different widths"), "{mixed}");
    assert!(run(&[0xFF], &[]).unwrap_err().contains("encoding"));
}

#[test]
fn loading_validates_the_description() {
    let good = program(vec![]);
    assert!(validate(&good));
    assert!(!validate(&[]));
    assert!(!validate(&good[..good.len() - 1]));
}

#[test]
fn numeric_destinations_pick_the_conversion_node_of_their_width() {
    assert_eq!(conversion_node(1, true), conversion_node(1, false));
    assert_ne!(conversion_node(2, true), conversion_node(2, false));
    assert!(conversion_node(8, false).is_some());
    assert!(conversion_node(3, false).is_none());
}
