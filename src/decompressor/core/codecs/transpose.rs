//! Decoders of the `transpose` feature: `transpose` (2), `transpose_split`
//! (4) and the legacy `transpose_split2` / `transpose_split4` /
//! `transpose_split8` (30 to 32).
//!
//! Ports `decode_transpose_binding.c` (`DI_transpose`,
//! `DI_transpose_split`, `DI_transposesplit{2,4,8}_bytes`) and
//! `decode_transpose_kernel.c` (`ZS_transposeDecode`,
//! `ZS_splitTransposeDecode`). A transposed struct stream holds the first
//! byte of every element, then the second, and so on; decoding interleaves
//! the byte columns back into elements. `transpose` keeps the columns in
//! one struct stream, the split variants in one serial stream each.
//!
//! The reference's encoder stopped writing `transpose` and the fixed-width
//! split variants at format version 10 (their decoders still accept every
//! version), so frames of the supported versions 21 to 27 only reach them
//! when written by other means. `transpose_split` is the current encoder.
//! Format versions before 21 have no other difference for these codecs.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_transpose`: transposed bytes back to structs.
pub(crate) const TRANSPOSE: Desc = Desc {
    id: 2,
    min_version: 3,
    regens: &[Type::Struct],
    variable_regens: false,
    inputs: &[Type::Struct],
    variable_inputs: 0,
    decode: transpose,
};

/// `ZL_StandardTransformID_transpose_split`: one serial stream per byte of
/// the struct.
pub(crate) const TRANSPOSE_SPLIT: Desc = Desc {
    id: 4,
    min_version: 11,
    regens: &[Type::Struct],
    variable_regens: false,
    inputs: &[],
    variable_inputs: Type::Serial.bit(),
    decode: transpose_split,
};

/// `ZL_StandardTransformID_transpose_split2`.
pub(crate) const TRANSPOSE_SPLIT2: Desc = Desc {
    id: 30,
    min_version: 3,
    regens: &[Type::Struct],
    variable_regens: false,
    inputs: &[Type::Serial; 2],
    variable_inputs: 0,
    decode: transpose_split,
};

/// `ZL_StandardTransformID_transpose_split4`.
pub(crate) const TRANSPOSE_SPLIT4: Desc = Desc {
    id: 31,
    min_version: 3,
    regens: &[Type::Struct],
    variable_regens: false,
    inputs: &[Type::Serial; 4],
    variable_inputs: 0,
    decode: transpose_split,
};

/// `ZL_StandardTransformID_transpose_split8`.
pub(crate) const TRANSPOSE_SPLIT8: Desc = Desc {
    id: 32,
    min_version: 3,
    regens: &[Type::Struct],
    variable_regens: false,
    inputs: &[Type::Serial; 8],
    variable_inputs: 0,
    decode: transpose_split,
};

/// Interleaves the byte columns `columns` (each `count` bytes) into
/// `columns.len()`-byte elements appended to `out`
/// (`ZS_splitTransposeDecode`).
fn interleave(out: &mut alloc::vec::Vec<u8>, columns: &[&[u8]], count: usize) {
    let width = columns.len();
    let start = out.len();
    out.resize(start + count * width, 0);
    let dst = &mut out[start..];
    match *columns {
        [a, b] => interleave_fixed(dst, [a, b], count),
        [a, b, c, d] => interleave_fixed(dst, [a, b, c, d], count),
        [a, b, c, d, e, f, g, h] => interleave_fixed(dst, [a, b, c, d, e, f, g, h], count),
        _ => {
            for (pos, column) in columns.iter().enumerate() {
                for (element, &byte) in dst.chunks_exact_mut(width).zip(&column[..count]) {
                    element[pos] = byte;
                }
            }
        }
    }
}

/// [`interleave`] for a width known at compile time.
fn interleave_fixed<const W: usize>(dst: &mut [u8], columns: [&[u8]; W], count: usize) {
    let columns = columns.map(|column| &column[..count]);
    for (index, element) in dst.as_chunks_mut::<W>().0.iter_mut().enumerate() {
        for (byte, column) in element.iter_mut().zip(&columns) {
            *byte = column[index];
        }
    }
}

/// `DI_transpose`: the input is the transposition of `n` elements of `w`
/// bytes, that is `w` elements of `n` bytes (the columns); the result is the
/// original `n` elements of `w` bytes. An empty input stays empty.
fn transpose(
    _: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let input = &inputs[0];
    let count = input.len();
    let width = input.width;
    // The input is the transposition of `new_count` elements of
    // `new_width` bytes: `new_width` columns of `new_count` bytes.
    let (new_count, new_width) = if count == 0 {
        (0, width)
    } else {
        (width, count)
    };
    let out = regens[0].fixed(new_width, new_count)?;
    if new_count == 0 {
        return Ok(());
    }
    let bytes = &input.bytes[..count * width];
    // `new_width` columns, each `new_count` bytes: column `p` holds byte
    // `p` of every element.
    let columns: alloc::vec::Vec<&[u8]> = bytes.chunks_exact(new_count).collect();
    interleave(out, &columns, new_count);
    Ok(())
}

/// `DI_transpose_split` and `DI_transposesplit{2,4,8}_bytes`: one serial
/// column per byte of the struct, all of the same length.
fn transpose_split(
    _: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let Some(first) = inputs.first() else {
        return Err(corrupt());
    };
    let count = first.len();
    let mut columns = alloc::vec::Vec::with_capacity(inputs.len());
    for input in inputs {
        if input.len() != count {
            return Err(corrupt());
        }
        columns.push(&input.bytes[..count]);
    }
    let out = regens[0].fixed(columns.len(), count)?;
    interleave(out, &columns, count);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn run(desc: &Desc, inputs: &[Input<'_>]) -> Result<Stream, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[]);
        let mut regens = [Stream::default()];
        regens[0].prepare(desc.regen_type(0), 1 << 16);
        (desc.decode)(&mut ctx, inputs, &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream)
    }

    fn structs(width: usize, bytes: &[u8]) -> Input<'_> {
        Input {
            ty: Type::Struct,
            width,
            bytes,
            lens: &[],
        }
    }

    /// The transposition the encoder writes: column `p` holds byte `p` of
    /// every element.
    fn transposed(bytes: &[u8], width: usize) -> Vec<Vec<u8>> {
        (0..width)
            .map(|p| bytes.chunks_exact(width).map(|e| e[p]).collect())
            .collect()
    }

    #[test]
    fn split_columns_interleave_for_every_width() {
        for width in 1..=11usize {
            for count in [0usize, 1, 2, 3, 31, 100] {
                let bytes: Vec<u8> = (0..width * count).map(|i| (i * 7 + 3) as u8).collect();
                let columns = transposed(&bytes, width);
                let inputs: Vec<Input<'_>> = columns.iter().map(|c| Input::serial(c)).collect();
                let out = run(&TRANSPOSE_SPLIT, &inputs).unwrap();
                assert_eq!(out.bytes(), bytes, "{width} x {count}");
                assert_eq!(out.as_input().width, width);
                assert_eq!(out.as_input().len(), count);
            }
        }
    }

    #[test]
    fn legacy_variants_take_a_fixed_number_of_columns() {
        for (desc, width) in [
            (&TRANSPOSE_SPLIT2, 2usize),
            (&TRANSPOSE_SPLIT4, 4),
            (&TRANSPOSE_SPLIT8, 8),
        ] {
            let bytes: Vec<u8> = (0..width * 9).map(|i| i as u8).collect();
            let columns = transposed(&bytes, width);
            let inputs: Vec<Input<'_>> = columns.iter().map(|c| Input::serial(c)).collect();
            assert_eq!(run(desc, &inputs).unwrap().bytes(), bytes);
            assert_eq!(desc.inputs.len(), width);
        }
    }

    #[test]
    fn mismatched_or_missing_columns_are_corrupt() {
        assert_eq!(run(&TRANSPOSE_SPLIT, &[]).map(|_| ()), Err(corrupt()));
        let (a, b) = ([1u8, 2], [3u8]);
        assert_eq!(
            run(&TRANSPOSE_SPLIT, &[Input::serial(&a), Input::serial(&b)]).map(|_| ()),
            Err(corrupt())
        );
    }

    #[test]
    fn transpose_turns_columns_back_into_elements() {
        for width in [1usize, 2, 3, 4, 8, 9] {
            for count in [0usize, 1, 5, 40] {
                let original: Vec<u8> = (0..width * count).map(|i| (i * 13 + 1) as u8).collect();
                // What `DI_transpose` reads: the encoder's transposition,
                // a stream of `width` elements of `count` bytes.
                let columns: Vec<u8> = transposed(&original, width).concat();
                let input = if count == 0 {
                    structs(width, &[])
                } else {
                    structs(count, &columns)
                };
                // The decoder sees `width` elements of width `count`, and
                // regenerates `count` elements of width `width`.
                let out = run(&TRANSPOSE, &[input]).unwrap();
                assert_eq!(out.bytes(), original, "{width} x {count}");
                if count > 0 {
                    assert_eq!(out.as_input().width, width);
                } else {
                    assert_eq!(out.as_input().len(), 0);
                }
            }
        }
    }

    #[test]
    fn transposed_output_is_limited_by_the_budget() {
        let bytes = [0u8; 64];
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[]);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::Struct, 32);
        assert_eq!(
            (TRANSPOSE.decode)(&mut ctx, &[structs(8, &bytes)], &mut regens),
            Err(corrupt())
        );
    }
}
