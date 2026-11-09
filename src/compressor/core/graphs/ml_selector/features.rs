//! The feature generator of the ML selector
//! (`compress/selectors/ml/features.c`): the statistics of an integer stream
//! a model's trees split on.
//!
//! Floating point follows the reference expression by expression: no fused
//! multiply-adds, the mean accumulated in two interleaved sums, and the
//! 8-bit path computed from a histogram with the reference's `unsigned`
//! products. Square roots are correctly rounded everywhere.

use alloc::vec::Vec;

use crate::shared::estimate::{
    CardinalityEstimate, compute_unsigned_range, estimate_cardinality_fixed,
};

/// `sqrt`, correctly rounded with or without `std`.
fn sqrt(x: f64) -> f64 {
    #[cfg(not(feature = "no_std"))]
    return x.sqrt();
    #[cfg(feature = "no_std")]
    return libm::sqrt(x);
}

/// `FeatureGenId`: which generator a model uses. Only the integer generator
/// exists; a blob naming any other fails to load.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum FeatureGen {
    /// `FeatureGen_integer` (`FeatureGenId_Int`).
    Integer,
}

impl FeatureGen {
    /// `FeatureGen_getFeatureGen`.
    pub(crate) fn from_id(id: i32) -> Option<Self> {
        (id == 0).then_some(Self::Integer)
    }

    /// `FeatureGen_getId`.
    pub(crate) const fn id(self) -> i32 {
        match self {
            Self::Integer => 0,
        }
    }

    /// Generates the features of a numeric stream.
    pub(crate) fn generate(self, bytes: &[u8], width: usize) -> Vec<LabeledFeature> {
        match self {
            Self::Integer => integer_features(bytes, width),
        }
    }
}

/// `LabeledFeature`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct LabeledFeature {
    /// The feature's name.
    pub(crate) label: &'static str,
    /// Its value.
    pub(crate) value: f32,
}

/// `Moments`.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct Moments {
    mean: f64,
    stddev: f64,
    variance: f64,
    skewness: f64,
    kurtosis: f64,
}

/// `readElement`: a little-endian element of `width` bytes.
fn read_element(bytes: &[u8], width: usize, index: usize) -> u64 {
    let mut word = [0u8; 8];
    word[..width].copy_from_slice(&bytes[index * width..(index + 1) * width]);
    u64::from_le_bytes(word)
}

/// `calcMean`.
fn calc_mean(bytes: &[u8], width: usize, nb_elts: usize) -> f64 {
    if nb_elts == 0 {
        return 0.0;
    }
    // Two accumulators, as the reference does; the order of additions matters.
    let (mut sum0, mut sum1) = (0f64, 0f64);
    let mut i = 0usize;
    while i + 2 < nb_elts {
        sum0 += read_element(bytes, width, i) as f64;
        sum1 += read_element(bytes, width, i + 1) as f64;
        i += 2;
    }
    let mut sum = sum0 + sum1;
    while i < nb_elts {
        sum += read_element(bytes, width, i) as f64;
        i += 1;
    }
    sum / nb_elts as f64
}

/// `calcMoments_uint8`: the moments from a histogram of the bytes.
fn calc_moments_u8(bytes: &[u8]) -> Moments {
    let mut counts = [0u32; 256];
    for &byte in bytes {
        counts[usize::from(byte)] = counts[usize::from(byte)].wrapping_add(1);
    }
    let nb_elts = bytes.len();
    let mut moments = Moments::default();
    let mut sum = 0f64;
    for (i, &count) in counts.iter().enumerate() {
        // `i * count` is an `unsigned` product in the reference.
        sum += (i as u32).wrapping_mul(count) as f64;
    }
    let mean = sum / nb_elts as f64;
    moments.mean = mean;
    if nb_elts <= 1 {
        return moments;
    }
    let (mut var_sum, mut skew_sum, mut kurt_sum) = (0f64, 0f64, 0f64);
    for (i, &count) in counts.iter().enumerate() {
        let delta = i as u32 as f64 - mean;
        let count = f64::from(count);
        var_sum += delta * delta * count;
        skew_sum += delta * delta * delta * count;
        kurt_sum += delta * delta * delta * delta * count;
    }
    finish_moments(&mut moments, nb_elts, var_sum, skew_sum, kurt_sum);
    moments
}

/// The shared tail of `calcMoments` and `calcMoments_uint8`.
fn finish_moments(moments: &mut Moments, nb_elts: usize, var: f64, skew: f64, kurt: f64) {
    let n = nb_elts as f64;
    let biased = sqrt(var / n);
    moments.stddev = sqrt(var / (nb_elts - 1) as f64);
    moments.variance = var / (n - 1.0);
    moments.skewness = skew / (biased * biased * biased) / n;
    moments.kurtosis = kurt / (biased * biased * biased * biased) / n - 3.0;
}

/// `calcMoments`: mean, unbiased variance and deviation, normalized skewness
/// and kurtosis, by the naive two-pass algorithm.
fn calc_moments(bytes: &[u8], width: usize, nb_elts: usize) -> Moments {
    let mean = calc_mean(bytes, width, nb_elts);
    let mut moments = Moments {
        mean,
        ..Moments::default()
    };
    if nb_elts <= 1 {
        return moments;
    }
    let (mut var_sum, mut skew_sum, mut kurt_sum) = (0f64, 0f64, 0f64);
    for i in 0..nb_elts {
        let delta = read_element(bytes, width, i) as f64 - mean;
        var_sum += delta * delta;
        skew_sum += delta * delta * delta;
        kurt_sum += delta * delta * delta * delta;
    }
    finish_moments(&mut moments, nb_elts, var_sum, skew_sum, kurt_sum);
    moments
}

/// `FeatureGen_integer`: the 11 features of a numeric stream of `width`-byte
/// elements, in the reference's order.
pub(crate) fn integer_features(bytes: &[u8], width: usize) -> Vec<LabeledFeature> {
    let nb_elts = bytes.len() / width.max(1);
    let range = compute_unsigned_range(bytes, width);
    let range_size = range.max.wrapping_sub(range.min);
    let range_size_plus_one = if range_size == u64::MAX {
        range_size
    } else {
        range_size + 1
    };
    let max_card = range_size_plus_one.min(nb_elts as u64);
    let card: CardinalityEstimate =
        estimate_cardinality_fixed(bytes, width, max_card).unwrap_or_default();
    let moments = if width == 1 {
        calc_moments_u8(bytes)
    } else {
        calc_moments(bytes, width, nb_elts)
    };
    let feature = |label, value: f32| LabeledFeature { label, value };
    alloc::vec![
        feature("nbElts", nb_elts as f32),
        feature("eltWidth", width as f32),
        feature("cardinality", card.estimate as f32),
        feature("cardinality_upper", card.estimate_upper_bound as f32),
        feature("cardinality_lower", card.estimate_lower_bound as f32),
        feature("range_size", range_size as f32),
        feature("mean", moments.mean as f32),
        feature("variance", moments.variance as f32),
        feature("stddev", moments.stddev as f32),
        feature("skewness", moments.skewness as f32),
        feature("kurtosis", moments.kurtosis as f32),
    ]
}
