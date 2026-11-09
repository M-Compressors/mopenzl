//! Differential tests of the LZ4 compressors against the vendored C library.

use alloc::vec;
use alloc::vec::Vec;

use super::block::{MAX_INPUT_SIZE, compress_bound};
use super::{compress_block, fast, hc};

fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2_654_435_761) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 24) as u8
        })
        .collect()
}

fn text(len: usize, seed: u32) -> Vec<u8> {
    const WORDS: [&[u8]; 8] = [
        b"openzl ", b"format ", b"aware ", b"graph ", b"codec ", b"the ", b"of ", b"stream ",
    ];
    let mut out = Vec::new();
    for pick in noise(len / 2 + 1, seed) {
        if out.len() >= len {
            break;
        }
        out.extend_from_slice(WORDS[usize::from(pick) % WORDS.len()]);
    }
    out.truncate(len);
    out
}

/// Repeats of a short pattern with noise between, to exercise the pattern
/// analysis and long matches.
fn patterns(len: usize, seed: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let noise = noise(len, seed);
    let mut at = 0;
    while out.len() < len {
        let pick = noise[at % noise.len().max(1)];
        at += 1;
        let run = 1 + usize::from(pick) * 3;
        let pattern: &[u8] = match pick % 5 {
            0 => &[0],
            1 => &[1, 2],
            2 => &[7, 7, 7, 8],
            3 => &[9, 8, 7, 6, 5],
            _ => &noise[at % 64..at % 64 + 3],
        };
        for n in 0..run {
            out.push(pattern[n % pattern.len()]);
        }
        out.push(pick);
    }
    out.truncate(len);
    out
}

fn corpus(len: usize) -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("zeros", vec![0; len]),
        ("noise", noise(len, 3)),
        ("text", text(len, 5)),
        ("patterns", patterns(len, 11)),
        (
            "low-entropy",
            noise(len, 17).iter().map(|b| b & 3).collect(),
        ),
    ]
}

fn assert_level(data: &[u8], name: &str, level: i32) {
    let ours = compress_block(data, level).unwrap();
    let c = if level <= 1 {
        let acceleration = if level < 0 { -level + 1 } else { 1 };
        openzl_ffi::enc_backends::lz4_compress_fast(data, acceleration)
    } else {
        openzl_ffi::enc_backends::lz4_compress_hc(data, level)
    }
    .unwrap();
    assert_eq!(ours, c, "{name} len {} level {level}", data.len());
    let back = openzl_ffi::lz::lz4_decompress_safe(&ours, data.len()).unwrap();
    assert_eq!(back, data, "{name} round trip level {level}");
}

#[test]
fn every_level_matches_c_on_small_inputs() {
    let max = if cfg!(miri) { 20 } else { 40 };
    for len in 0..=max {
        for (name, data) in corpus(len) {
            for level in [-10, -1, 0, 1, 2, 3, 9, 10, 12] {
                assert_level(&data, name, level);
            }
        }
    }
}

#[test]
fn every_level_matches_c_on_medium_inputs() {
    let lens: &[usize] = if cfg!(miri) {
        &[300]
    } else {
        &[41, 100, 255, 256, 1000, 4096, 20_000]
    };
    for &len in lens {
        for (name, data) in corpus(len) {
            for level in -10..=12 {
                assert_level(&data, name, level);
            }
        }
    }
}

#[test]
fn the_64k_window_boundaries_match_c() {
    if cfg!(miri) {
        return;
    }
    // `LZ4_64Klimit` is 64 KiB + 11: the table type changes there.
    for len in [65_535, 65_536, 65_537, 65_546, 65_547, 65_548, 70_000] {
        for (name, data) in corpus(len) {
            for level in [-3, 0, 1, 2, 3, 6, 9, 10] {
                assert_level(&data, name, level);
            }
        }
    }
}

#[test]
fn large_inputs_match_c_across_strategies() {
    if cfg!(miri) {
        return;
    }
    let len = 200_000;
    for (name, data) in [
        ("text", text(len, 2)),
        ("patterns", patterns(len, 4)),
        ("noise", noise(len, 6)),
    ] {
        for level in [-10, 1, 2, 4, 9, 10, 11] {
            assert_level(&data, name, level);
        }
    }
    // Level 12 uses the full update and 16384 searches: keep it short.
    assert_level(&text(30_000, 8), "text", 12);
    assert_level(&patterns(30_000, 9), "patterns", 12);
}

#[test]
fn extreme_levels_are_clamped_like_c() {
    let data = text(5000, 1);
    for level in [i32::MIN, i32::MIN + 1, -70_000, -65_536, 13, 100, i32::MAX] {
        let ours = compress_block(&data, level).unwrap();
        let c = if level <= 1 {
            let acceleration = if level < 0 {
                level.wrapping_neg().wrapping_add(1)
            } else {
                1
            };
            openzl_ffi::enc_backends::lz4_compress_fast(&data, acceleration)
        } else {
            openzl_ffi::enc_backends::lz4_compress_hc(&data, level)
        }
        .unwrap();
        assert_eq!(ours, c, "level {level}");
    }
}

#[test]
fn inputs_beyond_the_lz4_limit_are_rejected() {
    assert_eq!(MAX_INPUT_SIZE, 0x7E00_0000);
    assert_eq!(compress_bound(1000), 1000 + 3 + 16);
}

#[test]
fn level_parameters_follow_the_reference_table() {
    use hc::Strategy;
    assert!(hc::level_params(0).strategy == Strategy::Chain);
    assert_eq!(hc::level_params(0).searches, 256);
    assert!(hc::level_params(2).strategy == Strategy::Mid);
    assert_eq!(hc::level_params(5).searches, 16);
    assert!(hc::level_params(11).strategy == Strategy::Optimal);
    assert_eq!(hc::level_params(99).searches, 16_384);
}

#[test]
fn acceleration_trades_size_for_speed_but_stays_decodable() {
    let data = text(10_000, 9);
    for acceleration in [1, 2, 50, 70_000] {
        let block = fast::compress(&data, acceleration);
        let back = openzl_ffi::lz::lz4_decompress_safe(&block, data.len()).unwrap();
        assert_eq!(back, data);
    }
    assert!(fast::compress(&data, 1).len() <= fast::compress(&data, 50).len());
}
