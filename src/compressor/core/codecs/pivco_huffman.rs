//! The PivCo-Huffman node (`codecs/pivco_huffman/encode_pivco_binding.c`,
//! `encode_pivco_kernel.c`, `common_pivco_kernel.c`), node 87, format
//! version 27 only.
//!
//! The codec emits the Huffman weights (a numeric byte stream) and a
//! bit stream that codes the input through the Huffman tree level by level:
//! at each node of the tree the ranks of the symbols below it are
//! partitioned by the next code bit into a bitmap (and, for an internal
//! node, the count of ones), then the two halves recurse. Runs of equal-length
//! codes form flat leaves coded as plain fixed-width indices. The header is
//! the decoded size and, when the input spans several 32 KiB blocks, the
//! block size.
//!
//! The reference ships SSE/AVX2/AVX-512/NEON kernels that must match its
//! portable kernel byte for byte; this port is the portable kernel, as the
//! other kernels are by definition equivalent.

use alloc::vec::Vec;

use super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::entropy::{HISTOGRAM_PID, HUF_CTABLE_PID, huf};
use crate::shared::stats::histogram::Histogram;
use crate::shared::varint;

/// The `pivco_huffman` encoder.
pub(crate) static PIVCO_HUFFMAN: FnEncoder = FnEncoder(encode);

/// `ZL_PIVCO_DEFAULT_BLOCK_SIZE`.
const DEFAULT_BLOCK_SIZE: usize = 32 * 1024;
/// `ZL_PIVCO_MAX_SYMBOLS`.
const MAX_SYMBOLS: usize = 256;
/// `ZL_PIVCO_MAX_TABLE_LOG`.
const MAX_TABLE_LOG: u32 = 12;
/// `ZL_PIVCO_MAX_WEIGHT`.
const MAX_WEIGHT: usize = MAX_TABLE_LOG as usize + 1;
/// `ZL_PIVCO_MAX_TREE_NODES`.
const MAX_TREE_NODES: usize = 2 * MAX_SYMBOLS - 1;
/// `ZL_PIVCO_HUFFMAN_SLOP`: bytes the kernels may over-write, which the
/// writer keeps free at the end of its destination.
const SLOP: usize = 64;

/// `ZL_nextPow2`.
const fn next_pow2(upper_bound: u64) -> u32 {
    if upper_bound <= 1 {
        0
    } else {
        64 - (upper_bound - 1).leading_zeros()
    }
}

/// A leaf under construction: its symbols, a power-of-two count of them.
type Leaf = Vec<u8>;

/// The tree of one weight table (`ZL_PivCoHuffmanTree`): rank-indexed arrays
/// derived from the weights alone.
#[derive(Debug)]
struct Tree {
    symbol_to_rank: [u8; MAX_SYMBOLS],
    rank_to_symbol: [u8; MAX_SYMBOLS],
    rank_to_flat_depth: [u8; MAX_SYMBOLS],
    rank_to_codeword: [u16; MAX_SYMBOLS],
    num_ranks: usize,
}

impl Tree {
    /// `ZL_PivCoHuffmanTree_build`, or `None` when the weights are not a
    /// complete prefix code (`ZL_PivCoHuffman_computeTableLog` fails).
    fn new(weights: &[u8]) -> Option<Self> {
        if weights.is_empty() || weights.len() > MAX_SYMBOLS {
            return None;
        }
        let mut sum = 0u32;
        for &weight in weights {
            if u32::from(weight) > MAX_TABLE_LOG {
                return None;
            }
            sum += (1u32 << weight) >> 1;
        }
        if !sum.is_power_of_two() {
            return None;
        }
        let table_log = sum.trailing_zeros();
        if table_log > MAX_TABLE_LOG {
            return None;
        }
        let table_log = table_log as usize;

        // Leaves bucketed by weight: single symbols in ascending order.
        let mut buckets: [Vec<Leaf>; MAX_WEIGHT + 1] = Default::default();
        for (symbol, &weight) in weights.iter().enumerate() {
            if weight != 0 {
                buckets[usize::from(weight)].push(alloc::vec![symbol as u8]);
            }
        }
        // Collapse runs of equal-weight leaves into flat leaves, shallowest
        // weight first: peel the largest power-of-two suffix while more than
        // two remain.
        for weight in (1..=table_log).rev() {
            while buckets[weight].len() > 2 {
                let count = buckets[weight].len();
                let bits = count.ilog2() as usize;
                let split = count - (1 << bits);
                let flat: Leaf = buckets[weight]
                    .drain(split..)
                    .flat_map(Vec::into_iter)
                    .collect();
                buckets.get_mut(weight + bits)?.push(flat);
            }
        }
        let min_weight = (1..=MAX_WEIGHT).find(|&weight| !buckets[weight].is_empty())?;
        let levels = (table_log + 2).checked_sub(min_weight)?;

        let mut tree = Self {
            symbol_to_rank: [0; MAX_SYMBOLS],
            rank_to_symbol: [0; MAX_SYMBOLS],
            rank_to_flat_depth: [0; MAX_SYMBOLS],
            rank_to_codeword: [0; MAX_SYMBOLS],
            num_ranks: 0,
        };
        // Canonical codewords: consecutive per leaf, doubling per level.
        let mut codeword = 0u32;
        let mut rank = 0usize;
        for level in 0..levels {
            for leaf in &buckets[table_log + 1 - level] {
                let bits = leaf.len().ilog2() as usize;
                let total = level + bits;
                if rank + leaf.len() > MAX_SYMBOLS || total > 16 {
                    return None;
                }
                for (index, &symbol) in leaf.iter().enumerate() {
                    let word = (codeword << bits) + index as u32;
                    tree.symbol_to_rank[usize::from(symbol)] = (rank + index) as u8;
                    tree.rank_to_symbol[rank + index] = symbol;
                    tree.rank_to_flat_depth[rank + index] = bits as u8;
                    tree.rank_to_codeword[rank + index] = if total == 0 {
                        0
                    } else {
                        (word << (16 - total)) as u16
                    };
                }
                rank += leaf.len();
                codeword += 1;
            }
            codeword <<= 1;
        }
        tree.num_ranks = rank;
        (rank != 0).then_some(tree)
    }

    /// `ZL_PivCoHuffmanTree_leafFlatDepth`.
    const fn depth(&self, first: usize) -> usize {
        self.rank_to_flat_depth[first] as usize
    }

    /// `ZL_PivCoHuffmanTree_rangeIsLeaf`.
    const fn is_leaf(&self, first: usize, end: usize) -> bool {
        1usize << self.rank_to_flat_depth[first] == end - first
    }

    /// `ZL_PivCoHuffmanTree_rangeIsConstantLeaf`.
    const fn is_constant(&self, first: usize, end: usize) -> bool {
        self.is_leaf(first, end) && self.rank_to_flat_depth[first] == 0
    }

    /// `ZL_PivCoHuffmanTree_splitRank`: the first rank of the node whose
    /// codeword has bit `level` set.
    fn split(&self, level: usize, first: usize, end: usize) -> Option<usize> {
        let mask = 0x8000u16.checked_shr(level as u32)?;
        (first + 1..end).find(|&rank| self.rank_to_codeword[rank] & mask != 0)
    }
}

/// `ZL_PivCoHuffmanEncode_bound`: the output capacity that guarantees
/// success, or `None` where the reference returns `SIZE_MAX`.
fn encode_bound(src_size: usize, block_size: usize) -> Option<usize> {
    if src_size == 0 {
        return Some(0);
    }
    let block_size = src_size.min(block_size);
    let blocks = src_size / block_size + usize::from(!src_size.is_multiple_of(block_size));
    let count_bits = next_pow2(block_size as u64 + 1) as usize;
    let overhead_bits_per_node = 7 + count_bits;
    let overhead_bits_per_block = overhead_bits_per_node * MAX_TREE_NODES;
    let overhead_bits = blocks.checked_mul(overhead_bits_per_block)?;
    let overhead_bytes = overhead_bits.div_ceil(8);
    let data_bytes = src_size + src_size / 2;
    let bound = data_bytes.checked_add(overhead_bytes)?.checked_add(SLOP)?;
    (bound >= src_size).then_some(bound)
}

/// `ZS_BitCStreamFF` plus the byte-aligned bitmap regions the kernels fill
/// directly: bits are written least significant first into a buffer of
/// `cap` bytes.
#[derive(Debug)]
struct FfWriter {
    bytes: Vec<u8>,
    container: u64,
    nb_bits: usize,
    cap: usize,
}

impl FfWriter {
    fn new(cap: usize) -> Self {
        Self {
            bytes: Vec::new(),
            container: 0,
            nb_bits: 0,
            cap,
        }
    }

    /// `ZS_BitCStreamFF_write`.
    fn write(&mut self, value: u64, nb_bits: usize) {
        let mask = (1u64 << nb_bits) - 1;
        self.container |= (value & mask) << self.nb_bits;
        self.nb_bits += nb_bits;
    }

    /// `ZS_BitCStreamFF_flush`: a no-op once the pointer reaches the safety
    /// limit.
    fn flush(&mut self) {
        let limit = if self.cap < 8 { 0 } else { self.cap - 8 + 1 };
        if self.bytes.len() >= limit {
            return;
        }
        let nb_bytes = self.nb_bits >> 3;
        self.bytes
            .extend_from_slice(&self.container.to_le_bytes()[..nb_bytes]);
        self.nb_bits &= 7;
        self.container >>= nb_bytes << 3;
    }

    /// `bitWriterReserveBitmap`: a byte-aligned region of `nb_bits` bits,
    /// appended zeroed; returns its start. `None` where the reference's
    /// reservation fails or would leave less than [`SLOP`] bytes.
    fn reserve_bitmap(&mut self, nb_bits: usize) -> Option<usize> {
        // ZS_BitCStreamFF_reserveAlignedBits
        let buffered = self.nb_bits.div_ceil(8);
        let used = self.bytes.len();
        if buffered > self.cap.checked_sub(used)? {
            return None;
        }
        let nb_bytes = nb_bits.div_ceil(8);
        let available = self.cap - used - buffered;
        if nb_bytes > available {
            return None;
        }
        self.bytes
            .extend_from_slice(&self.container.to_le_bytes()[..buffered]);
        self.container = 0;
        self.nb_bits = 0;
        let start = self.bytes.len();
        // The reference's own check on the returned pointer.
        if (self.cap - start).checked_sub(nb_bytes)? < SLOP {
            return None;
        }
        self.bytes.resize(start + nb_bytes, 0);
        self.nb_bits = nb_bits & 7;
        Some(start)
    }

    /// `ZS_BitCStreamFF_commitReservedBits`: the partial byte of the region
    /// goes back into the container.
    fn commit_reserved(&mut self, start: usize, nb_bits: usize) {
        if self.nb_bits == 0 {
            self.container = 0;
            return;
        }
        let tail = start + nb_bits / 8;
        self.container = u64::from(self.bytes[tail]) & ((1u64 << self.nb_bits) - 1);
        self.bytes.truncate(tail);
    }

    /// `ZS_BitCStreamFF_finish`: the stream, or `None` when the trailing
    /// bits do not fit.
    fn finish(mut self) -> Option<Vec<u8>> {
        let to_write = self.nb_bits.div_ceil(8);
        if self.cap.checked_sub(self.bytes.len())? < to_write {
            return None;
        }
        self.bytes
            .extend_from_slice(&self.container.to_le_bytes()[..to_write]);
        Some(self.bytes)
    }
}

/// Packs `values`, each `bits` wide, least significant first.
fn pack(values: impl Iterator<Item = u64>, bits: usize, out: &mut [u8]) {
    let mut container = 0u64;
    let mut held = 0usize;
    let mut at = 0usize;
    for value in values {
        container |= (value & ((1u64 << bits) - 1)) << held;
        held += bits;
        while held >= 8 {
            out[at] = container as u8;
            at += 1;
            container >>= 8;
            held -= 8;
        }
    }
    if held > 0 {
        out[at] = container as u8;
    }
}

/// `encodeNode`: codes the node covering ranks `[first, end)` at `level`
/// from the `ranks` of the symbols below it.
fn encode_node(
    tree: &Tree,
    writer: &mut FfWriter,
    ranks: &[u8],
    level: usize,
    first: usize,
    end: usize,
) -> Option<()> {
    let num_ranks = ranks.len();
    if tree.is_leaf(first, end) {
        let depth = tree.depth(first);
        // A constant leaf emits nothing; a flat leaf packs `depth` bits per rank.
        if depth != 0 {
            let bits = num_ranks * depth;
            let start = writer.reserve_bitmap(bits)?;
            pack(
                ranks
                    .iter()
                    .map(|&rank| u64::from(rank.wrapping_sub(first as u8))),
                depth,
                &mut writer.bytes[start..start + bits.div_ceil(8)],
            );
            writer.commit_reserved(start, bits);
        }
        return Some(());
    }

    let split = tree.split(level, first, end)?;
    let lhs_const = tree.is_constant(first, split);
    let rhs_const = tree.is_constant(split, end);

    let start = writer.reserve_bitmap(num_ranks)?;
    let split_rank = split as u8;
    let mut lhs = Vec::new();
    let mut rhs = Vec::new();
    pack(
        ranks.iter().map(|&rank| u64::from(rank >= split_rank)),
        1,
        &mut writer.bytes[start..start + num_ranks.div_ceil(8)],
    );
    for &rank in ranks {
        if rank >= split_rank {
            if !rhs_const {
                rhs.push(rank);
            }
        } else if !lhs_const {
            lhs.push(rank);
        }
    }
    let num_ones = ranks.iter().filter(|&&rank| rank >= split_rank).count();
    writer.commit_reserved(start, num_ranks);

    if !(lhs_const && rhs_const) {
        // The count of ones, in as many bits as `num_ranks` needs.
        writer.write(num_ones as u64, next_pow2(num_ranks as u64 + 1) as usize);
        writer.flush();
    }

    if !lhs_const {
        encode_node(tree, writer, &lhs, level + 1, first, split)?;
    }
    if !rhs_const {
        encode_node(tree, writer, &rhs, level + 1, split, end)?;
    }
    Some(())
}

/// `ZL_PivCoHuffman_encode`: the bit stream for `src` under `weights`, or
/// `None` where the reference returns `SIZE_MAX`.
fn pivco_encode(
    dst_capacity: usize,
    weights: &[u8],
    table_log: u32,
    src: &[u8],
    block_size: usize,
) -> Option<Vec<u8>> {
    if src.is_empty() || table_log == 0 {
        return Some(Vec::new());
    }
    let tree = Tree::new(weights)?;
    let block_size = src.len().min(block_size);
    let mut writer = FfWriter::new(dst_capacity);
    for block in src.chunks(block_size) {
        let ranks: Vec<u8> = block
            .iter()
            .map(|&symbol| tree.symbol_to_rank[usize::from(symbol)])
            .collect();
        encode_node(&tree, &mut writer, &ranks, 0, 0, tree.num_ranks)?;
    }
    writer.finish()
}

/// `buildWeights`: the Huffman weights of the input and the table log.
fn build_weights(cx: &NodeCx<'_, '_>, src: &[u8]) -> Result<(Vec<u8>, u32), NodeError> {
    if src.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let owned;
    let histogram = match cx
        .ref_param(HISTOGRAM_PID)
        .and_then(|object| object.downcast_ref::<Histogram>())
    {
        Some(histogram) => histogram,
        None => {
            if u32::try_from(src.len()).is_err() {
                return Err(NodeError::InvalidInput(
                    "PivCo-Huffman input is too large to histogram",
                ));
            }
            let mut built = Histogram::new(255);
            built.build8(src);
            owned = built;
            &owned
        }
    };
    let max_symbol = histogram.max_symbol;
    let mut weights = alloc::vec![0u8; max_symbol as usize + 1];
    if histogram.cardinality == 1 {
        weights[max_symbol as usize] = 1;
        return Ok((weights, 0));
    }
    let built;
    let ctable = match cx
        .ref_param(HUF_CTABLE_PID)
        .and_then(|object| object.downcast_ref::<huf::HufCTable>())
    {
        Some(ctable) => ctable,
        None => {
            let table_log = huf::optimal_table_log(MAX_TABLE_LOG, src.len(), max_symbol);
            built = huf::build_ctable(&histogram.count, max_symbol, table_log)
                .map_err(|_| NodeError::InvalidInput("HUF_buildCTable failed"))?;
            &built
        }
    };
    let table_log = ctable.table_log();
    if table_log > MAX_TABLE_LOG {
        return Err(NodeError::InvalidInput(
            "the Huffman table log is too large",
        ));
    }
    for (symbol, weight) in weights.iter_mut().enumerate() {
        let num_bits = ctable.nb_bits(symbol);
        *weight = if num_bits == 0 {
            0
        } else {
            (table_log + 1 - num_bits) as u8
        };
    }
    Ok((weights, table_log))
}

/// `EI_pivco_huffman`.
fn encode<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let src = inputs[0].bytes();
    let (weights, table_log) = build_weights(cx, src)?;

    let block_size = DEFAULT_BLOCK_SIZE;
    let mut header = Vec::with_capacity(2 * varint::MAX_LEN);
    varint::push(&mut header, src.len() as u64);
    if block_size < src.len() {
        varint::push(&mut header, block_size as u64);
    }
    cx.send_header(&header)?;

    cx.emit(0, 1, weights.clone())?;

    let capacity = encode_bound(src.len(), block_size)
        .ok_or(NodeError::InvalidInput("PivCo-Huffman encoding failed"))?;
    let bits = pivco_encode(capacity, &weights, table_log, src, block_size)
        .ok_or(NodeError::InvalidInput("PivCo-Huffman encoding failed"))?;
    cx.emit(1, 1, bits)?;
    Ok(())
}
