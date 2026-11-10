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
//! They are one decoder with different speed. This port decodes groups of
//! four tokens the same way (offsets of the group first, from a table, then
//! the copies as fixed-size blocks) and finishes every block with the checked
//! loop, which is the specification. Lengths and offsets are computed in
//! 32-bit arithmetic as there.

use alloc::vec::Vec;

use super::lz_copy::{copy_literals, copy_match};
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
        tokens: tokens.bytes.as_chunks::<2>().0,
        offsets: Numbers::new(offsets.bytes),
        extra_literals: Numbers::new(extra_literals.bytes),
        extra_matches: Numbers::new(extra_matches.bytes),
    }
    .expand(out)
}

/// A stream of 32-bit integers consumed front to back.
struct Numbers<'a>(&'a [[u8; 4]]);

impl<'a> Numbers<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self(bytes.as_chunks::<4>().0)
    }

    /// The next value, or a corrupted stream when none is left.
    fn next(&mut self) -> Result<u32, DecodeError> {
        let (first, rest) = self.0.split_first().ok_or_else(corrupt)?;
        self.0 = rest;
        Ok(u32::from_le_bytes(*first))
    }

    /// Whether every value was consumed.
    const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The smallest match, in fields, for fields of `1 << bits` bytes.
const fn min_match(bits: u32) -> u32 {
    match bits {
        0 => 4,
        1 => 2,
        _ => 1,
    }
}

/// The decoder's position in its streams.
#[derive(Clone, Copy)]
struct Walk {
    /// Bytes written to the output.
    pos: usize,
    /// Literal bytes consumed.
    used: usize,
    /// The three most recent offsets, in bytes.
    reps: [u32; 3],
}

/// Tokens per group of the check-free loop.
const GROUP: usize = 4;
/// The bytes of one block copy.
const BLOCK: usize = 16;

/// The literal bytes a group of tokens without extra lengths reads at
/// most, scratch included, for fields of `1 << bits` bytes.
const fn literal_margin(bits: u32) -> usize {
    GROUP * (14usize << bits).div_ceil(BLOCK) * BLOCK
}

/// The limits of tokens that carry no extra length, for fields of
/// `1 << BITS` bytes.
struct Shape<const BITS: u32>;

impl<const BITS: u32> Shape<BITS> {
    const MIN: u32 = min_match(BITS);
    /// Blocks that cover the longest match (code 14).
    const MATCH_BLOCKS: usize = (((14 + Self::MIN) as usize) << BITS).div_ceil(BLOCK);
    /// Output bytes one token touches at most, scratch included.
    const OUT_STEP: usize = (14usize << BITS) + Self::MATCH_BLOCKS * BLOCK;
    /// The longest literal run, in bytes, the short-group path copies as
    /// one block: never an extra length (code 15).
    const SHORT_LITERAL: usize = if UNIT < 14 << BITS { UNIT } else { 14 << BITS };
    /// The longest match, in bytes, the short-group path copies as one
    /// block.
    const SHORT_MATCH: usize = if UNIT < ((14 + Self::MIN) as usize) << BITS {
        UNIT
    } else {
        ((14 + Self::MIN) as usize) << BITS
    };
    /// Output room a group needs.
    const OUT_MARGIN: usize = GROUP * Self::OUT_STEP;
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
    tokens: &'a [[u8; 2]],
    offsets: Numbers<'a>,
    extra_literals: Numbers<'a>,
    extra_matches: Numbers<'a>,
}

/// Copies the literals and the match of one token with fixed-size blocks.
///
/// The caller guarantees the margins of [`Shape`] and a token without
/// extra lengths. Returns whether the token is valid.
#[inline(always)]
fn fast_token<const BITS: u32, const L: usize>(
    buf: &mut [u8],
    window: &[u8; L],
    pos: &mut usize,
    taken: &mut usize,
    offset: u32,
    token: u32,
) -> bool {
    let literal_code = (token >> OFFSET_BITS) & ((1 << LITERAL_BITS) - 1);
    let match_code = (token >> (OFFSET_BITS + LITERAL_BITS)) & ((1 << MATCH_BITS) - 1);
    let literal_len = (literal_code as usize) << BITS;
    let match_len = ((match_code + Shape::<BITS>::MIN) as usize) << BITS;
    // The first block of every run is unconditional.
    let (Some(from), Some(to)) = (
        window.get(*taken..).and_then(<[u8]>::first_chunk::<BLOCK>),
        buf.get_mut(*pos..)
            .and_then(<[u8]>::first_chunk_mut::<BLOCK>),
    ) else {
        return false;
    };
    *to = *from;
    let mut at = BLOCK;
    while at < literal_len {
        buf[*pos + at..*pos + at + BLOCK]
            .copy_from_slice(&window[*taken + at..*taken + at + BLOCK]);
        at += BLOCK;
    }
    *taken += literal_len;
    *pos += literal_len;
    let offset = offset as usize;
    // Offset 0 reads bytes not written yet in the reference.
    if offset.wrapping_sub(1) >= *pos {
        return false;
    }
    let source = *pos - offset;
    if offset >= BLOCK {
        buf.copy_within(source..source + BLOCK, *pos);
        let mut at = BLOCK;
        while at < match_len {
            buf.copy_within(source + at..source + at + BLOCK, *pos + at);
            at += BLOCK;
        }
    } else {
        copy_match(buf, *pos, offset, match_len);
    }
    *pos += match_len;
    true
}

/// A 4-bit length code in each of the four 16-bit lanes of a token word.
const LANES: u64 = 0x0001_0001_0001_0001;

/// What the four offset codes of a group do, indexed by the codes packed two
/// bits each (token 0 in the low bits). Each entry holds, in 3-bit fields
/// from the low end: the candidate each token takes as its offset, the
/// candidates that become the three repeat offsets, and the number of
/// explicit offsets used. Candidate 0 to 2 are the repeat offsets, 3 to 6
/// the next four explicit offsets (the reference's `ZS_kOffsetShuffle`).
static OFFSET_PLANS: [u32; 256] = offset_plans();

const fn offset_plans() -> [u32; 256] {
    let mut plans = [0; 256];
    let mut mask = 0;
    while mask < 256 {
        let mut reps = [0u32, 1, 2];
        let mut fresh = 0;
        let mut plan = 0;
        let mut token = 0;
        while token < GROUP {
            let code = (mask >> (2 * token)) & 3;
            let take = if code == 3 {
                fresh += 1;
                3 + fresh - 1
            } else {
                reps[code as usize]
            };
            reps = match code {
                0 => reps,
                1 => [reps[1], reps[0], reps[2]],
                2 => [reps[2], reps[0], reps[1]],
                _ => [take, reps[0], reps[1]],
            };
            plan |= take << (3 * token);
            token += 1;
        }
        plan |= reps[0] << 12;
        plan |= reps[1] << 15;
        plan |= reps[2] << 18;
        plan |= fresh << 21;
        plans[mask as usize] = plan;
        mask += 1;
    }
    plans
}

/// The codes of the four tokens of `word`, two bits each.
#[inline(always)]
const fn offset_codes(word: u64) -> usize {
    // Each code moves to bits 48 + 2 * token without carries.
    const GATHER: u64 = (1 << 48) | (1 << 34) | (1 << 20) | (1 << 6);
    ((word & (3 * LANES)).wrapping_mul(GATHER) >> 48) as usize & 0xff
}

/// The offsets of the four tokens of `word`, the new repeat offsets and
/// the explicit offsets used.
#[inline(always)]
fn resolve_group<const BITS: u32>(
    word: u64,
    explicit: &[[u8; 4]; GROUP],
    reps: &mut [u32; 3],
) -> ([u32; GROUP], usize) {
    let codes = offset_codes(word);
    let fresh = |index: usize| u32::from_le_bytes(explicit[index]) << BITS;
    if codes == 0xff {
        // Four explicit offsets: the common case of unrelated matches.
        let offsets = [fresh(0), fresh(1), fresh(2), fresh(3)];
        *reps = [offsets[3], offsets[2], offsets[1]];
        return (offsets, GROUP);
    }
    let plan = OFFSET_PLANS[codes];
    let [r0, r1, r2] = *reps;
    let candidates = [r0, r1, r2, fresh(0), fresh(1), fresh(2), fresh(3), 0];
    let pick = |shift: u32| candidates[(plan >> shift) as usize & 7];
    *reps = [pick(12), pick(15), pick(18)];
    (
        [pick(0), pick(3), pick(6), pick(9)],
        (plan >> 21) as usize & 7,
    )
}

/// The longest run, in bytes, the short-group path copies as one block.
const UNIT: usize = 16;
/// Output bytes a short group writes at most, scratch included.
const SHORT_OUT: usize = GROUP * 2 * UNIT;
/// Literal bytes a short group reads at most.
const SHORT_LITERALS: usize = GROUP * UNIT;

/// Copies the four tokens of `word`, into
/// the window after `pos` with one block per run. Returns the output and
/// literal bytes they take, or `None` when a match source is not entirely
/// before `pos` (the caller redoes the group with the general path, which
/// writes the same bytes and decides on validity).
#[inline(always)]
fn short_group<const BITS: u32>(
    buf: &mut [u8],
    literals: &[u8],
    pos: usize,
    resolved: &[u32; GROUP],
    word: u64,
) -> Option<(usize, usize)> {
    // The last start of a source block that ends before `pos`.
    let last_source = pos.checked_sub(UNIT)?;
    let (head, tail) = buf.split_at_mut(pos);
    let window = tail.first_chunk_mut::<SHORT_OUT>()?;
    let source = literals.first_chunk::<SHORT_LITERALS>()?;
    let mut out = 0;
    let mut used = 0;
    macro_rules! token {
        ($u:expr) => {{
            let token = (word >> (16 * $u)) as usize;
            let literal_len = ((token >> OFFSET_BITS) & ((1 << LITERAL_BITS) - 1)) << BITS;
            let match_len = (((token >> (OFFSET_BITS + LITERAL_BITS)) & ((1 << MATCH_BITS) - 1))
                + Shape::<BITS>::MIN as usize)
                << BITS;
            // A longer run, or an extra length (code 15 is always longer
            // than the limit), takes the general path.
            if literal_len > Shape::<BITS>::SHORT_LITERAL || match_len > Shape::<BITS>::SHORT_MATCH
            {
                return None;
            }
            window[out..out + UNIT].copy_from_slice(&source[used..used + UNIT]);
            out += literal_len;
            used += literal_len;
            // The source block must lie entirely before the window; an
            // offset too large wraps past `last_source`.
            let start = (pos + out).wrapping_sub(resolved[$u] as usize);
            if start > last_source {
                return None;
            }
            window[out..out + UNIT].copy_from_slice(&head[start..start + UNIT]);
            out += match_len;
        }};
    }
    token!(0);
    token!(1);
    token!(2);
    token!(3);
    Some((out, used))
}

impl Fields<'_> {
    /// Appends the decoded fields to the empty `out`.
    fn expand(mut self, out: &mut Vec<u8>) -> Result<(), DecodeError> {
        // The output is written in place; bytes past `pos` are scratch.
        out.resize(self.limit, 0);
        let buf = out.as_mut_slice();
        let mut walk = Walk {
            pos: 0,
            used: 0,
            reps: [1u32 << self.bits, 2u32 << self.bits, 4u32 << self.bits],
        };
        match self.bits {
            0 => self.groups::<0, { literal_margin(0) }>(buf, &mut walk)?,
            1 => self.groups::<1, { literal_margin(1) }>(buf, &mut walk)?,
            2 => self.groups::<2, { literal_margin(2) }>(buf, &mut walk)?,
            3 => self.groups::<3, { literal_margin(3) }>(buf, &mut walk)?,
            _ => {}
        }
        while let Some((token, rest)) = self.tokens.split_first() {
            self.tokens = rest;
            let token = u32::from(u16::from_le_bytes(*token));
            let offset = self.resolve(&mut walk.reps, token & ((1 << OFFSET_BITS) - 1))?;
            self.copy(buf, &mut walk, offset, token)?;
        }
        let literals = &self.literals[walk.used..];
        if literals.len() > self.limit - walk.pos {
            return Err(corrupt());
        }
        buf[walk.pos..walk.pos + literals.len()].copy_from_slice(literals);
        out.truncate(walk.pos + literals.len());
        if self.offsets.is_empty()
            && self.extra_literals.is_empty()
            && self.extra_matches.is_empty()
        {
            Ok(())
        } else {
            Err(corrupt())
        }
    }

    /// Decodes groups of [`GROUP`] tokens while the output, the literals
    /// and the offsets leave room for a group of tokens that carry no
    /// extra length. The group's offsets are resolved first, the way the
    /// reference does, then each token is copied with fixed-size blocks
    /// and one margin check per group. A group with an extra length
    /// finishes with the checked [`Self::copy`].
    ///
    /// `LM` is the literal margin of the group.
    #[inline(never)]
    fn groups<const BITS: u32, const LM: usize>(
        &mut self,
        buf: &mut [u8],
        walk: &mut Walk,
    ) -> Result<(), DecodeError> {
        let total = self.literals.len();
        let mut literals = &self.literals[walk.used..];
        let all = self.tokens.as_flattened().as_chunks::<8>().0;
        let mut offsets = self.offsets.0;
        let mut pos = walk.pos;
        let mut reps = walk.reps;
        let mut done = 0;
        let result = 'groups: loop {
            let Some(group) = all.get(done) else {
                break Ok(());
            };
            let Some(explicit) = offsets.first_chunk::<GROUP>() else {
                break Ok(());
            };
            let Some(window) = literals.first_chunk::<LM>() else {
                break Ok(());
            };
            if buf.len() - pos < Shape::<BITS>::OUT_MARGIN {
                break Ok(());
            }
            done += 1;
            let word = u64::from_le_bytes(*group);
            let (resolved, fresh) = resolve_group::<BITS>(word, explicit, &mut reps);
            offsets = &offsets[fresh..];
            if let Some((out, taken)) = short_group::<BITS>(buf, literals, pos, &resolved, word) {
                pos += out;
                literals = &literals[taken..];
                continue;
            }
            let mut used = 0;
            // The extra lengths of a token end the fast path for the rest
            // of the group, which the checked copy finishes.
            macro_rules! token {
                ($u:expr) => {{
                    let token = (word >> (16 * $u)) as u32 & 0xffff;
                    let literal_code = (token >> OFFSET_BITS) & ((1 << LITERAL_BITS) - 1);
                    let match_code =
                        (token >> (OFFSET_BITS + LITERAL_BITS)) & ((1 << MATCH_BITS) - 1);
                    if literal_code == MAX_CODE || match_code == MAX_CODE {
                        literals = &literals[used..];
                        self.offsets.0 = offsets;
                        let mut tail = Walk {
                            pos,
                            used: total - literals.len(),
                            reps,
                        };
                        self.finish(buf, &mut tail, &resolved, word, $u)?;
                        pos = tail.pos;
                        literals = &self.literals[tail.used..];
                        offsets = self.offsets.0;
                        continue 'groups;
                    }
                    if !fast_token::<BITS, LM>(
                        buf,
                        window,
                        &mut pos,
                        &mut used,
                        resolved[$u],
                        token,
                    ) {
                        break 'groups Err(corrupt());
                    }
                }};
            }
            token!(0);
            token!(1);
            token!(2);
            token!(3);
            literals = &literals[used..];
        };
        self.tokens = &self.tokens[GROUP * done..];
        self.offsets.0 = offsets;
        *walk = Walk {
            pos,
            used: total - literals.len(),
            reps,
        };
        result
    }

    /// The tokens of a group from `first` on, with every check.
    #[inline(never)]
    fn finish(
        &mut self,
        buf: &mut [u8],
        walk: &mut Walk,
        resolved: &[u32; GROUP],
        word: u64,
        first: usize,
    ) -> Result<(), DecodeError> {
        for (index, &offset) in resolved.iter().enumerate().skip(first) {
            let token = (word >> (16 * index)) as u32 & 0xffff;
            self.copy(buf, walk, offset, token)?;
        }
        Ok(())
    }

    /// The offset of a token with offset code `code`, and the new repeat
    /// offsets.
    fn resolve(&mut self, reps: &mut [u32; 3], code: u32) -> Result<u32, DecodeError> {
        Ok(match code {
            3 => {
                let offset = self.offsets.next()? << self.bits;
                *reps = [offset, reps[0], reps[1]];
                offset
            }
            0 => reps[0],
            1 => {
                *reps = [reps[1], reps[0], reps[2]];
                reps[0]
            }
            _ => {
                *reps = [reps[2], reps[0], reps[1]];
                reps[0]
            }
        })
    }

    /// Copies the literals and the match of one token with every check.
    fn copy(
        &mut self,
        buf: &mut [u8],
        walk: &mut Walk,
        offset: u32,
        token: u32,
    ) -> Result<(), DecodeError> {
        let bits = self.bits;
        let literal_code = (token >> OFFSET_BITS) & ((1 << LITERAL_BITS) - 1);
        let match_code = (token >> (OFFSET_BITS + LITERAL_BITS)) & ((1 << MATCH_BITS) - 1);
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

        let room = self.limit - walk.pos;
        if literal_len.saturating_add(match_len) > room {
            return Err(corrupt());
        }
        let literals = &self.literals[walk.used..];
        if literal_len > literals.len() {
            return Err(corrupt());
        }
        copy_literals(buf, walk.pos, literals, literal_len);
        walk.used += literal_len;
        walk.pos += literal_len;
        let offset = offset as usize;
        // Offset 0 reads bytes not written yet in the reference.
        if offset == 0 || offset > walk.pos {
            return Err(corrupt());
        }
        copy_match(buf, walk.pos, offset, match_len);
        walk.pos += match_len;
        Ok(())
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

    /// A deterministic stream of numbers.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, bound: u32) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) % u64::from(bound)) as u32
        }
    }

    /// A valid stream of `tokens` tokens over fields of `width` bytes and the
    /// bytes the reference decoder (byte by byte) writes for it.
    fn random_stream(width: usize, tokens: usize, seed: u64) -> (Block<'static>, Vec<u8>) {
        random_stream_with(width, tokens, seed, Mix::Mixed)
    }

    /// The length statistics of a random stream.
    #[derive(Clone, Copy, Debug)]
    enum Mix {
        /// Extra lengths, long runs and near offsets.
        Mixed,
        /// Short runs, no extra lengths, any offset.
        Short,
        /// Short runs and offsets of a few fields.
        Near,
    }

    fn random_stream_with(
        width: usize,
        tokens: usize,
        seed: u64,
        mix: Mix,
    ) -> (Block<'static>, Vec<u8>) {
        let bits = width.trailing_zeros();
        let min_match = match width {
            1 => 4,
            2 => 2,
            _ => 1,
        };
        let mut rng = Lcg(seed);
        let mut block = Block {
            width,
            ..Block::default()
        };
        let mut expected: Vec<u8> = Vec::new();
        let mut reps = [1usize << bits, 2 << bits, 4 << bits];
        for _ in 0..tokens {
            // Mostly short runs, some saturated (extra lengths) and a few long.
            let mut literal_code = match (mix, rng.next(10)) {
                (Mix::Mixed, 0) => 15,
                (Mix::Mixed, 1) => rng.next(15),
                _ => rng.next(4),
            };
            if expected.is_empty() {
                // The first match needs bytes to copy.
                literal_code = 8;
            }
            let match_code = match (mix, rng.next(10)) {
                (Mix::Mixed, 0) => 15,
                (Mix::Mixed, 1) => rng.next(15),
                (Mix::Mixed, _) => rng.next(5),
                _ => rng.next(3),
            };
            let literal_fields = if literal_code == 15 {
                let extra = if rng.next(4) == 0 {
                    rng.next(40)
                } else {
                    rng.next(3)
                };
                block.extra_literals.push(extra);
                15 + extra as usize
            } else {
                literal_code as usize
            };
            let match_fields = if match_code == 15 {
                let extra = if rng.next(4) == 0 {
                    rng.next(40)
                } else {
                    rng.next(3)
                };
                block.extra_matches.push(extra);
                15 + min_match + extra as usize
            } else {
                match_code as usize + min_match
            };
            let literal_len = literal_fields << bits;
            let match_len = match_fields << bits;
            for _ in 0..literal_len {
                let byte = rng.next(256) as u8;
                block.literals.push(byte);
                expected.push(byte);
            }
            // A repeat offset when it is valid, else an explicit one.
            let code = rng.next(4) as usize;
            let usable = |offset: usize| offset != 0 && offset <= expected.len();
            let code = if code == 3 || !usable(reps[code.min(2)]) {
                3
            } else {
                code
            };
            let offset = if code == 3 {
                // Small offsets (below one block) and large ones.
                let near = match mix {
                    Mix::Mixed => rng.next(3) == 0,
                    Mix::Short => false,
                    Mix::Near => true,
                };
                let fields = if near {
                    1 + rng.next(5.min((expected.len() >> bits) as u32)) as usize
                } else {
                    1 + rng.next((expected.len() >> bits) as u32) as usize
                };
                let offset = fields << bits;
                block.offsets.push((offset >> bits) as u32);
                reps = [offset, reps[0], reps[1]];
                offset
            } else {
                let offset = reps[code];
                reps = match code {
                    0 => reps,
                    1 => [reps[1], reps[0], reps[2]],
                    _ => [reps[2], reps[0], reps[1]],
                };
                offset
            };
            block
                .tokens
                .push(token(code as u16, literal_code as u16, match_code as u16));
            for _ in 0..match_len {
                let byte = expected[expected.len() - offset];
                expected.push(byte);
            }
        }
        // Literals after the last token.
        for _ in 0..(rng.next(5) as usize) << bits {
            let byte = rng.next(256) as u8;
            block.literals.push(byte);
            expected.push(byte);
        }
        (block, expected)
    }

    #[test]
    fn random_streams_decode_like_the_byte_by_byte_reference() {
        let tokens = if cfg!(miri) { 40 } else { 600 };
        for width in [1usize, 2, 4, 8, 16] {
            for seed in 0..(if cfg!(miri) { 2 } else { 6 }) {
                let (block, expected) = random_stream(width, tokens, seed * 7919 + width as u64);
                let fields = expected.len() / width;
                let mut header = Vec::new();
                let mut value = fields as u64;
                loop {
                    let byte = (value & 0x7f) as u8;
                    value >>= 7;
                    if value == 0 {
                        header.push(byte);
                        break;
                    }
                    header.push(byte | 0x80);
                }
                let block = Block {
                    header: &header,
                    ..block
                };
                assert_eq!(
                    block.run(expected.len() + 64).as_deref(),
                    Ok(expected.as_slice()),
                    "width {width}, seed {seed}"
                );
            }
        }
    }

    #[test]
    fn random_streams_with_a_corrupted_offset_are_rejected() {
        let tokens = if cfg!(miri) { 40 } else { 600 };
        for width in [1usize, 4, 8] {
            let (mut block, expected) = random_stream(width, tokens, 99 + width as u64);
            let mut header = Vec::new();
            let mut value = (expected.len() / width) as u64;
            loop {
                let byte = (value & 0x7f) as u8;
                value >>= 7;
                if value == 0 {
                    header.push(byte);
                    break;
                }
                header.push(byte | 0x80);
            }
            // The last explicit offset reaches before the start of the output.
            let last = block.offsets.len() - 1;
            block.offsets[last] = u32::MAX >> 4;
            let block = Block {
                header: &header,
                ..block
            };
            assert_eq!(
                block.run(expected.len() + 64),
                Err(corrupt()),
                "width {width}"
            );
        }
    }

    /// The decoder of the reference's checked loop, byte by byte.
    fn reference(block: &Block<'_>, fields: usize) -> Option<Vec<u8>> {
        let width = block.width;
        let bits = width.trailing_zeros();
        let limit = fields * width;
        if block.tokens.is_empty() {
            return (block.literals.len() <= limit).then_some(block.literals.clone());
        }
        let (mut offsets, mut extra_literals, mut extra_matches) = (
            block.offsets.iter(),
            block.extra_literals.iter(),
            block.extra_matches.iter(),
        );
        let mut reps = [1u32 << bits, 2 << bits, 4 << bits];
        let mut out: Vec<u8> = Vec::new();
        let mut literals = block.literals.as_slice();
        for &token in &block.tokens {
            let token = u32::from(token);
            let offset = match token & 3 {
                3 => {
                    let offset = *offsets.next()? << bits;
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
            let mut literal_len = (token >> 2) & 15;
            if literal_len == 15 {
                literal_len = literal_len.wrapping_add(*extra_literals.next()?);
            }
            let mut match_len = ((token >> 6) & 15) + min_match(bits);
            if (token >> 6) & 15 == 15 {
                match_len = match_len.wrapping_add(*extra_matches.next()?);
            }
            let (literal_len, match_len) =
                ((literal_len << bits) as usize, (match_len << bits) as usize);
            if literal_len.saturating_add(match_len) > limit - out.len()
                || literal_len > literals.len()
            {
                return None;
            }
            out.extend_from_slice(&literals[..literal_len]);
            literals = &literals[literal_len..];
            if offset == 0 || offset as usize > out.len() {
                return None;
            }
            for _ in 0..match_len {
                out.push(out[out.len() - offset as usize]);
            }
        }
        if literals.len() > limit - out.len() {
            return None;
        }
        out.extend_from_slice(literals);
        let drained = offsets.next().is_none()
            && extra_literals.next().is_none()
            && extra_matches.next().is_none();
        (drained && out.len().is_multiple_of(width)).then_some(out)
    }

    /// The header of a block of `fields` fields.
    fn header_of(fields: usize) -> Vec<u8> {
        let mut header = Vec::new();
        let mut value = fields as u64;
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                header.push(byte);
                return header;
            }
            header.push(byte | 0x80);
        }
    }

    #[test]
    fn compile_time_tables_match_a_runtime_build() {
        // The tables are built in const context; building them again at run
        // time checks the const fns against their compiled use.
        assert_eq!(core::hint::black_box(offset_plans()), OFFSET_PLANS);
        for bits in 0..4 {
            let margin = literal_margin(core::hint::black_box(bits));
            assert_eq!(margin % BLOCK, 0);
            assert!(margin >= GROUP * (14 << bits));
        }
    }

    #[test]
    fn offset_plans_resolve_every_code_pattern_like_the_sequential_rule() {
        let explicit = [[10, 0, 0, 0], [20, 0, 0, 0], [30, 0, 0, 0], [40, 0, 0, 0]];
        for codes in 0..256usize {
            let word = (0..4).fold(0u64, |word, token| {
                // Other bits of the token must not matter.
                word | ((((codes >> (2 * token)) & 3) as u64 | 0xfffc) << (16 * token))
            });
            assert_eq!(offset_codes(word), codes);
            let mut reps = [5u32, 6, 7];
            let (offsets, fresh) = resolve_group::<2>(word, &explicit, &mut reps);
            let mut expected_reps = [5u32, 6, 7];
            let mut used = 0;
            for (token, &offset) in offsets.iter().enumerate() {
                let code = (codes >> (2 * token)) & 3;
                let want = match code {
                    3 => {
                        used += 1;
                        let offset = (used * 10) << 2;
                        expected_reps = [offset, expected_reps[0], expected_reps[1]];
                        offset
                    }
                    0 => expected_reps[0],
                    1 => {
                        expected_reps = [expected_reps[1], expected_reps[0], expected_reps[2]];
                        expected_reps[0]
                    }
                    _ => {
                        expected_reps = [expected_reps[2], expected_reps[0], expected_reps[1]];
                        expected_reps[0]
                    }
                };
                assert_eq!(offset, want, "codes {codes:#x}, token {token}");
            }
            assert_eq!(
                (reps, fresh),
                (expected_reps, used as usize),
                "codes {codes:#x}"
            );
        }
    }

    #[test]
    fn streams_of_short_runs_take_the_block_paths() {
        let tokens = if cfg!(miri) { 80 } else { 3000 };
        for width in [1usize, 2, 4, 8] {
            for mix in [Mix::Short, Mix::Near, Mix::Mixed] {
                let (block, expected) = random_stream_with(width, tokens, 17 + width as u64, mix);
                let header = header_of(expected.len() / width);
                let block = Block {
                    header: &header,
                    ..block
                };
                assert_eq!(
                    block.run(expected.len() + 64).as_deref(),
                    Ok(expected.as_slice()),
                    "width {width}, {mix:?}"
                );
            }
        }
    }

    #[test]
    fn mutated_streams_agree_with_the_byte_by_byte_reference() {
        let tokens = if cfg!(miri) { 60 } else { 400 };
        let rounds = if cfg!(miri) { 6 } else { 60 };
        let mut rng = Lcg(0x5eed);
        for width in [1usize, 2, 4, 8] {
            for mix in [Mix::Short, Mix::Near, Mix::Mixed] {
                for round in 0..rounds {
                    let (mut block, expected) =
                        random_stream_with(width, tokens, round * 31 + width as u64, mix);
                    let mut fields = expected.len() / width;
                    match rng.next(6) {
                        0 => {
                            let at = rng.next(block.tokens.len() as u32) as usize;
                            block.tokens[at] = rng.next(1 << 10) as u16;
                        }
                        1 if !block.offsets.is_empty() => {
                            let at = rng.next(block.offsets.len() as u32) as usize;
                            block.offsets[at] = [0, 1, 7, u32::MAX >> 3][rng.next(4) as usize];
                        }
                        2 => fields = fields.saturating_sub(1 + rng.next(8) as usize),
                        3 => {
                            let keep = rng.next(block.literals.len() as u32) as usize;
                            block.literals.truncate(keep);
                        }
                        4 => {
                            block
                                .tokens
                                .truncate(rng.next(block.tokens.len() as u32) as usize);
                        }
                        _ => {}
                    }
                    let header = header_of(fields);
                    let block = Block {
                        header: &header,
                        ..block
                    };
                    assert_eq!(
                        block.run(1 << 20).ok(),
                        reference(&block, fields),
                        "width {width}, {mix:?}, round {round}"
                    );
                }
            }
        }
    }
}
