//! The `field_lz` node (`EI_fieldLz`, `encode_lz_binding.c`, with
//! `encode_field_lz.c`), node 9, codec 24.
//!
//! An LZ77 parse over fixed-width fields of 2, 4 or 8 bytes. Matches and
//! literal runs are whole fields. The node emits five streams: the literal
//! fields (struct), one 16-bit token per sequence (numeric), the offsets
//! (numeric, 32-bit), and the literal and match lengths that overflow the
//! token (numeric, 32-bit). The codec header is `varint(number of fields)`.
//!
//! A token holds a 2-bit offset kind (a repcode 0 to 2 or 3 for "new
//! offset"), a 4-bit literal length and a 4-bit match length, the latter
//! two saturating at 15 into the extra-length streams.
//!
//! The compression level (the node's override parameter 181, else the
//! global level) selects the match finder: levels up to 3 use the fast
//! finder ([`fast`], with a long hash from level 2), higher levels the
//! greedy finder ([`greedy`]). The decompression level is not read here.

mod fast;
mod greedy;
#[cfg(test)]
mod tests;

use alloc::vec::Vec;

use super::FnEncoder;
use super::lz_common::hash::load;
use super::lz_common::window::Window;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::varint;

/// The node encoder.
pub(crate) static FIELD_LZ: FnEncoder = FnEncoder(encode);

/// `ZL_FIELD_LZ_COMPRESSION_LEVEL_OVERRIDE_PID`.
const COMPRESSION_LEVEL_OVERRIDE: i32 = crate::graph::params::field_lz::COMPRESSION_LEVEL_OVERRIDE;
/// `kLzContentSizeBytes`.
const CONTENT_LIMIT: usize = 4_000_000_000;
/// `kTokenOFBits`.
const TOKEN_OF_BITS: u32 = 2;
/// `kTokenLLBits`.
const TOKEN_LL_BITS: u32 = 4;
/// `kMaxLitLengthCode`.
const MAX_LIT_LENGTH_CODE: u32 = (1 << TOKEN_LL_BITS) - 1;
/// `kMaxMatchLengthCode`.
const MAX_MATCH_LENGTH_CODE: u32 = (1 << 4) - 1;

/// `ZS_sequence`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Sequence {
    /// Literal bytes before the match.
    pub(super) literal_length: u32,
    /// A repcode index, or the offset in fields.
    pub(super) match_code: u32,
    /// Match bytes.
    pub(super) match_length: u32,
    /// `ZS_mt_rep` rather than `ZS_mt_lz`.
    pub(super) rep: bool,
}

/// `ZS_seqStore`: the literals and sequences of a parse.
#[derive(Debug, Default)]
pub(super) struct SeqStore {
    literals: Vec<u8>,
    sequences: Vec<Sequence>,
}

impl SeqStore {
    /// `ZS_seqStore_store`: the `literal_length` bytes of `src` from
    /// `anchor` and the sequence.
    pub(super) fn store(&mut self, src: &[u8], anchor: usize, seq: Sequence) {
        self.literals
            .extend_from_slice(&src[anchor..anchor + seq.literal_length as usize]);
        self.sequences.push(seq);
    }

    /// `ZS_seqStore_storeLastLiterals`: the bytes of `src` from `anchor`.
    pub(super) fn store_last_literals(&mut self, src: &[u8], anchor: usize) {
        self.literals.extend_from_slice(&src[anchor..]);
    }
}

/// `check_match`: whether `k` bytes (2, 3, 4, or 8 for any other `k`) at
/// `a` and `b` are equal. A negative position never matches.
pub(super) fn check_match(src: &[u8], a: isize, b: isize, k: u32) -> bool {
    let (Ok(a), Ok(b)) = (usize::try_from(a), usize::try_from(b)) else {
        return false;
    };
    match k {
        2 => load::<2>(src, a) == load::<2>(src, b),
        3 => load::<3>(src, a) == load::<3>(src, b),
        4 => load::<4>(src, a) == load::<4>(src, b),
        _ => load::<8>(src, a) == load::<8>(src, b),
    }
}

/// `ZS_countFields`: the bytes from `ip` that equal those from `matched`,
/// in whole fields, up to the end of `src`.
pub(super) fn count_fields(src: &[u8], ip: isize, matched: isize, field_size: u32) -> usize {
    let start = ip;
    let (mut ip, mut matched) = (ip, matched);
    let limit = src.len() as isize - field_size as isize + 1;
    while ip < limit && check_match(src, ip, matched, field_size) {
        ip += field_size as isize;
        matched += field_size as isize;
    }
    (ip - start) as usize
}

/// `kMinMatch`: the shortest match, in fields.
const fn min_match(field_size: usize) -> usize {
    match field_size {
        1 => 4,
        2 => 2,
        _ => 1,
    }
}

/// `ZL_FieldLz_maxNbSequences`.
const fn max_sequences(n_elts: usize, field_size: usize) -> usize {
    n_elts / min_match(field_size) + 1
}

/// The streams of `field_lz` besides the literals.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct Tokens {
    /// One token per sequence.
    pub(crate) tokens: Vec<u16>,
    /// The new offsets, in fields.
    pub(crate) offsets: Vec<u32>,
    /// Literal lengths that did not fit the token, minus 15.
    pub(crate) extra_literal_lengths: Vec<u32>,
    /// Match lengths that did not fit the token, minus 15.
    pub(crate) extra_match_lengths: Vec<u32>,
}

/// `resolveLevel`: the match finder parameters of `level`.
///
/// Returns `(greedy, large, table_log)`.
fn resolve_level(n_elts: usize, level: i32) -> (bool, bool, u32) {
    let level = if level <= 0 { 3 } else { level.min(5) };
    let greedy = level > 3;
    let large = level != 1;
    let table_log = match level {
        1 | 2 => 18,
        3 => 19,
        4 => 20,
        _ => 22,
    };
    let src_log = (n_elts as u32 + 1).ilog2() + 1;
    (greedy, large, table_log.min(src_log).max(10))
}

/// `writeOutSequences`: the token streams of a parse.
fn write_sequences(
    store: &SeqStore,
    field_size: usize,
    capacity: usize,
) -> Result<Tokens, NodeError> {
    const TOO_SMALL: NodeError = NodeError::InvalidInput("field_lz output buffer too small");
    let bits = field_size.ilog2();
    let minimum = min_match(field_size) as u32;
    let mut out = Tokens::default();
    for seq in &store.sequences {
        let mut token = 0u16;
        if seq.rep {
            token |= seq.match_code as u16;
        } else {
            token |= 3;
            if out.offsets.len() >= capacity {
                return Err(TOO_SMALL);
            }
            out.offsets.push(seq.match_code);
        }
        let literal_code = seq.literal_length >> bits;
        let match_code = (seq.match_length >> bits).wrapping_sub(minimum);
        if literal_code < MAX_LIT_LENGTH_CODE {
            token |= (literal_code << TOKEN_OF_BITS) as u16;
        } else {
            token |= (MAX_LIT_LENGTH_CODE << TOKEN_OF_BITS) as u16;
            if out.extra_literal_lengths.len() >= capacity {
                return Err(TOO_SMALL);
            }
            out.extra_literal_lengths
                .push(literal_code - MAX_LIT_LENGTH_CODE);
        }
        if match_code < MAX_MATCH_LENGTH_CODE {
            token |= (match_code << (TOKEN_OF_BITS + TOKEN_LL_BITS)) as u16;
        } else {
            token |= (MAX_MATCH_LENGTH_CODE << (TOKEN_OF_BITS + TOKEN_LL_BITS)) as u16;
            if out.extra_match_lengths.len() >= capacity {
                return Err(TOO_SMALL);
            }
            out.extra_match_lengths
                .push(match_code - MAX_MATCH_LENGTH_CODE);
        }
        if out.tokens.len() >= capacity {
            return Err(TOO_SMALL);
        }
        out.tokens.push(token);
    }
    Ok(out)
}

/// `ZS2_FieldLz_compress`: the literals and token streams of `src`, a whole
/// number of `field_size`-byte fields, at `level`.
///
/// # Errors
///
/// A field size that is not a power of two.
pub(crate) fn compress(
    src: &[u8],
    field_size: usize,
    level: i32,
) -> Result<(Vec<u8>, Tokens), NodeError> {
    if !field_size.is_power_of_two() {
        return Err(NodeError::InvalidParam(
            "field_lz field size is not a power of two",
        ));
    }
    let n_elts = src.len() / field_size;
    let (greedy, large, table_log) = resolve_level(n_elts, level);
    let mut window = Window::new(8);
    window.update(src.len() as u32);
    let low_limit = window.low_limit();
    let width = field_size as u32;
    let store = if greedy {
        greedy::parse(src, width, table_log, low_limit)
    } else {
        fast::parse(src, width, table_log, large, low_limit)
    };
    let tokens = write_sequences(&store, field_size, max_sequences(n_elts, field_size))?;
    Ok((store.literals, tokens))
}

/// Writes `values` as little-endian 32-bit numbers.
fn words(values: &[u32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// `EI_fieldLz`.
fn encode<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let n_elts = input.n_elts();
    let width = input.width();
    if !width.is_power_of_two() || width == 1 || width > 8 {
        return Err(NodeError::InvalidInput(
            "field_lz needs 2, 4 or 8 byte fields",
        ));
    }
    if input.content_size() > CONTENT_LIMIT {
        return Err(NodeError::InvalidInput("field_lz input is too large"));
    }
    let level = cx
        .int(COMPRESSION_LEVEL_OVERRIDE)
        .unwrap_or_else(|| cx.level());
    let (literals, tokens) = compress(input.bytes(), width, level)?;
    let mut header = Vec::new();
    varint::push(&mut header, n_elts as u64);
    cx.send_header(&header)?;
    cx.emit(0, width, literals)?;
    let token_bytes = tokens.tokens.iter().flat_map(|t| t.to_le_bytes()).collect();
    cx.emit(1, 2, token_bytes)?;
    cx.emit(2, 4, words(&tokens.offsets))?;
    cx.emit(3, 4, words(&tokens.extra_literal_lengths))?;
    cx.emit(4, 4, words(&tokens.extra_match_lengths))?;
    Ok(())
}
