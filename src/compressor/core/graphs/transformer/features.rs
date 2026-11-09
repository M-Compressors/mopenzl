//! Numeric feature extraction of the Transformer selector
//! (`compress/selectors/transformer/numeric_extract.c`).
//!
//! The reference has one scan per width, with different arithmetic that the
//! models were trained on: Q32 fixed point for widths 1 to 4 and doubles for
//! width 8, a full feature set for width 1 and "decision" subsets for the
//! others. Each scan is ported separately and in the reference's operation
//! order; only the fields the scorers and score guards read are computed
//! (the reference leaves the others zero on the paths the selector takes).
//!
//! Elements are read little endian, as the reference does on its supported
//! hosts.

use alloc::vec;
use alloc::vec::Vec;

use super::cardinality::{
    U8_BITMAP_WORDS, U16_BITMAP_WORDS, estimate_cardinality, estimate_cardinality_bitmap,
    estimate_d8_cardinality, estimate_pair_cardinality, estimate_pair_cardinality_bitmap,
};
use super::stats::{self, Kmv};
use super::wide::{self, Q32_SCALE};

/// The legacy feature set (`TRS_NumericFeatures`) of widths 1, 2 and 4, the
/// fields the scorers and score guards consume.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Features {
    /// Smallest element.
    pub(crate) min_u: u64,
    /// Largest element.
    pub(crate) max_u: u64,
    /// Number of elements.
    pub(crate) count: usize,
    /// Longest run of equal values (width 1 only).
    pub(crate) max_run_length: usize,
    /// Zero elements (width 1 only).
    pub(crate) zero_count: usize,
    /// Upward transitions (width 1 only).
    pub(crate) delta_up_count: usize,
    /// Downward transitions (width 1 only).
    pub(crate) delta_down_count: usize,
    /// Smallest signed delta, zero-anchored.
    pub(crate) delta_min_s: i64,
    /// Largest signed delta, zero-anchored.
    pub(crate) delta_max_s: i64,
    /// Mean absolute deviation in Q32 (width 1 only).
    pub(crate) mean_abs_dev_u_fp: u64,
    /// Mean absolute delta in Q32.
    pub(crate) mean_abs_delta_u_fp: u64,
    /// Estimated distinct values.
    pub(crate) cardinality_est: u64,
    /// Estimated distinct consecutive pairs.
    pub(crate) pair_cardinality_est: u64,
    /// Estimated distinct 8-byte windows (widths 1 and 2).
    pub(crate) d8_cardinality_est: u64,
    /// Sampled sorted-gap deviation in Q32.
    pub(crate) sorted_gap_cv_fp: u64,
    /// Coefficient of variation of the gaps between changes, Q32 (width 1 only).
    pub(crate) transition_gap_cv_fp: u64,
    /// Smallest trailing-zero count of the nonzero elements.
    pub(crate) min_lb0: u64,
    /// Matching 4-byte windows (width 1 only).
    pub(crate) match4: u64,
    /// `max_u - min_u`.
    pub(crate) range_u: u64,
    /// The signed range.
    pub(crate) range_s: u64,
    /// Zero fraction in Q32.
    pub(crate) zero_ratio_fp: u64,
    /// Upward-transition fraction in Q32.
    pub(crate) delta_up_ratio_fp: u64,
    /// Downward-transition fraction in Q32.
    pub(crate) delta_down_ratio_fp: u64,
}

/// The width-8 feature set (`TRS_NumericFeaturesV2`), the fields the num64
/// scorer and its guards consume.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FeaturesV2 {
    /// Number of elements.
    pub(crate) count: u64,
    /// Smallest element.
    pub(crate) min_u: u64,
    /// Largest element.
    pub(crate) max_u: u64,
    /// `max_u - min_u`.
    pub(crate) range_u: u64,
    /// The signed range.
    pub(crate) range_s: u64,
    /// Estimated distinct values.
    pub(crate) cardinality_est: u64,
    /// Estimated distinct consecutive pairs.
    pub(crate) pair_cardinality_est: u64,
    /// Smallest trailing-zero count of the nonzero elements.
    pub(crate) min_lb0: u64,
    /// Zero fraction.
    pub(crate) zero_ratio: f64,
    /// Upward-transition fraction.
    pub(crate) delta_up_ratio: f64,
    /// Downward-transition fraction.
    pub(crate) delta_down_ratio: f64,
    /// Mean absolute delta.
    pub(crate) mean_abs_delta_u: f64,
    /// Sampled sorted-gap deviation.
    pub(crate) sorted_gap_nmad: f64,
    /// Fraction of retained gaps equal to the most common one.
    pub(crate) sorted_gap_mode: f64,
}

/// `to_signed`: sign-extends a zero-extended value of `width` bytes.
const fn to_signed(value: u64, width: usize) -> i64 {
    match width {
        1 => value as u8 as i8 as i64,
        2 => value as u16 as i16 as i64,
        4 => value as u32 as i32 as i64,
        _ => value as i64,
    }
}

/// The bit `value` of a presence bitmap.
fn bitmap_set(words: &mut [u64], value: u64) {
    words[(value >> 6) as usize] |= 1 << (value & 63);
}

/// `TRS_numeric_half_width_threshold`: the first value that needs more than
/// half of the element's bits.
pub(super) const fn half_width_threshold(width: usize) -> u64 {
    1u64 << (width * 4)
}

/// `NumericFeatures_extract_num8_from_bytes` (through
/// `TRS_numericFeatures_extract_from_bytes`): the full width-1 features.
pub(crate) fn extract_num8(data: &[u8]) -> Features {
    let n = data.len();
    let mut f = Features {
        count: n,
        ..Features::default()
    };
    if n == 0 {
        return f;
    }
    let mut min_u = u64::from(data[0]);
    let mut max_u = min_u;
    let mut sum_u = min_u;
    let mut min_s = i64::from(data[0] as i8);
    let mut max_s = min_s;
    let mut sum_abs_delta_u: u128 = 0;
    let mut zero_count = usize::from(data[0] == 0);
    let mut current_run_length = 1usize;
    let mut max_run_length = 1usize;
    let mut delta_up_count = 0usize;
    let mut delta_down_count = 0usize;
    let mut delta_min_s = 0i64;
    let mut delta_max_s = 0i64;
    let mut min_lb0 = 64u64;
    let mut nonzero_count = usize::from(data[0] != 0);
    let mut prev_u = u64::from(data[0]);
    let mut prev_s = min_s;
    let mut have_transition_pos = false;
    let mut last_transition_pos = 0usize;
    let mut transition_gap_count = 0usize;
    let mut transition_gap_sum = 0.0f64;
    let mut transition_gap_square_sum = 0.0f64;
    let mut kmv = Kmv::new();
    let mut seen_values = [0u64; U8_BITMAP_WORDS];
    let mut seen_pairs = vec![0u64; U16_BITMAP_WORDS];

    if data[0] != 0 {
        min_lb0 = u64::from(u32::from(data[0]).trailing_zeros());
    }
    bitmap_set(&mut seen_values, u64::from(data[0]));
    kmv.track(u64::from(data[0]));

    for (i, &byte) in data.iter().enumerate().skip(1) {
        let val_u = u64::from(byte);
        let val_s = i64::from(byte as i8);
        let pair_key = (prev_u << 8) | val_u;

        bitmap_set(&mut seen_values, val_u);
        bitmap_set(&mut seen_pairs, pair_key);

        min_u = min_u.min(val_u);
        max_u = max_u.max(val_u);
        min_s = min_s.min(val_s);
        max_s = max_s.max(val_s);
        sum_u = sum_u.wrapping_add(val_u);

        if val_u == 0 {
            zero_count += 1;
        } else {
            nonzero_count += 1;
            min_lb0 = min_lb0.min(u64::from(u32::from(byte).trailing_zeros()));
        }

        let abs_delta_u = wide::abs_diff_u64(val_u, prev_u);
        let delta_s = val_s - prev_s;
        sum_abs_delta_u += u128::from(abs_delta_u);
        delta_min_s = delta_min_s.min(delta_s);
        delta_max_s = delta_max_s.max(delta_s);

        if val_u != prev_u {
            if val_u > prev_u {
                delta_up_count += 1;
            } else {
                delta_down_count += 1;
            }
            if have_transition_pos {
                let gap = (i - last_transition_pos) as f64;
                transition_gap_sum += gap;
                transition_gap_square_sum += gap * gap;
                transition_gap_count += 1;
            }
            last_transition_pos = i;
            have_transition_pos = true;
        }

        if val_u == prev_u {
            current_run_length += 1;
            max_run_length = max_run_length.max(current_run_length);
        } else {
            current_run_length = 1;
        }

        kmv.track(val_u);
        prev_u = val_u;
        prev_s = val_s;
    }

    f.min_u = min_u;
    f.max_u = max_u;
    f.max_run_length = max_run_length;
    f.zero_count = zero_count;
    f.delta_up_count = delta_up_count;
    f.delta_down_count = delta_down_count;
    f.delta_min_s = delta_min_s;
    f.delta_max_s = delta_max_s;
    f.min_lb0 = if nonzero_count == 0 { 0 } else { min_lb0 };
    f.range_u = max_u - min_u;
    f.range_s = wide::i64_abs_diff_to_u64(max_s, min_s);
    f.zero_ratio_fp = (zero_count as u64).wrapping_mul(Q32_SCALE) / n as u64;

    let n_transitions = n - 1;
    if n_transitions > 0 {
        f.delta_up_ratio_fp =
            (delta_up_count as u64).wrapping_mul(Q32_SCALE) / n_transitions as u64;
        f.delta_down_ratio_fp =
            (delta_down_count as u64).wrapping_mul(Q32_SCALE) / n_transitions as u64;
        f.mean_abs_delta_u_fp = wide::scaled_div_to_u64(sum_abs_delta_u, n_transitions);
    }

    let mean_u = sum_u / n as u64;
    let mut sum_abs_dev_u = 0u64;
    for &byte in data {
        sum_abs_dev_u = sum_abs_dev_u.wrapping_add(wide::abs_diff_u64(u64::from(byte), mean_u));
    }
    f.mean_abs_dev_u_fp = wide::scaled_div_to_u64(u128::from(sum_abs_dev_u), n);

    f.cardinality_est = estimate_cardinality_bitmap(&seen_values, n);
    f.pair_cardinality_est = estimate_pair_cardinality_bitmap(&seen_pairs, n);
    f.sorted_gap_cv_fp = kmv.gap_nmad_fp();
    f.transition_gap_cv_fp = stats::encode_double_fp(stats::transition_gap_cv(
        transition_gap_sum,
        transition_gap_square_sum,
        transition_gap_count,
    ));
    f.match4 = stats::lz_matches(data);
    f.d8_cardinality_est = estimate_d8_cardinality(data);
    f
}

/// Reads `bytes` as little-endian elements of `width` bytes.
fn widen(bytes: &[u8], width: usize) -> Vec<u64> {
    bytes
        .chunks_exact(width)
        .map(|chunk| {
            let mut word = [0u8; 8];
            word[..width].copy_from_slice(chunk);
            u64::from_le_bytes(word)
        })
        .collect()
}

/// `NumericFeatures_extract_sub64_decision` (through
/// `TRS_numericFeatures_extract_sub64_decision_from_bytes`): the decision
/// features of width 2 and 4 data (`width` is 2 or 4).
pub(crate) fn extract_sub64_decision(bytes: &[u8], width: usize) -> Features {
    let data = widen(bytes, width);
    let n = data.len();
    let mut f = Features {
        count: n,
        ..Features::default()
    };
    if n == 0 {
        return f;
    }
    let mut min_u = data[0];
    let mut max_u = min_u;
    let mut min_s = to_signed(data[0], width);
    let mut max_s = min_s;
    let mut sum_abs_delta_u: u128 = 0;
    let mut delta_min_s = 0i64;
    let mut delta_max_s = 0i64;
    let mut zero_count = usize::from(data[0] == 0);
    let mut delta_up_count = 0usize;
    let mut delta_down_count = 0usize;
    let mut min_lb0 = 64u64;
    let mut nonzero_count = usize::from(data[0] != 0);
    let mut prev_u = data[0];
    let mut prev_s = min_s;
    let mut kmv = Kmv::new();
    // Width 2 values always fit the 16-bit presence bitmap; the reference
    // checks that per element and the check can never fail.
    let use_seen_u16 = width == 2;
    let mut seen_u16 = vec![0u64; if use_seen_u16 { U16_BITMAP_WORDS } else { 0 }];

    if data[0] != 0 {
        min_lb0 = u64::from((data[0] as u32).trailing_zeros());
    }
    if use_seen_u16 {
        bitmap_set(&mut seen_u16, data[0]);
    }
    kmv.track(data[0]);

    for &val_u in &data[1..] {
        let val_s = to_signed(val_u, width);
        let delta_s = val_s - prev_s;

        if use_seen_u16 {
            bitmap_set(&mut seen_u16, val_u);
        }
        min_u = min_u.min(val_u);
        max_u = max_u.max(val_u);
        min_s = min_s.min(val_s);
        max_s = max_s.max(val_s);

        if val_u == 0 {
            zero_count += 1;
        } else {
            nonzero_count += 1;
            min_lb0 = min_lb0.min(u64::from((val_u as u32).trailing_zeros()));
        }

        delta_min_s = delta_min_s.min(delta_s);
        delta_max_s = delta_max_s.max(delta_s);

        if val_u != prev_u {
            if val_u > prev_u {
                delta_up_count += 1;
            } else {
                delta_down_count += 1;
            }
        }

        sum_abs_delta_u += u128::from(wide::abs_diff_u64(val_u, prev_u));
        kmv.track(val_u);
        prev_u = val_u;
        prev_s = val_s;
    }

    f.min_u = min_u;
    f.max_u = max_u;
    f.delta_min_s = delta_min_s;
    f.delta_max_s = delta_max_s;
    f.min_lb0 = if nonzero_count == 0 { 0 } else { min_lb0 };
    f.range_u = max_u - min_u;
    f.range_s = wide::i64_abs_diff_to_u64(max_s, min_s);
    f.zero_ratio_fp = (zero_count as u64).wrapping_mul(Q32_SCALE) / n as u64;

    let n_transitions = n - 1;
    if n_transitions > 0 {
        f.delta_up_ratio_fp =
            (delta_up_count as u64).wrapping_mul(Q32_SCALE) / n_transitions as u64;
        f.delta_down_ratio_fp =
            (delta_down_count as u64).wrapping_mul(Q32_SCALE) / n_transitions as u64;
        f.mean_abs_delta_u_fp = wide::scaled_div_to_u64(sum_abs_delta_u, n_transitions);
    }

    f.cardinality_est = if use_seen_u16 {
        estimate_cardinality_bitmap(&seen_u16, n)
    } else {
        estimate_cardinality(&data)
    };
    f.pair_cardinality_est = estimate_pair_cardinality(&data);
    f.sorted_gap_cv_fp = kmv.gap_nmad_fp();
    if width == 2 {
        f.d8_cardinality_est = estimate_d8_cardinality(bytes);
    }
    f
}

/// `TRS_numericFeaturesV2_extract_num64_decision`: the decision features of
/// width 8 data.
pub(crate) fn extract_num64_decision(bytes: &[u8]) -> FeaturesV2 {
    let data = widen(bytes, 8);
    let n = data.len();
    let mut f = FeaturesV2 {
        count: n as u64,
        ..FeaturesV2::default()
    };
    if n == 0 {
        return f;
    }
    let mut min_u = data[0];
    let mut max_u = data[0];
    let mut min_s = data[0] as i64;
    let mut max_s = min_s;
    let mut sum_abs_delta_u: u128 = 0;
    let mut zero_count = u64::from(data[0] == 0);
    let mut delta_up_count = 0u64;
    let mut delta_down_count = 0u64;
    let mut min_lb0 = 64u64;
    let mut nonzero_count = u64::from(data[0] != 0);
    let mut prev_u = data[0];
    let mut kmv = Kmv::new();

    if data[0] != 0 {
        min_lb0 = u64::from(data[0].trailing_zeros());
    }
    kmv.track(data[0]);

    for &val_u in &data[1..] {
        let val_s = val_u as i64;
        min_u = min_u.min(val_u);
        max_u = max_u.max(val_u);
        min_s = min_s.min(val_s);
        max_s = max_s.max(val_s);

        if val_u == 0 {
            zero_count += 1;
        } else {
            nonzero_count += 1;
            min_lb0 = min_lb0.min(u64::from(val_u.trailing_zeros()));
        }

        sum_abs_delta_u += u128::from(wide::abs_diff_u64(val_u, prev_u));
        if val_u != prev_u {
            if val_u > prev_u {
                delta_up_count += 1;
            } else {
                delta_down_count += 1;
            }
        }
        kmv.track(val_u);
        prev_u = val_u;
    }

    f.min_u = min_u;
    f.max_u = max_u;
    f.range_u = max_u - min_u;
    f.range_s = wide::i64_abs_diff_to_u64(max_s, min_s);
    f.cardinality_est = estimate_cardinality(&data);
    f.pair_cardinality_est = estimate_pair_cardinality(&data);
    f.min_lb0 = if nonzero_count == 0 { 0 } else { min_lb0 };
    f.sorted_gap_nmad = stats::decode_u64_fp(kmv.gap_nmad_fp());
    f.sorted_gap_mode = stats::sorted_gap_mode(data.iter().copied());
    f.zero_ratio = zero_count as f64 / n as f64;

    if n > 1 {
        let n_transitions = n - 1;
        f.mean_abs_delta_u = wide::u128_to_f64(sum_abs_delta_u) / n_transitions as f64;
        f.delta_up_ratio = delta_up_count as f64 / n_transitions as f64;
        f.delta_down_ratio = delta_down_count as f64 / n_transitions as f64;
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_views_and_thresholds() {
        assert_eq!(to_signed(0xFF, 1), -1);
        assert_eq!(to_signed(0x8000, 2), -32768);
        assert_eq!(to_signed(0xFFFF_FFFF, 4), -1);
        assert_eq!(to_signed(u64::MAX, 8), -1);
        assert_eq!(half_width_threshold(1), 16);
        assert_eq!(half_width_threshold(2), 256);
        assert_eq!(half_width_threshold(4), 65536);
        assert_eq!(half_width_threshold(8), 1 << 32);
    }

    #[test]
    fn empty_inputs_have_no_features() {
        assert_eq!(extract_num8(&[]), Features::default());
        assert_eq!(extract_sub64_decision(&[], 4), Features::default());
        assert_eq!(extract_num64_decision(&[]), FeaturesV2::default());
    }

    #[test]
    fn num8_features_of_a_ramp() {
        let data: Vec<u8> = (0..200u8).collect();
        let f = extract_num8(&data);
        assert_eq!((f.count, f.min_u, f.max_u, f.range_u), (200, 0, 199, 199));
        assert_eq!(f.delta_up_count, 199);
        assert_eq!(f.delta_down_count, 0);
        assert_eq!(f.max_run_length, 1);
        assert_eq!(f.zero_count, 1);
        assert_eq!(f.mean_abs_delta_u_fp, 1 << 32);
        assert_eq!(f.delta_up_ratio_fp, 1 << 32);
        assert_eq!(f.delta_down_ratio_fp, 0);
        assert_eq!(f.range_s, 255);
        assert_eq!(f.min_lb0, 0);
    }

    #[test]
    fn num8_features_of_a_constant_stream() {
        let f = extract_num8(&[9; 50]);
        assert_eq!((f.range_u, f.range_s, f.max_run_length), (0, 0, 50));
        assert_eq!(f.cardinality_est, 1);
        assert_eq!(f.pair_cardinality_est, 1);
        assert_eq!(f.sorted_gap_cv_fp, 0);
        let zeros = extract_num8(&[0; 50]);
        assert_eq!(zeros.min_lb0, 0);
        assert_eq!(zeros.zero_ratio_fp, 1 << 32);
    }

    #[test]
    fn sub64_decision_features_of_wide_and_signed_data() {
        let values: [u16; 4] = [0xFFFF, 1, 0x8000, 7];
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let f = extract_sub64_decision(&bytes, 2);
        assert_eq!((f.min_u, f.max_u, f.count), (1, 0xFFFF, 4));
        assert_eq!(f.range_s, 32_775);
        assert_eq!(f.delta_up_ratio_fp, (1u64 << 32) / 3);
        assert_eq!(f.delta_down_ratio_fp, (2u64 << 32) / 3);
        assert!(f.d8_cardinality_est > 0);
        let wide: Vec<u8> = [5u32, 5, 9, 1 << 31]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let f = extract_sub64_decision(&wide, 4);
        assert_eq!(f.d8_cardinality_est, 0);
        assert_eq!(f.max_u, 1 << 31);
        assert!(f.delta_min_s < 0);
    }

    #[test]
    fn num64_decision_features_use_doubles() {
        let bytes: Vec<u8> = [0u64, u64::MAX, 4, 4]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let f = extract_num64_decision(&bytes);
        assert_eq!((f.min_u, f.max_u, f.range_u), (0, u64::MAX, u64::MAX));
        assert_eq!(f.range_s, 5);
        assert_eq!(f.zero_ratio, 0.25);
        assert_eq!(f.delta_up_ratio, 1.0 / 3.0);
        assert_eq!(f.delta_down_ratio, 1.0 / 3.0);
        assert_eq!(f.min_lb0, 0);
        let single = extract_num64_decision(&7u64.to_le_bytes());
        assert_eq!(single.mean_abs_delta_u, 0.0);
        assert_eq!(single.min_lb0, 0);
        let zero = extract_num64_decision(&0u64.to_le_bytes());
        assert_eq!(zero.min_lb0, 0);
    }
}
