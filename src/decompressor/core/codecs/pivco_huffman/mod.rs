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

mod kernels;
mod tree;

use alloc::vec::Vec;

use fearless_simd::Level;

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
        self.bitmap_and_rest(bits).map(|(bitmap, _)| bitmap)
    }

    /// [`bitmap`](Self::bitmap), and the stream from its first byte to the
    /// end: kernels that read the bitmap a vector at a time may run into the
    /// bytes behind it.
    fn bitmap_and_rest(&mut self, bits: usize) -> Result<(&'a [u8], &'a [u8]), DecodeError> {
        let aligned = self
            .position
            .checked_next_multiple_of(8)
            .ok_or_else(corrupt)?;
        let end = aligned.checked_add(bits).ok_or_else(corrupt)?;
        if end > self.data.len() * 8 {
            return Err(corrupt());
        }
        let from = self.data.get(aligned / 8..).ok_or_else(corrupt)?;
        let bytes = from
            .get(..end.div_ceil(8) - aligned / 8)
            .ok_or_else(corrupt)?;
        self.position = end;
        Ok((bytes, from))
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
pub(super) enum Part<'a> {
    /// One symbol, repeated; nothing was written.
    Constant(u8),
    /// Decoded symbols in order. A node returns it empty, since the symbols
    /// are in the buffer it was given; the parent [binds](Part::bind) the
    /// buffer.
    Vector(&'a [u8]),
}

impl<'a> Part<'a> {
    /// The same part with `data` as the symbols of a vector.
    const fn bind(self, data: &'a [u8]) -> Self {
        match self {
            Self::Constant(symbol) => Self::Constant(symbol),
            Self::Vector(_) => Self::Vector(data),
        }
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

/// Decodes subtrees: the tree, the bit cursor and the SIMD level the merge
/// kernels dispatch on.
struct Walk<'t, 'r> {
    tree: &'t Tree,
    reader: Forward<'r>,
    level: Level,
}

impl Walk<'_, '_> {
    /// Decodes the subtree over ranks `first..end` at `level` into the
    /// `count` symbols it holds (`decodeNode`).
    ///
    /// `out` is written only by this node's final merge, so before that it is
    /// free scratch for the children, which keeps the recursion
    /// allocation-free: the children's own outputs go to `scratch` and each
    /// uses `out` as its scratch. Both buffers hold at least
    /// `count + kernels::SLACK` bytes, so a kernel may read a vector's source
    /// a whole step past its last symbol. The result is [`Part::Vector`] in
    /// `out`, or a constant.
    fn node(
        &mut self,
        level: usize,
        (first, end): (usize, usize),
        count: usize,
        out: &mut [u8],
        scratch: &mut [u8],
    ) -> Result<Part<'static>, DecodeError> {
        let room = count.checked_add(kernels::SLACK).ok_or_else(corrupt)?;
        let out = out.get_mut(..room).ok_or_else(corrupt)?;
        if self.tree.is_leaf(first, end) {
            let symbols = self.tree.leaf(first);
            let depth = self.tree.depth(first);
            if depth == 0 {
                return Ok(Part::Constant(symbols[0]));
            }
            let (_, bitmap) = self
                .reader
                .bitmap_and_rest(count.checked_mul(depth).ok_or_else(corrupt)?)?;
            kernels::flat(self.level, &mut out[..count], bitmap, depth, symbols);
            return Ok(Part::Vector(&[]));
        }
        if level >= self.tree.levels() {
            return Err(corrupt());
        }
        let split = self.tree.split(level, first, end).ok_or_else(corrupt)?;
        let both_constant =
            self.tree.is_constant(first, split) && self.tree.is_constant(split, end);
        let bitmap = self.reader.bitmap(count)?;
        // The count of right-hand symbols is omitted when the bitmap alone
        // says it.
        let ones = if both_constant {
            0
        } else {
            let ones = self.reader.int(bits_for(count + 1))?;
            if ones > count {
                return Err(corrupt());
            }
            ones
        };
        let zeros = count - ones;
        let scratch = scratch.get_mut(..room).ok_or_else(corrupt)?;
        // The left child decodes into the front of `scratch`, the right one
        // behind it; each takes all of `out` from its own start as scratch.
        let left = self.node(level + 1, (first, split), zeros, scratch, out)?;
        let right = {
            let behind = scratch.get_mut(zeros..).ok_or_else(corrupt)?;
            let behind_out = out.get_mut(zeros..).ok_or_else(corrupt)?;
            self.node(level + 1, (split, end), ones, behind, behind_out)?
        };
        // Both sources extend past their symbols (into each other's, and the
        // slack), which the wide kernels read.
        let scratch = &*scratch;
        let left = left.bind(scratch);
        let right = right.bind(scratch.get(zeros..).ok_or_else(corrupt)?);
        let merged = kernels::merge(self.level, &mut out[..count], bitmap, left, right);
        // The merge draws from each source as far as the bitmap says, so the
        // bitmap must agree with the count (a source read past its symbols
        // is zeros, so a disagreeing merge is safe, only wrong).
        if !both_constant && merged != ones {
            return Err(corrupt());
        }
        Ok(Part::Vector(&[]))
    }
}

impl Walk<'_, '_> {
    /// Decodes `out` block by block and checks the stream was consumed
    /// exactly.
    ///
    /// A block is decoded straight into `out` when the rest of `out` has the
    /// slack the kernels need behind it (the blocks to come are overwritten
    /// later); the last block of the output has none, and goes through a
    /// buffer of its own.
    fn blocks(
        &mut self,
        out: &mut [u8],
        block: usize,
        scratch: &mut [u8],
    ) -> Result<(), DecodeError> {
        let ranks = self.tree.ranks();
        let mut done = 0;
        while done < out.len() {
            let tail = &mut out[done..];
            let count = block.min(tail.len());
            if tail.len() >= count + kernels::SLACK {
                match self.node(0, (0, ranks), count, tail, scratch)? {
                    Part::Constant(symbol) => tail[..count].fill(symbol),
                    Part::Vector(_) => {}
                }
            } else {
                let mut work = Vec::new();
                work.try_reserve_exact(count + kernels::SLACK)
                    .or(Err(corrupt()))?;
                work.resize(count + kernels::SLACK, 0);
                match self.node(0, (0, ranks), count, &mut work, scratch)? {
                    Part::Constant(symbol) => tail[..count].fill(symbol),
                    Part::Vector(_) => tail[..count].copy_from_slice(&work[..count]),
                }
            }
            done += count;
        }
        if self.reader.finished() {
            Ok(())
        } else {
            Err(corrupt())
        }
    }
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
    let room = block.min(size) + kernels::SLACK;
    let mut scratch = Vec::new();
    scratch.try_reserve_exact(room).or(Err(corrupt()))?;
    scratch.resize(room, 0);
    let mut walk = Walk {
        tree: &tree,
        reader: Forward::new(bitstream.bytes),
        level: ctx.state.simd,
    };
    walk.blocks(out, block, &mut scratch)
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
    fn every_level_decodes_the_same_bytes() {
        let weights = [3, 2, 2];
        for backend in Backend::available() {
            let mut state = State::new(backend);
            let mut ctx = Ctx::new(&mut state, &[4]);
            let mut regens = [Stream::default()];
            regens[0].prepare(Type::Serial, 16);
            let weights = Input {
                ty: Type::Numeric,
                width: 1,
                bytes: &weights,
                lens: &[],
            };
            decode(
                &mut ctx,
                &[weights, Input::serial(&[0x26, 0x02])],
                &mut regens,
            )
            .unwrap();
            assert_eq!(regens[0].bytes(), [0, 1, 2, 0], "{backend}");
        }
    }
}
