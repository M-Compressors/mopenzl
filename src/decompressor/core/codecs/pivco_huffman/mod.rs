//! The PivCo-Huffman decoder of the `pivco-huffman` feature: `pivco_huffman`
//! (67, format version 27 and later).
//!
//! Ports `codecs/pivco_huffman/decode_pivco_binding.c`,
//! `decode_pivco_kernel.c` and the portable merge kernels of
//! `arch/decode_pivco_arch.c` (the AVX2, AVX-512 and Neon kernels decode the
//! same bitmaps), with the tree of `common_pivco_kernel.c` in [`tree`]. The
//! normative description is `codecs/pivco_huffman/spec.md`.
//!
//! Rather than decode one codeword at a time, a block of symbols is decoded
//! by walking the Huffman tree breadth-first: at each internal node a bitmap
//! (and the count of ones) says which child each symbol belongs to, and the
//! decoder scatters the children's symbols back into place.
//!
//! Inputs: the 1-byte Huffman weights (numeric) and the bitstream (serial).
//! Header: `decoded_size` varint, then an optional `block_size` varint;
//! further header bytes are ignored. The bitstream is read forwards,
//! least-significant bit first; bitmaps start on byte boundaries.

mod tree;

use alloc::vec::Vec;

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;
use crate::shared::varint;
use tree::Tree;

/// `ZL_PIVCO_MAX_BLOCK_SIZE`.
const MAX_BLOCK_SIZE: usize = 1 << 28;

/// `ZL_StandardTransformID_pivco_huffman`.
pub(crate) const PIVCO_HUFFMAN: Desc = Desc {
    id: 67,
    min_version: 27,
    regens: &[Type::Serial],
    variable_regens: false,
    inputs: &[Type::Numeric, Type::Serial],
    variable_inputs: 0,
    decode,
};

/// The forward bit cursor over the bitstream (`ZS_BitDStreamFF` as the
/// kernel uses it).
#[derive(Debug)]
struct Forward<'a> {
    data: &'a [u8],
    /// Bits consumed.
    position: usize,
}

impl<'a> Forward<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    /// Skips to the next byte boundary and takes `bits` bits as a bitmap of
    /// `ceil(bits / 8)` bytes (`ZS_BitDStreamFF_popAlignedBits`).
    fn bitmap(&mut self, bits: usize) -> Result<&'a [u8], DecodeError> {
        let aligned = self
            .position
            .checked_next_multiple_of(8)
            .ok_or_else(corrupt)?;
        let end = aligned.checked_add(bits).ok_or_else(corrupt)?;
        if end > self.data.len() * 8 {
            return Err(corrupt());
        }
        let bytes = self
            .data
            .get(aligned / 8..end.div_ceil(8))
            .ok_or_else(corrupt)?;
        self.position = end;
        Ok(bytes)
    }

    /// Reads a `bits`-bit integer, least significant bit first.
    fn int(&mut self, bits: u32) -> Result<usize, DecodeError> {
        let end = self.position + bits as usize;
        if end > self.data.len() * 8 {
            return Err(corrupt());
        }
        let mut word = [0u8; 8];
        let tail = self.data.get(self.position / 8..).unwrap_or_default();
        let len = tail.len().min(8);
        word[..len].copy_from_slice(&tail[..len]);
        let value = (u64::from_le_bytes(word) >> (self.position % 8)) & ((1u64 << bits) - 1);
        self.position = end;
        Ok(value as usize)
    }

    /// Whether the stream was consumed exactly, the last byte counting whole
    /// (`ZS_BitDStreamFF_finish`).
    const fn finished(&self) -> bool {
        self.position <= self.data.len() * 8 && self.position.div_ceil(8) == self.data.len()
    }
}

/// What a decoded subtree supplies to its parent.
#[derive(Copy, Clone, Debug)]
enum Part<'a> {
    /// One symbol, repeated; nothing was written.
    Constant(u8),
    /// Decoded symbols in order.
    Vector(&'a [u8]),
}

/// A source consumed in order while merging.
struct Source<'a> {
    part: Part<'a>,
    taken: usize,
}

impl Source<'_> {
    fn next(&mut self) -> u8 {
        let value = match self.part {
            Part::Constant(symbol) => symbol,
            // Choosing a source more often than it has symbols is corruption,
            // caught by the caller's count of ones; any byte will do here.
            Part::Vector(symbols) => symbols.get(self.taken).copied().unwrap_or(0),
        };
        self.taken += 1;
        value
    }
}

/// `ceil(log2(upper_bound))`: the bits that represent `0..upper_bound`
/// (`ZL_nextPow2`).
const fn bits_for(upper_bound: usize) -> u32 {
    if upper_bound <= 1 {
        0
    } else {
        usize::BITS - (upper_bound - 1).leading_zeros()
    }
}

/// Expands a flat leaf: one `depth`-bit index per output byte into
/// `symbols` (`mergeFlatDepth`).
fn merge_flat(out: &mut [u8], bitmap: &[u8], depth: usize, symbols: &[u8]) {
    let mask = (1u64 << depth) - 1;
    let mut bytes = bitmap.iter();
    let mut accumulator = 0u64;
    let mut available = 0usize;
    for slot in out {
        while available < depth {
            accumulator |= u64::from(bytes.next().copied().unwrap_or(0)) << available;
            available += 8;
        }
        *slot = symbols
            .get((accumulator & mask) as usize)
            .copied()
            .unwrap_or(0);
        accumulator >>= depth;
        available -= depth;
    }
}

/// Interleaves two parts into `out` by `bitmap`: a set bit takes the next
/// symbol of `right`, a clear bit of `left`. Returns the number of set bits
/// (`mergeGeneric`).
fn merge(out: &mut [u8], bitmap: &[u8], left: Part<'_>, right: Part<'_>) -> usize {
    let mut left = Source {
        part: left,
        taken: 0,
    };
    let mut right = Source {
        part: right,
        taken: 0,
    };
    let mut ones = 0;
    for (chunk, &byte) in out.chunks_mut(8).zip(bitmap) {
        for (bit, slot) in chunk.iter_mut().enumerate() {
            if byte >> bit & 1 != 0 {
                ones += 1;
                *slot = right.next();
            } else {
                *slot = left.next();
            }
        }
    }
    ones
}

/// Decodes the subtree over ranks `first..end` at `level` into the
/// `out.len()` symbols it holds (`decodeNode`).
///
/// `out` is written only by this node's final merge, so before that it is
/// free scratch for the children, which keeps the recursion allocation-free:
/// the children's own outputs go to `scratch` (at least `out.len()` bytes,
/// split between them) and each uses its share of `out` as its scratch.
fn node<'o>(
    tree: &Tree,
    reader: &mut Forward<'_>,
    level: usize,
    (first, end): (usize, usize),
    out: &'o mut [u8],
    scratch: &mut [u8],
) -> Result<Part<'o>, DecodeError> {
    let count = out.len();
    if tree.is_leaf(first, end) {
        let symbols = tree.leaf(first);
        let depth = tree.depth(first);
        if depth == 0 {
            return Ok(Part::Constant(symbols[0]));
        }
        let bitmap = reader.bitmap(count.checked_mul(depth).ok_or_else(corrupt)?)?;
        merge_flat(out, bitmap, depth, symbols);
        return Ok(Part::Vector(out));
    }
    if level >= tree.levels() {
        return Err(corrupt());
    }
    let split = tree.split(level, first, end).ok_or_else(corrupt)?;
    let both_constant = tree.is_constant(first, split) && tree.is_constant(split, end);
    let bitmap = reader.bitmap(count)?;
    // The count of right-hand symbols is omitted when the bitmap alone says it.
    let ones = if both_constant {
        0
    } else {
        reader.int(bits_for(count + 1))?
    };
    if ones > count {
        return Err(corrupt());
    }
    let zeros = count - ones;
    let (left_out, right_out) = scratch
        .get_mut(..count)
        .ok_or_else(corrupt)?
        .split_at_mut(zeros);
    let (left_scratch, right_scratch) = out.split_at_mut(zeros);
    let left = node(
        tree,
        reader,
        level + 1,
        (first, split),
        left_out,
        left_scratch,
    )?;
    let right = node(
        tree,
        reader,
        level + 1,
        (split, end),
        right_out,
        right_scratch,
    )?;
    let counted = merge(out, bitmap, left, right);
    if !both_constant && counted != ones {
        return Err(corrupt());
    }
    Ok(Part::Vector(out))
}

/// The codec header: `decoded_size` and the optional `block_size`, which
/// defaults to one block (`decodeHeader`).
fn header(bytes: &[u8]) -> Result<(usize, usize), DecodeError> {
    let (decoded, used) = varint::decode(bytes).map_err(|_| corrupt())?;
    let decoded = usize::try_from(decoded).or(Err(corrupt()))?;
    let rest = &bytes[used..];
    if rest.is_empty() {
        return Ok((decoded, decoded));
    }
    let (block, _) = varint::decode(rest).map_err(|_| corrupt())?;
    let block = usize::try_from(block).or(Err(corrupt()))?;
    if block == 0 {
        return Err(corrupt());
    }
    // Bytes after the block size are reserved for extensions and ignored.
    Ok((decoded, block))
}

/// `DI_pivco_huffman`.
fn decode(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let [weights, bitstream] = inputs else {
        return Err(corrupt());
    };
    if weights.width != 1 {
        return Err(corrupt());
    }
    let (size, block) = header(ctx.header)?;
    // An empty output pairs with empty weights (and an empty bitstream).
    if (size == 0) != weights.bytes.is_empty() || weights.bytes.len() > tree::MAX_SYMBOLS {
        return Err(corrupt());
    }
    let out = regens[0].fixed(1, size)?;
    if size == 0 {
        return if bitstream.bytes.is_empty() {
            Ok(())
        } else {
            Err(corrupt())
        };
    }
    if block > MAX_BLOCK_SIZE {
        return Err(corrupt());
    }
    let tree = Tree::new(weights.bytes).ok_or_else(corrupt)?;
    out.resize(size, 0);
    let mut scratch = Vec::new();
    scratch
        .try_reserve_exact(block.min(size))
        .or(Err(corrupt()))?;
    scratch.resize(block.min(size), 0);
    let mut reader = Forward::new(bitstream.bytes);
    for chunk in out.chunks_mut(block) {
        let scratch = &mut scratch[..chunk.len()];
        let ranks = tree.ranks();
        match node(&tree, &mut reader, 0, (0, ranks), &mut *chunk, scratch)? {
            Part::Constant(symbol) => chunk.fill(symbol),
            Part::Vector(_) => {}
        }
    }
    if reader.finished() {
        Ok(())
    } else {
        Err(corrupt())
    }
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::vec;

    fn run(weights: &[u8], bits: &[u8], header: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let weights = Input {
            ty: Type::Numeric,
            width: 1,
            bytes: weights,
            lens: &[],
        };
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, header);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::Serial, 1 << 16);
        decode(&mut ctx, &[weights, Input::serial(bits)], &mut regens)?;
        regens[0].validate()?;
        Ok(regens[0].bytes().to_vec())
    }

    #[test]
    fn signature_is_the_registrys() {
        assert_eq!(PIVCO_HUFFMAN.id, 67);
        assert_eq!(PIVCO_HUFFMAN.min_version, 27);
        assert_eq!(PIVCO_HUFFMAN.inputs, [Type::Numeric, Type::Serial]);
    }

    #[test]
    fn a_node_of_two_constants_needs_only_its_bitmap() {
        // Symbols 0 and 1 with one-bit codes; bitmap 0,1,1,0.
        assert_eq!(run(&[1, 1], &[0x06], &[4]), Ok(vec![0, 1, 1, 0]));
    }

    #[test]
    fn flat_leaves_pack_one_index_per_symbol() {
        // Four equal weights: a flat root with two bits per symbol.
        assert_eq!(run(&[1, 1, 1, 1], &[0x63], &[4]), Ok(vec![3, 0, 2, 1]));
    }

    #[test]
    fn internal_nodes_send_a_count_and_recurse_left_first() {
        // Weights 3,2,2: root bitmap 0110, two ones in 3 bits, then the right
        // child's own bitmap on the next byte.
        let weights = [3, 2, 2];
        assert_eq!(run(&weights, &[0x26, 0x02], &[4]), Ok(vec![0, 1, 2, 0]));
        // A count above the block, and one that disagrees with the bitmap.
        assert_eq!(run(&weights, &[0x76, 0x02], &[4]), Err(corrupt()));
        assert_eq!(run(&weights, &[0x16, 0x02], &[4]), Err(corrupt()));
        // Missing or extra bytes.
        assert_eq!(run(&weights, &[0x26], &[4]), Err(corrupt()));
        assert_eq!(run(&weights, &[0x26, 0x02, 0], &[4]), Err(corrupt()));
    }

    #[test]
    fn blocks_restart_on_byte_boundaries() {
        // Block size 2: two blocks of a two-symbol node.
        assert_eq!(run(&[1, 1], &[0x02, 0x01], &[4, 2]), Ok(vec![0, 1, 1, 0]));
        // A trailing partial block.
        assert_eq!(run(&[1, 1], &[0x02, 0x01], &[3, 2]), Ok(vec![0, 1, 1]));
        // Extra header bytes are ignored.
        assert_eq!(
            run(&[1, 1], &[0x02, 0x01], &[4, 2, 9, 9]),
            Ok(vec![0, 1, 1, 0])
        );
    }

    #[test]
    fn a_lone_symbol_is_a_constant_root() {
        assert_eq!(run(&[0, 1], &[], &[5]), Ok(vec![1; 5]));
        // It still reads no bits, so a stray byte is corruption.
        assert_eq!(run(&[0, 1], &[0], &[5]), Err(corrupt()));
    }

    #[test]
    fn empty_outputs_pair_with_empty_inputs() {
        assert_eq!(run(&[], &[], &[0]), Ok(vec![]));
        assert_eq!(run(&[], &[0], &[0]), Err(corrupt()));
        assert_eq!(run(&[1, 1], &[], &[0]), Err(corrupt()));
        assert_eq!(run(&[], &[], &[3]), Err(corrupt()));
    }

    #[test]
    fn malformed_headers_and_weights_are_rejected() {
        let bits = [0x06];
        assert_eq!(run(&[1, 1], &bits, &[]), Err(corrupt()));
        assert_eq!(run(&[1, 1], &bits, &[0x80]), Err(corrupt()));
        assert_eq!(run(&[1, 1], &bits, &[4, 0x80]), Err(corrupt()));
        // A zero block size, and one above 2^28 (varint of 2^28 + 1).
        assert_eq!(run(&[1, 1], &bits, &[4, 0]), Err(corrupt()));
        assert_eq!(
            run(&[1, 1], &bits, &[4, 0x81, 0x80, 0x80, 0x80, 0x01]),
            Err(corrupt())
        );
        // Weights that are not a code, and too many of them.
        assert_eq!(run(&[1, 1, 1], &bits, &[4]), Err(corrupt()));
        assert_eq!(run(&[0; 257], &bits, &[4]), Err(corrupt()));
        // A width-2 weights stream, and the wrong number of inputs.
        let wide = Input {
            ty: Type::Numeric,
            width: 2,
            bytes: &[1, 0, 1, 0],
            lens: &[],
        };
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[4]);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::Serial, 64);
        assert_eq!(
            decode(&mut ctx, &[wide, Input::serial(&bits)], &mut regens),
            Err(corrupt())
        );
        assert_eq!(decode(&mut ctx, &[wide], &mut regens), Err(corrupt()));
    }

    #[test]
    fn bit_cursor_aligns_reads_and_checks_bounds() {
        let mut reader = Forward::new(&[0b1010_0101, 0xff]);
        assert_eq!(reader.int(3), Ok(0b101));
        assert_eq!(reader.bitmap(4), Ok(&[0xff][..]));
        assert_eq!(reader.position, 12);
        assert_eq!(reader.int(4), Ok(0xf));
        assert!(reader.finished());
        assert_eq!(reader.int(1), Err(corrupt()));
        assert_eq!(reader.bitmap(1), Err(corrupt()));
        assert_eq!(reader.bitmap(usize::MAX), Err(corrupt()));
        let mut empty = Forward::new(&[]);
        assert_eq!(empty.bitmap(0), Ok(&[][..]));
        assert!(empty.finished());
        assert_eq!(bits_for(0), 0);
        assert_eq!(bits_for(1), 0);
        assert_eq!(bits_for(2), 1);
        assert_eq!(bits_for(5), 3);
        assert_eq!(bits_for(1 << 28 | 1), 29);
    }

    #[test]
    fn merging_checks_sources_against_the_bitmap() {
        let mut out = [0u8; 4];
        // Choosing the right source more often than it holds symbols.
        let ones = merge(&mut out, &[0x0f], Part::Constant(7), Part::Vector(&[1]));
        assert_eq!(ones, 4);
        assert_eq!(out, [1, 0, 0, 0]);
        merge(&mut out, &[0x00], Part::Vector(&[5, 6]), Part::Constant(0));
        assert_eq!(out, [5, 6, 0, 0]);
    }
}
