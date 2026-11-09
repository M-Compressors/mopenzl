//! The divide-by codec (`DI_divide_by_int`, `decode_divide_by_kernel.c`),
//! feature `divide-by`.
//!
//! Multiplies every quotient by the divisor the codec header holds as a
//! varint. The divisor must be non-zero and fit the element width, and no
//! product may overflow it; the stream keeps its element width. Format
//! versions before 16 lack the codec, so there is no older layout.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;
use crate::shared::varint;

/// `ZL_StandardTransformID_divide_by`.
pub(crate) const DIVIDE_BY: Desc = Desc {
    id: 53,
    min_version: 16,
    regens: &[Type::Numeric],
    variable_regens: false,
    inputs: &[Type::Numeric],
    variable_inputs: 0,
    decode,
};

/// Reads a little-endian number of up to eight bytes.
fn load<const W: usize>(bytes: &[u8; W]) -> u64 {
    let mut value = [0u8; 8];
    for (to, from) in value.iter_mut().zip(bytes) {
        *to = *from;
    }
    u64::from_le_bytes(value)
}

fn decode(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let input = &inputs[0];
    // Bytes after the varint are ignored, as in the reference.
    let (divisor, _) = varint::decode(ctx.header).map_err(|_| corrupt())?;
    if divisor == 0 {
        return Err(corrupt());
    }
    let out = regens[0].fixed(input.width, input.len())?;
    out.resize(input.bytes.len(), 0);
    match input.width {
        1 => multiply::<1>(out, input.bytes, divisor),
        2 => multiply::<2>(out, input.bytes, divisor),
        4 => multiply::<4>(out, input.bytes, divisor),
        8 => multiply::<8>(out, input.bytes, divisor),
        _ => Err(corrupt()),
    }
}

/// `ZS_divideByDecode`, with the binding's overflow checks: every
/// quotient must not exceed `MAX / divisor` of the element width.
fn multiply<const W: usize>(dst: &mut [u8], src: &[u8], divisor: u64) -> Result<(), DecodeError> {
    let max = u64::MAX >> (64 - 8 * W);
    if divisor > max {
        return Err(corrupt());
    }
    let limit = max / divisor;
    for (to, from) in dst
        .as_chunks_mut::<W>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<W>().0)
    {
        let quotient = load(from);
        if quotient > limit {
            return Err(corrupt());
        }
        to.copy_from_slice(&(quotient * divisor).to_le_bytes()[..W]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    const DESC: Desc = DIVIDE_BY;
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

    fn le(width: usize, values: &[u64]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes()[..width].to_vec())
            .collect()
    }

    #[test]
    fn quotients_multiply_back() {
        for width in [1usize, 2, 4, 8] {
            let out = run(&[7], &[num(width, &le(width, &[0, 1, 2, 30]))]).unwrap();
            assert_eq!(out.bytes(), le(width, &[0, 7, 14, 210]));
        }
        // A multi-byte divisor and trailing header bytes (ignored).
        let out = run(&[0x80, 0x01, 0xff], &[num(2, &le(2, &[3]))]).unwrap();
        assert_eq!(out.bytes(), le(2, &[384]));
    }

    #[test]
    fn empty_streams_need_only_a_valid_divisor() {
        assert!(run(&[3], &[num(4, &[])]).unwrap().bytes().is_empty());
        assert_eq!(run(&[0], &[num(4, &[])]).err(), Some(corrupt()));
        assert_eq!(run(&[], &[num(4, &[])]).err(), Some(corrupt()));
        // A varint cut short.
        assert_eq!(run(&[0x80], &[num(4, &[])]).err(), Some(corrupt()));
    }

    #[test]
    fn products_may_not_overflow_the_width() {
        // 255 / 5 = 51 is the largest byte quotient.
        assert!(run(&[5], &[num(1, &[51])]).is_ok());
        assert_eq!(run(&[5], &[num(1, &[52])]).err(), Some(corrupt()));
        // The divisor 65535 leaves room for the quotient 1 only.
        assert!(run(&[0xff, 0xff, 0x03], &[num(2, &le(2, &[1]))]).is_ok());
        assert_eq!(
            run(&[0xff, 0xff, 0x03], &[num(2, &le(2, &[2]))]).err(),
            Some(corrupt())
        );
        assert_eq!(
            run(&[2], &[num(8, &le(8, &[(u64::MAX / 2) + 1]))]).err(),
            Some(corrupt())
        );
    }

    #[test]
    fn divisors_wider_than_the_element_are_corrupt() {
        assert_eq!(run(&[0x80, 0x02], &[num(1, &[1])]).err(), Some(corrupt()));
        assert_eq!(
            run(&[0xff, 0xff, 0x04], &[num(2, &le(2, &[0]))]).err(),
            Some(corrupt())
        );
        assert_eq!(
            run(&[0x80, 0x80, 0x80, 0x80, 0x10], &[num(4, &le(4, &[0]))]).err(),
            Some(corrupt())
        );
    }
}
