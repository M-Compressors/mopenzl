//! Decoders of the `tokenize` feature: `tokenize_fixed` (36),
//! `tokenize_numeric` (37) and `tokenize_string` (38).
//!
//! Ports `codecs/tokenize/decode_tokenize_binding.c` and
//! `decode_tokenize_kernel.c`. A tokenized stream is an alphabet of unique
//! values (input 0) and one numeric index per element (input 1); decoding
//! replaces each index with its alphabet entry. The width-specialised
//! `tokenize2to1`/`4to2`/`Varto4` kernels are not reached by the bindings
//! and are not ported.
//!
//! Deviation: the reference replaces an out-of-range index of a fixed-width
//! stream with symbol 0 "for speed"; it is an error here, as it is for
//! strings in the reference.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;
use alloc::vec::Vec;

/// `ZL_StandardTransformID_tokenize_fixed`: structs from an alphabet of
/// structs.
pub(crate) const TOKENIZE_FIXED: Desc = Desc {
    id: 36,
    min_version: 8,
    regens: &[Type::Struct],
    variable_regens: false,
    inputs: &[Type::Struct, Type::Numeric],
    variable_inputs: 0,
    decode: decode_fixed,
};

/// `ZL_StandardTransformID_tokenize_numeric`: numbers from an alphabet of
/// numbers.
pub(crate) const TOKENIZE_NUMERIC: Desc = Desc {
    id: 37,
    min_version: 8,
    regens: &[Type::Numeric],
    variable_regens: false,
    inputs: &[Type::Numeric, Type::Numeric],
    variable_inputs: 0,
    decode: decode_fixed,
};

/// `ZL_StandardTransformID_tokenize_string`: strings from an alphabet of
/// strings.
pub(crate) const TOKENIZE_STRING: Desc = Desc {
    id: 38,
    min_version: 11,
    regens: &[Type::String],
    variable_regens: false,
    inputs: &[Type::String, Type::Numeric],
    variable_inputs: 0,
    decode: decode_string,
};

/// Calls `f` with each index of `indices` (little-endian, `width` bytes).
fn each_index<const W: usize>(
    indices: &[u8],
    mut f: impl FnMut(usize) -> Result<(), DecodeError>,
) -> Result<(), DecodeError> {
    for chunk in indices.as_chunks::<W>().0 {
        let mut word = [0u8; 8];
        word[..W].copy_from_slice(chunk);
        // An index beyond the address space is out of range like any other.
        f(usize::try_from(u64::from_le_bytes(word)).unwrap_or(usize::MAX))?;
    }
    Ok(())
}

/// Runs `f` over the indices of an index stream of any valid width.
fn for_indices(
    indices: &Input<'_>,
    f: impl FnMut(usize) -> Result<(), DecodeError>,
) -> Result<(), DecodeError> {
    match indices.width {
        1 => each_index::<1>(indices.bytes, f),
        2 => each_index::<2>(indices.bytes, f),
        4 => each_index::<4>(indices.bytes, f),
        8 => each_index::<8>(indices.bytes, f),
        _ => Err(corrupt()),
    }
}

/// `DI_tokenize`: fixed-width alphabet entries (structs or numbers).
fn decode_fixed(
    _: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let (alphabet, indices) = (&inputs[0], &inputs[1]);
    let width = alphabet.width;
    let out = regens[0].fixed(width, indices.len())?;
    let entries = alphabet.len();
    if entries == 0 && indices.len() != 0 {
        return Err(corrupt());
    }
    for_indices(indices, |index| {
        // `tokenizeDecodeEltWidthIdxWidth` decodes an out-of-range index as
        // entry 0 rather than failing; so does this decoder.
        let index = if index >= entries { 0 } else { index };
        let at = index * width;
        out.extend_from_slice(alphabet.bytes.get(at..at + width).ok_or_else(corrupt)?);
        Ok(())
    })
}

/// `DI_tokenizeVSF`: string alphabet entries.
fn decode_string(
    _: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let (alphabet, indices) = (&inputs[0], &inputs[1]);
    let entries = alphabet.lens.len();
    let count = indices.len();
    // Offset of each entry in the alphabet's content, plus its end.
    let mut starts: Vec<usize> = Vec::new();
    starts.try_reserve(entries + 1).or(Err(corrupt()))?;
    let mut at = 0usize;
    for &len in alphabet.lens {
        starts.push(at);
        at = at.checked_add(len as usize).ok_or_else(corrupt)?;
    }
    starts.push(at);
    if at != alphabet.bytes.len() {
        return Err(corrupt());
    }
    // The size of the result is known from the indices alone.
    let mut total = 0usize;
    for_indices(indices, |index| {
        let len = alphabet.lens.get(index).ok_or_else(corrupt)?;
        total = total.checked_add(*len as usize).ok_or_else(corrupt)?;
        Ok(())
    })?;
    // The reference also requires at least one token per alphabet entry.
    if count < entries {
        return Err(corrupt());
    }
    let (bytes, lens) = regens[0].strings(count, total)?;
    for_indices(indices, |index| {
        let (&start, &end) = starts
            .get(index)
            .zip(starts.get(index + 1))
            .ok_or_else(corrupt)?;
        bytes.extend_from_slice(alphabet.bytes.get(start..end).ok_or_else(corrupt)?);
        lens.push(alphabet.lens[index]);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;

    fn run(desc: &Desc, inputs: &[Input<'_>], budget: usize) -> Result<Stream, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[]);
        let mut regens = [Stream::default()];
        regens[0].prepare(desc.regen_type(0), budget);
        (desc.decode)(&mut ctx, inputs, &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream)
    }

    fn typed(ty: Type, width: usize, bytes: &[u8]) -> Input<'_> {
        Input {
            ty,
            width,
            bytes,
            lens: &[],
        }
    }

    fn numbers<const W: usize>(values: &[u64]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|v| v.to_le_bytes()[..W].to_vec())
            .collect()
    }

    #[test]
    fn structs_come_from_the_alphabet_for_every_index_width() {
        let alphabet = *b"aabbcc";
        let wanted = *b"ccaabbaa";
        let alpha = typed(Type::Struct, 2, &alphabet);
        let idx = [2u64, 0, 1, 0];
        for (width, bytes) in [
            (1, numbers::<1>(&idx)),
            (2, numbers::<2>(&idx)),
            (4, numbers::<4>(&idx)),
            (8, numbers::<8>(&idx)),
        ] {
            let out = run(
                &TOKENIZE_FIXED,
                &[alpha, typed(Type::Numeric, width, &bytes)],
                64,
            )
            .unwrap();
            assert_eq!(out.bytes(), wanted);
            assert_eq!((out.ty(), out.as_input().width), (Type::Struct, 2));
        }
    }

    #[test]
    fn numbers_keep_the_alphabet_width() {
        let alphabet = numbers::<4>(&[10, 20, 30]);
        let indices = numbers::<1>(&[2, 2, 1]);
        let out = run(
            &TOKENIZE_NUMERIC,
            &[
                typed(Type::Numeric, 4, &alphabet),
                typed(Type::Numeric, 1, &indices),
            ],
            64,
        )
        .unwrap();
        assert_eq!(out.bytes(), numbers::<4>(&[30, 30, 20]));
        assert_eq!(out.as_input().width, 4);
    }

    #[test]
    fn fixed_streams_reject_bad_indices() {
        let alphabet = [1u8, 2];
        let alpha = typed(Type::Numeric, 1, &alphabet);
        // Out-of-range indices decode as entry 0, as in the reference.
        assert_eq!(
            run(
                &TOKENIZE_NUMERIC,
                &[alpha, typed(Type::Numeric, 1, &[2, 1, 255])],
                8
            )
            .unwrap()
            .bytes(),
            [1, 2, 1]
        );
        let huge = u64::MAX.to_le_bytes();
        assert_eq!(
            run(
                &TOKENIZE_NUMERIC,
                &[alpha, typed(Type::Numeric, 8, &huge)],
                8
            )
            .unwrap()
            .bytes(),
            [1]
        );
        assert!(
            run(
                &TOKENIZE_NUMERIC,
                &[alpha, typed(Type::Numeric, 3, &[0; 3])],
                8
            )
            .is_err()
        );
        assert!(
            run(
                &TOKENIZE_NUMERIC,
                &[alpha, typed(Type::Numeric, 1, &[0; 9])],
                8
            )
            .is_err()
        );
        // Without an alphabet only an empty stream decodes.
        let empty = typed(Type::Numeric, 1, &[]);
        assert!(
            run(&TOKENIZE_NUMERIC, &[empty, empty], 8)
                .unwrap()
                .bytes()
                .is_empty()
        );
        assert!(
            run(
                &TOKENIZE_NUMERIC,
                &[empty, typed(Type::Numeric, 1, &[0])],
                8
            )
            .is_err()
        );
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
    fn strings_come_from_the_alphabet() {
        let lens = [3u32, 0, 2];
        let alphabet = strings(b"abcde", &lens);
        let indices = numbers::<2>(&[2, 0, 1, 1, 0, 2]);
        let out = run(
            &TOKENIZE_STRING,
            &[alphabet, typed(Type::Numeric, 2, &indices)],
            256,
        )
        .unwrap();
        assert_eq!(out.bytes(), b"deabcabcde");
        assert_eq!(out.as_input().lens, [2, 3, 0, 0, 3, 2]);
    }

    #[test]
    fn string_streams_reject_bad_input() {
        let lens = [1u32, 1];
        let alphabet = strings(b"ab", &lens);
        let ok = numbers::<1>(&[0, 1]);
        assert!(
            run(
                &TOKENIZE_STRING,
                &[alphabet, typed(Type::Numeric, 1, &ok)],
                64
            )
            .is_ok()
        );
        // Index out of range.
        let bad = numbers::<1>(&[0, 2]);
        assert!(
            run(
                &TOKENIZE_STRING,
                &[alphabet, typed(Type::Numeric, 1, &bad)],
                64
            )
            .is_err()
        );
        // Fewer tokens than alphabet entries.
        let few = numbers::<1>(&[0]);
        assert!(
            run(
                &TOKENIZE_STRING,
                &[alphabet, typed(Type::Numeric, 1, &few)],
                64
            )
            .is_err()
        );
        // Lengths that miss the content.
        let short = strings(b"a", &lens);
        assert!(run(&TOKENIZE_STRING, &[short, typed(Type::Numeric, 1, &ok)], 64).is_err());
        let overflow = [u32::MAX, u32::MAX];
        let huge = strings(b"", &overflow);
        assert!(run(&TOKENIZE_STRING, &[huge, typed(Type::Numeric, 1, &ok)], 64).is_err());
        // Result beyond the budget.
        assert!(
            run(
                &TOKENIZE_STRING,
                &[alphabet, typed(Type::Numeric, 1, &ok)],
                9
            )
            .is_err()
        );
        // Empty on both sides.
        let none = strings(b"", &[]);
        let out = run(&TOKENIZE_STRING, &[none, typed(Type::Numeric, 1, &[])], 64).unwrap();
        assert!(out.as_input().lens.is_empty());
    }
}
