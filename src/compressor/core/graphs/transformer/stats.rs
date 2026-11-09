//! Numeric statistics of the Transformer selector
//! (`compress/selectors/transformer/numeric_stats.c`): the KMV sample, the
//! sorted-gap mode, the `match4` count and the transition-gap coefficient of
//! variation. Their arithmetic is part of the trained feature contract.

use alloc::vec::Vec;

use fearless_simd::Level;

use super::math;
use super::wide::{self, Q32_SCALE};
use crate::shared::xxh3;

/// `TRS_NUMERIC_KMV_K`.
pub(super) const KMV_K: usize = 256;
/// `TRS_NUMERIC_MATCH4_TABLE_ENTRIES`.
const MATCH4_TABLE_ENTRIES: usize = 4096;
/// `TRS_SORTED_GAP_MAX_VALUES`.
const SORTED_GAP_MAX_VALUES: usize = 512;
/// `TRS_SORTED_GAP_KEEP_WIDTH`.
const SORTED_GAP_KEEP_WIDTH: usize = 384;
/// `TRS_SORTED_GAP_BATCH_VALUES`.
const SORTED_GAP_BATCH_VALUES: usize = 512;
/// `TRS_MATCH4_TABLE_BITS`.
const MATCH4_TABLE_BITS: u32 = 12;
/// `TRS_MATCH4_HASH_MULTIPLIER`.
const MATCH4_HASH_MULTIPLIER: u32 = 2_654_435_761;

/// `XXH3_64bits` of the little-endian bytes of `value`.
pub(super) fn xxh3_u64(value: u64) -> u64 {
    xxh3::hash64(Level::baseline(), &value.to_le_bytes())
}

/// One KMV sample entry (`TRS_NumericKmvEntry`).
#[derive(Copy, Clone, Debug, Default)]
pub(super) struct KmvEntry {
    hash: u64,
    value: u64,
}

/// The KMV sample: a max-heap on the hash of the `KMV_K` smallest hashes
/// seen. Repeated values may occupy several slots, as in the reference.
#[derive(Debug)]
pub(super) struct Kmv {
    heap: Vec<KmvEntry>,
}

impl Kmv {
    /// An empty sample.
    pub(super) fn new() -> Self {
        Self {
            heap: Vec::with_capacity(KMV_K),
        }
    }

    /// `TRS_numeric_kmv_track_value`.
    pub(super) fn track(&mut self, value: u64) {
        let hash = xxh3_u64(value);
        self.insert(hash, value);
    }

    /// `kmv_heap_sift_down` from the root.
    fn sift_down(&mut self) {
        let size = self.heap.len();
        let mut index = 0;
        loop {
            let mut largest = index;
            let left = 2 * index + 1;
            let right = 2 * index + 2;
            if left < size && self.heap[left].hash > self.heap[largest].hash {
                largest = left;
            }
            if right < size && self.heap[right].hash > self.heap[largest].hash {
                largest = right;
            }
            if largest == index {
                break;
            }
            self.heap.swap(index, largest);
            index = largest;
        }
    }

    /// `kmv_heap_insert`.
    fn insert(&mut self, hash: u64, value: u64) {
        if self.heap.len() < KMV_K {
            let mut index = self.heap.len();
            self.heap.push(KmvEntry { hash, value });
            while index > 0 {
                let parent = (index - 1) / 2;
                if self.heap[parent].hash >= self.heap[index].hash {
                    break;
                }
                self.heap.swap(index, parent);
                index = parent;
            }
        } else if hash < self.heap[0].hash {
            self.heap[0] = KmvEntry { hash, value };
            self.sift_down();
        }
    }

    /// `TRS_numeric_kmv_compute_gap_nmad_fp`: the sampled sorted-gap
    /// normalized mean absolute deviation in Q32. Consumes the sample.
    pub(super) fn gap_nmad_fp(mut self) -> u64 {
        let entries = &mut self.heap;
        if entries.len() < 2 {
            return 0;
        }
        // Entries with equal values are identical (the hash is a function of
        // the value), so the unstable sort cannot reorder anything visible.
        entries.sort_unstable_by_key(|entry| entry.value);
        entries.dedup_by_key(|entry| entry.value);
        if entries.len() < 2 {
            return 0;
        }
        let gaps: Vec<u64> = entries
            .windows(2)
            .map(|pair| pair[1].value.wrapping_sub(pair[0].value))
            .collect();
        let n_gaps = gaps.len() as u64;
        let sum_gaps: u128 = gaps.iter().map(|&gap| u128::from(gap)).sum();
        let mean_gap = wide::div_u64_to_u64(sum_gaps, n_gaps);
        let sum_abs_dev: u128 = gaps
            .iter()
            .map(|&gap| u128::from(wide::abs_diff_u64(gap, mean_gap)))
            .sum();
        let numerator = sum_abs_dev << 32;
        let denominator = u128::from(n_gaps) * u128::from(mean_gap);
        wide::div_u128_to_u64(numerator, denominator)
    }
}

/// `SortedGapAccumulator`: the 512 smallest distinct values, kept sorted.
struct SortedGap {
    retained: Vec<u64>,
    pending: Vec<u64>,
}

impl SortedGap {
    /// `sorted_gap_flush`.
    fn flush(&mut self) {
        self.pending.sort_unstable();
        self.pending.dedup();
        if self.retained.is_empty() {
            core::mem::swap(&mut self.retained, &mut self.pending);
            self.pending.clear();
            return;
        }
        let mut output: Vec<u64> = Vec::with_capacity(SORTED_GAP_MAX_VALUES);
        let (mut retained_index, mut pending_index) = (0, 0);
        while output.len() < SORTED_GAP_MAX_VALUES
            && (retained_index < self.retained.len() || pending_index < self.pending.len())
        {
            let value;
            if pending_index == self.pending.len()
                || (retained_index < self.retained.len()
                    && self.retained[retained_index] < self.pending[pending_index])
            {
                value = self.retained[retained_index];
                retained_index += 1;
            } else if retained_index == self.retained.len()
                || self.pending[pending_index] < self.retained[retained_index]
            {
                value = self.pending[pending_index];
                pending_index += 1;
            } else {
                value = self.retained[retained_index];
                retained_index += 1;
                pending_index += 1;
            }
            if output.last() != Some(&value) {
                output.push(value);
            }
        }
        self.retained = output;
        self.pending.clear();
    }

    /// `sorted_gap_add`.
    fn add(&mut self, value: u64) {
        if self.retained.len() == SORTED_GAP_MAX_VALUES
            && value >= self.retained[SORTED_GAP_MAX_VALUES - 1]
        {
            return;
        }
        self.pending.push(value);
        if self.pending.len() == SORTED_GAP_BATCH_VALUES {
            self.flush();
        }
    }
}

/// `gap_frequency_in_suffix`.
fn gap_frequency_in_suffix(sorted: &[u64], gap_end: usize, end: usize) -> usize {
    let gap = sorted[gap_end] - sorted[gap_end - 1];
    (gap_end..end)
        .filter(|&i| sorted[i] - sorted[i - 1] == gap)
        .count()
}

/// `compute_sorted_gap_mode`.
fn sorted_gap_mode_of(smallest: &[u64]) -> f64 {
    let width = smallest.len().min(SORTED_GAP_KEEP_WIDTH);
    if width < 2 {
        return 0.0;
    }
    let start = smallest.len() - width;
    let mut max_gap_count = 0;
    for i in start + 1..smallest.len() {
        max_gap_count = max_gap_count.max(gap_frequency_in_suffix(smallest, i, smallest.len()));
    }
    max_gap_count as f64 / (width - 1) as f64
}

/// `TRS_numeric_compute_sorted_gap_mode`: the fraction of retained adjacent
/// gaps equal to the most common gap, over the 384 largest of the 512
/// smallest distinct values.
pub(super) fn sorted_gap_mode(values: impl Iterator<Item = u64>) -> f64 {
    let mut accumulator = SortedGap {
        retained: Vec::with_capacity(SORTED_GAP_MAX_VALUES),
        pending: Vec::with_capacity(SORTED_GAP_BATCH_VALUES),
    };
    let mut any = false;
    for value in values {
        any = true;
        accumulator.add(value);
    }
    if !any {
        return 0.0;
    }
    if !accumulator.pending.is_empty() {
        accumulator.flush();
    }
    sorted_gap_mode_of(&accumulator.retained)
}

/// `TRS_numeric_compute_lz_matches_with_table`: matching 4-byte windows of
/// the byte stream, through a 4096-entry table that starts zeroed (so an
/// all-zero window matches an empty slot, as the trained models expect).
pub(super) fn lz_matches(data: &[u8]) -> u64 {
    if data.len() < 4 {
        return 0;
    }
    let mut table = alloc::vec![0u32; MATCH4_TABLE_ENTRIES];
    let mut matches = 0;
    for window in data.windows(4) {
        let sequence = u32::from_le_bytes([window[0], window[1], window[2], window[3]]);
        let hash =
            (sequence.wrapping_mul(MATCH4_HASH_MULTIPLIER) >> (32 - MATCH4_TABLE_BITS)) as usize;
        if table[hash] == sequence {
            matches += 1;
        }
        table[hash] = sequence;
    }
    matches
}

/// `TRS_numeric_compute_transition_gap_cv`.
pub(super) fn transition_gap_cv(sum_gaps: f64, sum_gap_squares: f64, n_gaps: usize) -> f64 {
    if n_gaps == 0 {
        return 0.0;
    }
    let mean_gap = sum_gaps / n_gaps as f64;
    if mean_gap <= 0.0 {
        return 0.0;
    }
    let mut variance = (sum_gap_squares / n_gaps as f64) - (mean_gap * mean_gap);
    if variance < 0.0 {
        variance = 0.0;
    }
    math::sqrt(variance) / mean_gap
}

/// `TRS_numeric_encode_double_fp`: a positive finite value with 32
/// fractional bits, saturating; zero otherwise.
pub(super) fn encode_double_fp(value: f64) -> u64 {
    if value <= 0.0 || !value.is_finite() {
        return 0;
    }
    let scaled = value * Q32_SCALE as f64;
    let rounded = scaled + 0.5;
    if rounded >= 18_446_744_073_709_551_616.0 {
        return u64::MAX;
    }
    rounded as u64
}

/// `decode_u64_fp`: a Q32 value as a double.
pub(super) fn decode_u64_fp(x: u64) -> f64 {
    x as f64 / Q32_SCALE as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kmv_gap_nmad_of_an_arithmetic_progression_is_zero() {
        let mut kmv = Kmv::new();
        for value in 0..100u64 {
            kmv.track(value * 7);
        }
        assert_eq!(kmv.gap_nmad_fp(), 0);
    }

    #[test]
    fn kmv_gap_nmad_needs_two_distinct_values() {
        let mut kmv = Kmv::new();
        assert_eq!(Kmv::new().gap_nmad_fp(), 0);
        kmv.track(5);
        assert_eq!(kmv.gap_nmad_fp(), 0);
        let mut same = Kmv::new();
        same.track(5);
        same.track(5);
        assert_eq!(same.gap_nmad_fp(), 0);
    }

    #[test]
    fn kmv_keeps_the_smallest_hashes_once_full() {
        let mut kmv = Kmv::new();
        for value in 0..10_000u64 {
            kmv.track(value);
        }
        assert_eq!(kmv.heap.len(), KMV_K);
        let largest = kmv.heap.iter().map(|entry| entry.hash).max().unwrap();
        assert_eq!(largest, kmv.heap[0].hash);
        let mut hashes: Vec<u64> = (0..10_000u64).map(xxh3_u64).collect();
        hashes.sort_unstable();
        assert_eq!(largest, hashes[KMV_K - 1]);
        assert!(kmv.gap_nmad_fp() > 0);
    }

    #[test]
    fn sorted_gap_mode_counts_the_most_common_gap() {
        assert_eq!(sorted_gap_mode(core::iter::empty()), 0.0);
        assert_eq!(sorted_gap_mode([5u64].into_iter()), 0.0);
        // 0, 2, 4, ... : every gap is 2.
        assert_eq!(sorted_gap_mode((0..100u64).map(|i| i * 2)), 1.0);
        // Gaps 1,1,2: two of three.
        assert!((sorted_gap_mode([0u64, 1, 2, 4].into_iter()) - 2.0 / 3.0).abs() < 1e-15);
        // More than a batch, descending, with duplicates: the 512 smallest are 0..512.
        let values = (0..3000u64).rev().chain(0..3000);
        assert_eq!(sorted_gap_mode(values), 1.0);
    }

    #[test]
    fn match4_counts_repeated_windows() {
        assert_eq!(lz_matches(&[1, 2, 3]), 0);
        assert_eq!(lz_matches(&[7; 4]), 0);
        // The all-zero window matches an empty slot.
        assert_eq!(lz_matches(&[0; 4]), 1);
        let data: Vec<u8> = (0..4u8).cycle().take(64).collect();
        assert!(lz_matches(&data) > 40);
    }

    #[test]
    fn transition_gap_cv_and_fixed_point_encoding() {
        assert_eq!(transition_gap_cv(0.0, 0.0, 0), 0.0);
        assert_eq!(transition_gap_cv(-1.0, 1.0, 1), 0.0);
        // Gaps 1 and 3: mean 2, variance 1.
        assert_eq!(transition_gap_cv(4.0, 10.0, 2), 0.5);
        // Negative variance clamps to zero.
        assert_eq!(transition_gap_cv(4.0, 7.0, 2), 0.0);
        assert_eq!(encode_double_fp(0.0), 0);
        assert_eq!(encode_double_fp(f64::NAN), 0);
        assert_eq!(encode_double_fp(f64::INFINITY), 0);
        assert_eq!(encode_double_fp(1.0), 1 << 32);
        assert_eq!(encode_double_fp(1e30), u64::MAX);
        assert_eq!(decode_u64_fp(1 << 33), 2.0);
    }
}
