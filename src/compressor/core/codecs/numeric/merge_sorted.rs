//! The merge-sorted node (`EI_mergeSorted`, `encode_merge_sorted_binding.c`
//! and `encode_merge_sorted_kernel.c`), codec 43.
//!
//! The input, 32-bit, is split into maximal strictly increasing runs (at most
//! 64). The outputs are the sorted distinct values of all runs and, per value,
//! a bitset saying which runs contain it. The bitset elements are
//! `2^ceil(log2(ceil(runs / 8)))` bytes wide (two bytes for an empty input,
//! where `nextPow2(0)` is 0 and the reference uses `nbSrcs == 0 ? 1`). The header lists
//! the length of every run as a varint.
//!
//! The reference merges with a priority queue; the result is the sorted set
//! union with membership bits, which this port computes with a plain k-way
//! scan. The merge order never reaches the output.

use alloc::vec::Vec;

use super::super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::numeric::{elements, push_le};
use crate::shared::varint;

/// `ZL_NODE_MERGE_SORTED` (node 29).
pub(crate) static MERGE_SORTED: FnEncoder = FnEncoder(merge_sorted);

/// `kMaxNbSrcs`.
const MAX_RUNS: usize = 64;

/// `getSortedRuns`: the lengths of the maximal strictly increasing runs.
///
/// # Errors
///
/// More than [`MAX_RUNS`] runs.
fn run_lengths(values: &[u32]) -> Result<Vec<usize>, NodeError> {
    let mut runs = Vec::new();
    let mut start = 0;
    for index in 1..values.len() {
        if values[index] <= values[index - 1] {
            runs.push(index - start);
            start = index;
            if runs.len() >= MAX_RUNS {
                return Err(NodeError::InvalidInput("too many sorted runs"));
            }
        }
    }
    if !values.is_empty() {
        runs.push(values.len() - start);
    }
    Ok(runs)
}

/// `ZS2_MergeSorted_merge`: the distinct values in increasing order and the
/// bitset of the runs holding each.
fn merge(values: &[u32], runs: &[usize]) -> (Vec<u64>, Vec<u32>) {
    let mut cursors = Vec::with_capacity(runs.len());
    let mut start = 0;
    for &len in runs {
        cursors.push((start, start + len));
        start += len;
    }
    let mut bitsets = Vec::new();
    let mut merged = Vec::new();
    while let Some(min) = cursors
        .iter()
        .filter(|(at, end)| at < end)
        .map(|&(at, _)| values[at])
        .min()
    {
        let mut bitset = 0u64;
        for (run, (at, end)) in cursors.iter_mut().enumerate() {
            if *at < *end && values[*at] == min {
                bitset |= 1 << run;
                *at += 1;
            }
        }
        bitsets.push(bitset);
        merged.push(min);
    }
    (bitsets, merged)
}

fn merge_sorted<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    if input.width() != 4 {
        return Err(NodeError::InvalidInput("merge_sorted takes 32-bit elements"));
    }
    let values: Vec<u32> = elements(input.bytes(), 4).map(|value| value as u32).collect();
    let runs = run_lengths(&values)?;
    let bitset_width_log = if runs.is_empty() {
        1
    } else {
        (runs.len().div_ceil(8)).next_power_of_two().trailing_zeros() as usize
    };
    let bitset_width = 1usize << bitset_width_log;
    let mut header = Vec::new();
    for &len in &runs {
        varint::push(&mut header, len as u64);
    }
    cx.send_header(&header)?;
    let (bitsets, merged) = merge(&values, &runs);
    let mut bitset_bytes = Vec::with_capacity(bitsets.len() * bitset_width);
    for &bitset in &bitsets {
        push_le(&mut bitset_bytes, bitset, bitset_width);
    }
    let mut merged_bytes = Vec::with_capacity(merged.len() * 4);
    for &value in &merged {
        push_le(&mut merged_bytes, u64::from(value), 4);
    }
    cx.emit(0, bitset_width, bitset_bytes)?;
    cx.emit(1, 4, merged_bytes)?;
    Ok(())
}

/// The number of maximal strictly increasing runs, capped at one more than
/// [`MAX_RUNS`] (the selector graph's `nbRuns` loop).
fn count_runs(values: &[u8]) -> usize {
    let mut runs = 0;
    let mut previous = None;
    for value in elements(values, 4) {
        match previous {
            None => runs = 1,
            Some(before) if value <= before => {
                runs += 1;
                if runs > MAX_RUNS {
                    break;
                }
            }
            Some(_) => {}
        }
        previous = Some(value);
    }
    runs
}

/// Whether a 32-bit stream has at most [`MAX_RUNS`] runs, which is what the
/// `merge_sorted` graph tests before choosing the node.
pub(crate) fn within_run_limit(values: &[u8]) -> bool {
    count_runs(values) <= MAX_RUNS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_split_at_every_non_increase() {
        assert_eq!(run_lengths(&[]), Ok(Vec::new()));
        assert_eq!(run_lengths(&[5]), Ok(alloc::vec![1]));
        assert_eq!(run_lengths(&[1, 2, 2, 3, 1]), Ok(alloc::vec![2, 2, 1]));
        let many: Vec<u32> = (0..65).collect();
        let descending: Vec<u32> = many.iter().rev().copied().collect();
        assert_eq!(run_lengths(&many), Ok(alloc::vec![65]));
        assert!(run_lengths(&descending[..64]).is_ok());
        assert!(run_lengths(&descending).is_err());
    }

    #[test]
    fn the_merge_is_the_union_with_membership_bits() {
        let values = [1, 3, 5, 3, 4, 5, 9];
        let runs = run_lengths(&values).unwrap();
        assert_eq!(runs, [3, 4]);
        assert_eq!(merge(&values, &runs), (alloc::vec![1, 3, 2, 3, 2], alloc::vec![1, 3, 4, 5, 9]));
    }

    #[test]
    fn counting_stops_past_the_limit() {
        let bytes = |values: &[u32]| values.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>();
        assert_eq!(count_runs(&[]), 0);
        assert_eq!(count_runs(&bytes(&[4])), 1);
        assert_eq!(count_runs(&bytes(&[1, 2, 1, 2])), 2);
        let descending: Vec<u32> = (0..100).rev().collect();
        assert_eq!(count_runs(&bytes(&descending)), MAX_RUNS + 1);
    }
}
