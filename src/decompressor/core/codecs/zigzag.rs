//! The zigzag codec (`DI_zigzag_num`, `decode_zigzag_kernel.c`), feature
//! `zigzag`.
//!
//! Maps each unsigned code back to the signed number it stands for:
//! `(z >> 1) ^ -(z & 1)`, at the element width. The stream keeps its
//! element width.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_zigzag`.
pub(crate) const ZIGZAG: Desc = Desc {
    id: 3,
    min_version: 3,
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

fn decode(_: &mut Ctx<'_>, inputs: &[Input<'_>], regens: &mut [Stream]) -> Result<(), DecodeError> {
    let input = &inputs[0];
    let out = regens[0].fixed(input.width, input.len())?;
    out.resize(input.bytes.len(), 0);
    match input.width {
        1 => unzigzag::<1>(out, input.bytes),
        2 => unzigzag::<2>(out, input.bytes),
        4 => unzigzag::<4>(out, input.bytes),
        8 => unzigzag::<8>(out, input.bytes),
        _ => return Err(corrupt()),
    }
    Ok(())
}

/// Decodes `src` into `dst`, `W` bytes per number (`ZL_zigzagDecode`).
///
/// Working in 64 bits is exact: the mask only differs from the narrow one
/// above the element width, which the store drops.
fn unzigzag<const W: usize>(dst: &mut [u8], src: &[u8]) {
    for (to, from) in dst
        .as_chunks_mut::<W>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<W>().0)
    {
        let code = load(from);
        let value = (code >> 1) ^ 0u64.wrapping_sub(code & 1);
        to.copy_from_slice(&value.to_le_bytes()[..W]);
    }
}

#[cfg(test)]
mod tests {
    const DESC: Desc = ZIGZAG;
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
    fn codes_map_back_to_signed_values() {
        let out = run(&[], &[num(1, &[0, 1, 2, 3, 4, 254, 255])]).unwrap();
        assert_eq!(out.bytes(), [0, 255, 1, 254, 2, 127, 128]);
        let out = run(&[], &[num(2, &le(2, &[0, 1, 2, 0xffff]))]).unwrap();
        assert_eq!(out.bytes(), le(2, &[0, 0xffff, 1, 0x8000]));
        let out = run(&[], &[num(4, &le(4, &[5, 6, 0xffff_ffff]))]).unwrap();
        assert_eq!(out.bytes(), le(4, &[0xffff_fffd, 3, 0x8000_0000]));
        let out = run(&[], &[num(8, &le(8, &[u64::MAX, u64::MAX - 1]))]).unwrap();
        assert_eq!(out.bytes(), le(8, &[1 << 63, (1 << 63) - 1]));
    }

    #[test]
    fn an_empty_stream_stays_empty_and_keeps_its_width() {
        let out = run(&[], &[num(8, &[])]).unwrap();
        assert!(out.bytes().is_empty());
        assert_eq!(out.as_input().width, 8);
    }

    #[test]
    fn output_beyond_the_budget_is_corrupt() {
        assert_eq!(run(&[], &[num(1, &[0; 70_000])]).err(), Some(corrupt()));
    }

    #[test]
    fn widths_other_than_numeric_ones_are_corrupt() {
        assert_eq!(run(&[], &[num(3, &[0; 6])]).err(), Some(corrupt()));
    }
}
