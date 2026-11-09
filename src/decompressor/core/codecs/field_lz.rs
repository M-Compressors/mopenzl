//! The field-wise LZ codec (`DI_fieldLz`), feature `field-lz`.
//!
//! Ports `codecs/lz/decode_field_lz.c` (`ZS2_FieldLz_decompress`) and the
//! `DI_fieldLz` binding of `decode_lz_binding.c`. The codec compresses a
//! stream of fixed-width fields (any power-of-two width) as LZ77 over whole
//! fields. Five inputs, in the reference's order:
//!
//! 0. the literal fields (struct, the output's width);
//! 1. one 16-bit token per sequence (struct): bits 0-1 the offset code,
//!    bits 2-5 the literal-length code, bits 6-9 the match-length code;
//! 2. the explicit offsets, in fields (numeric, 32-bit);
//! 3. the extra literal lengths, in fields (numeric, 32-bit);
//! 4. the extra match lengths, in fields (numeric, 32-bit).
//!
//! Offset code 3 takes the next explicit offset; codes 0 to 2 pick one of
//! the three most recent offsets (initially 1, 2 and 4 fields) and move it
//! to the front. A length code below 15 is the length itself; 15 adds the
//! next extra length. Match lengths start from a minimum of 4 fields for
//! 1-byte fields, 2 for 2-byte fields and 1 otherwise. After the last token
//! the unused literals end the output, and every explicit offset and extra
//! length must have been used. The header is the output size in fields as a
//! varint with nothing after it. With no tokens the output is the literals.
//!
//! The reference selects among decoders unrolled by four tokens and tuned to
//! the length statistics (and uses AVX2 for the offsets when available).
//! Those are one decoder with different speed, and the loop that ends every
//! block, the one ported here, is its specification. Lengths and offsets are
//! computed in 32-bit arithmetic as there.

use alloc::vec::Vec;

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;
use crate::shared::varint;

/// `ZL_StandardTransformID_field_lz`.
pub(crate) const FIELD_LZ: Desc = Desc {
    id: 24,
    min_version: 3,
    regens: &[Type::Struct],
    variable_regens: false,
    inputs: &[
        Type::Struct,
        Type::Struct,
        Type::Numeric,
        Type::Numeric,
        Type::Numeric,
    ],
    variable_inputs: 0,
    decode,
};

/// Bits of the offset code in a token (`kTokenOFBits`).
const OFFSET_BITS: u32 = 2;
/// Bits of the literal-length code (`kTokenLLBits`).
const LITERAL_BITS: u32 = 4;
/// Bits of the match-length code (`kTokenMLBits`).
const MATCH_BITS: u32 = 4;
/// The literal-length and match-length code that reads an extra length.
const MAX_CODE: u32 = 15;

fn decode(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let [literals, tokens, offsets, extra_literals, extra_matches] = inputs else {
        return Err(corrupt());
    };
    let width = literals.width;
    if !width.is_power_of_two()
        || tokens.width != 2
        || offsets.width != 4
        || extra_literals.width != 4
        || extra_matches.width != 4
    {
        return Err(corrupt());
    }
    let (capacity, used) = varint::decode(ctx.header).map_err(|_| corrupt())?;
    if used != ctx.header.len() {
        return Err(corrupt());
    }
    let capacity = usize::try_from(capacity).unwrap_or(usize::MAX);
    let out = regens[0].fixed(width, capacity)?;
    let limit = capacity.checked_mul(width).ok_or_else(corrupt)?;
    if tokens.bytes.is_empty() {
        if literals.bytes.len() > limit {
            return Err(corrupt());
        }
        out.extend_from_slice(literals.bytes);
        return Ok(());
    }
    // The reference shifts 32-bit values by this many bits.
    let bits = width.trailing_zeros();
    if bits >= u32::BITS {
        return Err(corrupt());
    }
    Fields {
        bits,
        min_match: match width {
            1 => 4,
            2 => 2,
            _ => 1,
        },
        limit,
        literals: literals.bytes,
        tokens: tokens.bytes,
        offsets: Numbers::new(offsets.bytes),
        extra_literals: Numbers::new(extra_literals.bytes),
        extra_matches: Numbers::new(extra_matches.bytes),
    }
    .expand(out)
}

/// A stream of 32-bit integers consumed front to back.
struct Numbers<'a>(core::slice::Iter<'a, [u8; 4]>);

impl<'a> Numbers<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self(bytes.as_chunks::<4>().0.iter())
    }

    /// The next value, or a corrupted stream when none is left.
    fn next(&mut self) -> Result<u32, DecodeError> {
        let bytes = self.0.next().ok_or_else(corrupt)?;
        Ok(u32::from_le_bytes(*bytes))
    }

    /// Whether every value was consumed.
    fn is_empty(&self) -> bool {
        self.0.len() == 0
    }
}

/// The inputs of the codec, with the field width as a shift.
struct Fields<'a> {
    /// log2 of the field width in bytes.
    bits: u32,
    /// The smallest match, in fields.
    min_match: u32,
    /// The output capacity, in bytes.
    limit: usize,
    literals: &'a [u8],
    tokens: &'a [u8],
    offsets: Numbers<'a>,
    extra_literals: Numbers<'a>,
    extra_matches: Numbers<'a>,
}

impl Fields<'_> {
    /// Appends the decoded fields to the empty `out`.
    fn expand(mut self, out: &mut Vec<u8>) -> Result<(), DecodeError> {
        let bits = self.bits;
        let mut literals = self.literals;
        // The three most recent offsets, in bytes.
        let mut reps = [1u32 << bits, 2u32 << bits, 4u32 << bits];
        for token in self.tokens.as_chunks::<2>().0 {
            let token = u32::from(u16::from_le_bytes(*token));
            let offset_code = token & ((1 << OFFSET_BITS) - 1);
            let literal_code = (token >> OFFSET_BITS) & ((1 << LITERAL_BITS) - 1);
            let match_code = (token >> (OFFSET_BITS + LITERAL_BITS)) & ((1 << MATCH_BITS) - 1);
            let offset = match offset_code {
                3 => {
                    let offset = self.offsets.next()? << bits;
                    reps = [offset, reps[0], reps[1]];
                    offset
                }
                0 => reps[0],
                1 => {
                    reps = [reps[1], reps[0], reps[2]];
                    reps[0]
                }
                _ => {
                    reps = [reps[2], reps[0], reps[1]];
                    reps[0]
                }
            };
            let mut literal_len = literal_code;
            if literal_code == MAX_CODE {
                literal_len = literal_len.wrapping_add(self.extra_literals.next()?);
            }
            let literal_len = (literal_len << bits) as usize;
            let mut match_len = match_code + self.min_match;
            if match_code == MAX_CODE {
                match_len = match_len.wrapping_add(self.extra_matches.next()?);
            }
            let match_len = (match_len << bits) as usize;

            let room = self.limit - out.len();
            if literal_len.saturating_add(match_len) > room {
                return Err(corrupt());
            }
            if literal_len > literals.len() {
                return Err(corrupt());
            }
            let (now, rest) = literals.split_at(literal_len);
            out.extend_from_slice(now);
            literals = rest;
            let offset = offset as usize;
            // Offset 0 reads bytes not written yet in the reference.
            if offset == 0 || offset > out.len() {
                return Err(corrupt());
            }
            copy_match(out, offset, match_len);
        }
        if literals.len() > self.limit - out.len() {
            return Err(corrupt());
        }
        out.extend_from_slice(literals);
        if self.offsets.is_empty()
            && self.extra_literals.is_empty()
            && self.extra_matches.is_empty()
        {
            Ok(())
        } else {
            Err(corrupt())
        }
    }
}

/// Appends `length` bytes starting `offset` bytes back; an offset below
/// `length` repeats the pattern.
fn copy_match(out: &mut Vec<u8>, offset: usize, length: usize) {
    let from = out.len() - offset;
    let mut done = 0;
    while done < length {
        let chunk = offset.min(length - done);
        out.extend_from_within(from..from + chunk);
        done += chunk;
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::super::State;
    use super::*;
    use crate::Backend;

    /// The five inputs of one block.
    struct Block<'a> {
        width: usize,
        header: &'a [u8],
        literals: Vec<u8>,
        tokens: Vec<u16>,
        offsets: Vec<u32>,
        extra_literals: Vec<u32>,
        extra_matches: Vec<u32>,
    }

    impl Default for Block<'_> {
        fn default() -> Self {
            Self {
                width: 1,
                header: &[0],
                literals: Vec::new(),
                tokens: Vec::new(),
                offsets: Vec::new(),
                extra_literals: Vec::new(),
                extra_matches: Vec::new(),
            }
        }
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    impl Block<'_> {
        fn run(&self, budget: usize) -> Result<Vec<u8>, DecodeError> {
            let tokens: Vec<u8> = self.tokens.iter().flat_map(|t| t.to_le_bytes()).collect();
            let (offsets, extra_literals, extra_matches) = (
                words(&self.offsets),
                words(&self.extra_literals),
                words(&self.extra_matches),
            );
            let fixed = |ty, width, bytes| Input {
                ty,
                width,
                bytes,
                lens: &[],
            };
            let inputs = [
                fixed(Type::Struct, self.width, &self.literals),
                fixed(Type::Struct, 2, &tokens),
                fixed(Type::Numeric, 4, &offsets),
                fixed(Type::Numeric, 4, &extra_literals),
                fixed(Type::Numeric, 4, &extra_matches),
            ];
            self.run_inputs(&inputs, budget)
        }

        fn run_inputs(&self, inputs: &[Input<'_>], budget: usize) -> Result<Vec<u8>, DecodeError> {
            let mut state = State::new(Backend::default());
            let mut ctx = Ctx::new(&mut state, self.header);
            let mut regens = [Stream::default()];
            regens[0].prepare(Type::Struct, budget);
            decode(&mut ctx, inputs, &mut regens)?;
            regens[0].validate()?;
            Ok(regens[0].bytes().to_vec())
        }
    }

    /// A token from its codes.
    const fn token(offset: u16, literal: u16, matched: u16) -> u16 {
        offset | literal << 2 | matched << 6
    }

    #[test]
    fn repeat_offsets_rotate_and_explicit_offsets_push() {
        // Width 1: matches are at least 4 bytes, offsets start at 1, 2, 4.
        let block = Block {
            header: &[40],
            literals: b"abczQ".to_vec(),
            tokens: vec![
                token(3, 3, 0), // new offset 3: "abc" + "abca"
                token(1, 0, 1), // rep 1 (=1): 5 x 'a'
                token(2, 1, 0), // rep 2 (=2): 'z' + "azaz"
                token(0, 0, 0), // rep 0 (=2): "azaz"
            ],
            offsets: vec![3],
            ..Block::default()
        };
        let out = block.run(64).unwrap();
        assert_eq!(out, b"abcabcaaaaaazazazazazQ");
    }

    #[test]
    fn extra_lengths_extend_codes_of_fifteen() {
        // 15 + 5 literals, then a match of 15 + 4 + 3 extra = 22 bytes.
        let literals: Vec<u8> = (b'a'..b'a' + 20).collect();
        let block = Block {
            header: &[60],
            literals: literals.clone(),
            tokens: vec![token(3, 15, 15)],
            offsets: vec![20],
            extra_literals: vec![5],
            extra_matches: vec![3],
            ..Block::default()
        };
        let out = block.run(64).unwrap();
        let mut expected = literals.clone();
        expected.extend(literals.iter().cycle().take(22));
        assert_eq!(out, expected);
    }

    #[test]
    fn wider_fields_scale_lengths_and_offsets() {
        // Width 2: a match is at least 2 fields; the offset is in fields.
        let block = Block {
            width: 2,
            header: &[4],
            literals: b"ABCD".to_vec(),
            tokens: vec![token(0, 2, 0)],
            ..Block::default()
        };
        assert_eq!(block.run(64).unwrap(), b"ABCDCDCD");
        // Width 4 and 8: a match is one field at least.
        for width in [4usize, 8] {
            let literals: Vec<u8> = (0..2 * width as u8).collect();
            let block = Block {
                width,
                header: &[3],
                literals: literals.clone(),
                tokens: vec![token(1, 2, 0)],
                ..Block::default()
            };
            // rep 1 is two fields back, the whole literal run.
            let mut expected = literals.clone();
            expected.extend_from_slice(&literals[..width]);
            assert_eq!(block.run(256).unwrap(), expected);
        }
        // Width 16 has no specialised decoder in the reference either.
        let literals: Vec<u8> = (0..32).collect();
        let block = Block {
            width: 16,
            header: &[3],
            literals: literals.clone(),
            tokens: vec![token(1, 2, 0)],
            ..Block::default()
        };
        assert_eq!(block.run(256).unwrap().len(), 48);
    }

    #[test]
    fn without_tokens_the_literals_are_the_output() {
        let block = Block {
            header: &[3],
            literals: b"xyz".to_vec(),
            ..Block::default()
        };
        assert_eq!(block.run(64).unwrap(), b"xyz");
        let block = Block {
            width: 4,
            header: &[1],
            literals: vec![1, 2, 3, 4, 5, 6, 7, 8],
            ..Block::default()
        };
        assert_eq!(block.run(64), Err(corrupt()));
        // Leftover offsets are not checked without tokens, as in the reference.
        let block = Block {
            header: &[0],
            offsets: vec![7],
            ..Block::default()
        };
        assert_eq!(block.run(64).unwrap(), b"");
    }

    #[test]
    fn malformed_blocks_are_rejected() {
        let base = || Block {
            header: &[8],
            literals: b"abcd".to_vec(),
            tokens: vec![token(0, 4, 0)],
            ..Block::default()
        };
        assert_eq!(base().run(64).unwrap(), b"abcddddd");
        // Header: missing, truncated, with leftover bytes, too large.
        for header in [&[][..], &[0x80], &[8, 0], &[200, 1]] {
            assert_eq!(Block { header, ..base() }.run(64), Err(corrupt()));
        }
        // Output size: the declared capacity bounds the decoded bytes.
        assert_eq!(
            Block {
                header: &[7],
                ..base()
            }
            .run(64),
            Err(corrupt())
        );
        // Too few literals, too little room for a literal run.
        assert_eq!(
            Block {
                literals: b"abc".to_vec(),
                ..base()
            }
            .run(64),
            Err(corrupt())
        );
        // Offsets: missing, left over, zero, beyond the output.
        let explicit = |offsets: Vec<u32>| Block {
            tokens: vec![token(3, 4, 0)],
            offsets,
            ..base()
        };
        assert_eq!(explicit(vec![]).run(64), Err(corrupt()));
        assert_eq!(explicit(vec![1, 1]).run(64), Err(corrupt()));
        assert_eq!(explicit(vec![0]).run(64), Err(corrupt()));
        assert_eq!(explicit(vec![5]).run(64), Err(corrupt()));
        assert_eq!(explicit(vec![4]).run(64).unwrap(), b"abcdabcd");
        // Extra lengths: missing, left over.
        let extra = |extra_literals: Vec<u32>, extra_matches: Vec<u32>, code| Block {
            tokens: vec![token(0, code, code)],
            literals: b"abcdefghijklmnopqrstuvwxyz".to_vec(),
            extra_literals,
            extra_matches,
            header: &[40],
            ..base()
        };
        assert_eq!(extra(vec![], vec![0], 15).run(64), Err(corrupt()));
        assert_eq!(extra(vec![0], vec![], 15).run(64), Err(corrupt()));
        assert_eq!(extra(vec![0, 0], vec![0], 15).run(64), Err(corrupt()));
        assert_eq!(extra(vec![0], vec![0, 0], 15).run(64), Err(corrupt()));
        // Lengths computed in 32 bits wrap like the reference's.
        assert_eq!(extra(vec![u32::MAX], vec![0], 15).run(64), Err(corrupt()));
        // Literals left after the last token overflow the output.
        let tail = Block {
            header: &[5],
            literals: b"abcdefgh".to_vec(),
            tokens: vec![token(0, 4, 0)],
            ..Block::default()
        };
        assert_eq!(tail.run(64), Err(corrupt()));
    }

    #[test]
    fn input_shapes_are_checked() {
        let block = Block {
            header: &[8],
            literals: b"abcd".to_vec(),
            tokens: vec![token(0, 4, 0)],
            ..Block::default()
        };
        let good = |ty, width, bytes: &'static [u8]| Input {
            ty,
            width,
            bytes,
            lens: &[],
        };
        let mk = |literal_width, token_width, word_width: [usize; 3]| {
            [
                good(Type::Struct, literal_width, b"abcd"),
                good(Type::Struct, token_width, &[16, 0]),
                good(Type::Numeric, word_width[0], &[]),
                good(Type::Numeric, word_width[1], &[]),
                good(Type::Numeric, word_width[2], &[]),
            ]
        };
        assert!(block.run_inputs(&mk(1, 2, [4, 4, 4]), 64).is_ok());
        // A width that is not a power of two, or a wrong word width.
        assert_eq!(block.run_inputs(&mk(3, 2, [4, 4, 4]), 64), Err(corrupt()));
        assert_eq!(block.run_inputs(&mk(1, 1, [4, 4, 4]), 64), Err(corrupt()));
        assert_eq!(block.run_inputs(&mk(1, 2, [8, 4, 4]), 64), Err(corrupt()));
        assert_eq!(block.run_inputs(&mk(1, 2, [4, 2, 4]), 64), Err(corrupt()));
        assert_eq!(block.run_inputs(&mk(1, 2, [4, 4, 1]), 64), Err(corrupt()));
        // The wrong number of inputs.
        assert_eq!(
            block.run_inputs(&mk(1, 2, [4, 4, 4])[..4], 64),
            Err(corrupt())
        );
    }
}
