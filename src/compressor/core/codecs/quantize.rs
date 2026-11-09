//! The quantize nodes (`EI_quantizeOffsets`, `EI_quantizeLengths`,
//! `encode_quantize_{binding,kernel}.c`, `common_quantize.c`), nodes 43 and
//! 44, codecs 25 and 26.
//!
//! Each 32-bit value becomes a one-byte code and a number of extra bits. The
//! codes form the first output (numeric, width 1); the extra bits, packed
//! least significant bit first and in value order, form the second (serial).
//! Offsets use one code per power of two (value 0 is an error); lengths keep
//! the values below 16 as their own codes (no extra bits) and quantize the
//! rest by powers of two.

use alloc::vec::Vec;

use super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;

/// The offsets encoder.
pub(crate) static QUANTIZE_OFFSETS: FnEncoder =
    FnEncoder(|cx, inputs| quantize(cx, &inputs[0], &OFFSETS));
/// The lengths encoder.
pub(crate) static QUANTIZE_LENGTHS: FnEncoder =
    FnEncoder(|cx, inputs| quantize(cx, &inputs[0], &LENGTHS));

/// `ZL_Quantize32Params`, reduced to what the encoder reads.
struct Scheme {
    /// Values below this use `value_to_code`; larger ones the power of two.
    max_pow2: u32,
    /// The code of the first power of two minus its logarithm.
    delta: u32,
    /// The code of each value below `max_pow2`.
    value_to_code: &'static [u8],
    /// Extra bits of code `c` (`bits(c)`).
    bits: fn(u8) -> u32,
}

/// `ZL_quantizeOffsetsParams`: code `c` is `1 << c` plus `c` bits.
static OFFSETS: Scheme = Scheme {
    max_pow2: 0,
    delta: 0,
    value_to_code: &[0],
    bits: offset_bits,
};

/// `offsetBits`: code `c` has `c` extra bits.
fn offset_bits(code: u8) -> u32 {
    u32::from(code)
}

/// `lengthBits`: none below code 16, then `c - 12`.
fn length_bits(code: u8) -> u32 {
    if code < 16 { 0 } else { u32::from(code) - 12 }
}

/// `ZL_quantizeLengthsParams`: codes 0 to 15 are the values; from 16 on,
/// code `c` is `1 << (c - 12)` plus `c - 12` bits.
static LENGTHS: Scheme = Scheme {
    max_pow2: 16,
    delta: 12,
    value_to_code: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    bits: length_bits,
};

impl Scheme {
    /// `ZL_code32`.
    fn code(&self, value: u32) -> u8 {
        if value >= self.max_pow2 {
            (value.ilog2() + self.delta) as u8
        } else {
            self.value_to_code[value as usize]
        }
    }
}

/// `ZS2_quantize32Encode` and `EI_quantize`.
fn quantize<'a>(
    cx: &mut NodeCx<'_, 'a>,
    input: &Stream<'a>,
    scheme: &Scheme,
) -> Result<(), NodeError> {
    if input.width() != 4 {
        return Err(NodeError::InvalidInput("quantize needs 32-bit numbers"));
    }
    let count = input.n_elts();
    let mut codes = Vec::with_capacity(count);
    let mut bits = Vec::with_capacity(count);
    let (mut buffer, mut filled) = (0u64, 0u32);
    for chunk in input.bytes().as_chunks::<4>().0 {
        let value = u32::from_le_bytes(*chunk);
        if scheme.max_pow2 == 0 && value == 0 {
            return Err(NodeError::InvalidInput("a quantized offset cannot be 0"));
        }
        let code = scheme.code(value);
        codes.push(code);
        let extra = (scheme.bits)(code);
        buffer |= u64::from(value & ((1u64 << extra) - 1) as u32) << filled;
        filled += extra;
        while filled >= 8 {
            bits.push(buffer as u8);
            buffer >>= 8;
            filled -= 8;
        }
    }
    if filled > 0 {
        bits.push(buffer as u8);
    }
    cx.emit(0, 1, codes)?;
    cx.emit(1, 1, bits)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::core::codecs::test_util::run_node;
    use crate::compressor::core::data::{Bytes, Type};

    fn numbers(values: &[u32], width: usize) -> Stream<'static> {
        let bytes = values
            .iter()
            .flat_map(|&v| u64::from(v).to_le_bytes()[..width].to_vec())
            .collect::<Vec<u8>>();
        Stream::new(Type::Numeric, width, Bytes::from(bytes))
    }

    #[test]
    fn offsets_use_one_code_per_power_of_two() {
        let out = run_node(43, &[], 3, &[numbers(&[1, 2, 3, 4, 255, 1 << 31], 4)]).unwrap();
        assert_eq!(out.singles[0].bytes(), [0, 1, 1, 2, 7, 31]);
        // Extra bits: 0 bits, then 1 bit (0), 1 bit (1), 2 bits (0), 7 bits
        // (127), 31 bits (0), packed least significant bit first.
        let mut expected = 0u64;
        let mut at = 0;
        for (value, bits) in [(0u64, 1), (1, 1), (0, 2), (127, 7), (0, 31)] {
            expected |= value << at;
            at += bits;
        }
        assert_eq!(
            out.singles[1].bytes(),
            &expected.to_le_bytes()[..(at as usize).div_ceil(8)]
        );
    }

    #[test]
    fn lengths_keep_small_values_and_quantize_the_rest() {
        let out = run_node(44, &[], 3, &[numbers(&[0, 15, 16, 31, 32, 1 << 31], 4)]).unwrap();
        assert_eq!(out.singles[0].bytes(), [0, 15, 16, 16, 17, 43]);
        // 16 and 31 take 4 bits, 32 takes 5, 2^31 takes 31.
        assert_eq!(
            out.singles[1].content_size(),
            (4 + 4 + 5 + 31usize).div_ceil(8)
        );
    }

    #[test]
    fn empty_input_gives_empty_streams() {
        for node in [43, 44] {
            let out = run_node(node, &[], 3, &[numbers(&[], 4)]).unwrap();
            assert_eq!(out.singles[0].content_size(), 0);
            assert_eq!(out.singles[1].content_size(), 0);
        }
    }

    #[test]
    fn a_zero_offset_and_other_widths_are_rejected() {
        assert!(matches!(
            run_node(43, &[], 3, &[numbers(&[3, 0], 4)]),
            Err(NodeError::InvalidInput(_))
        ));
        // Zero is a valid length.
        assert!(run_node(44, &[], 3, &[numbers(&[0], 4)]).is_ok());
        for width in [1, 2, 8] {
            for node in [43, 44] {
                assert!(matches!(
                    run_node(node, &[], 3, &[numbers(&[1], width)]),
                    Err(NodeError::InvalidInput(_))
                ));
            }
        }
    }
}
