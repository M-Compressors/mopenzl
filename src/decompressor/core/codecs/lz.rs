//! The generic LZ codec (`DI_lz`), feature `lz`.
//!
//! Ports `codecs/lz/decode_lz_binding.c` (`DI_lz`) and
//! `codecs/lz/decode_lz_kernel.c` (`ZL_Lz_decode`). Four inputs: the
//! literals (serial) and the offsets, literal lengths and match lengths
//! (numeric, each of its own width 1, 2, 4 or 8, one entry per sequence).
//! The header is the decoded size as a varint with nothing after it. The
//! output is the literals and matches in sequence order followed by the
//! literals no sequence used, and must fill the declared size exactly.
//!
//! The reference has loops specialised for 16-bit and 32-bit streams plus a
//! generic one, all with one meaning; this port is the generic loop, with
//! the widths read as little-endian integers.

use super::lz_copy::{copy_literals, copy_match};
use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;
use crate::shared::varint;

/// `ZL_StandardTransformID_lz`.
pub(crate) const LZ: Desc = Desc {
    id: 19,
    min_version: 24,
    regens: &[Type::Serial],
    variable_regens: false,
    inputs: &[Type::Serial, Type::Numeric, Type::Numeric, Type::Numeric],
    variable_inputs: 0,
    decode,
};

fn decode(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let [literals, offsets, literal_lens, match_lens] = inputs else {
        return Err(corrupt());
    };
    let sequences = offsets.len();
    if literal_lens.len() != sequences || match_lens.len() != sequences {
        return Err(corrupt());
    }
    let (size, used) = varint::decode(ctx.header).map_err(|_| corrupt())?;
    if used != ctx.header.len() {
        return Err(corrupt());
    }
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    let out = regens[0].fixed(1, size)?;
    let sequences = Sequences {
        literals: literals.bytes,
        offsets: Numbers::new(offsets),
        literal_lens: Numbers::new(literal_lens),
        match_lens: Numbers::new(match_lens),
        count: sequences,
    };
    sequences.expand(out, size)
}

/// A numeric stream read as unsigned little-endian integers of one width.
#[derive(Copy, Clone)]
struct Numbers<'a> {
    bytes: &'a [u8],
    width: usize,
}

impl<'a> Numbers<'a> {
    fn new(input: &Input<'a>) -> Self {
        Self {
            bytes: input.bytes,
            width: input.width,
        }
    }

    /// Element `index`; the caller keeps it below the stream's length.
    #[inline]
    fn get(&self, index: usize) -> u64 {
        match self.width {
            1 => u64::from(self.bytes[index]),
            2 => u64::from(u16::from_le_bytes(self.chunk(index))),
            4 => u64::from(u32::from_le_bytes(self.chunk(index))),
            _ => {
                let mut value = [0u8; 8];
                let start = index * self.width;
                let width = self.width.min(8);
                value[..width].copy_from_slice(&self.bytes[start..start + width]);
                u64::from_le_bytes(value)
            }
        }
    }

    /// The `N` bytes of element `index`.
    #[inline]
    fn chunk<const N: usize>(&self, index: usize) -> [u8; N] {
        let start = index * N;
        let mut value = [0u8; N];
        value.copy_from_slice(&self.bytes[start..start + N]);
        value
    }
}

/// The inputs of the codec.
struct Sequences<'a> {
    literals: &'a [u8],
    offsets: Numbers<'a>,
    literal_lens: Numbers<'a>,
    match_lens: Numbers<'a>,
    count: usize,
}

impl Sequences<'_> {
    /// `ZL_Lz_decode`: appends the `size` decoded bytes to the empty `out`.
    fn expand(&self, out: &mut alloc::vec::Vec<u8>, size: usize) -> Result<(), DecodeError> {
        let mut literals = self.literals;
        // The output is written in place; bytes past `pos` are scratch.
        out.resize(size, 0);
        let buf = out.as_mut_slice();
        let mut pos = 0;
        for sequence in 0..self.count {
            let literal_len =
                usize::try_from(self.literal_lens.get(sequence)).unwrap_or(usize::MAX);
            let offset = usize::try_from(self.offsets.get(sequence)).unwrap_or(usize::MAX);
            let match_len = usize::try_from(self.match_lens.get(sequence)).unwrap_or(usize::MAX);
            // Literals: enough of them and room for them.
            if literal_len > literals.len() || literal_len > size - pos {
                return Err(corrupt());
            }
            copy_literals(buf, pos, literals, literal_len);
            literals = &literals[literal_len..];
            pos += literal_len;
            // Match: a real offset and room for it.
            if offset == 0 || offset > pos || match_len > size - pos {
                return Err(corrupt());
            }
            copy_match(buf, pos, offset, match_len);
            pos += match_len;
        }
        // The literals no sequence used end the output, which must be full.
        if literals.len() > size - pos {
            return Err(corrupt());
        }
        buf[pos..pos + literals.len()].copy_from_slice(literals);
        pos += literals.len();
        if pos == size { Ok(()) } else { Err(corrupt()) }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::super::State;
    use super::*;
    use crate::Backend;

    /// Literals, three numeric streams and a header.
    type Case<'a> = (&'a [u8], Streams<'a>, &'a [u8]);
    /// Width and values of the offsets, literal lengths and match lengths.
    type Streams<'a> = [(usize, &'a [u64]); 3];

    fn numeric(width: usize, values: &[u64]) -> (usize, Vec<u8>) {
        let bytes = values
            .iter()
            .flat_map(|value| value.to_le_bytes()[..width].to_vec())
            .collect();
        (width, bytes)
    }

    /// Runs the codec on `literals` and `[(width, values)]` streams.
    fn run(
        header: &[u8],
        literals: &[u8],
        streams: Streams<'_>,
        budget: usize,
    ) -> Result<Vec<u8>, DecodeError> {
        let encoded: Vec<_> = streams
            .iter()
            .map(|&(width, values)| numeric(width, values))
            .collect();
        let mut inputs = vec![Input::serial(literals)];
        inputs.extend(encoded.iter().map(|(width, bytes)| Input {
            ty: Type::Numeric,
            width: *width,
            bytes,
            lens: &[],
        }));
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, header);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::Serial, budget);
        decode(&mut ctx, &inputs, &mut regens)?;
        regens[0].validate()?;
        Ok(regens[0].bytes().to_vec())
    }

    #[test]
    fn sequences_expand_with_literals_matches_and_a_tail() {
        // "ab", 5 bytes at distance 2 ("ababa"), "cd", 3 at distance 7 ("aba"), tail.
        let out = run(
            &[16],
            b"abcdTAIL",
            [(2, &[2, 7]), (1, &[2, 2]), (8, &[5, 3])],
            64,
        )
        .unwrap();
        assert_eq!(out, b"abababacdabaTAIL");
        // The narrowest and widest integers hold the same values.
        let wide = run(
            &[16],
            b"abcdTAIL",
            [(8, &[2, 7]), (4, &[2, 2]), (2, &[5, 3])],
            64,
        )
        .unwrap();
        assert_eq!(wide, out);
        // No sequences: the literals are the output.
        assert_eq!(
            run(&[4], b"abcd", [(1, &[]), (1, &[]), (1, &[])], 64).unwrap(),
            b"abcd"
        );
        assert_eq!(
            run(&[0], b"", [(1, &[]), (1, &[]), (1, &[])], 64).unwrap(),
            b""
        );
    }

    #[test]
    fn long_matches_repeat_short_offsets() {
        let out = run(&[103], b"xy", [(1, &[2]), (1, &[2]), (2, &[101])], 128).unwrap();
        let mut expected = b"xy".to_vec();
        expected.extend(b"xy".iter().cycle().take(101));
        assert_eq!(out, expected);
    }

    #[test]
    fn mismatched_counts_and_headers_are_rejected() {
        let ok = [(1usize, &[1u64][..]), (1, &[1]), (1, &[1])];
        assert!(run(&[3], b"ab", ok, 64).is_ok());
        assert_eq!(
            run(&[3], b"ab", [(1, &[1]), (1, &[1, 1]), (1, &[1])], 64),
            Err(corrupt())
        );
        assert_eq!(
            run(&[3], b"ab", [(1, &[1]), (1, &[1]), (1, &[])], 64),
            Err(corrupt())
        );
        // Leftover, missing and overlong headers.
        assert_eq!(run(&[3, 0], b"ab", ok, 64), Err(corrupt()));
        assert_eq!(run(&[], b"ab", ok, 64), Err(corrupt()));
        assert_eq!(run(&[0x80], b"ab", ok, 64), Err(corrupt()));
        // A size beyond the budget, and one that does not match the content.
        assert_eq!(run(&[100], b"ab", ok, 64), Err(corrupt()));
        assert_eq!(run(&[4], b"ab", ok, 64), Err(corrupt()));
        assert_eq!(run(&[2], b"ab", ok, 64), Err(corrupt()));
        // Missing inputs.
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[0]);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::Serial, 8);
        assert_eq!(decode(&mut ctx, &[], &mut regens), Err(corrupt()));
    }

    #[test]
    fn corrupt_sequences_are_rejected() {
        // literal length 3 with 2 literals; 4 of 2 total; offset 0; offset
        // beyond the output; match past the end; tail past the end.
        let cases: [Case<'_>; 7] = [
            (b"ab", [(1, &[1]), (1, &[3]), (1, &[1])], &[7]),
            (b"abcd", [(1, &[1]), (1, &[3]), (1, &[1])], &[2]),
            (b"ab", [(1, &[0]), (1, &[2]), (1, &[2])], &[6]),
            (b"ab", [(1, &[3]), (1, &[2]), (1, &[2])], &[6]),
            (b"ab", [(1, &[1]), (1, &[2]), (1, &[9])], &[6]),
            (b"abcdef", [(1, &[1]), (1, &[2]), (1, &[1])], &[5]),
            (b"ab", [(1, &[1]), (1, &[2]), (1, &[1])], &[9]),
        ];
        for (literals, streams, header) in cases {
            assert_eq!(
                run(header, literals, streams, 64),
                Err(corrupt()),
                "{literals:?}"
            );
        }
        // Values above `isize::MAX` are negative in the reference.
        for streams in [
            [(8, &[1][..]), (8, &[u64::MAX]), (8, &[1])],
            [(8, &[u64::MAX]), (8, &[2]), (8, &[1])],
            [(8, &[1]), (8, &[2]), (8, &[u64::MAX])],
            [(8, &[1]), (8, &[1 << 63]), (8, &[1])],
        ] {
            assert_eq!(run(&[8], b"ab", streams, 64), Err(corrupt()));
        }
    }
}
