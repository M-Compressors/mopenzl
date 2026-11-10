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

use fearless_simd::{Bytes, Level, Simd, SimdBase, dispatch, u8x16, u8x64, u16x8, u32x4, u64x2};

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

/// Most leaves one weight bucket can hold: two remaining symbols and a flat
/// leaf for each of the other weights.
const MAX_RUNS: usize = 16;

/// A leaf under construction: a run of the symbols grouped by weight, of a
/// power-of-two length.
#[derive(Copy, Clone, Default)]
struct Run {
    start: usize,
    len: usize,
}

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
        let mut counts = [0usize; MAX_WEIGHT + 1];
        for &weight in weights {
            if u32::from(weight) > MAX_TABLE_LOG {
                return None;
            }
            sum += (1u32 << weight) >> 1;
            counts[usize::from(weight)] += 1;
        }
        if !sum.is_power_of_two() {
            return None;
        }
        let table_log = sum.trailing_zeros();
        if table_log > MAX_TABLE_LOG {
            return None;
        }
        let table_log = table_log as usize;

        // The symbols grouped by weight, ascending within a weight: every
        // leaf below is a contiguous run of this array.
        let mut next = [0usize; MAX_WEIGHT + 1];
        let mut taken = 0;
        for weight in 1..=MAX_WEIGHT {
            next[weight] = taken;
            taken += counts[weight];
        }
        let first = next;
        let mut grouped = [0u8; MAX_SYMBOLS];
        for (symbol, &weight) in weights.iter().enumerate() {
            if weight != 0 {
                let at = &mut next[usize::from(weight)];
                grouped[*at] = symbol as u8;
                *at += 1;
            }
        }

        // Leaves bucketed by weight. Collapse runs of equal-weight symbols
        // into flat leaves, shallowest weight first: peel the largest
        // power-of-two suffix while more than two remain. A flat leaf lands
        // in a bucket of larger weight, already processed, so it is never
        // flattened again and a bucket holds its own remaining symbols
        // first, then the flat leaves in the order they arrived.
        let mut buckets = [[Run::default(); MAX_RUNS]; MAX_WEIGHT + 1];
        let mut sizes = [0usize; MAX_WEIGHT + 1];
        for weight in (1..=MAX_WEIGHT).rev() {
            let mut len = counts[weight];
            while weight <= table_log && len > 2 {
                let bits = len.ilog2() as usize;
                let split = len - (1 << bits);
                let size = sizes.get_mut(weight + bits)?;
                *buckets[weight + bits].get_mut(*size)? = Run {
                    start: first[weight] + split,
                    len: 1 << bits,
                };
                *size += 1;
                len = split;
            }
            for single in 0..len {
                *buckets[weight].get_mut(sizes[weight])? = Run {
                    start: first[weight] + single,
                    len: 1,
                };
                sizes[weight] += 1;
            }
        }
        // The deepest non-empty bucket fixes the number of levels.
        let min_weight = (1..=MAX_WEIGHT).find(|&weight| sizes[weight] != 0)?;
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
            let weight = table_log + 1 - level;
            for run in &buckets[weight][..sizes[weight]] {
                let bits = run.len.ilog2() as usize;
                let total = level + bits;
                if rank + run.len > MAX_SYMBOLS || total > 16 {
                    return None;
                }
                for index in 0..run.len {
                    let word = (codeword << bits) + index as u32;
                    let symbol = grouped[run.start + index];
                    tree.symbol_to_rank[usize::from(symbol)] = (rank + index) as u8;
                    tree.rank_to_symbol[rank + index] = symbol;
                    tree.rank_to_flat_depth[rank + index] = bits as u8;
                    tree.rank_to_codeword[rank + index] = if total == 0 {
                        0
                    } else {
                        (word << (16 - total)) as u16
                    };
                }
                rank += run.len;
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

/// Where the right-hand ranks of a partition go.
enum Rhs<'a> {
    /// Nowhere: the right child is constant.
    Discard,
    /// Over the source, which the write position never passes.
    InPlace,
    /// Into another buffer.
    To(&'a mut [u8]),
}

/// Whether `simd` is the AVX-512 level, whose partition is 64 ranks wide.
#[inline(always)]
fn is_wide<S: Simd>(simd: S) -> bool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        matches!(simd.level(), Level::Avx512(_))
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        let _ = simd;
        false
    }
}

/// Partitions the first `num` ranks of `src` at `split`: writes the bitmap
/// (set for ranks at or above `split`), appends the left ranks to `lhs` and
/// the right ranks to `rhs` in order, and returns the count of right ranks.
/// The destinations may be written up to sixty-four bytes past the ranks they
/// receive (`partitionFull`, `partitionRight`, `partitionNone`).
fn partition(
    level: Level,
    src: &mut [u8],
    num: usize,
    split: u8,
    bitmap: &mut [u8],
    lhs: Option<&mut [u8]>,
    rhs: Rhs<'_>,
) -> usize {
    if level.is_fallback() {
        partition_scalar(src, num, split, bitmap, lhs, rhs)
    } else {
        dispatch!(level, simd => partition_simd(simd, src, num, split, bitmap, lhs, rhs))
    }
}

/// The `fearless_simd` kernel of [`partition`]: the widest the level supports.
#[inline(always)]
fn partition_simd<S: Simd>(
    simd: S,
    src: &mut [u8],
    num: usize,
    split: u8,
    bitmap: &mut [u8],
    lhs: Option<&mut [u8]>,
    rhs: Rhs<'_>,
) -> usize {
    // The specialization on "left kept" and "right destination" is a
    // const-generic parameter of the kernels.
    macro_rules! sides {
        ($kernel:ident) => {
            match (lhs, rhs) {
                (None, Rhs::Discard) => {
                    $kernel::<S, false, DISCARD>(simd, src, num, split, bitmap, &mut [], &mut [])
                }
                (Some(lhs), Rhs::Discard) => {
                    $kernel::<S, true, DISCARD>(simd, src, num, split, bitmap, lhs, &mut [])
                }
                (None, Rhs::InPlace) => {
                    $kernel::<S, false, IN_PLACE>(simd, src, num, split, bitmap, &mut [], &mut [])
                }
                (Some(lhs), Rhs::InPlace) => {
                    $kernel::<S, true, IN_PLACE>(simd, src, num, split, bitmap, lhs, &mut [])
                }
                (None, Rhs::To(rhs)) => {
                    $kernel::<S, false, OTHER>(simd, src, num, split, bitmap, &mut [], rhs)
                }
                (Some(lhs), Rhs::To(rhs)) => {
                    $kernel::<S, true, OTHER>(simd, src, num, split, bitmap, lhs, rhs)
                }
            }
        };
    }
    if is_wide(simd) {
        sides!(partition_wide)
    } else {
        sides!(partition_narrow)
    }
}

/// The scalar baseline of [`partition`]: branch-free per rank.
fn partition_scalar(
    src: &mut [u8],
    num: usize,
    split: u8,
    bitmap: &mut [u8],
    mut lhs: Option<&mut [u8]>,
    mut rhs: Rhs<'_>,
) -> usize {
    bitmap.fill(0);
    let (mut zeros, mut ones) = (0usize, 0usize);
    for index in 0..num.min(src.len()) {
        let rank = src[index];
        let bit = rank >= split;
        bitmap[index / 8] |= u8::from(bit) << (index % 8);
        if let Some(slot) = lhs.as_mut().and_then(|lhs| lhs.get_mut(zeros)) {
            *slot = rank;
        }
        let slot = match &mut rhs {
            Rhs::Discard => None,
            Rhs::InPlace => src.get_mut(ones),
            Rhs::To(dst) => dst.get_mut(ones),
        };
        if let Some(slot) = slot {
            *slot = rank;
        }
        ones += usize::from(bit);
        zeros += usize::from(!bit);
    }
    ones
}

/// The shuffle controls that compact sixteen ranks into one vector laid out
/// as the right ranks ascending followed by the left ranks descending, so
/// that one store keeps the right ranks and the byte-reversed vector holds the
/// left ranks ascending at its front.
///
/// The first table is the control of the lower eight lanes by their bitmap
/// byte: its right lanes at the front, its left lanes reversed at the back,
/// `0x80` (a zeroed lane) between. The second is the control of the upper
/// eight lanes, as a row of 24 bytes that is read through a 16-byte window at
/// offset `8 - popcount(low byte)`: that slides its right lanes behind the
/// lower half's and its left lanes in front of the lower half's. The two fill
/// disjoint lanes, so an unsigned minimum merges them.
const PARTITION: ([[u8; 16]; 256], [[u8; 24]; 256]) = {
    let mut low = [[0x80u8; 16]; 256];
    let mut high = [[0x80u8; 24]; 256];
    let mut byte = 0usize;
    while byte < 256 {
        let (mut right, mut left) = (0usize, 0usize);
        let mut lane = 0usize;
        while lane < 8 {
            if (byte >> lane) & 1 != 0 {
                low[byte][right] = lane as u8;
                high[byte][8 + right] = 8 + lane as u8;
                right += 1;
            } else {
                low[byte][15 - left] = lane as u8;
                high[byte][15 - left] = 8 + lane as u8;
                left += 1;
            }
            lane += 1;
        }
        byte += 1;
    }
    (low, high)
};

/// The ranks of `chunk` selected by `bits` first, ascending, then the others
/// descending (see [`PARTITION`]).
#[inline(always)]
fn compact<S: Simd>(simd: S, chunk: u8x16<S>, bits: u16) -> u8x16<S> {
    let low = usize::from(bits as u8);
    let skip = 8 - (bits as u8).count_ones() as usize;
    let lower = u8x16::load_array_ref(simd, &PARTITION.0[low]);
    let upper = match PARTITION.1[usize::from(bits >> 8)].get(skip..) {
        Some(row) => row.first_chunk::<16>(),
        None => None,
    };
    let upper = u8x16::load_array_ref(simd, upper.unwrap_or(&[0x80; 16]));
    simd.swizzle_dyn_u8x16(chunk, simd.min_u8x16(lower, upper))
}

/// Writes `lanes` into `dst` at `at`, as far as `dst` extends.
#[inline(always)]
fn put<const N: usize>(dst: &mut [u8], at: usize, lanes: [u8; N]) {
    match dst.get_mut(at..).and_then(<[u8]>::first_chunk_mut::<N>) {
        Some(window) => *window = lanes,
        None => put_edge(dst, at, &lanes),
    }
}

/// [`put`] where fewer than `N` bytes remain.
#[cold]
#[inline(never)]
fn put_edge(dst: &mut [u8], at: usize, lanes: &[u8]) {
    if let Some(rest) = dst.get_mut(at..) {
        let len = rest.len().min(lanes.len());
        rest[..len].copy_from_slice(&lanes[..len]);
    }
}

/// The right-hand destination of [`partition_narrow`] and [`partition_wide`].
const DISCARD: u8 = 0;
/// Over the source ([`Rhs::InPlace`]).
const IN_PLACE: u8 = 1;
/// Into a buffer of its own ([`Rhs::To`]).
const OTHER: u8 = 2;

/// The 16-rank kernel of [`partition`], specialized on whether the left ranks
/// are kept (`LEFT`) and where the right ones go (`RIGHT`): one shuffle per
/// step compacts both sides.
#[inline(always)]
fn partition_narrow<S: Simd, const LEFT: bool, const RIGHT: u8>(
    simd: S,
    src: &mut [u8],
    num: usize,
    split: u8,
    bitmap: &mut [u8],
    lhs: &mut [u8],
    rhs: &mut [u8],
) -> usize {
    let limit = simd.splat_u8x16(split);
    let reverse = u8x16::load_array(simd, [15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]);
    let (mut zeros, mut ones) = (0usize, 0usize);
    let full = num / 16;
    let (pairs, _) = bitmap.as_chunks_mut::<2>();
    for (step, pair) in pairs.iter_mut().take(full).enumerate() {
        let Some(bytes) = src.get(step * 16..).and_then(<[u8]>::first_chunk::<16>) else {
            break;
        };
        let chunk = u8x16::load_array_ref(simd, bytes);
        let bits = simd.to_bitmask_mask8x16(simd.simd_ge_u8x16(chunk, limit)) as u16;
        *pair = bits.to_le_bytes();
        let kept = bits.count_ones() as usize;
        if LEFT || RIGHT != DISCARD {
            let packed = compact(simd, chunk, bits);
            match RIGHT {
                OTHER => put(rhs, ones, packed.into()),
                // Over ranks already consumed: the write position never
                // passes the read position.
                IN_PLACE => put(src, ones, packed.into()),
                _ => {}
            }
            if LEFT {
                put(lhs, zeros, simd.swizzle_dyn_u8x16(packed, reverse).into());
            }
        }
        ones += kept;
        zeros += 16 - kept;
    }
    let at = full * 16;
    let lanes = num - at;
    if lanes != 0 {
        let mut bytes = [0u8; 16];
        let rest = src.get(at..num).unwrap_or_default();
        bytes[..rest.len()].copy_from_slice(rest);
        let chunk = u8x16::load_array(simd, bytes);
        let valid = (1u16 << lanes) - 1;
        // The lanes past the end count as left: they follow the real left
        // ranks, which are all that is kept.
        let bits = simd.to_bitmask_mask8x16(simd.simd_ge_u8x16(chunk, limit)) as u16 & valid;
        let word = bits.to_le_bytes();
        let region = bitmap.get_mut(full * 2..).unwrap_or_default();
        let len = region.len().min(2);
        region[..len].copy_from_slice(&word[..len]);
        if LEFT || RIGHT != DISCARD {
            let packed = compact(simd, chunk, bits);
            match RIGHT {
                OTHER => put(rhs, ones, packed.into()),
                IN_PLACE => put(src, ones, packed.into()),
                _ => {}
            }
            if LEFT {
                put(lhs, zeros, simd.swizzle_dyn_u8x16(packed, reverse).into());
            }
        }
        ones += bits.count_ones() as usize;
    }
    ones
}

/// The 64-rank kernel of [`partition`] for AVX-512: `compress` packs the ranks
/// each mask selects at the front of a vector.
#[inline(always)]
fn partition_wide<S: Simd, const LEFT: bool, const RIGHT: u8>(
    simd: S,
    src: &mut [u8],
    num: usize,
    split: u8,
    bitmap: &mut [u8],
    lhs: &mut [u8],
    rhs: &mut [u8],
) -> usize {
    let limit = simd.splat_u8x64(split);
    let (mut zeros, mut ones) = (0usize, 0usize);
    let full = num / 64;
    let (words, _) = bitmap.as_chunks_mut::<8>();
    for (step, word) in words.iter_mut().take(full).enumerate() {
        let Some(bytes) = src.get(step * 64..).and_then(<[u8]>::first_chunk::<64>) else {
            break;
        };
        let chunk = u8x64::load_array_ref(simd, bytes);
        let bits = simd.to_bitmask_mask8x64(simd.simd_ge_u8x64(chunk, limit));
        *word = bits.to_le_bytes();
        let kept = bits.count_ones() as usize;
        match RIGHT {
            OTHER => put(
                rhs,
                ones,
                simd.compress_u8x64(chunk, simd.from_bitmask_mask8x64(bits))
                    .into(),
            ),
            // Over ranks already consumed: the write position never passes
            // the read position.
            IN_PLACE => put(
                src,
                ones,
                simd.compress_u8x64(chunk, simd.from_bitmask_mask8x64(bits))
                    .into(),
            ),
            _ => {}
        }
        if LEFT {
            put(
                lhs,
                zeros,
                simd.compress_u8x64(chunk, simd.from_bitmask_mask8x64(!bits))
                    .into(),
            );
        }
        ones += kept;
        zeros += 64 - kept;
    }
    let at = full * 64;
    let lanes = num - at;
    if lanes != 0 {
        let mut bytes = [0u8; 64];
        let rest = src.get(at..num).unwrap_or_default();
        bytes[..rest.len()].copy_from_slice(rest);
        let chunk = u8x64::load_array(simd, bytes);
        let valid = (1u64 << lanes) - 1;
        let bits = simd.to_bitmask_mask8x64(simd.simd_ge_u8x64(chunk, limit)) & valid;
        let word = bits.to_le_bytes();
        let region = bitmap.get_mut(full * 8..).unwrap_or_default();
        let len = region.len().min(8);
        region[..len].copy_from_slice(&word[..len]);
        match RIGHT {
            OTHER => put(
                rhs,
                ones,
                simd.compress_u8x64(chunk, simd.from_bitmask_mask8x64(bits))
                    .into(),
            ),
            IN_PLACE => put(
                src,
                ones,
                simd.compress_u8x64(chunk, simd.from_bitmask_mask8x64(bits))
                    .into(),
            ),
            _ => {}
        }
        if LEFT {
            put(
                lhs,
                zeros,
                simd.compress_u8x64(chunk, simd.from_bitmask_mask8x64(!bits))
                    .into(),
            );
        }
        ones += bits.count_ones() as usize;
    }
    ones
}

/// Translates `symbols` to ranks through `table` into the front of `ranks`
/// (which is at least as long). The AVX-512 level looks sixty-four symbols up
/// per step in four vectors of 64 table entries.
fn translate(level: Level, table: &[u8; MAX_SYMBOLS], symbols: &[u8], ranks: &mut [u8]) {
    dispatch!(level, simd => translate_simd(simd, table, symbols, ranks))
}

/// [`translate`] at one SIMD level.
#[inline(always)]
fn translate_simd<S: Simd>(simd: S, table: &[u8; MAX_SYMBOLS], symbols: &[u8], ranks: &mut [u8]) {
    if simd.level().is_fallback() || !is_wide(simd) {
        for (rank, &symbol) in ranks.iter_mut().zip(symbols) {
            *rank = table[usize::from(symbol)];
        }
        return;
    }
    let pages = table.as_chunks::<64>().0;
    let [first, second, third, fourth] =
        [0, 1, 2, 3].map(|page| u8x64::load_array_ref(simd, &pages[page]));
    let low = simd.splat_u8x64(0x7F);
    let high = simd.splat_u8x64(0x80);
    let look = |chunk: u8x64<S>| {
        let index = simd.and_u8x64(chunk, low);
        let lower = simd.concat_swizzle_dyn_u8x64(first, second, index);
        let upper = simd.concat_swizzle_dyn_u8x64(third, fourth, index);
        simd.select_u8x64(simd.simd_ge_u8x64(chunk, high), upper, lower)
    };
    let (blocks, tail) = symbols.as_chunks::<64>();
    let (outputs, _) = ranks.as_chunks_mut::<64>();
    for (symbols, ranks) in blocks.iter().zip(outputs.iter_mut()) {
        *ranks = look(u8x64::load_array_ref(simd, symbols)).into();
    }
    if !tail.is_empty() {
        let mut bytes = [0u8; 64];
        bytes[..tail.len()].copy_from_slice(tail);
        let found: [u8; 64] = look(u8x64::load_array(simd, bytes)).into();
        let at = blocks.len() * 64;
        put(ranks, at, found);
    }
}

/// Packs `ranks`, each minus `first`, `DEPTH` bits wide and least
/// significant first, into `out` (`packFlatDepth`).
#[inline(always)]
fn pack_depth<const DEPTH: usize>(ranks: &[u8], first: u8, out: &mut [u8]) {
    let mask = ((1u16 << DEPTH) - 1) as u8;
    let full = ranks.len() / 8;
    let (body, tail) = ranks.split_at(full * 8);
    let (packed, rest) = out.split_at_mut((full * DEPTH).min(out.len()));
    let window = |group: &[u8]| {
        let mut window = 0u64;
        for (index, &rank) in group.iter().enumerate() {
            window |= u64::from(rank.wrapping_sub(first) & mask) << (index * DEPTH);
        }
        window.to_le_bytes()
    };
    let (groups, _) = body.as_chunks::<8>();
    let (fields, _) = packed.as_chunks_mut::<DEPTH>();
    for (group, bytes) in groups.iter().zip(fields) {
        let word = window(group);
        bytes.copy_from_slice(&word[..DEPTH]);
    }
    if !tail.is_empty() {
        let word = window(tail);
        let len = rest.len().min(8);
        rest[..len].copy_from_slice(&word[..len]);
    }
}

/// Packs `ranks`, each minus `first` and `depth` bits wide, least significant
/// first into `out` (`packFlatDepth`). `depth` is 1 to 8.
fn pack(level: Level, ranks: &[u8], first: u8, depth: usize, out: &mut [u8]) {
    if level.is_fallback() {
        pack_scalar(ranks, first, depth, out)
    } else {
        dispatch!(level, simd => pack_simd(simd, ranks, first, depth, out))
    }
}

/// The scalar baseline of [`pack`].
fn pack_scalar(ranks: &[u8], first: u8, depth: usize, out: &mut [u8]) {
    match depth {
        1 => pack_depth::<1>(ranks, first, out),
        2 => pack_depth::<2>(ranks, first, out),
        3 => pack_depth::<3>(ranks, first, out),
        4 => pack_depth::<4>(ranks, first, out),
        5 => pack_depth::<5>(ranks, first, out),
        6 => pack_depth::<6>(ranks, first, out),
        7 => pack_depth::<7>(ranks, first, out),
        _ => pack_depth::<8>(ranks, first, out),
    }
}

/// The `fearless_simd` kernel of [`pack`].
#[inline(always)]
fn pack_simd<S: Simd>(simd: S, ranks: &[u8], first: u8, depth: usize, out: &mut [u8]) {
    match depth {
        1 => pack_vector::<S, 1>(simd, ranks, first, out),
        2 => pack_vector::<S, 2>(simd, ranks, first, out),
        3 => pack_vector::<S, 3>(simd, ranks, first, out),
        4 => pack_vector::<S, 4>(simd, ranks, first, out),
        5 => pack_vector::<S, 5>(simd, ranks, first, out),
        6 => pack_vector::<S, 6>(simd, ranks, first, out),
        7 => pack_vector::<S, 7>(simd, ranks, first, out),
        _ => pack_bytes(simd, ranks, first, out),
    }
}

/// [`pack_simd`] at depth 8: each field is a byte.
#[inline(always)]
fn pack_bytes<S: Simd>(simd: S, ranks: &[u8], first: u8, out: &mut [u8]) {
    let base = simd.splat_u8x16(first);
    let (blocks, tail) = ranks.as_chunks::<16>();
    for (step, block) in blocks.iter().enumerate() {
        let lanes: [u8; 16] = simd
            .sub_u8x16(u8x16::load_array_ref(simd, block), base)
            .into();
        put(out, step * 16, lanes);
    }
    if !tail.is_empty() {
        let mut bytes = [first; 16];
        bytes[..tail.len()].copy_from_slice(tail);
        let lanes: [u8; 16] = simd.sub_u8x16(u8x16::load_array(simd, bytes), base).into();
        put(out, blocks.len() * 16, lanes);
    }
}

/// The two 64-bit lanes of sixteen packed fields (see [`pack_vector`]).
#[inline(always)]
fn pack_lanes<S: Simd, const DEPTH: usize>(simd: S, ranks: u8x16<S>, first: u8) -> [u64; 2] {
    let base = simd.splat_u8x16(first);
    let field = simd.splat_u8x16(((1u16 << DEPTH) - 1) as u8);
    let (low16, high16) = (simd.splat_u16x8(0x00FF), simd.splat_u16x8(0xFF00));
    let (low32, high32) = (simd.splat_u32x4(0xFFFF), simd.splat_u32x4(0xFFFF_0000));
    let (low64, high64) = (
        simd.splat_u64x2(0xFFFF_FFFF),
        simd.splat_u64x2(0xFFFF_FFFF_0000_0000),
    );
    let fields = simd.and_u8x16(simd.sub_u8x16(ranks, base), field);
    let pairs: u16x8<S> = fields.bitcast();
    let pairs = simd.or_u16x8(
        simd.and_u16x8(pairs, low16),
        simd.shr_u16x8(simd.and_u16x8(pairs, high16), (8 - DEPTH) as u32),
    );
    let quads: u32x4<S> = pairs.bitcast();
    let quads = simd.or_u32x4(
        simd.and_u32x4(quads, low32),
        simd.shr_u32x4(simd.and_u32x4(quads, high32), (16 - 2 * DEPTH) as u32),
    );
    let octets: u64x2<S> = quads.bitcast();
    simd.or_u64x2(
        simd.and_u64x2(octets, low64),
        simd.shr_u64x2(simd.and_u64x2(octets, high64), (32 - 4 * DEPTH) as u32),
    )
    .into()
}

/// [`pack_simd`] at a depth of 1 to 7 known at compile time.
///
/// Sixteen fields are packed by merging neighbours three times: byte pairs
/// into 16-bit lanes of `2 * DEPTH` bits, those into 32-bit lanes of
/// `4 * DEPTH` bits, those into 64-bit lanes of `8 * DEPTH` bits, which is
/// `DEPTH` bytes at the bottom of each. The two lanes are stored `DEPTH` bytes
/// apart, the second over the zeros the first leaves above its bytes.
#[inline(always)]
fn pack_vector<S: Simd, const DEPTH: usize>(simd: S, ranks: &[u8], first: u8, out: &mut [u8]) {
    let kernel = |ranks: u8x16<S>| pack_lanes::<S, DEPTH>(simd, ranks, first);
    let (blocks, tail) = ranks.as_chunks::<16>();
    for (step, block) in blocks.iter().enumerate() {
        let [low, high] = kernel(u8x16::load_array_ref(simd, block));
        put(out, step * 2 * DEPTH, low.to_le_bytes());
        put(out, step * 2 * DEPTH + DEPTH, high.to_le_bytes());
    }
    if !tail.is_empty() {
        let mut bytes = [first; 16];
        bytes[..tail.len()].copy_from_slice(tail);
        let [low, high] = kernel(u8x16::load_array(simd, bytes));
        let at = blocks.len() * 2 * DEPTH;
        put(out, at, low.to_le_bytes());
        put(out, at + DEPTH, high.to_le_bytes());
    }
}

/// The tree walk of one stream: the tree, the writer and the SIMD level.
struct Walk<'t> {
    tree: &'t Tree,
    writer: FfWriter,
    level: Level,
}

impl Walk<'_> {
    /// `encodeNode`: codes the node covering ranks `[first, end)` at `level`
    /// from the first `num` ranks of `ranks`, the ranks of the symbols below
    /// it. `scratch` is working space; both buffers hold at least `num` and
    /// sixteen spare bytes, and are consumed (the children reuse them without
    /// overlapping).
    fn node(
        &mut self,
        ranks: &mut [u8],
        scratch: &mut [u8],
        num: usize,
        level: usize,
        (first, end): (usize, usize),
    ) -> Option<()> {
        let tree = self.tree;
        if tree.is_leaf(first, end) {
            let depth = tree.depth(first);
            // A constant leaf emits nothing; a flat leaf packs `depth` bits
            // per rank.
            if depth != 0 {
                let bits = num * depth;
                let start = self.writer.reserve_bitmap(bits)?;
                let region = self.writer.bytes.get_mut(start..start + bits.div_ceil(8))?;
                pack(self.level, ranks.get(..num)?, first as u8, depth, region);
                self.writer.commit_reserved(start, bits);
            }
            return Some(());
        }

        let split = tree.split(level, first, end)?;
        let lhs_const = tree.is_constant(first, split);
        let rhs_const = tree.is_constant(split, end);

        let start = self.writer.reserve_bitmap(num)?;
        let bitmap = self.writer.bytes.get_mut(start..start + num.div_ceil(8))?;
        let (lhs, rhs) = match (lhs_const, rhs_const) {
            (true, true) => (None, Rhs::Discard),
            (false, true) => (Some(&mut *scratch), Rhs::Discard),
            (true, false) => (None, Rhs::To(&mut *scratch)),
            (false, false) => (Some(&mut *scratch), Rhs::InPlace),
        };
        let ones = partition(self.level, ranks, num, split as u8, bitmap, lhs, rhs);
        self.writer.commit_reserved(start, num);

        if !(lhs_const && rhs_const) {
            // The count of ones, in as many bits as `num` needs.
            self.writer
                .write(ones as u64, next_pow2(num as u64 + 1) as usize);
            self.writer.flush();
        }

        let zeros = num - ones;
        if !lhs_const {
            // The left ranks are in `scratch`; the right ones, if kept, are
            // at the front of `ranks`.
            let spare = if rhs_const {
                &mut *ranks
            } else {
                ranks.get_mut(ones..)?
            };
            self.node(scratch, spare, zeros, level + 1, (first, split))?;
        }
        if !rhs_const {
            if lhs_const {
                // The right ranks are in `scratch`.
                self.node(scratch, ranks, ones, level + 1, (split, end))?;
            } else {
                self.node(
                    ranks,
                    scratch.get_mut(zeros..)?,
                    ones,
                    level + 1,
                    (split, end),
                )?;
            }
        }
        Some(())
    }
}

/// `ZL_PivCoHuffman_encode`: the bit stream for `src` under `weights`, or
/// `None` where the reference returns `SIZE_MAX`.
fn pivco_encode(
    level: Level,
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
    let mut ranks = alloc::vec![0u8; block_size + SLOP];
    let mut scratch = alloc::vec![0u8; block_size + SLOP];
    Walk {
        tree: &tree,
        writer: FfWriter::new(dst_capacity),
        level,
    }
    .blocks(src, block_size, &mut ranks, &mut scratch)
}

impl Walk<'_> {
    /// Codes `src` block by block: translates each block to ranks and walks
    /// the tree over them. Returns the finished stream.
    fn blocks(
        mut self,
        src: &[u8],
        block_size: usize,
        ranks: &mut [u8],
        scratch: &mut [u8],
    ) -> Option<Vec<u8>> {
        for block in src.chunks(block_size) {
            translate(self.level, &self.tree.symbol_to_rank, block, ranks);
            self.node(ranks, scratch, block.len(), 0, (0, self.tree.num_ranks))?;
        }
        self.writer.finish()
    }
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
    let bits = pivco_encode(cx.simd, capacity, &weights, table_log, src, block_size)
        .ok_or(NodeError::InvalidInput("PivCo-Huffman encoding failed"))?;
    cx.emit(1, 1, bits)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Backend;
    use alloc::vec;

    fn ranks(len: usize, seed: usize, modulus: usize) -> Vec<u8> {
        (0..len)
            .map(|i| ((i.wrapping_mul(0x9E37_79B1).wrapping_add(seed) >> 11) % modulus) as u8)
            .collect()
    }

    /// Runs `partition` at `level` and returns the bitmap, both sides and the
    /// count of ones.
    fn run(
        level: Level,
        input: &[u8],
        split: u8,
        keep_left: bool,
        rhs: u8,
    ) -> (Vec<u8>, Vec<u8>, Vec<u8>, usize) {
        let num = input.len();
        let mut src = input.to_vec();
        src.resize(num + SLOP, 0xAA);
        let mut left = alloc::vec![0u8; num + SLOP];
        let mut right = alloc::vec![0u8; num + SLOP];
        let mut bitmap = alloc::vec![0xFFu8; num.div_ceil(8)];
        let rhs_to = match rhs {
            0 => Rhs::Discard,
            1 => Rhs::InPlace,
            _ => Rhs::To(&mut right),
        };
        let lhs = keep_left.then_some(&mut left[..]);
        let ones = partition(level, &mut src, num, split, &mut bitmap, lhs, rhs_to);
        let zeros = num - ones;
        let kept_left = if keep_left {
            left[..zeros].to_vec()
        } else {
            Vec::new()
        };
        let kept_right = match rhs {
            0 => Vec::new(),
            1 => src[..ones].to_vec(),
            _ => right[..ones].to_vec(),
        };
        (bitmap, kept_left, kept_right, ones)
    }

    #[test]
    fn partition_splits_ranks_the_same_at_every_level() {
        let lengths: &[usize] = if cfg!(miri) {
            &[0, 1, 15, 16, 17, 40]
        } else {
            &[0, 1, 7, 8, 9, 15, 16, 17, 31, 32, 33, 100, 1000]
        };
        for &len in lengths {
            let input = ranks(len, 3, 40);
            for split in [1u8, 17, 39] {
                let ones = input.iter().filter(|&&rank| rank >= split).count();
                let left: Vec<u8> = input.iter().copied().filter(|&r| r < split).collect();
                let right: Vec<u8> = input.iter().copied().filter(|&r| r >= split).collect();
                for backend in Backend::available() {
                    for (keep_left, rhs) in [(true, 1u8), (false, 2), (true, 0), (false, 0)] {
                        let (bitmap, got_left, got_right, got_ones) =
                            run(backend.0, &input, split, keep_left, rhs);
                        assert_eq!(got_ones, ones, "{backend}, length {len}");
                        if keep_left {
                            assert_eq!(got_left, left, "{backend}, length {len}");
                        }
                        if rhs != 0 {
                            assert_eq!(got_right, right, "{backend}, length {len}");
                        }
                        for (at, &rank) in input.iter().enumerate() {
                            let bit = bitmap[at / 8] >> (at % 8) & 1;
                            assert_eq!(bit, u8::from(rank >= split), "{backend}, at {at}");
                        }
                        // Bits past the end are clear.
                        if len % 8 != 0 {
                            assert_eq!(bitmap[len / 8] >> (len % 8), 0, "{backend}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn partition_stops_at_short_buffers() {
        // A destination with no room past the ranks is still filled in order.
        let input = ranks(20, 1, 8);
        for backend in Backend::available() {
            let mut src = input.clone();
            let mut left = alloc::vec![0u8; 20];
            let mut bitmap = [0u8; 3];
            let ones = partition(
                backend.0,
                &mut src,
                20,
                4,
                &mut bitmap,
                Some(&mut left[..]),
                Rhs::InPlace,
            );
            let zeros = 20 - ones;
            let expected: Vec<u8> = input.iter().copied().filter(|&r| r < 4).collect();
            assert_eq!(&left[..zeros], &expected[..], "{backend}");
        }
    }

    #[test]
    fn pack_writes_depth_bits_per_rank() {
        for depth in 1..=8usize {
            for len in [0usize, 1, 7, 8, 9, 17, 100] {
                let first = 3u8;
                let input: Vec<u8> = ranks(len, depth, 1 << depth)
                    .into_iter()
                    .map(|rank| rank.wrapping_add(first))
                    .collect();
                let mut expected = alloc::vec![0u8; (len * depth).div_ceil(8)];
                pack_scalar(&input, first, depth, &mut expected);
                for backend in Backend::available() {
                    let mut got = alloc::vec![0xA5u8; (len * depth).div_ceil(8)];
                    pack(backend.0, &input, first, depth, &mut got);
                    assert_eq!(got, expected, "{backend}, depth {depth}, length {len}");
                }
                let out = expected;
                for (at, &rank) in input.iter().enumerate() {
                    let mut value = 0usize;
                    for bit in 0..depth {
                        let position = at * depth + bit;
                        value |= usize::from(out[position / 8] >> (position % 8) & 1) << bit;
                    }
                    assert_eq!(
                        value,
                        usize::from(rank.wrapping_sub(first)),
                        "depth {depth}, at {at}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_level_writes_the_same_stream() {
        let tables: [(&[u8], u32); 3] = [
            (&[5, 4, 3, 2, 1, 1], 5),
            (&[2, 2, 2, 2, 2, 2, 2, 2], 4),
            (&[1, 1, 1, 1, 2, 2], 3),
        ];
        for (weights, table_log) in tables {
            for size in [1usize, 15, 64, 65, 300, if cfg!(miri) { 300 } else { 5000 }] {
                let data = ranks(size, 9, weights.len());
                let capacity = encode_bound(data.len(), 64).unwrap();
                let expected =
                    pivco_encode(Level::fallback(), capacity, weights, table_log, &data, 64);
                assert!(expected.is_some());
                for backend in Backend::available() {
                    let got = pivco_encode(backend.0, capacity, weights, table_log, &data, 64);
                    assert_eq!(got, expected, "{backend}, size {size}");
                }
            }
        }
    }

    /// Symbols, depths, codewords, levels and ranks of a tree.
    type Reference = (Vec<u8>, Vec<u8>, Vec<u16>, usize, usize);

    /// The tree of `weights` by the reference construction with owned leaf
    /// lists: (symbols, depths, codewords, levels, ranks).
    fn reference(weights: &[u8]) -> Option<Reference> {
        let sum: u32 = weights.iter().map(|&w| (1u32 << w) >> 1).sum();
        let table_log = sum.trailing_zeros() as usize;
        let mut buckets: Vec<Vec<Vec<u8>>> = vec![Vec::new(); 14];
        for (symbol, &weight) in weights.iter().enumerate() {
            if weight != 0 {
                buckets[usize::from(weight)].push(vec![symbol as u8]);
            }
        }
        for weight in (1..=table_log).rev() {
            while buckets[weight].len() > 2 {
                let count = buckets[weight].len();
                let bits = count.ilog2() as usize;
                let split = count - (1 << bits);
                let flat: Vec<u8> = buckets[weight].drain(split..).flatten().collect();
                buckets.get_mut(weight + bits)?.push(flat);
            }
        }
        let min_weight = (1..=13).find(|&weight| !buckets[weight].is_empty())?;
        let levels = (table_log + 2).checked_sub(min_weight)?;
        let (mut symbols, mut depths, mut words) =
            (vec![0u8; 256], vec![0u8; 256], vec![0u16; 256]);
        let (mut codeword, mut rank) = (0u32, 0usize);
        for level in 0..levels {
            for leaf in &buckets[table_log + 1 - level] {
                let bits = leaf.len().ilog2() as usize;
                let total = level + bits;
                if rank + leaf.len() > 256 || total > 16 {
                    return None;
                }
                for (index, &symbol) in leaf.iter().enumerate() {
                    let word = (codeword << bits) + index as u32;
                    symbols[rank + index] = symbol;
                    depths[rank + index] = bits as u8;
                    words[rank + index] = if total == 0 {
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
        Some((symbols, depths, words, levels, rank))
    }

    /// Weights of a random complete prefix code over `count` symbols.
    fn random_weights(seed: u64, count: usize) -> Vec<u8> {
        let mut state = seed | 1;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut lengths = vec![0u32];
        while lengths.len() < count {
            let at = (next() % lengths.len() as u64) as usize;
            if lengths[at] >= 11 {
                continue;
            }
            lengths[at] += 1;
            let again = lengths[at];
            lengths.push(again);
        }
        let longest = *lengths.iter().max().unwrap_or(&0);
        let mut weights = vec![0u8; 256];
        let mut slots: Vec<usize> = (0..256).collect();
        for length in lengths {
            let pick = (next() % slots.len() as u64) as usize;
            weights[slots.swap_remove(pick)] = (longest + 1 - length) as u8;
        }
        weights
    }

    #[test]
    fn construction_matches_the_owned_leaf_reference() {
        for seed in 1..200u64 {
            let count = 1 + (seed as usize * 7) % 256;
            let weights = random_weights(seed.wrapping_mul(0x9E37_79B9), count);
            let tree = Tree::new(&weights);
            let want = reference(&weights);
            assert_eq!(tree.is_some(), want.is_some(), "seed {seed}");
            let (Some(tree), Some((symbols, depths, words, _, ranks))) = (tree, want) else {
                continue;
            };
            assert_eq!(
                (tree.num_ranks, tree.num_ranks),
                (ranks, ranks),
                "seed {seed}"
            );
            assert_eq!(&tree.rank_to_symbol[..], &symbols[..], "seed {seed}");
            assert_eq!(&tree.rank_to_flat_depth[..], &depths[..], "seed {seed}");
            assert_eq!(&tree.rank_to_codeword[..], &words[..], "seed {seed}");
        }
    }

    #[test]
    fn ranks_invert_symbols() {
        let weights = random_weights(77, 100);
        let tree = Tree::new(&weights).unwrap();
        for rank in 0..tree.num_ranks {
            let symbol = tree.rank_to_symbol[rank];
            assert_eq!(usize::from(tree.symbol_to_rank[usize::from(symbol)]), rank);
        }
    }
}
