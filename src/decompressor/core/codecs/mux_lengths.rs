//! Decoder of the `mux-lengths` feature: `mux_lengths` (65).
//!
//! Ports `decode_mux_lengths_binding.c` and the scalar path of
//! `decode_mux_lengths_kernel.c` (`ZL_muxLengthsDecode`). The reference's
//! SSSE3 kernel for 16-bit lengths computes the same values; this port has
//! no vectorized path. Each byte of the muxed stream carries a literal
//! length in its low `split_point` bits and a match length (less the bias)
//! in the rest; a field at its maximum takes an extra value from the
//! numeric stream of long lengths, the literal length first.
//!
//! The one-byte codec header is `split_point | bias << 4`.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_mux_lengths`: literal and match lengths from the
/// muxed bytes and the long lengths.
pub(crate) const MUX_LENGTHS: Desc = Desc {
    id: 65,
    min_version: 24,
    regens: &[Type::Numeric, Type::Numeric],
    variable_regens: false,
    inputs: &[Type::Serial, Type::Numeric],
    variable_inputs: 0,
    decode: demux,
};

/// `DI_mux_lengths`.
fn demux(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let (muxed, long) = (&inputs[0], &inputs[1]);
    let &[header] = ctx.header else {
        return Err(corrupt());
    };
    let split = u32::from(header & 0x0f);
    let bias = u64::from(header >> 4);
    if split > 8 {
        return Err(corrupt());
    }
    let count = muxed.len();
    let [literals, matches] = regens else {
        return Err(corrupt());
    };
    let literals = literals.fixed(long.width, count)?;
    let matches = matches.fixed(long.width, count)?;
    let muxed = &muxed.bytes[..count];
    match long.width {
        1 => decode::<1>(literals, matches, muxed, long.bytes, split, bias),
        2 => decode::<2>(literals, matches, muxed, long.bytes, split, bias),
        4 => decode::<4>(literals, matches, muxed, long.bytes, split, bias),
        8 => decode::<8>(literals, matches, muxed, long.bytes, split, bias),
        _ => Err(corrupt()),
    }
}

/// `ZL_muxLengthsDecode_impl` for `W`-byte lengths: sums wrap at `W` bytes.
fn decode<const W: usize>(
    literals: &mut alloc::vec::Vec<u8>,
    matches: &mut alloc::vec::Vec<u8>,
    muxed: &[u8],
    long: &[u8],
    split: u32,
    bias: u64,
) -> Result<(), DecodeError> {
    let literal_mask = (1u64 << split) - 1;
    let match_max = bias + (1u64 << (8 - split)) - 1;
    let mut extra = long.as_chunks::<W>().0.iter();
    let mut next = move || -> Result<u64, DecodeError> {
        let chunk = extra.next().ok_or_else(corrupt)?;
        let mut word = [0u8; 8];
        word[..W].copy_from_slice(chunk);
        Ok(u64::from_le_bytes(word))
    };
    let mut remaining = long.len() / W;
    for &mux in muxed {
        let mut literal = u64::from(mux) & literal_mask;
        let mut length = bias + (u64::from(mux) >> split);
        // The literal length overflows first when both do.
        if literal == literal_mask {
            literal = literal.wrapping_add(next()?);
            remaining -= 1;
        }
        if length == match_max {
            length = length.wrapping_add(next()?);
            remaining -= 1;
        }
        literals.extend_from_slice(&literal.to_le_bytes()[..W]);
        matches.extend_from_slice(&length.to_le_bytes()[..W]);
    }
    if remaining != 0 {
        return Err(corrupt());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn run(
        header: &[u8],
        muxed: &[u8],
        width: usize,
        long: &[u8],
    ) -> Result<[Stream; 2], DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, header);
        let mut regens = [Stream::default(), Stream::default()];
        for stream in &mut regens {
            stream.prepare(Type::Numeric, 1 << 16);
        }
        let long = Input {
            ty: Type::Numeric,
            width,
            bytes: long,
            lens: &[],
        };
        demux(&mut ctx, &[Input::serial(muxed), long], &mut regens)?;
        regens.iter().try_for_each(Stream::validate)?;
        Ok(regens)
    }

    fn values(stream: &Stream, width: usize) -> Vec<u64> {
        stream
            .bytes()
            .chunks_exact(width)
            .map(|chunk| {
                let mut word = [0u8; 8];
                word[..width].copy_from_slice(chunk);
                u64::from_le_bytes(word)
            })
            .collect()
    }

    #[test]
    fn inline_fields_need_no_long_lengths() {
        // split 4: literal = low nibble, match = bias + high nibble.
        let [literals, matches] = run(&[0x24], &[0x21, 0x32, 0x00], 2, &[]).unwrap();
        assert_eq!(values(&literals, 2), [1, 2, 0]);
        assert_eq!(values(&matches, 2), [4, 5, 2]);
    }

    #[test]
    fn saturated_fields_read_long_lengths_literal_first() {
        // split 4, bias 0: 0xff saturates both fields (15 and 15).
        let long: Vec<u8> = [100u16, 200, 5, 6]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let [literals, matches] = run(&[0x04], &[0xff, 0x0f, 0xf3], 2, &long).unwrap();
        assert_eq!(values(&literals, 2), [115, 20, 3]);
        assert_eq!(values(&matches, 2), [215, 0, 21]);
    }

    #[test]
    fn extreme_split_points_use_only_one_field() {
        // split 0: no literal bits, so every literal length is a long one.
        let long: Vec<u8> = [7u32, 8].iter().flat_map(|v| v.to_le_bytes()).collect();
        let [literals, matches] = run(&[0x00], &[0x01, 0x02], 4, &long).unwrap();
        assert_eq!(values(&literals, 4), [7, 8]);
        assert_eq!(values(&matches, 4), [1, 2]);
        // split 8: no match bits, so every match length is a long one.
        let long: Vec<u8> = [3u64, 4].iter().flat_map(|v| v.to_le_bytes()).collect();
        let [literals, matches] = run(&[0x58], &[0x09, 0x01], 8, &long).unwrap();
        assert_eq!(values(&literals, 8), [9, 1]);
        assert_eq!(values(&matches, 8), [5 + 3, 5 + 4]);
    }

    #[test]
    fn lengths_wrap_at_their_width() {
        // A bias of 15 with a long length of 255 wraps in one byte.
        let [_, matches] = run(&[0xf4], &[0xf0], 1, &[255]).unwrap();
        assert_eq!(values(&matches, 1), [(15 + 15 + 255) & 0xff]);
        let [_, matches] = run(&[0x18], &[0x01], 8, &u64::MAX.to_le_bytes()).unwrap();
        assert_eq!(values(&matches, 8), [0]);
    }

    #[test]
    fn exhausted_or_leftover_long_lengths_are_corrupt() {
        assert_eq!(run(&[0x04], &[0x0f], 1, &[]).map(|_| ()), Err(corrupt()));
        assert_eq!(run(&[0x04], &[0xf1], 1, &[]).map(|_| ()), Err(corrupt()));
        assert_eq!(run(&[0x04], &[0x11], 1, &[1]).map(|_| ()), Err(corrupt()));
        assert_eq!(
            run(&[0x04], &[0x1f], 2, &[1, 0, 2, 0]).map(|_| ()),
            Err(corrupt())
        );
    }

    #[test]
    fn headers_must_be_one_byte_with_a_valid_split() {
        for header in [&[][..], &[0x04, 0x00], &[0x09], &[0x0f]] {
            assert_eq!(
                run(header, &[], 1, &[]).map(|_| ()),
                Err(corrupt()),
                "{header:?}"
            );
        }
    }

    #[test]
    fn empty_input_gives_empty_streams_of_the_long_width() {
        let [literals, matches] = run(&[0x04], &[], 4, &[]).unwrap();
        assert_eq!(literals.as_input().width, 4);
        assert_eq!(
            (literals.as_input().len(), matches.as_input().len()),
            (0, 0)
        );
    }
}
