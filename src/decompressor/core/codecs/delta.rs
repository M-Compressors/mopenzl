//! The integer delta codec (`DI_delta_int`, `decode_delta_kernel.c`),
//! feature `delta`.
//!
//! One numeric stream of deltas in, one numeric stream of the same width
//! out. Since format version 13 the first element travels in the codec
//! header (one element wide) and the stream holds the `n - 1` differences
//! that follow it; an empty header means an empty stream. The reference also
//! has a pre-13 layout (the first element inside the stream); frames older
//! than version 21 are not supported, so it is not ported. Sums wrap at the
//! element width.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_delta_int`.
pub(crate) const DELTA_INT: Desc = Desc {
    id: 1,
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

fn decode(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let input = &inputs[0];
    let width = input.width;
    if ctx.header.is_empty() {
        // No first element: there can be no deltas either.
        if input.len() != 0 {
            return Err(corrupt());
        }
        regens[0].fixed(width, 0)?;
        return Ok(());
    }
    if ctx.header.len() != width {
        return Err(corrupt());
    }
    let out = regens[0].fixed(width, input.len() + 1)?;
    out.resize((input.len() + 1) * width, 0);
    match width {
        1 => sum::<1>(out, ctx.header, input.bytes),
        2 => sum::<2>(out, ctx.header, input.bytes),
        4 => sum::<4>(out, ctx.header, input.bytes),
        8 => sum::<8>(out, ctx.header, input.bytes),
        _ => return Err(corrupt()),
    }
    Ok(())
}

/// Writes `first` then its running sum with every delta (`ZS_deltaDecode`).
fn sum<const W: usize>(out: &mut [u8], first: &[u8], deltas: &[u8]) {
    let mut total = first.first_chunk::<W>().map_or(0, load);
    let mut slots = out.as_chunks_mut::<W>().0.iter_mut();
    if let Some(slot) = slots.next() {
        slot.copy_from_slice(&total.to_le_bytes()[..W]);
    }
    for (slot, delta) in slots.zip(deltas.as_chunks::<W>().0) {
        total = total.wrapping_add(load(delta));
        slot.copy_from_slice(&total.to_le_bytes()[..W]);
    }
}

#[cfg(test)]
mod tests {
    const DESC: Desc = DELTA_INT;
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
    fn deltas_accumulate_from_the_header_element() {
        for width in [1usize, 2, 4, 8] {
            let header = le(width, &[10]);
            let deltas = le(width, &[1, 2, 3]);
            let out = run(&header, &[num(width, &deltas)]).unwrap();
            assert_eq!(out.bytes(), le(width, &[10, 11, 13, 16]));
            assert_eq!(out.as_input().width, width);
        }
    }

    #[test]
    fn sums_wrap_at_the_element_width() {
        let out = run(&[250], &[num(1, &[10, 255])]).unwrap();
        assert_eq!(out.bytes(), [250, 4, 3]);
        let out = run(&le(8, &[u64::MAX]), &[num(8, &le(8, &[2]))]).unwrap();
        assert_eq!(out.bytes(), le(8, &[u64::MAX, 1]));
    }

    #[test]
    fn a_header_alone_is_one_element() {
        let out = run(&[7, 0], &[num(2, &[])]).unwrap();
        assert_eq!(out.bytes(), [7, 0]);
    }

    #[test]
    fn an_empty_header_means_an_empty_stream() {
        let out = run(&[], &[num(4, &[])]).unwrap();
        assert!(out.bytes().is_empty());
        assert_eq!(out.as_input().width, 4);
        assert_eq!(run(&[], &[num(1, &[1])]).err(), Some(corrupt()));
    }

    #[test]
    fn a_header_that_is_not_one_element_is_corrupt() {
        assert_eq!(run(&[1, 2], &[num(4, &[])]).err(), Some(corrupt()));
        assert_eq!(run(&[1; 8], &[num(4, &[])]).err(), Some(corrupt()));
    }

    #[test]
    fn output_beyond_the_budget_is_corrupt() {
        let deltas = le(4, &[1; 20_000]);
        assert_eq!(run(&[0; 4], &[num(4, &deltas)]).err(), Some(corrupt()));
    }
}
