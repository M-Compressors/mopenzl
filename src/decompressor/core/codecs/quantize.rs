//! The quantize codecs (`DI_quantizeOffsets`, `DI_quantizeLengths`,
//! `decode_quantize_kernel.c`, `common_quantize.c`), feature `quantize`.
//!
//! Two inputs: one-byte codes (numeric) and a serial stream of extra bits.
//! A code `c` stands for `base[c]` plus the next `bits[c]` bits of the
//! stream (a forward, least-significant-bit-first reader); the output is
//! 32-bit numbers. Offsets use one power of two per code; lengths keep the
//! values below 16 as their own codes and quantize the rest by powers of
//! two. The reference's kernels differ only in how many values they unroll
//! per refill, so one straight loop gives the same values.
//!
//! The reference ignores bytes left in the bit stream but accepts reads a
//! few bits past its end when the final word is zero padded; this port
//! rejects any read past the end, which valid frames never make.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_quantize_offsets`.
pub(crate) const QUANTIZE_OFFSETS: Desc = Desc {
    id: 25,
    min_version: 3,
    regens: &[Type::Numeric],
    variable_regens: false,
    inputs: &[Type::Numeric, Type::Serial],
    variable_inputs: 0,
    decode: decode_offsets,
};

/// `ZL_StandardTransformID_quantize_lengths`.
pub(crate) const QUANTIZE_LENGTHS: Desc = Desc {
    id: 26,
    min_version: 3,
    regens: &[Type::Numeric],
    variable_regens: false,
    inputs: &[Type::Numeric, Type::Serial],
    variable_inputs: 0,
    decode: decode_lengths,
};

/// A quantization scheme: extra bits and base value of every code.
struct Params<const N: usize> {
    bits: [u8; N],
    base: [u32; N],
}

/// `ZL_quantizeOffsetsParams`: code `c` is `1 << c` plus `c` bits.
const OFFSETS: Params<32> = {
    let mut bits = [0u8; 32];
    let mut base = [0u32; 32];
    let mut code = 0;
    while code < 32 {
        bits[code] = code as u8;
        base[code] = 1 << code;
        code += 1;
    }
    Params { bits, base }
};

/// `ZL_quantizeLengthsParams`: codes 0 to 15 are the values themselves;
/// code `c` from 16 on is `1 << (c - 12)` plus `c - 12` bits.
const LENGTHS: Params<44> = {
    let mut bits = [0u8; 44];
    let mut base = [0u32; 44];
    let mut code = 0;
    while code < 44 {
        if code < 16 {
            base[code] = code as u32;
        } else {
            bits[code] = (code - 12) as u8;
            base[code] = 1 << (code - 12);
        }
        code += 1;
    }
    Params { bits, base }
};

fn decode_offsets(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    quantize(ctx, inputs, regens, &OFFSETS)
}

fn decode_lengths(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    quantize(ctx, inputs, regens, &LENGTHS)
}

/// Reads bits least significant first, tracking reads past the end.
struct Bits<'a> {
    bytes: &'a [u8],
    /// Bits consumed so far.
    position: usize,
}

impl Bits<'_> {
    /// The next `count` (at most 32) bits; bits past the end read as zero
    /// and show up in [`Self::overrun`].
    fn read(&mut self, count: u32) -> u32 {
        let first = self.position / 8;
        let shift = self.position % 8;
        let mut word = [0u8; 8];
        if let Some(rest) = self.bytes.get(first..) {
            for (to, from) in word.iter_mut().zip(rest) {
                *to = *from;
            }
        }
        self.position += count as usize;
        ((u64::from_le_bytes(word) >> shift) & ((1u64 << count) - 1)) as u32
    }

    /// Whether more bits were read than the stream holds.
    const fn overrun(&self) -> bool {
        self.position > self.bytes.len() * 8
    }
}

/// `ZS2_quantize32Decode` over `params`.
fn quantize<const N: usize>(
    _: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
    params: &Params<N>,
) -> Result<(), DecodeError> {
    let (codes, extra) = (&inputs[0], &inputs[1]);
    if codes.width != 1 {
        return Err(corrupt());
    }
    // `ZL_maxCode` checked against the scheme's code count.
    if codes.bytes.iter().any(|&code| usize::from(code) >= N) {
        return Err(corrupt());
    }
    let out = regens[0].fixed(4, codes.bytes.len())?;
    out.resize(codes.bytes.len() * 4, 0);
    let mut bits = Bits {
        bytes: extra.bytes,
        position: 0,
    };
    for (to, &code) in out.as_chunks_mut::<4>().0.iter_mut().zip(codes.bytes) {
        let code = usize::from(code);
        let value = params.base[code].wrapping_add(bits.read(u32::from(params.bits[code])));
        *to = value.to_le_bytes();
    }
    if bits.overrun() {
        return Err(corrupt());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    const DESC: Desc = QUANTIZE_OFFSETS;
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn run(header: &[u8], inputs: &[Input<'_>]) -> Result<Stream, DecodeError> {
        run_with(&DESC, header, inputs)
    }

    fn run_with(desc: &Desc, header: &[u8], inputs: &[Input<'_>]) -> Result<Stream, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, header);
        let mut regens = [Stream::default()];
        regens[0].prepare(desc.regen_type(0), 1 << 16);
        (desc.decode)(&mut ctx, inputs, &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream)
    }

    fn num(width: usize, bytes: &[u8]) -> Input<'_> {
        Input {
            ty: Type::Numeric,
            width,
            bytes,
            lens: &[],
        }
    }

    /// Packs `(value, bits)` pairs least significant bit first.
    fn pack(fields: &[(u64, u32)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let (mut buffer, mut filled) = (0u128, 0u32);
        for &(value, bits) in fields {
            buffer |= u128::from(value) << filled;
            filled += bits;
            while filled >= 8 {
                bytes.push(buffer as u8);
                buffer >>= 8;
                filled -= 8;
            }
        }
        if filled > 0 {
            bytes.push(buffer as u8);
        }
        bytes
    }

    fn values(stream: &Stream) -> Vec<u32> {
        stream
            .bytes()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect()
    }

    #[test]
    fn offsets_add_extra_bits_to_a_power_of_two() {
        // 1, 2 + 1, 4 + 3, 2^31 + 0x7fff_ffff.
        let bits = pack(&[(0, 0), (1, 1), (3, 2), (0x7fff_ffff, 31)]);
        let codes = [0, 1, 2, 31];
        let out = run(&[], &[num(1, &codes), Input::serial(&bits)]).unwrap();
        assert_eq!(values(&out), [1, 3, 7, 0xffff_ffff]);
        assert_eq!(out.as_input().width, 4);
    }

    #[test]
    fn lengths_keep_small_values_as_codes() {
        let codes = [0, 15, 16, 17, 43];
        let bits = pack(&[(0xa, 4), (0x1f, 5), (0x7fff_ffff, 31)]);
        let out = run_with(
            &QUANTIZE_LENGTHS,
            &[],
            &[num(1, &codes), Input::serial(&bits)],
        )
        .unwrap();
        assert_eq!(values(&out), [0, 15, 16 + 0xa, 32 + 0x1f, 0xffff_ffff]);
    }

    #[test]
    fn no_codes_need_no_bits() {
        let out = run(&[], &[num(1, &[]), Input::serial(&[])]).unwrap();
        assert!(out.bytes().is_empty());
    }

    #[test]
    fn codes_past_the_scheme_are_corrupt() {
        let bits = [0u8; 8];
        assert_eq!(
            run(&[], &[num(1, &[32]), Input::serial(&bits)]).err(),
            Some(corrupt())
        );
        assert_eq!(
            run_with(
                &QUANTIZE_LENGTHS,
                &[],
                &[num(1, &[44]), Input::serial(&bits)]
            )
            .err(),
            Some(corrupt())
        );
        assert!(
            run_with(
                &QUANTIZE_LENGTHS,
                &[],
                &[num(1, &[43]), Input::serial(&bits)]
            )
            .is_ok()
        );
    }

    #[test]
    fn wide_codes_are_corrupt() {
        assert_eq!(
            run(&[], &[num(2, &[0, 0]), Input::serial(&[])]).err(),
            Some(corrupt())
        );
    }

    #[test]
    fn reading_past_the_extra_bits_is_corrupt() {
        // Code 9 needs nine bits; one byte holds eight.
        assert_eq!(
            run(&[], &[num(1, &[9]), Input::serial(&[0xff])]).err(),
            Some(corrupt())
        );
        assert_eq!(
            run(&[], &[num(1, &[9]), Input::serial(&[])]).err(),
            Some(corrupt())
        );
        // Leftover bits are ignored.
        assert!(run(&[], &[num(1, &[1]), Input::serial(&[1, 2, 3])]).is_ok());
        // Exactly the stream's last bit.
        assert!(run(&[], &[num(1, &[8]), Input::serial(&[1, 1])]).is_ok());
        assert!(run(&[], &[num(1, &[23]), Input::serial(&[0; 3])]).is_ok());
        assert!(run(&[], &[num(1, &[25]), Input::serial(&[0; 3])]).is_err());
    }

    #[test]
    fn tables_have_the_reference_values() {
        assert_eq!(OFFSETS.base[31], 0x8000_0000);
        assert_eq!(LENGTHS.bits[15], 0);
        assert_eq!(LENGTHS.bits[16], 4);
        assert_eq!(LENGTHS.base[16], 16);
        assert_eq!(LENGTHS.bits[43], 31);
        assert_eq!(LENGTHS.base[43], 0x8000_0000);
    }
}
