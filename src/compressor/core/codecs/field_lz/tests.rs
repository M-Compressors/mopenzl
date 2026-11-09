//! Tests of the `field_lz` node and its match finders, against a reference
//! decoder written from the token semantics.

use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::compressor::core::codecs::test_util::{noise, run_node, text, u16s, u32s};
use crate::compressor::core::data::{Bytes, Type};

/// Rebuilds the input from its literals and tokens.
fn rebuild(literals: &[u8], tokens: &Tokens, width: usize) -> Vec<u8> {
    let (mut offsets, mut extra_ll, mut extra_ml) = (
        tokens.offsets.iter(),
        tokens.extra_literal_lengths.iter(),
        tokens.extra_match_lengths.iter(),
    );
    let minimum = min_match(width);
    let mut out = Vec::new();
    let mut lits = literals.chunks_exact(width);
    let mut recent = [1usize, 2];
    for &token in &tokens.tokens {
        let kind = usize::from(token & 3);
        let mut ll = usize::from((token >> 2) & 15);
        if ll == 15 {
            ll += *extra_ll.next().unwrap() as usize;
        }
        let mut ml = usize::from(token >> 6);
        if ml == 15 {
            ml += *extra_ml.next().unwrap() as usize;
        }
        ml += minimum;
        for _ in 0..ll {
            out.extend_from_slice(lits.next().unwrap());
        }
        let offset = if kind == 3 {
            let offset = *offsets.next().unwrap() as usize;
            recent = [offset, recent[0]];
            offset
        } else {
            let offset = recent[kind];
            recent.swap(0, kind.min(1));
            offset
        };
        for _ in 0..ml {
            let start = out.len() - offset * width;
            out.extend_from_within(start..start + width);
        }
    }
    for rest in lits {
        out.extend_from_slice(rest);
    }
    out
}

fn fields(width: usize, n: usize) -> Vec<Vec<u8>> {
    let long: Vec<u8> = (0..300 * width).map(|i| (i * i / 7) as u8).collect();
    vec![
        text(n * width, 1),
        noise(n * width, 2),
        vec![0; n * width],
        long.repeat(n / 300 + 1)[..n * width].to_vec(),
        (0..n)
            .flat_map(|i| ((i / 3) as u64).to_le_bytes()[..width].to_vec())
            .collect(),
    ]
}

#[test]
fn every_level_and_width_reproduces_the_input_from_its_tokens() {
    for width in [2usize, 4, 8] {
        for src in fields(width, 4_000) {
            for level in [-1, 0, 1, 2, 3, 4, 5, 8] {
                let (literals, tokens) = compress(&src, width, level).unwrap();
                assert_eq!(
                    rebuild(&literals, &tokens, width),
                    src,
                    "width {width} level {level}"
                );
                assert!(tokens.tokens.len() <= max_sequences(src.len() / width, width));
            }
        }
    }
}

#[test]
fn levels_resolve_to_the_documented_finder_and_table() {
    assert_eq!(resolve_level(1 << 20, 0), (false, true, 19));
    assert_eq!(resolve_level(1 << 20, -9), (false, true, 19));
    assert_eq!(resolve_level(1 << 20, 1), (false, false, 18));
    assert_eq!(resolve_level(1 << 20, 2), (false, true, 18));
    assert_eq!(resolve_level(1 << 20, 3), (false, true, 19));
    assert_eq!(resolve_level(1 << 20, 4), (true, true, 20));
    assert_eq!(resolve_level(1 << 22, 5), (true, true, 22));
    assert_eq!(resolve_level(1 << 22, 22), (true, true, 22));
    assert_eq!(resolve_level(1 << 20, 5), (true, true, 21));
    // The table shrinks with the input but never below 2^10.
    assert_eq!(resolve_level(1_000, 5), (true, true, 10));
    assert_eq!(resolve_level(0, 3), (false, true, 10));
    assert_eq!(resolve_level(100_000, 5), (true, true, 17));
}

#[test]
fn a_field_size_that_is_not_a_power_of_two_is_rejected() {
    assert!(matches!(
        compress(&[0; 12], 3, 3),
        Err(NodeError::InvalidParam(_))
    ));
}

#[test]
fn a_token_stream_beyond_the_capacity_is_an_error() {
    let long = text(8_000, 3);
    let store = {
        let mut store = SeqStore::default();
        // One new-offset sequence with both lengths overflowing the token.
        store.store(
            &long,
            0,
            Sequence {
                literal_length: 400,
                match_code: 5,
                match_length: 400,
                rep: false,
            },
        );
        store
    };
    let tokens = write_sequences(&store, 2, 4).unwrap();
    assert_eq!(
        (
            tokens.tokens.len(),
            tokens.offsets,
            tokens.extra_literal_lengths,
            tokens.extra_match_lengths
        ),
        (1, vec![5], vec![185], vec![183])
    );
    // Each output has its own capacity check; capacity 0 trips the first
    // that is written.
    assert!(write_sequences(&store, 2, 0).is_err());
    let mut rep_store = SeqStore::default();
    rep_store.store(
        &long,
        0,
        Sequence {
            literal_length: 400,
            match_code: 1,
            match_length: 4,
            rep: true,
        },
    );
    // A repcode sequence writes no offset: the literal length check fails.
    assert!(write_sequences(&rep_store, 2, 0).is_err());
    let mut short_store = SeqStore::default();
    short_store.store(
        &long,
        0,
        Sequence {
            literal_length: 4,
            match_code: 2,
            match_length: 400,
            rep: true,
        },
    );
    // Only the match length overflows: that check, then the token check.
    assert!(write_sequences(&short_store, 2, 0).is_err());
    let mut plain = SeqStore::default();
    plain.store(
        &long,
        0,
        Sequence {
            literal_length: 4,
            match_code: 2,
            match_length: 4,
            rep: true,
        },
    );
    assert!(write_sequences(&plain, 2, 0).is_err());
    assert!(write_sequences(&plain, 2, 1).is_ok());
}

#[test]
fn matches_compare_whole_fields_and_never_read_before_the_source() {
    let data = *b"abcdefgh";
    assert!(check_match(&data, 0, 0, 8));
    assert!(!check_match(&data, 0, 1, 2));
    assert!(check_match(b"abcabc", 0, 3, 3));
    assert!(check_match(b"abcdabcd", 0, 4, 4));
    assert!(check_match(b"abababab", 0, 2, 2));
    assert!(!check_match(&data, -2, 0, 2));
    assert!(!check_match(&data, 0, -2, 2));
    assert_eq!(count_fields(b"abababababX_", 2, 0, 2), 8);
    assert_eq!(count_fields(b"ababab", 2, 0, 2), 4);
    assert_eq!(count_fields(b"abcdef", 2, 0, 2), 0);
}

#[test]
fn the_node_emits_five_streams_a_field_count_header_and_honours_the_override() {
    let src = text(8_000, 5);
    let out = run_node(
        9,
        &[],
        3,
        &[Stream::new(Type::Struct, 4, Bytes::Borrowed(&src))],
    )
    .unwrap();
    assert_eq!(
        out.header.as_deref(),
        Some(&[0xD0, 0x0F][..]),
        "varint(2000)"
    );
    let [literals, tokens, offsets, extra_ll, extra_ml] = &out.singles[..] else {
        panic!("five outputs");
    };
    assert_eq!((literals.ty(), literals.width()), (Type::Struct, 4));
    assert_eq!((tokens.ty(), tokens.width()), (Type::Struct, 2));
    assert_eq!(
        (offsets.width(), extra_ll.width(), extra_ml.width()),
        (4, 4, 4)
    );
    let decoded = rebuild(
        literals.bytes(),
        &Tokens {
            tokens: u16s(tokens.bytes()),
            offsets: u32s(offsets.bytes()),
            extra_literal_lengths: u32s(extra_ll.bytes()),
            extra_match_lengths: u32s(extra_ml.bytes()),
        },
        4,
    );
    assert_eq!(decoded, src);

    let sizes = |ints: &[(i32, i32)], level: i32| {
        let input = Stream::new(Type::Struct, 4, Bytes::Borrowed(&src));
        run_node(9, ints, level, &[input]).unwrap().singles[1].content_size()
    };
    assert_eq!(sizes(&[(181, 5)], 1), sizes(&[], 5));
    assert_eq!(sizes(&[(181, 1)], 9), sizes(&[], 1));
}

#[test]
fn unsupported_widths_and_oversized_inputs_are_rejected() {
    for width in [1usize, 3, 16] {
        let data = vec![0u8; width * 8];
        let input = Stream::new(Type::Struct, width, Bytes::Borrowed(&data));
        assert!(matches!(
            run_node(9, &[], 3, &[input]),
            Err(NodeError::InvalidInput(_))
        ));
    }
}

#[test]
fn the_shortest_match_depends_on_the_field_size() {
    assert_eq!(
        [1, 2, 4, 8].map(min_match),
        [4, 2, 1, 1],
        "kMinMatch(fieldSize)"
    );
    assert_eq!(max_sequences(10, 2), 6);
    assert_eq!(max_sequences(10, 4), 11);
}
