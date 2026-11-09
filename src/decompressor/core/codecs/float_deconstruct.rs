//! The float-deconstruct codec (`DI_float_deconstruct`,
//! `decode_float_deconstruct_kernel.c`), feature `float-deconstruct`.
//!
//! Rebuilds floating-point numbers from two streams of equal length: a
//! struct stream of sign and fraction (`ins[0]`) and a serial stream of
//! exponent bytes (`ins[1]`). The header is one byte naming the type: 0 for
//! float32 (3-byte sign-fraction, 4-byte output), 1 for bfloat16 (1 byte, 2
//! bytes), 2 for float16 (2 bytes, 2 bytes). In a sign-fraction element bit
//! 0 is the sign and the higher bits the fraction. The pre-5 layout (no
//! header, float32) predates the supported format versions and is not
//! ported.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_float_deconstruct`.
pub(crate) const FLOAT_DECONSTRUCT: Desc = Desc {
    id: 33,
    min_version: 4,
    regens: &[Type::Numeric],
    variable_regens: false,
    inputs: &[Type::Struct, Type::Serial],
    variable_inputs: 0,
    decode,
};

fn decode(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let (sign_fraction, exponent) = (&inputs[0], &inputs[1]);
    let &[kind] = ctx.header else {
        return Err(corrupt());
    };
    // `FLTDECON_SignFracWidth` and `FLTDECON_ElementWidth`.
    let (sign_fraction_width, width) = match kind {
        0 => (3, 4),
        1 => (1, 2),
        2 => (2, 2),
        _ => return Err(corrupt()),
    };
    let count = exponent.len();
    if sign_fraction.len() != count
        || sign_fraction.width != sign_fraction_width
        || exponent.width != 1
    {
        return Err(corrupt());
    }
    let out = regens[0].fixed(width, count)?;
    out.resize(count * width, 0);
    let exponents = exponent.bytes.iter();
    let parts = sign_fraction.bytes.chunks_exact(sign_fraction_width);
    for ((to, &exp), part) in out.chunks_exact_mut(width).zip(exponents).zip(parts) {
        match kind {
            0 => {
                let packed = u32::from_le_bytes([part[0], part[1], part[2], 0]);
                let value = (u32::from(exp) << 23) | (packed << 31) | (packed >> 1);
                to.copy_from_slice(&value.to_le_bytes());
            }
            1 => {
                let packed = part[0];
                let value =
                    (u16::from(packed) << 15) | (u16::from(exp) << 7) | u16::from(packed >> 1);
                to.copy_from_slice(&value.to_le_bytes());
            }
            _ => {
                let packed = u16::from_le_bytes([part[0], part[1]]);
                let value = (packed << 15) | (u16::from(exp) << 10) | (packed >> 1);
                to.copy_from_slice(&value.to_le_bytes());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    const DESC: Desc = FLOAT_DECONSTRUCT;
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

    fn le(width: usize, values: &[u64]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes()[..width].to_vec())
            .collect()
    }

    fn sign_fraction_input(bytes: &[u8], width: usize) -> Input<'_> {
        Input {
            ty: Type::Struct,
            width,
            bytes,
            lens: &[],
        }
    }

    fn rebuild(
        kind: u8,
        sign_fraction: &[u8],
        width: usize,
        exponent: &[u8],
    ) -> Result<Vec<u8>, DecodeError> {
        let inputs = [
            sign_fraction_input(sign_fraction, width),
            Input::serial(exponent),
        ];
        run(&[kind], &inputs).map(|stream| stream.bytes().to_vec())
    }

    #[test]
    fn float32_parts_recombine() {
        // -1.5f32 = sign 1, exponent 127, fraction 0x400000.
        let packed: u32 = 1 | (0x40_0000 << 1);
        let out = rebuild(0, &packed.to_le_bytes()[..3], 3, &[127]).unwrap();
        assert_eq!(out, (-1.5f32).to_le_bytes());
        let out = rebuild(0, &[0, 0, 0, 0, 0, 0], 3, &[1, 255]).unwrap();
        assert_eq!(out, le(4, &[1 << 23, 255 << 23]));
    }

    #[test]
    fn bfloat16_parts_recombine() {
        // bf16 of -1.5 is 0xbfc0: sign 1, exponent 0x7f, fraction 0x40.
        let out = rebuild(1, &[1 | (0x40 << 1)], 1, &[0x7f]).unwrap();
        assert_eq!(out, 0xbfc0u16.to_le_bytes());
    }

    #[test]
    fn float16_parts_recombine() {
        // f16 of -1.5 is 0xbe00: sign 1, exponent 15, fraction 0x200.
        let packed: u16 = 1 | (0x200 << 1);
        let out = rebuild(2, &packed.to_le_bytes(), 2, &[15]).unwrap();
        assert_eq!(out, 0xbe00u16.to_le_bytes());
    }

    #[test]
    fn empty_streams_keep_the_output_width() {
        for (kind, width, out_width) in [(0u8, 3usize, 4usize), (1, 1, 2), (2, 2, 2)] {
            let stream = run(
                &[kind],
                &[sign_fraction_input(&[], width), Input::serial(&[])],
            )
            .unwrap();
            assert!(stream.bytes().is_empty());
            assert_eq!(stream.as_input().width, out_width);
        }
    }

    #[test]
    fn malformed_headers_are_corrupt() {
        let inputs = [sign_fraction_input(&[0; 3], 3), Input::serial(&[0])];
        for header in [&[][..], &[3], &[0, 0], &[255]] {
            assert_eq!(run(header, &inputs).err(), Some(corrupt()), "{header:?}");
        }
    }

    #[test]
    fn mismatched_streams_are_corrupt() {
        // Wrong element count or widths for the type.
        assert!(rebuild(0, &[0; 3], 3, &[0, 0]).is_err());
        assert!(rebuild(0, &[0; 2], 2, &[0]).is_err());
        assert!(rebuild(1, &[0; 2], 2, &[0, 0]).is_err());
        let wide = Input {
            ty: Type::Serial,
            width: 2,
            bytes: &[0, 0],
            lens: &[],
        };
        assert_eq!(
            run(&[1], &[sign_fraction_input(&[0], 1), wide]).err(),
            Some(corrupt())
        );
    }
}
