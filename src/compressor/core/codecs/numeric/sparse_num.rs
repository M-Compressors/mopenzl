//! The sparse-number nodes (`EI_sparse_num` and `EI_sparse_num_auto`,
//! `encode_sparse_num_binding.c` and `encode_sparse_num_kernel.c`), codec 66.
//!
//! The input is cut at every element that differs from a dominant symbol:
//! output 0 holds, per literal, the number of dominant symbols before it
//! (plus one final entry for a trailing run), output 1 the literals. Literals
//! may equal the dominant symbol only when a run of `2^32 - 1` symbols is
//! split. The dominant symbol is the header (little-endian, no longer than
//! the element) and is absent when it is zero.
//!
//! `sparse_num` always uses zero. `sparse_num_auto` takes the copy parameter
//! `ZL_SPARSE_NUM_DOMINANT_VALUE_PID` (8 bytes) or, without it, a majority
//! vote over the first 4096 elements (`votes == 0` means zero).

use alloc::vec::Vec;

use super::super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::numeric::{elements, push_le};

/// `ZL_SPARSE_NUM_DOMINANT_VALUE_PID`.
const DOMINANT_VALUE_PID: i32 = crate::graph::params::sparse_num::DOMINANT_VALUE;
/// `ZL_SPARSE_NUM_DOMINANT_SAMPLE_MAX`.
const DOMINANT_SAMPLE_MAX: usize = 4096;

/// `ZL_NODE_SPARSE_NUM` (node 54).
pub(crate) static SPARSE_NUM: FnEncoder = FnEncoder(|cx, inputs| encode_node(cx, inputs, false));
/// `ZL_NODE_SPARSE_NUM_AUTO` (node 55).
pub(crate) static SPARSE_NUM_AUTO: FnEncoder =
    FnEncoder(|cx, inputs| encode_node(cx, inputs, true));

/// `ZL_sparseNumSelectDominant`: a majority vote over the first elements.
fn select_dominant(bytes: &[u8], width: usize) -> u64 {
    let mut candidate = 0;
    let mut votes = 0usize;
    for value in elements(bytes, width).take(DOMINANT_SAMPLE_MAX) {
        if votes == 0 {
            candidate = value;
            votes = 1;
        } else if value == candidate {
            votes += 1;
        } else {
            votes -= 1;
        }
    }
    if votes == 0 { 0 } else { candidate }
}

/// `ZL_sparseNumDominantFitsWidth`.
const fn dominant_fits(dominant: u64, width: usize) -> bool {
    width >= 8 || dominant < 1 << (8 * width)
}

/// `ZL_sparseNumDistanceWidth`.
const fn distance_width(max_distance: u32) -> usize {
    if max_distance <= u8::MAX as u32 {
        1
    } else if max_distance <= u16::MAX as u32 {
        2
    } else {
        4
    }
}

/// `ZL_sparseNumComputeEncodeInfo` and `ZL_sparseNumEncode`: the distances,
/// the literals (`width` bytes each) and the width of a distance.
fn split(bytes: &[u8], width: usize, dominant: u64) -> (Vec<u32>, Vec<u8>, usize) {
    let mut distances = Vec::new();
    let mut literals = Vec::new();
    let mut run = 0u64;
    let mut max_distance = 0u32;
    for value in elements(bytes, width) {
        if value == dominant {
            if run == u64::from(u32::MAX) {
                // A full run: it is closed by this dominant symbol, which
                // becomes a literal.
                distances.push(u32::MAX);
                max_distance = u32::MAX;
                push_le(&mut literals, value, width);
                run = 0;
            } else {
                run += 1;
            }
            continue;
        }
        let distance = run as u32;
        max_distance = max_distance.max(distance);
        distances.push(distance);
        push_le(&mut literals, value, width);
        run = 0;
    }
    if run > 0 {
        let distance = run as u32;
        max_distance = max_distance.max(distance);
        distances.push(distance);
    }
    (distances, literals, distance_width(max_distance))
}

/// `ZL_sparseNumEncodeSelectedDominant`.
fn encode_dominant<'a>(
    cx: &mut NodeCx<'_, 'a>,
    input: &Stream<'a>,
    dominant: u64,
) -> Result<(), NodeError> {
    let width = input.width();
    if dominant != 0 {
        let size = (63 - dominant.leading_zeros() as usize) / 8 + 1;
        cx.send_header(&dominant.to_le_bytes()[..size])?;
    }
    let (distances, literals, distance_width) = split(input.bytes(), width, dominant);
    let mut distance_bytes = Vec::with_capacity(distances.len() * distance_width);
    for &distance in &distances {
        push_le(&mut distance_bytes, u64::from(distance), distance_width);
    }
    cx.emit(0, distance_width, distance_bytes)?;
    cx.emit(1, width, literals)?;
    Ok(())
}

/// `ZL_sparseNumEncoderDominant`: the explicit dominant symbol, or the vote.
fn auto_dominant(cx: &NodeCx<'_, '_>, input: &Stream<'_>) -> Result<u64, NodeError> {
    let Some(bytes) = super::param_ref(cx, DOMINANT_VALUE_PID) else {
        return Ok(select_dominant(input.bytes(), input.width()));
    };
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| {
        NodeError::InvalidParam("sparse_num dominant parameter must be uint64_t")
    })?;
    let explicit = u64::from_le_bytes(bytes);
    if !dominant_fits(explicit, input.width()) {
        return Err(NodeError::InvalidInput(
            "sparse_num dominant value does not fit input width",
        ));
    }
    Ok(if input.n_elts() == 0 { 0 } else { explicit })
}

/// `ZL_sparseNumEncodeNode`.
fn encode_node<'a>(
    cx: &mut NodeCx<'_, 'a>,
    inputs: &[Stream<'a>],
    auto: bool,
) -> Result<(), NodeError> {
    let input = &inputs[0];
    let dominant = if auto { auto_dominant(cx, input)? } else { 0 };
    encode_dominant(cx, input, dominant)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn zeros_split_into_distances_and_literals() {
        let (distances, literals, width) = split(&bytes(&[0, 0, 7, 0, 9, 0, 0]), 4, 0);
        assert_eq!(distances, [2, 1, 2]);
        assert_eq!(literals, bytes(&[7, 9]));
        assert_eq!(width, 1);
        let (distances, literals, width) = split(&[], 4, 0);
        assert!(distances.is_empty() && literals.is_empty());
        assert_eq!(width, 1);
        let (distances, _, _) = split(&bytes(&[5, 6]), 4, 0);
        assert_eq!(distances, [0, 0]);
    }

    #[test]
    fn a_nonzero_dominant_symbol_splits_the_same_way() {
        let (distances, literals, _) = split(&bytes(&[3, 3, 1, 3]), 4, 3);
        assert_eq!(distances, [2, 1]);
        assert_eq!(literals, bytes(&[1]));
    }

    #[test]
    fn distance_widths_follow_the_longest_run() {
        let mut data = alloc::vec![0u8; 300];
        data.push(1);
        assert_eq!(split(&data, 1, 0).2, 2);
        let long = alloc::vec![0u8; 70_000];
        assert_eq!(split(&long, 1, 0).2, 4);
        assert_eq!(distance_width(255), 1);
        assert_eq!(distance_width(256), 2);
        assert_eq!(distance_width(65_536), 4);
    }

    #[test]
    fn the_vote_picks_the_majority_of_the_prefix() {
        assert_eq!(select_dominant(&[], 1), 0);
        assert_eq!(select_dominant(&[5, 5, 1, 5], 1), 5);
        assert_eq!(select_dominant(&[5, 1], 1), 0);
        // Only the first 4096 elements vote.
        let mut data = alloc::vec![9u8; 2049];
        data.extend(alloc::vec![1u8; 2047]);
        data.extend(alloc::vec![1u8; 100]);
        assert_eq!(select_dominant(&data, 1), 9);
        assert!(dominant_fits(255, 1) && !dominant_fits(256, 1) && dominant_fits(u64::MAX, 8));
    }
}
