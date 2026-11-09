//! The bit-split codec (`DI_bitSplit`, `decode_bitSplit_kernel.c`,
//! `common_bitSplit_kernel.c`), feature `bitsplit`.
//!
//! Joins bit fields, each held by a numeric stream, back into one number per
//! element, lowest field first. The header is the output width in bytes (1,
//! 2, 4 or 8) followed by the bit width of every field but, when the fields
//! cover the whole number, the last one, which takes the remaining bits;
//! with `n` stored widths, `n` streams leave the top bits zero and `n + 1`
//! streams cover the number. Each stream's element width is the smallest of
//! 1, 2, 4 and 8 bytes that holds its field. Like the reference, fields are
//! not masked: a stored value wider than its field spills into the fields
//! above it.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_bitSplit`.
pub(crate) const BITSPLIT: Desc = Desc {
    id: 63,
    min_version: 24,
    regens: &[Type::Numeric],
    variable_regens: false,
    inputs: &[],
    variable_inputs: Type::Numeric.bit(),
    decode,
};

/// Elements joined per pass over the field streams.
const BLOCK: usize = 512;

/// The element width that holds `bits` bits (`ZL_bitSplit_outputEltWidth`).
const fn field_width(bits: u8) -> usize {
    match bits {
        0..=8 => 1,
        9..=16 => 2,
        17..=32 => 4,
        _ => 8,
    }
}

/// Reads a little-endian number of up to eight bytes.
fn load(bytes: &[u8]) -> u64 {
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
    let Some((&width, stored)) = ctx.header.split_first() else {
        return Err(corrupt());
    };
    let width = usize::from(width);
    if !matches!(width, 1 | 2 | 4 | 8) {
        return Err(corrupt());
    }
    let mut sum = 0usize;
    for &bits in stored {
        if bits == 0 {
            return Err(corrupt());
        }
        sum += usize::from(bits);
    }
    if sum > width * 8 {
        return Err(corrupt());
    }
    // Fields beyond the stored ones: the remainder when it is non-zero.
    let mut widths = [0u8; 64];
    let count = inputs.len();
    if count == stored.len() {
        widths[..count].copy_from_slice(stored);
    } else if count == stored.len() + 1 {
        // `sum <= 64` and every stored width is non-zero: at most 64 fields.
        if sum == width * 8 || count > widths.len() {
            return Err(corrupt());
        }
        widths[..stored.len()].copy_from_slice(stored);
        widths[stored.len()] = (width * 8 - sum) as u8;
    } else {
        return Err(corrupt());
    }
    if count == 0 {
        return Err(corrupt());
    }
    let elements = inputs[0].len();
    let mut shift = 0u32;
    let mut fields = [(&[][..], 0usize, 0u32); 64];
    for (field, (input, &bits)) in fields.iter_mut().zip(inputs.iter().zip(&widths)) {
        if input.len() != elements
            || usize::from(bits) > input.width * 8
            || input.width != field_width(bits)
        {
            return Err(corrupt());
        }
        *field = (input.bytes, input.width, shift);
        shift += u32::from(bits);
    }
    let out = regens[0].fixed(width, elements)?;
    out.resize(elements * width, 0);
    let fields = &fields[..count];
    let mut joined = [0u64; BLOCK];
    for (first, block) in (0..elements)
        .step_by(BLOCK)
        .zip(out.chunks_mut(BLOCK * width))
    {
        let len = block.len() / width;
        let joined = &mut joined[..len];
        joined.fill(0);
        for &(bytes, size, shift) in fields {
            let bytes = &bytes[first * size..(first + len) * size];
            for (acc, part) in joined.iter_mut().zip(bytes.chunks_exact(size)) {
                *acc |= load(part) << shift;
            }
        }
        for (to, value) in block.chunks_exact_mut(width).zip(joined.iter()) {
            to.copy_from_slice(&value.to_le_bytes()[..width]);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    const DESC: Desc = BITSPLIT;
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
    fn full_coverage_infers_the_last_width() {
        // 16-bit numbers as 7 + 8 + 1 bits: only two widths are stored.
        let low = [0x7fu8, 0x01];
        let middle = [0xffu8, 0x02];
        let top = [1u8, 0];
        let out = run(&[2, 7, 8], &[num(1, &low), num(1, &middle), num(1, &top)]).unwrap();
        assert_eq!(out.bytes(), le(2, &[0xffff, 0x0101 | (0x02 << 7)]));
        assert_eq!(out.as_input().width, 2);
    }

    #[test]
    fn partial_coverage_leaves_the_top_bits_zero() {
        let low = le(2, &[0x1ff, 0x3]);
        let high = [0x3fu8, 0x01];
        let out = run(&[4, 10, 6], &[num(2, &low), num(1, &high)]).unwrap();
        assert_eq!(out.bytes(), le(4, &[0x1ff | (0x3f << 10), 0x3 | (1 << 10)]));
    }

    #[test]
    fn every_field_width_class_joins() {
        let wide = le(8, &[0x000f_ffff_ffff_ffff]);
        let mid = le(2, &[0x7ff]);
        let top = [1u8];
        let out = run(&[8, 52, 11], &[num(8, &wide), num(2, &mid), num(1, &top)]).unwrap();
        assert_eq!(out.bytes(), le(8, &[u64::MAX]));
        let thirds = le(4, &[0x7f_ffff]);
        let out = run(
            &[4, 23, 8],
            &[num(4, &thirds), num(1, &[0xff]), num(1, &[1])],
        )
        .unwrap();
        assert_eq!(out.bytes(), le(4, &[u64::from(u32::MAX)]));
    }

    #[test]
    fn unmasked_fields_spill_into_the_next_one_like_the_reference() {
        let out = run(&[1, 4], &[num(1, &[0xff]), num(1, &[0])]).unwrap();
        assert_eq!(out.bytes(), [0xff]);
        let out = run(&[1, 4], &[num(1, &[0x1f]), num(1, &[0x01])]).unwrap();
        assert_eq!(out.bytes(), [0x1f | 0x10]);
    }

    #[test]
    fn blocks_join_across_the_batch_boundary() {
        let count = BLOCK * 2 + 3;
        let low: Vec<u8> = (0..count).map(|i| i as u8 & 0xf).collect();
        let high: Vec<u8> = (0..count).map(|i| (i >> 4) as u8 & 0xf).collect();
        let out = run(&[1, 4, 4], &[num(1, &low), num(1, &high)]).unwrap();
        let expected: Vec<u8> = (0..count)
            .map(|i| (i as u8 & 0xf) | (((i >> 4) as u8 & 0xf) << 4))
            .collect();
        assert_eq!(out.bytes(), expected);
    }

    #[test]
    fn empty_streams_keep_the_output_width() {
        let out = run(
            &[4, 8, 8, 8],
            &[num(1, &[]), num(1, &[]), num(1, &[]), num(1, &[])],
        )
        .unwrap();
        assert!(out.bytes().is_empty());
        assert_eq!(out.as_input().width, 4);
    }

    #[test]
    fn malformed_headers_are_corrupt() {
        let one = [num(1, &[0])];
        for header in [&[][..], &[3, 4], &[0, 4], &[1, 0], &[1, 9], &[1, 4, 4, 1]] {
            assert_eq!(run(header, &one).err(), Some(corrupt()), "{header:?}");
        }
    }

    #[test]
    fn stream_counts_must_match_the_widths() {
        let streams = [num(1, &[0]), num(1, &[0]), num(1, &[0])];
        // Two stored widths take two (partial) or three (full) streams.
        assert!(run(&[2, 4, 4], &streams[..2]).is_ok());
        assert_eq!(run(&[2, 4, 4], &streams[..1]).err(), Some(corrupt()));
        // The widths already cover the number, so a third stream is empty.
        assert_eq!(run(&[1, 4, 4], &streams).err(), Some(corrupt()));
        assert_eq!(run(&[1, 4, 4], &[]).err(), Some(corrupt()));
        assert_eq!(run(&[1], &[]).err(), Some(corrupt()));
        assert_eq!(run(&[1], &streams[..2]).err(), Some(corrupt()));
        // Sixty-four one-bit fields are the most there can be, whether the
        // last width is stored or implied.
        let many = alloc::vec![num(1, &[1]); 64];
        let mut header = alloc::vec![8u8];
        header.extend([1; 63]);
        assert!(run(&header, &many).is_ok());
        assert!(run(&header, &many[..63]).is_ok());
        let mut wide = alloc::vec![8u8];
        wide.extend([1; 64]);
        assert!(run(&wide, &many).is_ok());
        assert_eq!(
            run(&wide, &alloc::vec![num(1, &[1]); 65]).err(),
            Some(corrupt())
        );
    }

    #[test]
    fn stream_shapes_must_match_the_fields() {
        // Different lengths, a field wider than its stream, a stream wider
        // than its field needs.
        assert_eq!(
            run(&[2, 8], &[num(1, &[1, 2]), num(1, &[1])]).err(),
            Some(corrupt())
        );
        assert_eq!(run(&[2, 12], &[num(1, &[1])]).err(), Some(corrupt()));
        assert_eq!(run(&[2, 4], &[num(2, &le(2, &[1]))]).err(), Some(corrupt()));
        assert_eq!(
            run(&[2, 12, 4], &[num(1, &[1]), num(1, &[1])]).err(),
            Some(corrupt())
        );
    }

    #[test]
    fn field_widths_follow_the_bit_counts() {
        assert_eq!(
            [1u8, 8, 9, 16, 17, 32, 33, 64].map(field_width),
            [1, 1, 2, 2, 4, 4, 8, 8]
        );
    }
}
