//! Decoder of the `interleave` feature: `interleave_string` (61).
//!
//! Ports `decode_interleave_binding.c` (`DI_interleave`): the input is the
//! strings of `n` equally long streams woven round-robin, and the codec
//! header holds `n` as a 4-byte little-endian integer. The chunk must name
//! exactly `n` streams to regenerate.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// Streams `interleave_string` may split into (`ZL_INTERLEAVE_MAX_INPUTS`).
const MAX_STREAMS: usize = 512;

/// `ZL_StandardTransformID_interleave_string`.
pub(crate) const INTERLEAVE_STRING: Desc = Desc {
    id: 61,
    min_version: 20,
    regens: &[Type::String],
    variable_regens: true,
    inputs: &[Type::String],
    variable_inputs: 0,
    decode: deinterleave,
};

/// `DI_interleave`.
fn deinterleave(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let input = &inputs[0];
    let &[a, b, c, d] = ctx.header else {
        return Err(corrupt());
    };
    let streams = u32::from_le_bytes([a, b, c, d]) as usize;
    if streams == 0 || streams > MAX_STREAMS || streams != regens.len() {
        return Err(corrupt());
    }
    if !input.lens.len().is_multiple_of(streams) {
        return Err(corrupt());
    }
    let per_stream = input.lens.len() / streams;
    // Each regenerated stream's share of the contents.
    let mut totals = alloc::vec![0usize; streams];
    for (n, &len) in input.lens.iter().enumerate() {
        totals[n % streams] += len as usize;
    }
    let mut outputs = alloc::vec::Vec::with_capacity(streams);
    for (regen, &total) in regens.iter_mut().zip(&totals) {
        outputs.push(regen.strings(per_stream, total)?);
    }
    let mut rest = input.bytes;
    for round in input.lens.chunks_exact(streams) {
        for (&len, (bytes, lens)) in round.iter().zip(&mut outputs) {
            let (string, tail) = rest.split_at_checked(len as usize).ok_or_else(corrupt)?;
            bytes.extend_from_slice(string);
            lens.push(len);
            rest = tail;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn run(header: &[u8], input: Input<'_>, outputs: usize) -> Result<Vec<Stream>, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, header);
        let mut regens: Vec<Stream> = (0..outputs).map(|_| Stream::default()).collect();
        for stream in &mut regens {
            stream.prepare(Type::String, 1 << 16);
        }
        deinterleave(&mut ctx, &[input], &mut regens)?;
        regens.iter().try_for_each(Stream::validate)?;
        Ok(regens)
    }

    fn strings<'a>(bytes: &'a [u8], lens: &'a [u32]) -> Input<'a> {
        Input {
            ty: Type::String,
            width: 1,
            bytes,
            lens,
        }
    }

    #[test]
    fn strings_deal_out_round_robin() {
        let out = run(
            &3u32.to_le_bytes(),
            strings(b"a1bbcc2dd", &[1, 1, 2, 2, 1, 2]),
            3,
        )
        .unwrap();
        assert_eq!(out[0].bytes(), b"acc");
        assert_eq!(out[0].as_input().lens, [1, 2]);
        assert_eq!(out[1].bytes(), b"12");
        assert_eq!(out[2].bytes(), b"bbdd");
        assert_eq!(out[2].as_input().lens, [2, 2]);
        let single = run(&1u32.to_le_bytes(), strings(b"xyz", &[1, 0, 2]), 1).unwrap();
        assert_eq!(single[0].as_input().lens, [1, 0, 2]);
    }

    #[test]
    fn empty_inputs_make_empty_streams() {
        let out = run(&2u32.to_le_bytes(), strings(b"", &[]), 2).unwrap();
        assert!(out.iter().all(|s| s.as_input().len() == 0));
    }

    #[test]
    fn bad_headers_and_counts_are_corrupt() {
        let input = strings(b"ab", &[1, 1]);
        let two = 2u32.to_le_bytes();
        for (header, outputs) in [
            (&[][..], 2),
            (&two[..3], 2),
            (&[2, 0, 0, 0, 0], 2),
            (&[0, 0, 0, 0], 1),
            (&[1, 2, 0, 0], 513),
            (&two, 1),
            (&two, 3),
            (&3u32.to_le_bytes(), 3),
        ] {
            assert_eq!(
                run(header, input, outputs).map(|_| ()),
                Err(corrupt()),
                "{header:?} {outputs}"
            );
        }
    }
}
