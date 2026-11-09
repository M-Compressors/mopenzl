//! The split nodes (`encode_splitN_binding.c`, `encode_split_byrange_binding.c`):
//! `splitN` 66, `splitN_struct` 67, `splitN_num` 68 and `split_byrange` 49.
//!
//! `splitN` cuts its input into consecutive segments given by the
//! `ZL_SPLITN_SEGMENTSIZES_PID` parameter, a `size_t` array that is either a
//! reference parameter (`ZL_Edge_runSplitNode`) or a copy parameter
//! (`ZL_Compressor_registerSplitNode_withParams`). Every segment is a
//! zero-copy reference to the input carrying the channel metadata
//! `ZL_SPLIT_CHANNEL_ID`. A last segment size of 0 means "whatever is left".
//! The parser-function form (`ZL_SPLITN_PARSINGF_PID`) is not supported and
//! fails the node.
//!
//! `split_byrange` finds the segments itself: it looks for points where the
//! value range of a numeric stream jumps. The algorithm (block statistics,
//! confirmed splits, boundary refinement) is a statement-for-statement port
//! of the reference, including its `size_t`/`uint64_t` wraparound.

use alloc::vec::Vec;

use crate::compressor::core::codecs::FnEncoder;
use super::SEGMENT_SIZES_PID;
use super::util::{local_param, usize_array};
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::varint;

/// `ZL_SPLITN_PARSINGF_PID`: a parsing function, not supported.
const PARSING_FN_PID: i32 = 436;
/// `ZL_SPLIT_BYRANGE_MIN_SEGMENT_SIZE_PID`.
pub(crate) const BYRANGE_MIN_SEGMENT_SIZE_PID: i32 = crate::graph::params::split::BYRANGE_MIN_SEGMENT_SIZE;
/// `ZL_SPLIT_CHANNEL_ID`: the metadata every segment carries.
pub(crate) const SPLIT_CHANNEL_ID: i32 = crate::graph::params::split::CHANNEL_ID;

/// `EI_splitN`, for serial, struct and numeric inputs alike.
pub(crate) static SPLIT_N: FnEncoder = FnEncoder(split_n);
/// `EI_split_byrange`.
pub(crate) static SPLIT_BYRANGE: FnEncoder = FnEncoder(split_by_range);

/// `getSplitInstructions` (segment sizes only).
fn segment_sizes(cx: &NodeCx<'_, '_>) -> Result<Vec<usize>, NodeError> {
    if cx.ref_param(PARSING_FN_PID).is_some() {
        return Err(NodeError::UnsupportedParam("split parsing functions"));
    }
    let param = local_param(cx, SEGMENT_SIZES_PID).ok_or(NodeError::InvalidParam(
        "can't find any instruction to split",
    ))?;
    usize_array(&param).ok_or(NodeError::InvalidParam(
        "the split instructions have the wrong type",
    ))
}

/// Sends the codec header the decoder needs to rebuild an empty input of an
/// element width other than 1.
fn send_width_header(cx: &mut NodeCx<'_, '_>, width: usize) -> Result<(), NodeError> {
    let mut header = Vec::new();
    varint::push(&mut header, width as u64);
    cx.send_header(&header)
}

fn split_n<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let sizes = segment_sizes(cx)?;
    let in_size = input.n_elts();
    let width = input.width();
    if width != 1 && sizes.is_empty() {
        send_width_header(cx, width)?;
    }
    let mut pos = 0usize;
    for (n, &size) in sizes.iter().enumerate() {
        let mut size = size;
        if n == sizes.len() - 1 && size == 0 {
            // A last segment of size zero takes what is left.
            size = in_size - pos;
        }
        let end = pos
            .checked_add(size)
            .filter(|&end| end <= in_size)
            .ok_or(NodeError::InvalidParam(
                "split instructions require more length than input",
            ))?;
        let output = cx.reference(0, input, width, size, pos * width)?;
        cx.set_output_metadata(output, SPLIT_CHANNEL_ID, n as i32)?;
        pos = end;
    }
    if pos != in_size {
        return Err(NodeError::InvalidParam(
            "split instructions do not map exactly the entire input",
        ));
    }
    Ok(())
}

/// `DEFAULT_MIN_SEGMENT_SIZE`.
const DEFAULT_MIN_SEGMENT_SIZE: usize = 32;
/// `CONFIRMATION_WINDOW`.
const CONFIRMATION_WINDOW: usize = 7;
/// `GAP_FACTOR`.
const GAP_FACTOR: u64 = 2;
/// `STATIONARITY_NUM`.
const STATIONARITY_NUM: u64 = 13;
/// `STATIONARITY_DEN`.
const STATIONARITY_DEN: u64 = 10;
/// `BLOCK_DIVISOR`.
const BLOCK_DIVISOR: usize = 2;

/// `defaultMinSegSizeForWidth`.
const fn default_min_segment_size(width: usize) -> usize {
    if width <= 1 {
        4 * DEFAULT_MIN_SEGMENT_SIZE
    } else if width <= 2 {
        2 * DEFAULT_MIN_SEGMENT_SIZE
    } else {
        DEFAULT_MIN_SEGMENT_SIZE
    }
}

/// `computeBlockStats`: the minimum and maximum of every block.
fn block_stats(src: &[u64], block_size: usize, nb_blocks: usize) -> (Vec<u64>, Vec<u64>) {
    let mut block_min = Vec::with_capacity(nb_blocks);
    let mut block_max = Vec::with_capacity(nb_blocks);
    for b in 0..nb_blocks {
        let start = b * block_size;
        let end = (start + block_size).min(src.len());
        let block = &src[start..end];
        let mut min = block[0];
        let mut max = block[0];
        for &v in &block[1..] {
            if v < min {
                min = v;
            }
            if v > max {
                max = v;
            }
        }
        block_min.push(min);
        block_max.push(max);
    }
    (block_min, block_max)
}

/// The range statistics of a window of blocks.
struct Window {
    min: u64,
    max: u64,
    max_block_range: u64,
}

/// Min, max and widest block range of the blocks `first..=last`.
fn window(block_min: &[u64], block_max: &[u64], first: usize, last: usize) -> Window {
    let mut w = Window {
        min: block_min[first],
        max: block_max[first],
        max_block_range: block_max[first] - block_min[first],
    };
    for i in first + 1..=last {
        if block_min[i] < w.min {
            w.min = block_min[i];
        }
        if block_max[i] > w.max {
            w.max = block_max[i];
        }
        let range = block_max[i] - block_min[i];
        if range > w.max_block_range {
            w.max_block_range = range;
        }
    }
    w
}

/// Whether a window of range `range` and widest block `max_block_range` is
/// stationary (`windowRange * 10 <= maxBlockRange * 13`).
const fn is_stationary(range: u64, max_block_range: u64) -> bool {
    max_block_range == 0
        || (range <= u64::MAX / STATIONARITY_DEN
            && range.wrapping_mul(STATIONARITY_DEN)
                <= STATIONARITY_NUM.wrapping_mul(max_block_range))
}

/// `findConfirmedSplitBlocks`.
fn find_confirmed_split_blocks(
    block_min: &[u64],
    block_max: &[u64],
    m: usize,
    max_splits: usize,
) -> Vec<usize> {
    let nb_blocks = block_min.len();
    let mut splits = Vec::new();
    if m < 1 || nb_blocks < 2 * m + 1 {
        return splits;
    }
    let min_left_size = if m >= 2 { 2 } else { 1 };
    let mut left_bound = 0usize;
    let mut b = m - 1;
    while b + m + 1 < nb_blocks && splits.len() < max_splits {
        let l_start = if b >= m - 1 && b + 1 - m >= left_bound {
            b + 1 - m
        } else {
            left_bound
        };
        let l_size = b - l_start + 1;
        if l_size < min_left_size {
            b += 1;
            continue;
        }
        let left = window(block_min, block_max, l_start, b);
        let right = window(block_min, block_max, b + 2, b + m + 1);

        let non_overlap = left.max < right.min || right.max < left.min;
        if non_overlap {
            let typical_amp = left.max_block_range.max(right.max_block_range);
            let gap = if left.max < right.min {
                right.min - left.max
            } else {
                left.min - right.max
            };
            let transition_amp = block_max[b + 1] - block_min[b + 1];
            let mut significant = typical_amp == 0
                || gap > GAP_FACTOR.wrapping_mul(typical_amp)
                || transition_amp > GAP_FACTOR.wrapping_mul(typical_amp);
            if !significant {
                let left_stationary = is_stationary(left.max - left.min, left.max_block_range);
                let right_stationary = is_stationary(right.max - right.min, right.max_block_range);
                if left_stationary && right_stationary {
                    significant = true;
                }
            }
            if significant {
                splits.push(b);
                left_bound = b + 2;
                b += 2;
                continue;
            }
        }
        b += 1;
    }
    splits
}

/// Where `refineSplitBoundary` looks: the block layout of the stream.
struct Layout<'s> {
    src: &'s [u64],
    block_size: usize,
    block_min: &'s [u64],
    block_max: &'s [u64],
}

/// `refineSplitBoundary`: turns a block-level split into an element index.
fn refine_split_boundary(
    layout: &Layout<'_>,
    split_block: usize,
    seg_start_elt: usize,
    seg_end_blk: usize,
) -> usize {
    let Layout {
        src,
        block_size,
        block_min,
        block_max,
    } = *layout;
    let nb_elts = src.len();
    let mut trans_start = if split_block > 0 {
        (split_block - 1) * block_size
    } else {
        0
    };
    if trans_start < seg_start_elt {
        trans_start = seg_start_elt;
    }
    let trans_end = (split_block + 3) * block_size;
    let scan_end = trans_end.min(nb_elts);
    let scan_start = seg_start_elt.min(trans_start);

    if scan_end <= scan_start + 1 {
        return (split_block + 1) * block_size;
    }

    let trans_len = scan_end - trans_start;
    let mut suff_min = alloc::vec![0u64; trans_len];
    let mut suff_max = alloc::vec![0u64; trans_len];
    suff_min[trans_len - 1] = src[trans_start + trans_len - 1];
    suff_max[trans_len - 1] = suff_min[trans_len - 1];
    for i in (1..trans_len).rev() {
        let v = src[trans_start + i - 1];
        suff_min[i - 1] = if v < suff_min[i] { v } else { suff_min[i] };
        suff_max[i - 1] = if v > suff_max[i] { v } else { suff_max[i] };
    }

    let trans_end_blk = scan_end.div_ceil(block_size);
    if trans_end_blk < seg_end_blk {
        let mut ctx_min = block_min[trans_end_blk];
        let mut ctx_max = block_max[trans_end_blk];
        for i in trans_end_blk + 1..seg_end_blk {
            if block_min[i] < ctx_min {
                ctx_min = block_min[i];
            }
            if block_max[i] > ctx_max {
                ctx_max = block_max[i];
            }
        }
        for i in 0..trans_len {
            if ctx_min < suff_min[i] {
                suff_min[i] = ctx_min;
            }
            if ctx_max > suff_max[i] {
                suff_max[i] = ctx_max;
            }
        }
    }

    let mut pref_min = src[scan_start];
    let mut pref_max = pref_min;
    let mut best_pos = (split_block + 1) * block_size;
    let mut best_gap = 0u64;
    for (pos, &v) in src
        .iter()
        .enumerate()
        .take(scan_end)
        .skip(scan_start + 1)
    {
        if pos >= trans_start {
            let ti = pos - trans_start;
            let mut gap = 0u64;
            if pref_max < suff_min[ti] {
                gap = suff_min[ti] - pref_max;
            } else if suff_max[ti] < pref_min {
                gap = pref_min - suff_max[ti];
            }
            if gap > best_gap {
                best_gap = gap;
                best_pos = pos;
            }
        }
        if v < pref_min {
            pref_min = v;
        }
        if v > pref_max {
            pref_max = v;
        }
    }
    best_pos
}

/// `detectBoundaries_internal`: the segment sizes of `src`.
fn detect_boundaries(src: &[u64], min_seg_size: usize, max_segments: usize) -> Vec<usize> {
    let nb_elts = src.len();
    let block_size = if min_seg_size / BLOCK_DIVISOR > 0 {
        min_seg_size / BLOCK_DIVISOR
    } else {
        1
    };
    let nb_blocks = nb_elts.wrapping_add(block_size).wrapping_sub(1) / block_size;
    let m_raw = if nb_blocks >= 3 {
        (nb_blocks - 1) / 2
    } else {
        0
    };
    let m = m_raw.min(CONFIRMATION_WINDOW);
    let m_eff = if m >= 2 && nb_blocks > 2 * m {
        m
    } else {
        1
    };
    if nb_blocks < 3 {
        return alloc::vec![nb_elts];
    }

    let (block_min, block_max) = block_stats(src, block_size, nb_blocks);
    let splits = find_confirmed_split_blocks(&block_min, &block_max, m_eff, max_segments - 1);
    if splits.is_empty() {
        return alloc::vec![nb_elts];
    }

    let layout = Layout {
        src,
        block_size,
        block_min: &block_min,
        block_max: &block_max,
    };
    let mut sizes = Vec::new();
    let mut count = 0usize;
    let mut pos = 0usize;
    for (s, &split_block) in splits.iter().enumerate() {
        let seg_end_blk = if s + 1 < splits.len() {
            splits[s + 1] + 1
        } else {
            nb_blocks
        }
        .min(nb_blocks);
        let mut split_pos = refine_split_boundary(&layout, split_block, pos, seg_end_blk);
        if split_pos < pos.wrapping_add(min_seg_size) {
            split_pos = pos.wrapping_add(min_seg_size);
        }
        if split_pos > nb_elts.wrapping_sub(min_seg_size) {
            continue;
        }
        if split_pos <= pos {
            continue;
        }
        if count < max_segments {
            sizes.push(split_pos - pos);
        }
        count += 1;
        pos = split_pos;
    }
    if count < max_segments {
        sizes.push(nb_elts - pos);
    }
    sizes
}

fn split_by_range<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let nb_elts = input.n_elts();
    let width = input.width();
    let mut min_seg_size = default_min_segment_size(width);
    if let Some(value) = cx.int(BYRANGE_MIN_SEGMENT_SIZE_PID) {
        // `(size_t)paramValue`: a negative value wraps to a huge size.
        min_seg_size = value as usize;
    }

    if nb_elts == 0 {
        if width != 1 {
            send_width_header(cx, width)?;
        }
        return Ok(());
    }
    if nb_elts < min_seg_size.wrapping_mul(2) {
        let output = cx.reference(0, input, width, nb_elts, 0)?;
        cx.set_output_metadata(output, SPLIT_CHANNEL_ID, 0)?;
        return Ok(());
    }

    let block_size = if min_seg_size / BLOCK_DIVISOR > 0 {
        min_seg_size / BLOCK_DIVISOR
    } else {
        1
    };
    let nb_blocks = nb_elts.wrapping_add(block_size).wrapping_sub(1) / block_size;
    let values: Vec<u64> = input
        .bytes()
        .chunks_exact(width)
        .map(super::util::read_le)
        .collect();
    let sizes = detect_boundaries(&values, min_seg_size, nb_blocks + 1);

    let mut pos = 0usize;
    for (n, &size) in sizes.iter().enumerate() {
        let output = cx.reference(0, input, width, size, pos * width)?;
        cx.set_output_metadata(output, SPLIT_CHANNEL_ID, n as i32)?;
        pos += size;
    }
    Ok(())
}
