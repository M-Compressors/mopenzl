//! Tests of the `lz` node and its parsers, against a reference decoder
//! written from the sequence semantics.

use alloc::vec;
use alloc::vec::Vec;

use super::kernel::{
    self, DOUBLE_FAST, FAST, GREEDY, LAZY, LAZY2, Parameters, Sequences, max_sequences,
};
use super::*;
use crate::Backend;
use crate::compressor::core::codecs::test_util::{noise, run_node, text, u16s, u32s};

/// Rebuilds the input from its sequences (`ZL_Lz` decoding semantics).
fn rebuild(seq: &Sequences) -> Vec<u8> {
    let mut out = Vec::new();
    let mut literals = seq.literals.iter().copied();
    for i in 0..seq.offsets.len() {
        for _ in 0..seq.literal_lengths[i] {
            out.push(literals.next().unwrap());
        }
        let offset = seq.offsets[i] as usize;
        for _ in 0..seq.match_lengths[i] {
            out.push(out[out.len() - offset]);
        }
    }
    out.extend(literals);
    out
}

fn params(strategy: u32, window_log: u32, size: usize) -> Parameters {
    let mut params = Parameters::for_level(3);
    params.strategy = strategy;
    params.window_log = window_log;
    params.hash_log1 = 14;
    params.hash_log2 = 14;
    params.search_log = 3;
    params.adjust(size);
    params
}

#[test]
fn every_strategy_reproduces_the_input_from_its_sequences() {
    let mut inputs = vec![text(20_000, 1), noise(3_000, 2), vec![0; 5_000], Vec::new()];
    inputs.push(b"abcabcabcabcabcabcabcabcabcabcabcabcabcabcabc".repeat(5));
    for src in &inputs {
        for strategy in [FAST, DOUBLE_FAST, GREEDY, LAZY, LAZY2] {
            let params = params(strategy, 20, src.len());
            let seq = kernel::encode(Backend::default().0, src, &params);
            assert_eq!(&rebuild(&seq), src, "strategy {strategy}");
            for backend in Backend::available() {
                let other = kernel::encode(backend.0, src, &params);
                assert_eq!(other, seq, "{backend}, strategy {strategy}");
            }
            assert!(seq.offsets.len() <= max_sequences(src.len()));
            let width = params.offset_width();
            let limit = if width == 2 { 1 << 16 } else { 1 << 28 };
            assert!(seq.offsets.iter().all(|&o| o >= 1 && o < limit));
        }
    }
}

#[test]
fn lengths_beyond_sixteen_bits_split_into_several_sequences() {
    let mut src = noise(100, 3);
    src.extend_from_slice(&vec![0; 200_000]);
    for strategy in [FAST, DOUBLE_FAST, GREEDY, LAZY, LAZY2] {
        let params = params(strategy, 20, src.len());
        let seq = kernel::encode(Backend::default().0, &src, &params);
        assert_eq!(rebuild(&seq), src, "strategy {strategy}");
        assert!(seq.match_lengths.contains(&u16::MAX));
    }
    // A literal run beyond 16 bits, split into sequences with no match and
    // offset 1, followed by matches.
    let mut src = noise(140_000, 4);
    src.extend_from_slice(&noise(1_000, 5).repeat(50));
    let params = params(FAST, 28, src.len());
    let seq = kernel::encode(Backend::default().0, &src, &params);
    assert_eq!(rebuild(&seq), src);
    assert_eq!(seq.literal_lengths[0], u16::MAX);
    assert_eq!((seq.match_lengths[0], seq.offsets[0]), (0, 1));
}

#[test]
fn level_presets_and_adjustment_follow_the_reference() {
    assert_eq!(Parameters::for_level(0), Parameters::for_level(1));
    assert_eq!(Parameters::for_level(99), Parameters::for_level(7));
    let negative = Parameters::for_level(-5);
    assert_eq!((negative.strategy, negative.acceleration), (FAST, 5));
    assert_eq!(Parameters::for_level(i32::MIN).acceleration, 1 << 31);
    assert_eq!(Parameters::for_level(3).strategy, DOUBLE_FAST);
    assert_eq!(Parameters::for_level(5).strategy, GREEDY);
    assert_eq!(Parameters::for_level(7).strategy, LAZY2);

    let mut tiny = Parameters::for_level(7);
    tiny.adjust(0);
    assert_eq!((tiny.window_log, tiny.hash_log2), (0, 0));
    assert_eq!(tiny.hash_log1, 4, "a row table keeps at least one row");
    let mut fast = Parameters::for_level(1);
    fast.hash_log2 = 12;
    fast.adjust(1_000_000);
    assert_eq!(fast.hash_log2, 0, "one table only");
    let mut wild = Parameters {
        strategy: 99,
        window_log: 99,
        hash_log1: 99,
        hash_log2: 99,
        hash_length: 99,
        acceleration: 99_999,
        search_log: 99,
    };
    wild.adjust(1 << 30);
    assert_eq!(
        (
            wild.strategy,
            wild.window_log,
            wild.hash_log1,
            wild.hash_length
        ),
        (LAZY2, 28, 23, 7)
    );
    assert_eq!((wild.acceleration, wild.search_log), (10_000, 4));
    assert_eq!(wild.offset_width(), 4);
    let mut small = Parameters::for_level(3);
    small.adjust(1 << 16);
    assert_eq!(small.offset_width(), 2);
}

#[test]
fn sequence_bounds_follow_the_reference() {
    assert_eq!(max_sequences(0), 0);
    assert_eq!(max_sequences(65_535), 65_535 / 4 + 1 + 2);
}

#[test]
fn the_node_emits_four_streams_a_size_header_and_the_min_match_metadata() {
    let src = text(5_000, 5);
    let out = run_node(52, &[], 3, &[Stream::serial(&src)]).unwrap();
    assert_eq!(
        out.header.as_deref(),
        Some(&[0x88, 0x27][..]),
        "varint(5000)"
    );
    let [literals, offsets, lit_lens, match_lens] = &out.singles[..] else {
        panic!("four outputs");
    };
    assert_eq!(match_lens.metadata(), [(MIN_MATCH_LENGTH_METADATA_ID, 4)]);
    assert_eq!(
        (offsets.width(), lit_lens.width(), match_lens.width()),
        (2, 2, 2)
    );
    let seq = Sequences {
        literals: literals.bytes().to_vec(),
        offsets: u16s(offsets.bytes()).into_iter().map(u32::from).collect(),
        literal_lengths: u16s(lit_lens.bytes()),
        match_lengths: u16s(match_lens.bytes()),
    };
    assert_eq!(rebuild(&seq), src);
}

#[test]
fn a_large_window_gives_wide_offsets() {
    let mut src = noise(70_000, 6);
    src.extend_from_slice(&noise(70_000, 6));
    let out = run_node(52, &[(101, 20)], 3, &[Stream::serial(&src)]).unwrap();
    let offsets = &out.singles[1];
    assert_eq!(offsets.width(), 4);
    assert!(
        u32s(offsets.bytes())
            .iter()
            .any(|&o| o > u32::from(u16::MAX))
    );
}

#[test]
fn local_parameters_override_the_defaults_of_the_level() {
    let src = text(20_000, 7);
    let sizes = |ints: &[(i32, i32)], level: i32| {
        let out = run_node(52, ints, level, &[Stream::serial(&src)]).unwrap();
        out.singles[0].content_size()
    };
    // The node's level replaces the global one; a zero keeps the global.
    assert_eq!(sizes(&[(1, 7)], 1), sizes(&[], 7));
    assert_eq!(sizes(&[(1, 0)], 7), sizes(&[], 7));
    // A strategy override replaces the level's strategy only.
    assert_ne!(sizes(&[(102, 5)], 1), sizes(&[], 1));
    // Unknown parameters are ignored.
    assert_eq!(sizes(&[(5000, 1)], 3), sizes(&[], 3));
}

#[test]
fn empty_input_gives_empty_streams_and_a_zero_header() {
    let out = run_node(52, &[], 3, &[Stream::serial(&[])]).unwrap();
    assert_eq!(out.header.as_deref(), Some(&[0][..]));
    assert!(out.singles.iter().all(|s| s.content_size() == 0));
}

#[test]
fn metadata_can_only_be_set_on_a_created_singleton_output() {
    use crate::compressor::core::codecs::test_util::with_cx;
    with_cx(52, &[], 3, |mut cx| {
        assert_eq!(
            cx.set_int_metadata(3, 5, 1),
            Err(NodeError::OutcomeOutOfRange),
            "not created yet"
        );
        assert_eq!(
            cx.set_int_metadata(9, 5, 1),
            Err(NodeError::OutcomeOutOfRange),
            "no such outcome"
        );
        cx.emit(3, 2, vec![0, 0]).unwrap();
        assert_eq!(cx.set_int_metadata(3, 5, 1), Ok(()));
    });
}
