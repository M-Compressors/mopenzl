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
//! port hashes the string keys through a `BTreeMap` (numbers use a probing
//! table); the maps are only ever used for lookups, so their iteration order
//! never reaches the output.
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
    // The alphabet in first-occurrence order, and each element's position in it.
    let mut map = TokenMap::new();
    let mut alphabet: Vec<u64> = Vec::new();
    let mut ids: Vec<usize> = Vec::with_capacity(input.n_elts());
    for element in input.bytes().chunks_exact(width) {
        let token = read_le(element);
        ids.push(map.position(token, alphabet.len()));
        if map.len() > alphabet.len() {
            alphabet.push(token);
        }
    }
    // With the sort parameter the positions are ranks instead.
    let ranks: Option<Vec<usize>> = sort.then(|| {
        let mut order: Vec<usize> = (0..alphabet.len()).collect();
        order.sort_unstable_by_key(|&id| alphabet[id]);
        let mut ranks = alloc::vec![0usize; alphabet.len()];
        for (rank, &id) in order.iter().enumerate() {
            ranks[id] = rank;
        }
        alphabet.sort_unstable();
        ranks
    });

    let idx_width = index_width(alphabet.len(), width)?;
    let mut alphabet_bytes = Vec::with_capacity(alphabet.len() * width);
    for &token in &alphabet {
        push_le(&mut alphabet_bytes, token, width);
    }
    cx.emit(0, width, alphabet_bytes)?;
    if let Some(ranks) = &ranks {
        ids.iter_mut().for_each(|id| *id = ranks[*id]);
    }
    let mut index_bytes = Vec::with_capacity(ids.len() * idx_width);
    match idx_width {
        1 => index_bytes.extend(ids.iter().map(|&id| id as u8)),
        2 => ids
            .iter()
            .for_each(|&id| index_bytes.extend_from_slice(&(id as u16).to_le_bytes())),
        _ => ids
            .iter()
            .for_each(|&id| index_bytes.extend_from_slice(&(id as u32).to_le_bytes())),
    }
    cx.emit(1, idx_width, index_bytes)?;
    Ok(())
}

/// An open-addressing map from element values to their position in the
/// alphabet, linear probing at a load factor of at most one half.
struct TokenMap {
    keys: Vec<u64>,
    positions: Vec<usize>,
    len: usize,
    shift: u32,
}

impl TokenMap {
    const EMPTY: usize = usize::MAX;

    fn new() -> Self {
        Self::with_capacity_bits(10)
    }

    fn with_capacity_bits(bits: u32) -> Self {
        Self {
            keys: alloc::vec![0; 1 << bits],
            positions: alloc::vec![Self::EMPTY; 1 << bits],
            len: 0,
            shift: 64 - bits,
        }
    }

    /// Entries stored.
    const fn len(&self) -> usize {
        self.len
    }

    fn slot(&self, key: u64) -> usize {
        let mask = self.keys.len() - 1;
        let mut slot = (key.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> self.shift) as usize;
        while self.positions[slot] != Self::EMPTY && self.keys[slot] != key {
            slot = (slot + 1) & mask;
        }
        slot
    }

    /// The position of `key`; a key not yet in the map is stored at `next`.
    fn position(&mut self, key: u64, next: usize) -> usize {
        let mut slot = self.slot(key);
        if self.positions[slot] != Self::EMPTY {
            return self.positions[slot];
        }
        if (self.len + 1) * 2 > self.keys.len() {
            let mut grown = Self::with_capacity_bits(65 - self.shift);
            for (&key, &position) in self.keys.iter().zip(&self.positions) {
                if position != Self::EMPTY {
                    let slot = grown.slot(key);
                    grown.keys[slot] = key;
                    grown.positions[slot] = position;
                }
            }
            grown.len = self.len;
            *self = grown;
            slot = self.slot(key);
        }
        self.keys[slot] = key;
        self.positions[slot] = next;
        self.len += 1;
        next
    }
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

#[cfg(test)]
mod tests {
    use super::TokenMap;

    #[test]
    fn token_map_keeps_first_positions_across_growth() {
        let mut map = TokenMap::new();
        // Enough distinct keys to grow the table several times; key 0 and
        // colliding multiples exercise the probing.
        for key in 0..5000u64 {
            let key = key * 1024;
            assert_eq!(map.position(key, map.len()), (key / 1024) as usize);
        }
        assert_eq!(map.len(), 5000);
        for key in (0..5000u64).rev() {
            assert_eq!(map.position(key * 1024, usize::MAX - 1), key as usize);
        }
        assert_eq!(map.len(), 5000);
    }
}
