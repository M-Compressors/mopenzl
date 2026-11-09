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

/// Appends the `bits` low bits of each `W`-byte number of `src` to `out`,
/// least significant bit first.
fn pack<const W: usize>(out: &mut alloc::vec::Vec<u8>, src: &[u8], bits: u32) {
    if bits as usize == W * 8 {
        out.extend_from_slice(src);
        return;
    }
    if bits == 0 {
        return;
    }
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    // At most 7 pending bits plus a 64-bit field.
    let mut acc = 0u128;
    let mut pending = 0u32;
    for element in src.as_chunks::<W>().0 {
        let mut word = [0u8; 8];
        word[..W].copy_from_slice(element);
        acc |= u128::from(u64::from_le_bytes(word) & mask) << pending;
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
