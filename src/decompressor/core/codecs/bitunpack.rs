//! Decoder of the `bitunpack` feature: `bitunpack` (34).
//!
//! Ports `codecs/bitunpack/decode_bitunpack_binding.c`. The encoder
//! unpacked a bit-packed serial stream into numbers; the decoder packs the
//! numbers back, `nbBits` each, least significant bit first
//! (`ZS_bitpackEncode*` of `codecs/bitpack/common_bitpack_kernel.c`).
//!
//! The header is `[nbBits]` or `[nbBits, remBits]`: the second byte holds
//! the bits of the source's last byte that did not fill an element (format
//! version 7 and later), OR-ed back into the top of the last byte.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_bitunpack`: numbers packed back into bytes.
pub(crate) const BITUNPACK: Desc = Desc {
    id: 34,
    min_version: 6,
    regens: &[Type::Serial],
    variable_regens: false,
    inputs: &[Type::Numeric],
    variable_inputs: 0,
    decode,
};

/// `DI_bitunpack`.
fn decode(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let input = &inputs[0];
    let (&bits, rest) = ctx.header.split_first().ok_or_else(corrupt)?;
    if rest.len() > 1 {
        return Err(corrupt());
    }
    let bits = u32::from(bits);
    if bits as usize > 8 * input.width {
        return Err(corrupt());
    }
    let count = input.len();
    let total_bits = count.checked_mul(bits as usize).ok_or_else(corrupt)?;
    let size = total_bits.div_ceil(8);
    let out = regens[0].fixed(1, size)?;
    match input.width {
        1 => pack::<1>(out, input.bytes, bits),
        2 => pack::<2>(out, input.bytes, bits),
        4 => pack::<4>(out, input.bytes, bits),
        8 => pack::<8>(out, input.bytes, bits),
        _ => return Err(corrupt()),
    }
    if let Some(&rem) = rest.first() {
        // Leftover bits of the last byte, which the elements do not cover.
        let spare = size * 8 - total_bits;
        if spare == 0 {
            return Err(corrupt());
        }
        let last = out.last_mut().ok_or_else(corrupt)?;
        *last |= rem << (8 - spare);
    }
    Ok(())
}

/// Appends each `W`-byte number of `src` to `out` in `bits` bits, least
/// significant bit first, as `ZS_bitpackEncode` writes them.
///
/// The encoder only writes numbers that fit `bits`. A damaged frame can hold
/// wider ones, and the reference's portable kernels do not mask them, so
/// their excess bits spill into the fields above. This mirrors those
/// kernels:
/// - `bits` a multiple of 8, and every 64-bit number, are truncated to the
///   field (`ZS_bytepackEncode*`, `ZS_BitCStreamFF_write`);
/// - 8- and 16-bit numbers are OR-ed in whole (`ZS_bitpackEncode{8,16}_generic`);
/// - up to 64 one-bit fields of 32-bit numbers are summed with carries
///   (`bit1pack32`);
/// - other 32-bit numbers go through the 64-bit accumulator of
///   `ZS_bitpackEncode32_generic`, which drops what passes bit 63.
///
/// The reference's AVX2 and BMI2 kernels, which only an `-mavx2 -mbmi2`
/// build compiles, saturate such numbers instead.
fn pack<const W: usize>(out: &mut alloc::vec::Vec<u8>, src: &[u8], bits: u32) {
    if bits as usize == W * 8 {
        out.extend_from_slice(src);
        return;
    }
    if bits == 0 || src.is_empty() {
        return;
    }
    let numbers = src.as_chunks::<W>().0.iter().map(|element| {
        let mut word = [0u8; 8];
        word[..W].copy_from_slice(element);
        u64::from_le_bytes(word)
    });
    if bits.is_multiple_of(8) || W == 8 {
        pack_masked(out, numbers, bits);
    } else if W < 4 {
        pack_spilling(out, numbers, bits);
    } else if bits == 1 && src.len() <= 64 * W {
        pack_summed(out, numbers, src.len() / W);
    } else {
        pack_accumulated(out, numbers, src.len() / W, bits);
    }
}

/// Packs the `bits` low bits of each number.
fn pack_masked(out: &mut alloc::vec::Vec<u8>, numbers: impl Iterator<Item = u64>, bits: u32) {
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    // At most 7 pending bits plus a 64-bit field.
    let mut acc = 0u128;
    let mut pending = 0u32;
    for number in numbers {
        acc |= u128::from(number & mask) << pending;
        pending += bits;
        while pending >= 8 {
            out.push(acc as u8);
            acc >>= 8;
            pending -= 8;
        }
    }
    if pending > 0 {
        out.push(acc as u8);
    }
}

/// Packs numbers of at most 16 bits whole: fewer than 8 pending bits plus
/// 16 fit the accumulator, so no bit is lost before it is written or
/// falls past the last byte.
fn pack_spilling(out: &mut alloc::vec::Vec<u8>, numbers: impl Iterator<Item = u64>, bits: u32) {
    let mut acc = 0u64;
    let mut pending = 0u32;
    for number in numbers {
        acc |= number << pending;
        pending += bits;
        while pending >= 8 {
            out.push(acc as u8);
            acc >>= 8;
            pending -= 8;
        }
    }
    if pending > 0 {
        out.push(acc as u8);
    }
}

/// `bit1pack32`: the sum of number `i` times `2^i`, modulo `2^64`, in
/// `ceil(count / 8)` little-endian bytes.
fn pack_summed(out: &mut alloc::vec::Vec<u8>, numbers: impl Iterator<Item = u64>, count: usize) {
    let acc = numbers
        .enumerate()
        .fold(0u64, |acc, (i, number)| acc.wrapping_add(number << i));
    out.extend_from_slice(&acc.to_le_bytes()[..count.div_ceil(8)]);
}

/// `ZS_bitpackEncode32_generic`: rounds of `56 / bits` numbers OR-ed into a
/// 64-bit accumulator while 8 bytes of output remain, then one number at a
/// time, writing whole bytes once the next field may not fit.
fn pack_accumulated(
    out: &mut alloc::vec::Vec<u8>,
    mut numbers: impl Iterator<Item = u64>,
    count: usize,
    bits: u32,
) {
    let start = out.len();
    let size = (count * bits as usize).div_ceil(8);
    let mut acc = 0u64;
    let mut pos = 0u32;
    let packs = (56 / bits) as usize;
    for _ in 0..count / packs {
        if out.len() - start + 7 >= size {
            break;
        }
        for number in numbers.by_ref().take(packs) {
            acc |= number << pos;
            pos += bits;
        }
        let whole = pos / 8;
        commit(out, &mut acc, &mut pos, whole);
    }
    let limit = 63 - bits;
    for number in numbers {
        acc |= number << pos;
        pos += bits;
        if pos > limit {
            let whole = pos / 8;
            commit(out, &mut acc, &mut pos, whole);
        }
    }
    let rest = pos.div_ceil(8);
    commit(out, &mut acc, &mut pos, rest);
}

/// Writes the `bytes` low bytes of `acc` and keeps the bits past them.
fn commit(out: &mut alloc::vec::Vec<u8>, acc: &mut u64, pos: &mut u32, bytes: u32) {
    out.extend_from_slice(&acc.to_le_bytes()[..bytes as usize]);
    // `pos` stays below 64, so at most 7 whole bytes are written here and
    // the shift is in range; the last commit writes 8 and discards `acc`.
    *acc = acc.checked_shr(bytes * 8).unwrap_or(0);
    *pos &= 7;
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn run(
        header: &[u8],
        width: usize,
        bytes: &[u8],
        budget: usize,
    ) -> Result<Vec<u8>, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, header);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::Serial, budget);
        let input = Input {
            ty: Type::Numeric,
            width,
            bytes,
            lens: &[],
        };
        (BITUNPACK.decode)(&mut ctx, &[input], &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream.bytes().to_vec())
    }

    #[test]
    fn numbers_pack_least_significant_bit_first() {
        // 3 bits each: 1, 2, 3, 4 -> 001 010 011 100 = 0b1001_1101_0001
        let out = run(&[3], 1, &[1, 2, 3, 4], 16).unwrap();
        assert_eq!(out, [0xd1, 0x08]);
        let out = run(&[12], 2, &[0xff, 0x0f, 0x01, 0x00], 16).unwrap();
        assert_eq!(out, [0xff, 0x1f, 0x00]);
    }

    #[test]
    fn full_width_and_zero_width_fields() {
        let bytes: Vec<u8> = (0u8..16).collect();
        assert_eq!(run(&[64], 8, &bytes, 64).unwrap(), bytes);
        assert_eq!(run(&[16], 2, &bytes, 64).unwrap(), bytes);
        assert!(run(&[0], 4, &bytes, 64).unwrap().is_empty());
        assert!(run(&[7], 1, &[], 64).unwrap().is_empty());
        // 40 bits per number: the widest path through the accumulator.
        let wide = run(&[40], 8, &[0xff; 16], 64).unwrap();
        assert_eq!(wide, [0xff; 10]);
        assert_eq!(run(&[63], 8, &[0xff; 8], 64).unwrap().len(), 8);
    }

    #[test]
    fn leftover_bits_return_to_the_last_byte() {
        // Two 3-bit numbers use 6 bits; the 2 spare bits hold 0b10.
        let out = run(&[3, 0b10], 1, &[5, 6], 16).unwrap();
        assert_eq!(out, [0xb5]);
    }

    /// `count` numbers of `width` bytes from a xorshift stream, many of them
    /// wider than any field narrower than the width.
    fn wide_numbers(seed: u64, count: usize, width: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        (0..count)
            .flat_map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x.to_le_bytes()[..width].to_vec()
            })
            .collect()
    }

    #[test]
    fn numbers_wider_than_their_field_pack_like_the_reference() {
        // Counts around the 1-bit special case (64) and the 8-byte limit of
        // the 32-bit rounds.
        let counts = [1usize, 2, 3, 7, 8, 9, 15, 16, 17, 63, 64, 65, 100, 257];
        for width in [1usize, 2, 4, 8] {
            for bits in 1..=8 * width as u32 {
                for count in counts {
                    let numbers = wide_numbers(u64::from(bits) << 16 | count as u64, count, width);
                    let expected = openzl_ffi::enc_backends::bitpack_encode(&numbers, width, bits)
                        .expect("valid kernel arguments");
                    let ours = run(&[bits as u8], width, &numbers, 1 << 12).unwrap();
                    assert_eq!(
                        ours, expected,
                        "width {width}, {bits} bits, {count} numbers"
                    );
                }
            }
        }
    }

    #[test]
    fn one_bit_fields_of_32_bit_numbers_carry_into_each_other() {
        // `bit1pack32` adds the numbers: 3 at field 0 and 1 at field 1 sum
        // to 0b101, where OR-ing them would give 0b011.
        let numbers = [3u32, 1].map(u32::to_le_bytes).concat();
        assert_eq!(run(&[1], 4, &numbers, 16).unwrap(), [0b101]);
    }

    #[test]
    fn excess_bits_spill_into_the_next_field() {
        // 3-bit fields: 0b1111 leaves its top bit in field 1.
        assert_eq!(run(&[3], 1, &[0b1111, 0], 16).unwrap(), [0b1111]);
        // A byte-aligned field truncates instead.
        assert_eq!(run(&[8], 2, &[0x34, 0x12, 0, 0], 16).unwrap(), [0x34, 0]);
        // 64-bit numbers are masked to the field.
        assert_eq!(run(&[4], 8, &[0xff; 16], 16).unwrap(), [0xff]);
    }

    #[test]
    fn malformed_headers_are_rejected() {
        assert!(run(&[], 1, &[1], 16).is_err());
        assert!(run(&[3, 0, 0], 1, &[1], 16).is_err());
        // More bits than the width.
        assert!(run(&[9], 1, &[1], 16).is_err());
        // Spare bits promised but the elements fill whole bytes.
        assert!(run(&[8, 1], 1, &[1], 16).is_err());
        assert!(run(&[0, 1], 1, &[], 16).is_err());
        // Output beyond the budget, and a width no stream has.
        assert!(run(&[8], 1, &[1; 8], 4).is_err());
        assert!(run(&[8], 3, &[1; 6], 16).is_err());
    }
}
