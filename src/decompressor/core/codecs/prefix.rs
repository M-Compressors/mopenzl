//! The prefix codec (`DI_prefix`, `decode_prefix_kernel.c`), feature
//! `prefix`.
//!
//! Rebuilds sorted strings from their suffixes (`ins[0]`, strings) and, per
//! string, how many leading bytes it shares with the previous rebuilt string
//! (`ins[1]`, 32-bit numbers; the first must be 0). Both inputs have one
//! element per string. A shared length longer than the previous string, or a
//! rebuilt length beyond 32 bits, is corrupt.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_prefix`.
pub(crate) const PREFIX: Desc = Desc {
    id: 46,
    min_version: 11,
    regens: &[Type::String],
    variable_regens: false,
    inputs: &[Type::String, Type::Numeric],
    variable_inputs: 0,
    decode,
};

fn decode(_: &mut Ctx<'_>, inputs: &[Input<'_>], regens: &mut [Stream]) -> Result<(), DecodeError> {
    let (suffixes, matches) = (&inputs[0], &inputs[1]);
    if matches.len() != suffixes.lens.len() || matches.width != 4 {
        return Err(corrupt());
    }
    let matches = matches
        .bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|size| u32::from_le_bytes(*size));
    // `ZS_calcOriginalPrefixSize`.
    let total = matches
        .clone()
        .try_fold(suffixes.bytes.len() as u64, |total, size| {
            total.checked_add(u64::from(size))
        })
        .and_then(|total| usize::try_from(total).ok())
        .ok_or_else(corrupt)?;
    let (out, lens) = regens[0].strings(suffixes.lens.len(), total)?;
    let mut tail = suffixes.bytes;
    let mut previous = (0usize, 0u32);
    for (&suffix, shared) in suffixes.lens.iter().zip(matches) {
        if shared > previous.1 {
            return Err(corrupt());
        }
        let length = shared.checked_add(suffix).ok_or_else(corrupt)?;
        let start = out.len();
        out.extend_from_within(previous.0..previous.0 + shared as usize);
        let (suffix_bytes, rest) = tail.split_at_checked(suffix as usize).ok_or_else(corrupt)?;
        out.extend_from_slice(suffix_bytes);
        tail = rest;
        lens.push(length);
        previous = (start, length);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    const DESC: Desc = PREFIX;
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn run(bytes: &[u8], lens: &[u32], matches: &[u32]) -> Result<Stream, DecodeError> {
        let shared: Vec<u8> = matches.iter().flat_map(|m| m.to_le_bytes()).collect();
        let inputs = [
            Input {
                ty: Type::String,
                width: 1,
                bytes,
                lens,
            },
            Input {
                ty: Type::Numeric,
                width: 4,
                bytes: &shared,
                lens: &[],
            },
        ];
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[]);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::String, 1 << 16);
        (DESC.decode)(&mut ctx, &inputs, &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream)
    }

    #[test]
    fn strings_regain_their_shared_prefixes() {
        let out = run(b"applet", &[5, 1], &[0, 4]).unwrap();
        assert_eq!(out.bytes(), b"appleapplt");
        assert_eq!(out.as_input().lens, [5, 5]);
    }

    #[test]
    fn the_documented_example_decodes() {
        let out = run(b"applelytly", &[5, 2, 1, 2], &[0, 3, 2, 3]).unwrap();
        assert_eq!(out.bytes(), b"appleapplyaptaptly");
        assert_eq!(out.as_input().lens, [5, 5, 3, 5]);
    }

    #[test]
    fn empty_and_unshared_strings_decode() {
        let out = run(&[], &[], &[]).unwrap();
        assert!(out.bytes().is_empty());
        let out = run(b"ab", &[0, 2, 0], &[0, 0, 1]).unwrap();
        assert_eq!(out.bytes(), b"aba");
        assert_eq!(out.as_input().lens, [0, 2, 1]);
    }

    #[test]
    fn matches_beyond_the_previous_string_are_corrupt() {
        assert_eq!(run(b"a", &[1], &[1]).err(), Some(corrupt()));
        assert_eq!(run(b"ab", &[1, 1], &[0, 2]).err(), Some(corrupt()));
    }

    #[test]
    fn lengths_beyond_32_bits_are_corrupt() {
        // The first string is empty, so the second cannot share u32::MAX.
        assert_eq!(run(b"", &[0, 0], &[0, u32::MAX]).err(), Some(corrupt()));
    }

    #[test]
    fn mismatched_inputs_are_corrupt() {
        assert_eq!(run(b"ab", &[1, 1], &[0]).err(), Some(corrupt()));
        let shared = [0u8; 4];
        let narrow = [
            Input {
                ty: Type::String,
                width: 1,
                bytes: b"a",
                lens: &[1],
            },
            Input {
                ty: Type::Numeric,
                width: 2,
                bytes: &shared,
                lens: &[],
            },
        ];
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[]);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::String, 64);
        assert_eq!(
            (DESC.decode)(&mut ctx, &narrow, &mut regens),
            Err(corrupt())
        );
    }

    #[test]
    fn the_total_is_bounded_by_the_budget() {
        // Three 30000-byte strings need 90000 > 65536 bytes.
        let bytes = [b'x'; 30_000];
        assert_eq!(
            run(&bytes, &[30_000, 0, 0], &[0, 30_000, 30_000]).err(),
            Some(corrupt())
        );
        // Two fit.
        assert!(run(&bytes, &[30_000, 0], &[0, 30_000]).is_ok());
        // Sums that overflow 64 bits cannot be reached with 32-bit sizes,
        // but a share past the budget is still refused.
        assert_eq!(run(b"", &[0, 0], &[0, u32::MAX]).err(), Some(corrupt()));
    }
}
