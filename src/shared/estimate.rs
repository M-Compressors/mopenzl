//! Statistics estimators (`shared/estimate.c`): cardinality by linear
//! counting and HyperLogLog, stream dimensionality, element ranges and the
//! float-width guess.
//!
//! The numeric selectors rank candidate transforms by these estimates, so
//! they must match the reference bit for bit. Floating point is therefore
//! ported expression by expression in the reference's order, with the same
//! libm entry points (`log`, `exp`, `pow`, `sqrt`) and no fused
//! multiply-adds; the reference is built with `-ffp-contract=off` for the
//! same reason.
//!
//! # `no_std`
//!
//! With `std`, `log`, `exp`, `pow` and `sqrt` come from the platform libm,
//! which is what the reference links, so results agree on the same host.
//! `no_std` builds use the `libm` crate (which `fearless_simd` already pulls
//! in); its `exp`, `log` and `pow` can differ from a platform libm in the
//! last bit, so a `no_std` build has no byte-identity guarantee for the
//! decisions that depend on these estimates.
//!
//! Elements are read little endian, as the reference does on its supported
//! (little-endian) hosts.

use alloc::vec::Vec;

use fearless_simd::Level;

use super::xxh3;

/// The libm entry points the reference calls: the platform's through `std`,
/// the `libm` crate without it.
mod math {
    /// `log`.
    pub(super) fn ln(x: f64) -> f64 {
        #[cfg(not(feature = "no_std"))]
        return x.ln();
        #[cfg(feature = "no_std")]
        return libm::log(x);
    }

    /// `exp`.
    pub(super) fn exp(x: f64) -> f64 {
        #[cfg(not(feature = "no_std"))]
        return x.exp();
        #[cfg(feature = "no_std")]
        return libm::exp(x);
    }

    /// `pow`.
    pub(super) fn pow(x: f64, y: f64) -> f64 {
        #[cfg(not(feature = "no_std"))]
        return x.powf(y);
        #[cfg(feature = "no_std")]
        return libm::pow(x, y);
    }

    /// `sqrt` (correctly rounded everywhere).
    pub(super) fn sqrt(x: f64) -> f64 {
        #[cfg(not(feature = "no_std"))]
        return x.sqrt();
        #[cfg(feature = "no_std")]
        return libm::sqrt(x);
    }
}

/// `XXH3_64bits`.
fn xxh3_64(bytes: &[u8]) -> u64 {
    xxh3::hash64(Level::baseline(), bytes)
}

/// Reads the little-endian unsigned integer of `bytes` (at most 8 bytes).
fn read_le(bytes: &[u8]) -> u64 {
    let mut word = [0u8; 8];
    word[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(word)
}

/// `ZL_ElementRange`: the smallest and largest element.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ElementRange {
    /// The smallest element, 0 for an empty stream.
    pub(crate) min: u64,
    /// The largest element, 0 for an empty stream.
    pub(crate) max: u64,
}

/// `ZL_computeUnsignedRange`: the exact range of `src`, a stream of
/// little-endian unsigned integers of `elt_size` bytes (1, 2, 4 or 8; any
/// other width gives the empty range, where the reference asserts).
pub(crate) fn compute_unsigned_range(src: &[u8], elt_size: usize) -> ElementRange {
    if !matches!(elt_size, 1 | 2 | 4 | 8) {
        return ElementRange::default();
    }
    let mut min = u64::MAX;
    let mut max = 0u64;
    let mut any = false;
    for element in src.chunks_exact(elt_size) {
        let value = read_le(element);
        any = true;
        if value < min {
            min = value;
        }
        if value > max {
            max = value;
        }
    }
    if !any {
        min = 0;
    }
    ElementRange { min, max }
}

/// `ZL_CardinalityEstimate`: an estimate of the number of distinct elements,
/// with hard bounds and error bars.
///
/// `lower_bound <= estimate_lower_bound <= estimate <= estimate_upper_bound
/// <= upper_bound`. The hard bounds are 0 and `u64::MAX` when unknown; the
/// estimate bounds are soft.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CardinalityEstimate {
    /// A hard lower bound.
    pub(crate) lower_bound: u64,
    /// A soft lower bound on the estimate.
    pub(crate) estimate_lower_bound: u64,
    /// The estimate.
    pub(crate) estimate: u64,
    /// A soft upper bound on the estimate.
    pub(crate) estimate_upper_bound: u64,
    /// A hard upper bound.
    pub(crate) upper_bound: u64,
}

/// `ZL_ESTIMATE_CARDINALITY_ANY`.
// Ported from `estimate.c`; no graph reads it, so only the tests compile it.
#[cfg(test)]
pub(crate) const ESTIMATE_CARDINALITY_ANY: u64 = 0;
/// `ZL_ESTIMATE_CARDINALITY_16BITS`.
pub(crate) const ESTIMATE_CARDINALITY_16BITS: u64 = 65_536;

/// `ZL_MAX` on doubles: `(a > b) ? a : b`, which differs from `f64::max` for
/// NaN.
fn max_f64(a: f64, b: f64) -> f64 {
    if a > b { a } else { b }
}

/// `LinearCount_estimateValue`: `nbBuckets * log(nbBuckets / nbZeros)`.
fn linear_count_value(nb_zeros: usize, nb_buckets: usize) -> f64 {
    (nb_buckets as f64) * math::ln((nb_buckets as f64) / (nb_zeros as f64))
}

/// `LinearCount_estimateImpl`.
fn linear_count_estimate(nb_zeros: usize, nb_buckets: usize) -> CardinalityEstimate {
    let mut estimate = CardinalityEstimate {
        lower_bound: (nb_buckets - nb_zeros) as u64,
        upper_bound: u64::MAX,
        ..CardinalityEstimate::default()
    };
    if nb_zeros == 0 {
        // The map is saturated: the cardinality is unknown.
        estimate.estimate_lower_bound = u64::MAX;
        estimate.estimate = u64::MAX;
        estimate.estimate_upper_bound = u64::MAX;
    } else {
        let cardinality = linear_count_value(nb_zeros, nb_buckets);
        let t = cardinality / (nb_buckets as f64);
        // Double the error, and never report less than 10%.
        let error_ratio = 2.0
            * max_f64(
                0.1,
                math::sqrt(nb_buckets as f64) * math::pow(math::exp(t) - t - 1.0, 0.5)
                    / cardinality,
            );
        let error = error_ratio * cardinality;
        // Conversions saturate where the reference's are undefined.
        let truncated = cardinality as u64;
        estimate.estimate_lower_bound =
            ((cardinality - error) as u64).min(truncated.wrapping_sub(1));
        estimate.estimate = truncated;
        estimate.estimate_upper_bound =
            ((cardinality + error) as u64).max(truncated.wrapping_add(1));
    }
    estimate
}

/// `LinearCount_hash`.
const fn linear_count_hash(value: u64) -> u64 {
    let hash = value.wrapping_mul(0x9E37_79B1_85EB_CA87);
    hash ^ (hash << 47)
}

/// A bitmap of `1 << size_log` buckets (`LinearCount`).
struct LinearCount {
    size_log: u32,
    bits: Vec<u8>,
}

impl LinearCount {
    fn new(size_log: u32) -> Self {
        Self {
            size_log,
            bits: alloc::vec![0; 1 << size_log],
        }
    }

    fn bump(&mut self, hash: u64) {
        let index = (hash >> (64 - self.size_log)) as usize;
        self.bits[index] = 1;
    }

    fn nb_zeros(&self) -> usize {
        self.bits.iter().filter(|&&bit| bit == 0).count()
    }

    fn estimate(&self) -> CardinalityEstimate {
        linear_count_estimate(self.nb_zeros(), 1 << self.size_log)
    }
}

/// `HLL_BUCKET_LOG`.
const HLL_BUCKET_LOG: u32 = 10;
/// `HLL_NB_BUCKETS`.
const HLL_NB_BUCKETS: usize = 1 << HLL_BUCKET_LOG;
/// `HLL_ALPHA`.
const HLL_ALPHA: f64 = 0.7213 / (1.0 + (1.079 / (HLL_NB_BUCKETS as f64)));

/// `HyperLogLog_hash`: XXH3 of the little-endian bytes of the value.
fn hll_hash(value: u64) -> u64 {
    xxh3_64(&value.to_le_bytes())
}

/// `HyperLogLog`: 1024 buckets of one bit per observed trailing-zero count.
struct HyperLogLog {
    buckets: [u32; HLL_NB_BUCKETS],
}

impl HyperLogLog {
    const fn new() -> Self {
        Self {
            buckets: [0; HLL_NB_BUCKETS],
        }
    }

    fn bump(&mut self, hash: u64) {
        let bucket = (hash >> (64 - HLL_BUCKET_LOG)) as usize;
        // The lowest set bit, truncated to 32 bits like the reference.
        let update = hash & !hash.wrapping_sub(1);
        self.buckets[bucket] |= update as u32;
    }

    fn nb_zeros(&self) -> usize {
        self.buckets.iter().filter(|&&bucket| bucket == 0).count()
    }

    fn harmonic_mean(&self) -> f64 {
        let mut mean = 0.0f64;
        for &bucket in &self.buckets {
            let bits = 32 - bucket.leading_zeros();
            mean += math::pow(2.0, -f64::from(bits));
        }
        1.0 / mean
    }

    fn estimate(&self) -> CardinalityEstimate {
        let nb_zeros = self.nb_zeros();
        if nb_zeros == 0 {
            hll_estimate(HLL_NB_BUCKETS, self.harmonic_mean(), HLL_ALPHA)
        } else {
            linear_count_estimate(nb_zeros, HLL_NB_BUCKETS)
        }
    }
}

/// `HyperLogLog_estimateImpl`.
fn hll_estimate(nb_buckets: usize, harmonic_mean: f64, alpha: f64) -> CardinalityEstimate {
    let nb_buckets_squared = (nb_buckets as f64) * (nb_buckets as f64);
    let cardinality = alpha * nb_buckets_squared * harmonic_mean;
    let error_ratio = 2.0 * 1.04 / math::sqrt(HLL_NB_BUCKETS as f64);
    let error = cardinality * error_ratio;
    CardinalityEstimate {
        lower_bound: nb_buckets as u64,
        estimate_lower_bound: (cardinality - error) as u64,
        estimate: cardinality as u64,
        estimate_upper_bound: (cardinality + error) as u64,
        upper_bound: u64::MAX,
    }
}

/// `ZS_estimateLinearSizeLog`.
fn linear_size_log(early_exit: u64) -> u32 {
    let value = early_exit as u32;
    let high_bit = 31 - value.leading_zeros();
    let nb_bits = if value.is_power_of_two() {
        high_bit
    } else {
        high_bit + 1
    };
    nb_bits.clamp(5, 13)
}

/// `ZS_CardinalityEstimate_fixup`: clamps the bounds into a consistent chain.
fn fixup(mut estimate: CardinalityEstimate, upper_bound: u64) -> CardinalityEstimate {
    if estimate.upper_bound > upper_bound {
        estimate.upper_bound = upper_bound;
    }
    if estimate.estimate_upper_bound > estimate.upper_bound {
        estimate.estimate_upper_bound = estimate.upper_bound;
    }
    if estimate.estimate > estimate.estimate_upper_bound {
        estimate.estimate = estimate.estimate_upper_bound;
    }
    if estimate.estimate_lower_bound > estimate.estimate {
        estimate.estimate_lower_bound = estimate.estimate;
    }
    if estimate.estimate_lower_bound < estimate.lower_bound {
        estimate.estimate_lower_bound = estimate.lower_bound;
    }
    estimate
}

/// `ZS_estimate1`: exact, by presence table.
fn estimate_bytes(src: &[u8]) -> CardinalityEstimate {
    let mut present = [false; 256];
    for &byte in src {
        present[usize::from(byte)] = true;
    }
    let cardinality = present.iter().filter(|&&p| p).count() as u64;
    CardinalityEstimate {
        lower_bound: cardinality,
        estimate_lower_bound: cardinality,
        estimate: cardinality,
        estimate_upper_bound: cardinality,
        upper_bound: cardinality,
    }
}

/// `ZS_estimate_internal` for 2, 4 and 8 byte elements.
fn estimate_words(src: &[u8], elt_size: usize, early_exit: u64) -> CardinalityEstimate {
    let elements = src.chunks_exact(elt_size).map(read_le);
    if early_exit <= ESTIMATE_CARDINALITY_16BITS {
        let mut count = LinearCount::new(linear_size_log(early_exit));
        for value in elements {
            count.bump(linear_count_hash(value));
        }
        count.estimate()
    } else {
        let mut hll = HyperLogLog::new();
        for value in elements {
            hll.bump(hll_hash(value));
        }
        hll.estimate()
    }
}

/// `ZL_estimateCardinality_fixed`: the cardinality of `src`, a packed array
/// of little-endian unsigned integers of `elt_size` bytes (1, 2, 4 or 8;
/// `None` for any other width).
///
/// `early_exit` is the largest cardinality the caller cares about; it picks
/// the sketch (linear counting up to 65536, HyperLogLog above) and may be
/// [`ESTIMATE_CARDINALITY_ANY`].
pub(crate) fn estimate_cardinality_fixed(
    src: &[u8],
    elt_size: usize,
    early_exit: u64,
) -> Option<CardinalityEstimate> {
    if !matches!(elt_size, 1 | 2 | 4 | 8) {
        return None;
    }
    let nb_elts = (src.len() / elt_size) as u64;
    let early_exit = if early_exit == 0 {
        u64::MAX
    } else {
        early_exit
    };
    let mut upper_bound = if elt_size == 8 {
        u64::MAX
    } else {
        1u64 << (elt_size * 8)
    };
    upper_bound = upper_bound.min(nb_elts);
    let early_exit = early_exit.min(upper_bound);
    if nb_elts == 0 {
        return Some(CardinalityEstimate::default());
    }
    let src = &src[..(nb_elts as usize) * elt_size];
    let estimate = if elt_size == 1 {
        estimate_bytes(src)
    } else {
        estimate_words(src, elt_size, early_exit)
    };
    Some(fixup(estimate, upper_bound))
}

/// `ZL_estimateCardinality_variable`: the cardinality of elements of any
/// size, hashed with XXH3.
// Ported from `estimate.c`; no graph reads it, so only the tests compile it.
#[cfg(test)]
pub(crate) fn estimate_cardinality_variable(
    elements: &[&[u8]],
    early_exit: u64,
) -> CardinalityEstimate {
    let nb_elts = elements.len() as u64;
    let early_exit = if early_exit == 0 {
        u64::MAX
    } else {
        early_exit
    };
    let early_exit = early_exit.min(nb_elts);
    if nb_elts == 0 {
        return CardinalityEstimate::default();
    }
    let estimate = if early_exit <= ESTIMATE_CARDINALITY_16BITS {
        let mut count = LinearCount::new(linear_size_log(early_exit));
        for element in elements {
            count.bump(xxh3_64(element));
        }
        count.estimate()
    } else {
        let mut hll = HyperLogLog::new();
        for element in elements {
            hll.bump(xxh3_64(element));
        }
        hll.estimate()
    };
    fixup(estimate, nb_elts)
}

/// `LinearCount_estimateRounded`: rounds to nearest; a saturated map reports
/// its bucket count.
fn linear_count_rounded(nb_zeros: usize, nb_buckets: usize) -> u64 {
    if nb_zeros == 0 {
        return nb_buckets as u64;
    }
    (linear_count_value(nb_zeros, nb_buckets) + 0.5) as u64
}

/// `HASHED_LINEAR_COUNT_SIZE_LOG`.
const HASHED_LINEAR_COUNT_SIZE_LOG: u32 = 13;

/// `ZL_estimateCardinality_hashed`: the cardinality of derived elements the
/// caller hashes itself.
///
/// `linear_count_hash` and `hyper_log_log_hash` are called with
/// `0..nb_hashes` in order (only the one the bound selects). Linear counting
/// is used when `cardinality_upper_bound` is at most 65536, always with 8192
/// buckets and rounding to nearest; HyperLogLog otherwise, truncating. The
/// result is capped at `cardinality_upper_bound`.
pub(crate) fn estimate_cardinality_hashed(
    nb_hashes: usize,
    cardinality_upper_bound: usize,
    mut linear_count_hash: impl FnMut(usize) -> u64,
    mut hyper_log_log_hash: impl FnMut(usize) -> u64,
) -> u64 {
    if nb_hashes == 0 || cardinality_upper_bound == 0 {
        return 0;
    }
    let estimate = if cardinality_upper_bound as u64 <= ESTIMATE_CARDINALITY_16BITS {
        let mut count = LinearCount::new(HASHED_LINEAR_COUNT_SIZE_LOG);
        for i in 0..nb_hashes {
            count.bump(linear_count_hash(i));
        }
        linear_count_rounded(count.nb_zeros(), 1 << HASHED_LINEAR_COUNT_SIZE_LOG)
    } else {
        let mut hll = HyperLogLog::new();
        for i in 0..nb_hashes {
            hll.bump(hyper_log_log_hash(i));
        }
        let nb_zeros = hll.nb_zeros();
        if nb_zeros > 0 {
            linear_count_rounded(nb_zeros, HLL_NB_BUCKETS)
        } else {
            hll_estimate(HLL_NB_BUCKETS, hll.harmonic_mean(), HLL_ALPHA).estimate
        }
    };
    estimate.min(cardinality_upper_bound as u64)
}

/// `ZL_DimensionalityStatus`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
// Ported from `estimate.c`; no graph reads it, so only the tests compile it.
#[cfg(test)]
pub(crate) enum DimensionalityStatus {
    /// No dimensionality detected.
    #[default]
    None,
    /// The data may be 2D, but not strongly.
    Possibly2D,
    /// The data is very likely 2D.
    Likely2D,
}

/// `ZL_DimensionalityEstimate`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
// Ported from `estimate.c`; no graph reads it, so only the tests compile it.
#[cfg(test)]
pub(crate) struct DimensionalityEstimate {
    /// What the dimensionality is.
    pub(crate) dimensionality: DimensionalityStatus,
    /// The estimated stride, in elements.
    pub(crate) stride: usize,
    /// The matches at an exact multiple of the stride.
    pub(crate) stride_matches: usize,
    /// The matches at any offset.
    pub(crate) total_matches: usize,
}

/// `ZL_hash2` .. `ZL_hash8` for the element sizes the estimator uses.
// Ported from `estimate.c`; no graph reads it, so only the tests compile it.
#[cfg(test)]
fn element_hash(element: &[u8], hash_log: u32) -> usize {
    match element.len() {
        1 => usize::from(element[0]) & ((1 << hash_log) - 1),
        2 => {
            let u = read_le(element) as u32;
            ((u << 16).wrapping_mul(506_832_829) >> (32 - hash_log)) as usize
        }
        // The reference reads four bytes; the fourth is shifted out.
        3 => {
            let u = read_le(element) as u32;
            ((u << 8).wrapping_mul(506_832_829) >> (32 - hash_log)) as usize
        }
        4 => {
            let u = read_le(element) as u32;
            (u.wrapping_mul(2_654_435_761) >> (32 - hash_log)) as usize
        }
        _ => (read_le(element).wrapping_mul(0xCF1B_BCDC_B7A5_6463) >> (64 - hash_log)) as usize,
    }
}

/// `isPeak`.
// Ported from `estimate.c`; no graph reads it, so only the tests compile it.
#[cfg(test)]
fn is_peak(freq: &[u32], pos: usize, size: usize) -> bool {
    let sum = if pos < 8 {
        freq[pos - 1].wrapping_add(freq[pos + 1])
    } else {
        let mut sum = freq[pos - 2].wrapping_add(freq[pos - 1]);
        if pos + 2 >= size {
            sum = sum.wrapping_mul(2);
        } else {
            sum = sum.wrapping_add(freq[pos + 1]).wrapping_add(freq[pos + 2]);
        }
        sum
    };
    freq[pos] > sum
}

/// `ZL_estimateDimensionality`: whether `src`, `elt_size`-byte elements
/// (1, 2, 3, 4 or 8; `None` otherwise), looks like rows of a 2D array, and
/// with which stride.
// Ported from `estimate.c`; no graph reads it, so only the tests compile it.
#[cfg(test)]
pub(crate) fn estimate_dimensionality(
    src: &[u8],
    elt_size: usize,
) -> Option<DimensionalityEstimate> {
    if !matches!(elt_size, 1 | 2 | 3 | 4 | 8) {
        return None;
    }
    const CHAIN_LOG: u32 = 10;
    const MAX_OFFSET: u32 = 1 << CHAIN_LOG;
    const CHAIN_MASK: u32 = (1 << CHAIN_LOG) - 1;
    let hash_log: u32 = if elt_size == 1 { 8 } else { 10 };
    let mut hash_table = alloc::vec![0u32; 1 << hash_log];
    let mut chain_table = alloc::vec![0u32; 1 << CHAIN_LOG];
    let mut freq_table = alloc::vec![0u32; (1 << CHAIN_LOG) + 1];
    let mut matches = 0u32;

    let elements: Vec<&[u8]> = src.chunks_exact(elt_size).collect();
    for (pos, ip) in elements.iter().enumerate() {
        let pos = pos as u32;
        let hash = element_hash(ip, hash_log);
        let mut found = hash_table[hash];
        let min_match = if pos > MAX_OFFSET {
            pos - MAX_OFFSET
        } else {
            1
        };
        while found >= min_match {
            let next = chain_table[(found & CHAIN_MASK) as usize];
            if elements[found as usize] == *ip {
                freq_table[(pos - found) as usize] += 1;
                matches += 1;
            }
            found = next;
        }
        chain_table[(pos & CHAIN_MASK) as usize] = found;
        hash_table[hash] = pos;
    }

    let min_share = matches >> 4;
    let mut stride = 0usize;
    let mut stride_matches = 0usize;
    let size = MAX_OFFSET as usize + 1;
    for o in 2..=MAX_OFFSET as usize {
        if !is_peak(&freq_table, o, size) {
            continue;
        }
        let mut share = freq_table[o];
        let mut m = 2;
        while o * m <= MAX_OFFSET as usize {
            if freq_table[o * m] >= freq_table[o].wrapping_mul(2) {
                // The next peak is much stronger: do not count this one.
                m += 1;
                continue;
            }
            if is_peak(&freq_table, o * m, size) {
                share = share.wrapping_add(freq_table[o * m]);
            }
            m += 1;
        }
        if share as usize > stride_matches {
            stride = o;
            stride_matches = share as usize;
        }
    }
    let dimensionality = if stride == 0 {
        DimensionalityStatus::None
    } else if stride_matches as u64 > u64::from(min_share) {
        DimensionalityStatus::Likely2D
    } else {
        DimensionalityStatus::Possibly2D
    };
    Some(DimensionalityEstimate {
        dimensionality,
        stride,
        stride_matches,
        total_matches: matches as usize,
    })
}

/// `LOG2_TABLE`, generated in Python as `0x80000 ^ int(math.log2(i) *
/// (1 << 16))` for `i in range(1 << 8, 1 << 9)`.
// Ported from `estimate.c`; no graph reads it, so only the tests compile it.
#[cfg(test)]
const LOG2_TABLE: [u16; 256] = [
    0, 368, 735, 1101, 1465, 1828, 2190, 2550, 2909, 3266, 3622, 3977, 4331, 4683, 5034, 5383,
    5731, 6078, 6424, 6769, 7112, 7454, 7794, 8134, 8472, 8809, 9145, 9480, 9813, 10146, 10477,
    10807, 11136, 11463, 11790, 12115, 12440, 12763, 13085, 13406, 13726, 14045, 14363, 14680,
    14995, 15310, 15624, 15936, 16248, 16558, 16868, 17176, 17484, 17790, 18096, 18400, 18704,
    19006, 19308, 19608, 19908, 20207, 20505, 20801, 21097, 21392, 21686, 21980, 22272, 22563,
    22854, 23143, 23432, 23720, 24007, 24293, 24578, 24862, 25146, 25429, 25710, 25991, 26272,
    26551, 26829, 27107, 27384, 27660, 27935, 28210, 28483, 28756, 29028, 29300, 29570, 29840,
    30109, 30377, 30644, 30911, 31177, 31442, 31707, 31971, 32234, 32496, 32757, 33018, 33278,
    33538, 33796, 34054, 34312, 34568, 34824, 35079, 35334, 35588, 35841, 36093, 36345, 36596,
    36847, 37096, 37346, 37594, 37842, 38089, 38336, 38582, 38827, 39071, 39315, 39559, 39801,
    40044, 40285, 40526, 40766, 41006, 41245, 41483, 41721, 41959, 42195, 42431, 42667, 42902,
    43136, 43370, 43603, 43836, 44068, 44299, 44530, 44760, 44990, 45219, 45448, 45676, 45904,
    46131, 46357, 46583, 46808, 47033, 47257, 47481, 47704, 47927, 48149, 48371, 48592, 48813,
    49033, 49253, 49472, 49690, 49909, 50126, 50343, 50560, 50776, 50992, 51207, 51421, 51635,
    51849, 52062, 52275, 52487, 52699, 52910, 53121, 53331, 53541, 53751, 53960, 54168, 54376,
    54584, 54791, 54998, 55204, 55410, 55615, 55820, 56024, 56228, 56432, 56635, 56837, 57040,
    57242, 57443, 57644, 57844, 58044, 58244, 58443, 58642, 58841, 59039, 59236, 59433, 59630,
    59827, 60023, 60218, 60413, 60608, 60802, 60996, 61190, 61383, 61576, 61768, 61960, 62152,
    62343, 62534, 62724, 62914, 63104, 63293, 63482, 63671, 63859, 64047, 64234, 64421, 64608,
    64794, 64980, 65165, 65351,
];

/// `LOG2_TABLE_PRECISION_BITS`.
// Ported from `estimate.c`; no graph reads it, so only the tests compile it.
#[cfg(test)]
const LOG2_TABLE_PRECISION_BITS: u32 = 8;
/// `LOG2_TABLE_MULTIPLIER`.
// Ported from `estimate.c`; no graph reads it, so only the tests compile it.
#[cfg(test)]
const LOG2_TABLE_MULTIPLIER: i64 = 1 << 16;

/// `ZL_calculateEntropy`: an approximation (within about 0.1%) of the Shannon
/// entropy, in bits per element, of the distribution `count`.
// Ported from `estimate.c`; no graph reads it, so only the tests compile it.
#[cfg(test)]
fn calculate_entropy(count: &[u32; 256], total_elements: usize) -> f64 {
    if total_elements == 0 {
        return 0.0;
    }
    let mut int_entropy = 0i64;
    let normalize = (1i64 << 62) / (total_elements as i64);
    for &curr_count in count {
        let mut log_index = i64::from(curr_count).wrapping_mul(normalize);
        let clz = ((log_index as u64) | (1 << (LOG2_TABLE_PRECISION_BITS + 1))).leading_zeros();
        let shift = (62 - LOG2_TABLE_PRECISION_BITS) as i64 - i64::from(clz) + 1;
        log_index >>= shift;
        log_index &= (1 << LOG2_TABLE_PRECISION_BITS) - 1;
        int_entropy = int_entropy.wrapping_add((-i64::from(curr_count)).wrapping_mul(
            i64::from(LOG2_TABLE[log_index as usize]) - i64::from(clz) * LOG2_TABLE_MULTIPLIER,
        ));
    }
    (int_entropy as f64)
        / ((total_elements as u64).wrapping_mul(LOG2_TABLE_MULTIPLIER as u64) as f64)
        - 1.0
}

/// `combinedHistogramEntropy`: the entropy, times `src_size`, of the sum of
/// the histograms selected by the bits of `selector`.
// Ported from `estimate.c`; no graph reads it, so only the tests compile it.
#[cfg(test)]
fn combined_histogram_entropy(histograms: &[[u16; 256]; 8], src_size: usize, selector: u32) -> f64 {
    let mut histogram = [0u32; 256];
    for (h, source) in histograms.iter().enumerate() {
        if selector & (1 << h) != 0 {
            for (total, &count) in histogram.iter_mut().zip(source) {
                *total += u32::from(count);
            }
        }
    }
    calculate_entropy(&histogram, src_size) * (src_size as f64)
}

/// `ZL_guessFloatWidth`: the likely width in bytes of floating-point data in
/// `src` (1, 2, 4 or 8); any width for data that is not floating point.
// Ported from `estimate.c`; no graph reads it, so only the tests compile it.
#[cfg(test)]
pub(crate) fn guess_float_width(src: &[u8]) -> usize {
    let mut src_size = src.len();
    if !src_size.is_multiple_of(2) {
        return 1;
    }
    let can_be_float32 = src_size.is_multiple_of(4);
    let can_be_float64 = src_size.is_multiple_of(8);

    src_size = src_size.min(32_768);
    src_size &= !7usize;

    let mut histograms = [[0u16; 256]; 8];
    for group in src[..src_size].as_chunks::<8>().0 {
        for (histogram, &byte) in histograms.iter_mut().zip(group) {
            histogram[usize::from(byte)] += 1;
        }
    }

    let u8_entropy = combined_histogram_entropy(&histograms, src_size, 0xFF);
    let f16_entropy = combined_histogram_entropy(&histograms, src_size / 2, 0x55)
        + combined_histogram_entropy(&histograms, src_size / 2, 0xAA);
    if u8_entropy * 0.99 <= f16_entropy {
        return 1;
    }
    if !can_be_float32 {
        return 2;
    }
    let f32_entropy = combined_histogram_entropy(&histograms, src_size / 4, 0x11)
        + combined_histogram_entropy(&histograms, src_size / 4, 0x22)
        + combined_histogram_entropy(&histograms, src_size / 4, 0x44)
        + combined_histogram_entropy(&histograms, src_size / 4, 0x88);
    if f16_entropy * 0.99 <= f32_entropy {
        return 2;
    }
    if !can_be_float64 {
        return 4;
    }
    let mut f64_entropy = 0.0f64;
    for h in 0..8 {
        f64_entropy += combined_histogram_entropy(&histograms, src_size / 8, 1 << h);
    }
    if f32_entropy * 0.99 <= f64_entropy {
        return 4;
    }
    8
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use openzl_ffi::enc_structure as c;

    fn noise(len: usize, seed: u32) -> Vec<u8> {
        let mut state = seed.wrapping_mul(2_654_435_761) | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state >> 24) as u8
            })
            .collect()
    }

    /// Little-endian integers of `width` bytes drawn from `distinct` values.
    fn values(n: usize, width: usize, distinct: u64, seed: u32) -> Vec<u8> {
        noise(n * 8, seed)
            .as_chunks::<8>()
            .0
            .iter()
            .flat_map(|chunk| {
                let v = u64::from_le_bytes(*chunk) % distinct;
                v.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes()[..width].to_vec()
            })
            .collect()
    }

    fn as_array(e: CardinalityEstimate) -> [u64; 5] {
        [
            e.lower_bound,
            e.estimate_lower_bound,
            e.estimate,
            e.estimate_upper_bound,
            e.upper_bound,
        ]
    }

    #[test]
    fn fixed_cardinality_matches_the_reference() {
        let n = if cfg!(miri) { 300 } else { 20_000 };
        for width in [1, 2, 4, 8] {
            for distinct in [1u64, 3, 50, 200, 255, 256, 1000, 5000, 70_000, 1 << 40] {
                let data = values(n, width, distinct, 7);
                for early in [0, 1, 100, 128, 256, 5000, 65_536, 65_537, 1 << 20] {
                    let ours = estimate_cardinality_fixed(&data, width, early).unwrap();
                    assert_eq!(
                        as_array(ours),
                        c::cardinality_fixed(&data, width, early),
                        "w{width} d{distinct} early{early}"
                    );
                }
            }
            let empty = estimate_cardinality_fixed(&[], width, 0).unwrap();
            assert_eq!(as_array(empty), c::cardinality_fixed(&[], width, 0));
            let one = values(1, width, 5, 1);
            assert_eq!(
                as_array(estimate_cardinality_fixed(&one, width, 0).unwrap()),
                c::cardinality_fixed(&one, width, 0)
            );
        }
        assert!(estimate_cardinality_fixed(&[0; 12], 3, 0).is_none());
        assert_eq!(ESTIMATE_CARDINALITY_ANY, 0);
    }

    #[test]
    fn large_inputs_use_hyper_log_log_like_the_reference() {
        let n = if cfg!(miri) { 400 } else { 90_000 };
        for width in [4, 8] {
            for distinct in [1u64 << 40, 20_000] {
                let data = values(n, width, distinct, 9);
                for early in [0, 70_000, 1 << 24] {
                    let ours = estimate_cardinality_fixed(&data, width, early).unwrap();
                    assert_eq!(
                        as_array(ours),
                        c::cardinality_fixed(&data, width, early),
                        "w{width} d{distinct} early{early}"
                    );
                }
            }
        }
    }

    #[test]
    fn variable_cardinality_matches_the_reference() {
        let n = if cfg!(miri) { 200 } else { 6000 };
        for distinct in [1usize, 40, 300, 4000, 90_000] {
            let pieces: Vec<Vec<u8>> = (0..n)
                .map(|i| alloc::format!("item-{}", (i * 7919) % distinct).into_bytes())
                .collect();
            let flat: Vec<u8> = pieces.concat();
            let sizes: Vec<usize> = pieces.iter().map(Vec::len).collect();
            let slices: Vec<&[u8]> = pieces.iter().map(Vec::as_slice).collect();
            for early in [0, 100, 65_536, 100_000] {
                assert_eq!(
                    as_array(estimate_cardinality_variable(&slices, early)),
                    c::cardinality_variable(&flat, &sizes, early),
                    "d{distinct} early{early}"
                );
            }
        }
        assert_eq!(
            estimate_cardinality_variable(&[], 0),
            CardinalityEstimate::default()
        );
    }

    #[test]
    fn hashed_cardinality_matches_the_reference() {
        let n = if cfg!(miri) { 500 } else { 30_000 };
        let hashes: Vec<u64> = noise(n * 8, 3)
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_le_bytes(*c))
            .collect();
        for distinct in [1usize, 100, 5000, 30_000] {
            let used: Vec<u64> = (0..hashes.len())
                .map(|i| hashes[i % distinct.min(n)])
                .collect();
            for bound in [1usize, 100, 65_536, 65_537, 1 << 30] {
                let ours = estimate_cardinality_hashed(used.len(), bound, |i| used[i], |i| used[i]);
                assert_eq!(
                    ours,
                    c::cardinality_hashed(&used, &used, bound),
                    "d{distinct} bound{bound}"
                );
            }
        }
        assert_eq!(estimate_cardinality_hashed(0, 5, |_| 0, |_| 0), 0);
        assert_eq!(estimate_cardinality_hashed(5, 0, |_| 0, |_| 0), 0);
        // A saturated linear count reports its bucket count.
        let spread: Vec<u64> = (0..200_000u64)
            .map(|i| i.wrapping_mul(0x9E37_79B9_7F4A_7C15))
            .collect();
        assert_eq!(
            estimate_cardinality_hashed(spread.len(), 65_536, |i| spread[i], |_| 0),
            c::cardinality_hashed(&spread, &spread, 65_536)
        );
        // A HyperLogLog with an empty bucket falls back to linear counting.
        assert_eq!(
            estimate_cardinality_hashed(10, 100_000, |_| 0, |i| i as u64),
            c::cardinality_hashed(
                &(0..10).collect::<Vec<u64>>(),
                &(0..10).collect::<Vec<u64>>(),
                100_000
            )
        );
    }

    #[test]
    fn dimensionality_matches_the_reference() {
        let n = if cfg!(miri) { 600 } else { 5000 };
        for width in [1usize, 2, 3, 4, 8] {
            let random = noise(n * width, 11);
            let stride = 37;
            let rows: Vec<u8> = (0..n * width)
                .map(|i| {
                    let (element, byte) = (i / width, i % width);
                    ((element % stride) * 5 + byte) as u8
                })
                .collect();
            let mixed: Vec<u8> = rows
                .iter()
                .zip(noise(n * width, 12))
                .map(|(&r, x)| if x < 40 { x } else { r })
                .collect();
            for (name, data) in [
                ("random", random),
                ("rows", rows),
                ("mixed", mixed),
                ("empty", vec![]),
            ] {
                let ours = estimate_dimensionality(&data, width).unwrap();
                let theirs = c::dimensionality(&data, width);
                assert_eq!(
                    [
                        ours.dimensionality as usize,
                        ours.stride,
                        ours.stride_matches,
                        ours.total_matches
                    ],
                    theirs,
                    "w{width} {name}"
                );
            }
        }
        assert!(estimate_dimensionality(&[0; 10], 5).is_none());
        assert_eq!(DimensionalityStatus::default(), DimensionalityStatus::None);
        assert_ne!(
            DimensionalityStatus::Likely2D,
            DimensionalityStatus::Possibly2D
        );
    }

    #[test]
    fn float_width_matches_the_reference() {
        let n = if cfg!(miri) { 200 } else { 6000 };
        let f32s: Vec<u8> = (0..n)
            .flat_map(|i| ((i as f32) * 0.37 + 1000.0).to_le_bytes())
            .collect();
        let f64s: Vec<u8> = (0..n)
            .flat_map(|i| ((i as f64).sin() * 1e5).to_le_bytes())
            .collect();
        let f16s: Vec<u8> = (0..n as u16)
            .flat_map(|i| (0x3C00 + (i % 700)).to_le_bytes())
            .collect();
        for (name, data) in [
            ("f32", f32s),
            ("f64", f64s),
            ("f16", f16s),
            ("noise", noise(n * 8, 5)),
            ("text", b"hello world, floats are not text".repeat(40)),
            ("odd", vec![1, 2, 3]),
            ("empty", vec![]),
            ("six", noise(6 * 100, 9)),
        ] {
            assert_eq!(guess_float_width(&data), c::float_width(&data), "{name}");
        }
    }

    #[test]
    fn ranges_match_the_reference() {
        for width in [1, 2, 4, 8] {
            for n in [0, 1, 100] {
                let data = values(n, width, 1 << 20, 4);
                let ours = compute_unsigned_range(&data, width);
                assert_eq!([ours.min, ours.max], c::range(&data, width));
            }
        }
        assert_eq!(
            compute_unsigned_range(&[1, 2, 3], 3),
            ElementRange::default()
        );
    }

    #[test]
    fn linear_size_log_is_clamped() {
        assert_eq!(linear_size_log(1), 5);
        assert_eq!(linear_size_log(64), 6);
        assert_eq!(linear_size_log(65), 7);
        assert_eq!(linear_size_log(65_536), 13);
    }
}
