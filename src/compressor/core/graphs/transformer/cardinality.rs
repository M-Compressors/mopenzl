//! The Transformer's cardinality features
//! (`compress/selectors/transformer/cardinality.c`).
//!
//! The hashes and the estimator policy reproduce the values the models were
//! trained on, so they are frozen here rather than shared with other
//! estimators: a saturated 8192-bucket linear count reports 8192, bitmaps
//! feed their distinct values through the estimator instead of counting
//! them exactly.

use alloc::vec::Vec;

use fearless_simd::Level;

use super::stats::xxh3_u64;
use crate::shared::estimate::estimate_cardinality_hashed;
use crate::shared::xxh3;

/// `TRS_CARDINALITY_U8_BITMAP_WORDS`.
pub(super) const U8_BITMAP_WORDS: usize = 4;
/// `TRS_CARDINALITY_U16_BITMAP_WORDS`.
pub(super) const U16_BITMAP_WORDS: usize = 1024;

/// `linear_hash_u64`.
const fn linear_hash(value: u64) -> u64 {
    let hash = value.wrapping_mul(0x9E37_79B1_85EB_CA87);
    hash ^ (hash << 47)
}

/// `XXH3_64bits` of the pair of little-endian 64-bit words.
fn pair_hash(first: u64, second: u64) -> u64 {
    let mut pair = [0u8; 16];
    pair[..8].copy_from_slice(&first.to_le_bytes());
    pair[8..].copy_from_slice(&second.to_le_bytes());
    xxh3::hash64(Level::baseline(), &pair)
}

/// `TRS_estimate_cardinality_u64` and `_u32`: the distinct values of
/// `data`, capped at its length.
pub(super) fn estimate_cardinality(data: &[u64]) -> u64 {
    estimate_cardinality_hashed(
        data.len(),
        data.len(),
        |i| linear_hash(data[i]),
        |i| xxh3_u64(data[i]),
    )
}

/// `TRS_estimate_pair_cardinality_u64` and `_u32`: the distinct consecutive
/// pairs, capped at `len - 1`; zero below two elements.
pub(super) fn estimate_pair_cardinality(data: &[u64]) -> u64 {
    if data.len() < 2 {
        return 0;
    }
    let pairs = data.len() - 1;
    estimate_cardinality_hashed(
        pairs,
        pairs,
        |i| pair_hash(data[i], data[i + 1]),
        |i| pair_hash(data[i], data[i + 1]),
    )
}

/// The indices of the set bits of `words`, ascending.
fn set_bits(words: &[u64]) -> Vec<u32> {
    let mut out = Vec::new();
    for (word_index, &word) in words.iter().enumerate() {
        let mut remaining = word;
        while remaining != 0 {
            out.push((word_index * 64) as u32 + remaining.trailing_zeros());
            remaining &= remaining - 1;
        }
    }
    out
}

/// `TRS_estimate_cardinality_u8_bitmap` and `_u16_bitmap`: the distinct
/// values of a presence bitmap, fed through the estimator and capped at
/// `n_elements`.
pub(super) fn estimate_cardinality_bitmap(words: &[u64], n_elements: usize) -> u64 {
    let values = set_bits(words);
    estimate_cardinality_hashed(
        values.len(),
        n_elements,
        |i| linear_hash(u64::from(values[i])),
        |i| xxh3_u64(u64::from(values[i])),
    )
}

/// `TRS_estimate_pair_cardinality_u8_bitmap`: the distinct byte pairs of a
/// 65536-bit bitmap (`previous << 8 | current`), capped at `n_elements - 1`.
pub(super) fn estimate_pair_cardinality_bitmap(words: &[u64], n_elements: usize) -> u64 {
    if n_elements < 2 {
        return 0;
    }
    let keys = set_bits(words);
    let hash = |i: usize| pair_hash(u64::from(keys[i] >> 8), u64::from(keys[i] & 0xFF));
    estimate_cardinality_hashed(keys.len(), n_elements - 1, hash, hash)
}

/// `TRS_estimate_d8_cardinality`: the distinct overlapping 8-byte windows
/// of the little-endian byte stream, capped at `len - 7`; zero below eight
/// bytes.
pub(super) fn estimate_d8_cardinality(data: &[u8]) -> u64 {
    if data.len() < 8 {
        return 0;
    }
    let windows = data.len() - 7;
    let hash = |i: usize| xxh3::hash64(Level::baseline(), &data[i..i + 8]);
    estimate_cardinality_hashed(windows, windows, hash, hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_cardinality_is_capped_by_the_length() {
        assert_eq!(estimate_cardinality(&[]), 0);
        assert_eq!(estimate_cardinality(&[5]), 1);
        assert_eq!(estimate_cardinality(&[7; 100]), 1);
        let distinct: Vec<u64> = (0..1000).collect();
        let estimate = estimate_cardinality(&distinct);
        assert!((900..=1000).contains(&estimate), "{estimate}");
    }

    #[test]
    fn large_inputs_use_hyper_log_log() {
        let distinct: Vec<u64> = (0..100_000).collect();
        let estimate = estimate_cardinality(&distinct);
        assert!((85_000..=100_000).contains(&estimate), "{estimate}");
    }

    #[test]
    fn pairs_bitmaps_and_windows_use_hyper_log_log_beyond_64k_elements() {
        let data: Vec<u64> = (0..70_000).collect();
        let pairs = estimate_pair_cardinality(&data);
        assert!((60_000..=70_000).contains(&pairs), "{pairs}");
        let mut bitmap = [0u64; U8_BITMAP_WORDS];
        bitmap[0] = u64::MAX;
        bitmap[1] = 0xFF;
        // 72 values observed among 100000 elements: a linear count cannot
        // be used, the bound selects HyperLogLog.
        let estimate = estimate_cardinality_bitmap(&bitmap, 100_000);
        assert!((60..=90).contains(&estimate), "{estimate}");
        let mut pair_bitmap = alloc::vec![0u64; U16_BITMAP_WORDS];
        for key in 0..300u32 {
            pair_bitmap[(key >> 6) as usize] |= 1 << (key & 63);
        }
        let estimate = estimate_pair_cardinality_bitmap(&pair_bitmap, 100_000);
        assert!((250..=350).contains(&estimate), "{estimate}");
    }

    #[test]
    fn pair_cardinality_needs_two_elements() {
        assert_eq!(estimate_pair_cardinality(&[1]), 0);
        assert_eq!(estimate_pair_cardinality(&[1, 2]), 1);
        assert_eq!(estimate_pair_cardinality(&[1, 2, 1, 2, 1, 2, 1]), 2);
    }

    #[test]
    fn bitmaps_estimate_their_set_bits() {
        let mut bitmap = [0u64; U8_BITMAP_WORDS];
        for value in [0u32, 3, 64, 255] {
            bitmap[(value >> 6) as usize] |= 1 << (value & 63);
        }
        assert_eq!(estimate_cardinality_bitmap(&bitmap, 1000), 4);
        assert_eq!(estimate_cardinality_bitmap(&bitmap, 2), 2);
        assert_eq!(set_bits(&bitmap), [0, 3, 64, 255]);
    }

    #[test]
    fn byte_pair_bitmap_estimates_distinct_pairs() {
        let mut pairs = alloc::vec![0u64; U16_BITMAP_WORDS];
        for key in [0x0102u32, 0x0201, 0x0102] {
            pairs[(key >> 6) as usize] |= 1 << (key & 63);
        }
        assert_eq!(estimate_pair_cardinality_bitmap(&pairs, 1), 0);
        assert_eq!(estimate_pair_cardinality_bitmap(&pairs, 10), 2);
        assert_eq!(estimate_pair_cardinality_bitmap(&pairs, 2), 1);
    }

    #[test]
    fn d8_cardinality_counts_distinct_windows() {
        assert_eq!(estimate_d8_cardinality(&[1; 7]), 0);
        assert_eq!(estimate_d8_cardinality(&[1; 8]), 1);
        assert_eq!(estimate_d8_cardinality(&[1; 100]), 1);
        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        assert!(estimate_d8_cardinality(&data) > 200);
    }
}
