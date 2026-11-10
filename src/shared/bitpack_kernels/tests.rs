use alloc::vec::Vec;

use super::writer::{BitWriter, TooSmall};
use super::{decode, encode, encode_bound};

/// Deterministic values below `1 << bits`, as little-endian elements.
fn elements(count: usize, width: usize, bits: u32, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::new();
    for _ in 0..count {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let value = if bits == 64 {
            state
        } else {
            state & ((1u64 << bits) - 1)
        };
        out.extend_from_slice(&value.to_le_bytes()[..width]);
    }
    out
}

#[test]
fn encode_then_decode_round_trips_for_every_width_and_bit_count() {
    for width in [1usize, 2, 4, 8] {
        for bits in 1..=(width as u32 * 8) {
            for count in [0usize, 1, 2, 7, 8, 9, 63, 64, 65, 200] {
                let src = elements(count, width, bits, 12345 + u64::from(bits));
                let packed = encode(&src, width, bits);
                assert_eq!(
                    packed.len(),
                    encode_bound(count, bits),
                    "w{width} b{bits} n{count}"
                );
                let (back, used) = decode(&packed, count, width, bits);
                assert_eq!(back, src, "w{width} b{bits} n{count}");
                assert_eq!(used, packed.len());
            }
        }
    }
}

#[test]
fn encode_lays_fields_out_least_significant_bit_first() {
    // Three 3-bit fields 5, 2, 7 -> 0b1_1101_0101 over two bytes.
    assert_eq!(encode(&[5, 2, 7], 1, 3), [0b1101_0101, 0b0000_0001]);
    // A field as wide as the element is a plain copy.
    assert_eq!(encode(&[1, 2, 3, 4], 2, 16), [1, 2, 3, 4]);
    assert!(encode(&[1, 2], 1, 0).is_empty());
    assert!(encode(&[], 4, 7).is_empty());
}

#[test]
fn encode_matches_the_header_packer_for_narrow_fields() {
    for bits in 1..=32u32 {
        let src = elements(97, 4, bits, 99);
        let values = src
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| u32::from_le_bytes(*chunk));
        let mut expected = Vec::new();
        crate::shared::bitpack::pack(&mut expected, values, bits);
        assert_eq!(encode(&src, 4, bits), expected, "bits {bits}");
    }
}

#[test]
fn bit_writer_matches_encode_for_wide_fields() {
    for bits in [1u32, 13, 31, 32, 33, 57, 63, 64] {
        let src = elements(50, 8, bits, 7);
        let expected = encode(&src, 8, bits);
        let mut writer = BitWriter::new(expected.len());
        for chunk in src.as_chunks::<8>().0 {
            let value = u64::from_le_bytes(*chunk);
            if bits > 32 {
                writer.write((value & 0xFFFF_FFFF) as usize, 32);
                writer.flush();
                writer.write((value >> 32) as usize, bits as usize - 32);
            } else {
                writer.write(value as usize, bits as usize);
            }
            writer.flush();
        }
        assert_eq!(writer.finish().unwrap(), expected, "bits {bits}");
    }
}

#[test]
fn bit_writer_reports_a_capacity_that_is_too_small() {
    let mut writer = BitWriter::new(1);
    writer.write(0x1FF, 9);
    assert_eq!(writer.finish(), Err(TooSmall));
    let mut writer = BitWriter::new(2);
    writer.write(0x1FF, 9);
    assert_eq!(writer.finish().unwrap(), [0xFF, 0x01]);
}

#[test]
fn bit_writer_reserves_aligned_regions_and_resumes() {
    let mut writer = BitWriter::new(16);
    writer.write(0b101, 3);
    let range = writer.reserve_aligned_bits(12).unwrap();
    assert_eq!(range, 1..3);
    writer.reserved_mut(range).copy_from_slice(&[0xAB, 0x0C]);
    writer.commit_reserved_bits();
    writer.write(0b11, 2);
    writer.flush();
    // The reserved region holds 12 bits (0xAB, then 0xC in the low nibble),
    // so the two new bits land above them.
    assert_eq!(writer.finish().unwrap(), [0b0000_0101, 0xAB, 0x3C]);

    let mut writer = BitWriter::new(16);
    let range = writer.reserve_aligned_bits(16).unwrap();
    writer.reserved_mut(range).copy_from_slice(&[1, 2]);
    writer.commit_reserved_bits();
    assert_eq!(writer.finish().unwrap(), [1, 2]);
}

#[test]
fn bit_writer_refuses_oversized_reservations() {
    let mut writer = BitWriter::new(2);
    assert!(writer.reserve_aligned_bits(24).is_none());
    assert!(writer.reserve_aligned_bits(usize::MAX).is_none());
    let mut writer = BitWriter::new(1);
    writer.write(0x3FF, 10);
    assert!(writer.reserve_aligned_bits(1).is_none());
}

#[test]
fn exp_golomb_codes_are_prefix_free_unary_lengths() {
    // order 0, value 0 -> "1"; value 1 -> "010"; value 2 -> "110".
    let mut writer = BitWriter::new(8);
    writer.write_exp_golomb(0, 0);
    writer.write_exp_golomb(1, 0);
    writer.write_exp_golomb(2, 0);
    writer.flush();
    assert_eq!(writer.finish().unwrap(), [0b0110_0101]);
}

#[test]
fn grouped_encode_matches_the_scalar_baseline() {
    for width in [1usize, 2, 4, 8] {
        for bits in 1..=(width as u32 * 8).min(20) {
            for count in [0usize, 1, 7, 8, 9, 15, 16, 17, 63, 64, 65, 129]
                .into_iter()
                .filter(|&count| !cfg!(miri) || count < 20)
            {
                let src = elements(count, width, bits, 31 + u64::from(bits));
                assert_eq!(
                    encode(&src, width, bits),
                    super::encode_scalar(&src, width, bits),
                    "w{width} b{bits} n{count}"
                );
            }
        }
    }
}

#[test]
fn bit_count_matches_the_scalar_baseline_at_every_level() {
    for width in [1usize, 2, 4, 8] {
        for bits in 1..=(width as u32 * 8) {
            for count in [0usize, 1, 5, 8, 31, 32, 33, 64, 100, 257]
                .into_iter()
                .filter(|&count| !cfg!(miri) || count < 70)
            {
                let mut src = elements(count, width, bits, 77 + u64::from(bits));
                if let Some(first) = src.get_mut(..width) {
                    // Make the top bit appear exactly once, in the first element.
                    let last = (bits - 1) as usize;
                    first[last / 8] |= 1 << (last % 8);
                }
                let expected = super::nb_bits_scalar(&src, width);
                for backend in crate::Backend::available() {
                    assert_eq!(
                        super::nb_bits(backend.0, &src, width),
                        expected,
                        "{backend} w{width} b{bits} n{count}"
                    );
                }
            }
        }
    }
}
