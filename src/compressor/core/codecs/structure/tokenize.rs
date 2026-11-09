//! The tokenize nodes (`encode_tokenize_binding.c`, `encode_tokenize_kernel.c`):
//! `tokenize_struct` 40, `tokenize_numeric` 41, `tokenize_string` 42 and the
//! private sorted variants `tokenize_sorted` 72 and `tokenize_string_sorted` 73.
//!
//! A tokenize node splits its input into an *alphabet* (the distinct values)
//! and an *index* stream (the alphabet position of every input element).
//! Without the sort parameter the alphabet follows first occurrence; with it
//! (`ZL_TOKENIZE_SORT_PID`, always set on the private sorted nodes) numerics
//! are sorted ascending and strings lexicographically. Indices use the
//! smallest width that holds the alphabet size.
//!
//! The reference's width-specific kernels (`TOK2_*`, `TOK4_*`, `TOKVarto4`)
//! and its `pdqsort` write exactly the stream a first-occurrence map and an
//! ascending sort of distinct keys produce, so a single implementation covers
//! them. Only the container differs: the reference hashes with XXH3, this
//! port orders keys in a `BTreeMap`; the map is only ever used for lookups,
//! so its iteration order never reaches the output.
//!
//! The custom tokenizer callback (`ZL_TOKENIZE_TOKENIZER_PID`) is not
//! supported and fails the node.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::compressor::core::codecs::FnEncoder;
use super::util::{push_le, read_le};
use crate::compressor::core::data::{Stream, Type};
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;

/// `ZL_TOKENIZE_SORT_PID`.
pub(crate) const SORT_PID: i32 = crate::graph::params::tokenize::SORT;
/// `ZL_TOKENIZE_TOKENIZER_PID`: a custom tokenizer, not supported.
const TOKENIZER_PID: i32 = 1;

/// `EI_tokenize`: `tokenize_struct`, `tokenize_numeric`, `tokenize_sorted`.
pub(crate) static TOKENIZE: FnEncoder = FnEncoder(tokenize);
/// `EI_tokenizeVSF`: `tokenize_string`, `tokenize_string_sorted`.
pub(crate) static TOKENIZE_STRING: FnEncoder = FnEncoder(tokenize_string);

/// `EI_tokenizeShouldSort`.
fn should_sort(cx: &NodeCx<'_, '_>) -> bool {
    cx.int(SORT_PID).is_some_and(|value| value != 0)
}

/// The smallest index width for an alphabet of `size` entries over elements
/// of `width` bytes, as `tokenizeImpl` picks it.
fn index_width(size: usize, elt_width: usize) -> Result<usize, NodeError> {
    if size as u64 > u64::from(u32::MAX) {
        return Err(NodeError::InvalidInput(
            "only 4 byte indices are supported",
        ));
    }
    if size <= 1 << 8 {
        Ok(1)
    } else if elt_width > 1 && size <= 1 << 16 {
        Ok(2)
    } else if elt_width > 2 {
        Ok(4)
    } else {
        Err(NodeError::InvalidInput("impossible alphabet size"))
    }
}

fn tokenize<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    if cx.copy_param(TOKENIZER_PID).is_some() {
        return Err(NodeError::UnsupportedParam("custom tokenizers"));
    }
    let sort = should_sort(cx);
    if input.ty() != Type::Numeric && sort {
        return Err(NodeError::InvalidInput(
            "sort only works on numeric inputs",
        ));
    }
    let width = input.width();
    if !matches!(width, 1 | 2 | 4 | 8) {
        return Err(NodeError::InvalidInput("element width not supported"));
    }
    let tokens: Vec<u64> = input.bytes().chunks_exact(width).map(read_le).collect();

    // The alphabet in first-occurrence order, and each token's position.
    let mut positions: BTreeMap<u64, usize> = BTreeMap::new();
    let mut alphabet: Vec<u64> = Vec::new();
    for &token in &tokens {
        positions.entry(token).or_insert_with(|| {
            alphabet.push(token);
            alphabet.len() - 1
        });
    }
    if sort {
        alphabet.sort_unstable();
        for (rank, token) in alphabet.iter().enumerate() {
            positions.insert(*token, rank);
        }
    }

    let idx_width = index_width(alphabet.len(), width)?;
    let mut alphabet_bytes = Vec::with_capacity(alphabet.len() * width);
    for &token in &alphabet {
        push_le(&mut alphabet_bytes, token, width);
    }
    cx.emit(0, width, alphabet_bytes)?;
    let mut index_bytes = Vec::with_capacity(tokens.len() * idx_width);
    for token in &tokens {
        push_le(&mut index_bytes, positions[token] as u64, idx_width);
    }
    cx.emit(1, idx_width, index_bytes)?;
    Ok(())
}

fn tokenize_string<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let sort = should_sort(cx);
    let lens = input.lens();

    let mut keys: Vec<&[u8]> = Vec::with_capacity(lens.len());
    let mut rest = input.bytes();
    for &len in lens {
        let (key, tail) = rest.split_at(len as usize);
        keys.push(key);
        rest = tail;
    }

    let mut positions: BTreeMap<&[u8], usize> = BTreeMap::new();
    let mut alphabet: Vec<&[u8]> = Vec::new();
    for &key in &keys {
        positions.entry(key).or_insert_with(|| {
            alphabet.push(key);
            alphabet.len() - 1
        });
    }
    if sort {
        // `vsfComparator`: memcmp over the common length, then the shorter
        // string first, which is the lexicographic order of byte slices.
        alphabet.sort_unstable();
        for (rank, key) in alphabet.iter().enumerate() {
            positions.insert(key, rank);
        }
    }

    let idx_width = if alphabet.len() as u64 > u64::from(u32::MAX) {
        return Err(NodeError::InvalidInput(
            "only 4 byte indices are supported",
        ));
    } else if alphabet.len() <= 1 << 8 {
        1
    } else if alphabet.len() <= 1 << 16 {
        2
    } else {
        4
    };

    let mut content = Vec::new();
    let mut alphabet_lens = Vec::with_capacity(alphabet.len());
    for key in &alphabet {
        content.extend_from_slice(key);
        alphabet_lens.push(key.len() as u32);
    }
    cx.emit_strings(0, content, alphabet_lens)?;
    let mut index_bytes = Vec::with_capacity(keys.len() * idx_width);
    for key in &keys {
        push_le(&mut index_bytes, positions[key] as u64, idx_width);
    }
    cx.emit(1, idx_width, index_bytes)?;
    Ok(())
}
