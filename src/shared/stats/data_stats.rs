//! Lazily computed statistics of a byte stream (`shared/data_stats.c`).

use alloc::boxed::Box;

use super::histogram::count8;
use crate::shared::entropy::{fse, huf};
use crate::shared::varint;

/// `LOG2_TABLE_PRECISION_BITS`.
const LOG2_TABLE_PRECISION_BITS: u32 = 8;
/// `LOG2_TABLE_MULTIPLIER`.
const LOG2_TABLE_MULTIPLIER: i64 = 1 << 16;

/// `LOG2_TABLE`: `0x80000 ^ int(log2(i) * (1 << 16))` for `i` in `256..512`
/// (pre-generated in the reference).
const LOG2_TABLE: [u16; 1 << LOG2_TABLE_PRECISION_BITS] = [
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

/// `ZL_nextPow2`: the `n` with `(1 << n) >= upper_bound`.
const fn next_pow2(upper_bound: u64) -> u32 {
    if upper_bound <= 1 {
        0
    } else {
        64 - (upper_bound - 1).leading_zeros()
    }
}

/// `ZL_calculateEntropy`: an estimate of the Shannon entropy, in bits per
/// symbol, of the distribution `count[0..=max_value]` over `total_elements`
/// samples. It uses a fixed-point logarithm table and is accurate to about
/// 0.1%.
pub(crate) fn calculate_entropy(count: &[u32], max_value: usize, total_elements: usize) -> f64 {
    if total_elements == 0 {
        return 0.0;
    }
    let mut int_entropy: i64 = 0;
    let normalize = (1i64 << 62) / total_elements as i64;
    for &curr_count in &count[..=max_value] {
        let mut log_index = (u64::from(curr_count) as i64).wrapping_mul(normalize);
        let clz =
            ((log_index as u64) | (1 << (LOG2_TABLE_PRECISION_BITS + 1))).leading_zeros() as i32;
        let shift = (62 - LOG2_TABLE_PRECISION_BITS as i32) - clz + 1;
        log_index >>= shift;
        log_index &= (1 << LOG2_TABLE_PRECISION_BITS) - 1;
        int_entropy += -i64::from(curr_count)
            * (i64::from(LOG2_TABLE[log_index as usize]) - i64::from(clz) * LOG2_TABLE_MULTIPLIER);
    }
    (int_entropy as f64)
        / ((total_elements as u64).wrapping_mul(LOG2_TABLE_MULTIPLIER as u64) as f64)
        - 1.0
}

/// `ZL_calculateEntropyU8`: [`calculate_entropy`] over 256 symbols.
pub(crate) fn calculate_entropy_u8(count: &[u32; 256], total_elements: usize) -> f64 {
    calculate_entropy(count, 255, total_elements)
}

/// `DataStatsU8`: statistics of one byte stream, each computed on first use.
#[derive(Debug)]
pub(crate) struct DataStatsU8<'a> {
    src: &'a [u8],
    histogram: Option<Box<[u32; 256]>>,
    delta_histogram: Option<Box<[u32; 256]>>,
    max_elt: Option<u8>,
    entropy: Option<f64>,
    delta_entropy: Option<f64>,
    cardinality: Option<usize>,
    huffman_size: Option<usize>,
    delta_huffman_size: Option<usize>,
    bitpacked_size: Option<usize>,
    flatpacked_size: Option<usize>,
    constant_size: Option<usize>,
}

impl<'a> DataStatsU8<'a> {
    /// `DataStatsU8_init`: statistics of `src`, which they borrow.
    pub(crate) const fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            histogram: None,
            delta_histogram: None,
            max_elt: None,
            entropy: None,
            delta_entropy: None,
            cardinality: None,
            huffman_size: None,
            delta_huffman_size: None,
            bitpacked_size: None,
            flatpacked_size: None,
            constant_size: None,
        }
    }

    /// `DataStatsU8_totalElements`.
    pub(crate) const fn total_elements(&self) -> usize {
        self.src.len()
    }

    /// `DataStatsU8_getHistogram`: the counts of the 256 byte values. The
    /// maximum element and cardinality come with it.
    pub(crate) fn histogram(&mut self) -> &[u32; 256] {
        if self.histogram.is_none() {
            let mut count = Box::new([0u32; 256]);
            let mut max_symbol = 255;
            let (_, cardinality) = count8(&mut count[..], &mut max_symbol, self.src);
            self.max_elt = Some(max_symbol as u8);
            self.cardinality = Some(cardinality as usize);
            self.histogram = Some(count);
        }
        self.histogram.as_deref().unwrap_or(&[0; 256])
    }

    /// `DataStatsU8_getCardinality`: the number of distinct byte values.
    pub(crate) fn cardinality(&mut self) -> usize {
        if let Some(cardinality) = self.cardinality {
            return cardinality;
        }
        let cardinality = self.histogram().iter().filter(|&&c| c != 0).count();
        *self.cardinality.get_or_insert(cardinality)
    }

    /// `DataStatsU8_getMaxElt`: the largest byte value, 0 for no data.
    pub(crate) fn max_elt(&mut self) -> u8 {
        if let Some(max_elt) = self.max_elt {
            return max_elt;
        }
        let max_elt = if self.src.is_empty() {
            0
        } else {
            self.histogram()
                .iter()
                .rposition(|&c| c != 0)
                .map_or(0, |index| index as u8)
        };
        *self.max_elt.get_or_insert(max_elt)
    }

    /// `DataStatsU8_getDeltaHistogram`: the counts of the differences of
    /// consecutive bytes (the first against 0).
    pub(crate) fn delta_histogram(&mut self) -> &[u32; 256] {
        if self.delta_histogram.is_none() {
            let mut count = Box::new([0u32; 256]);
            let mut prev = 0u8;
            for &curr in self.src {
                count[usize::from(curr.wrapping_sub(prev))] += 1;
                prev = curr;
            }
            self.delta_histogram = Some(count);
        }
        self.delta_histogram.as_deref().unwrap_or(&[0; 256])
    }

    /// `DataStatsU8_getEntropy`: the estimated Shannon entropy.
    pub(crate) fn entropy(&mut self) -> f64 {
        if let Some(entropy) = self.entropy {
            return entropy;
        }
        let total = self.total_elements();
        let entropy = calculate_entropy_u8(self.histogram(), total);
        *self.entropy.get_or_insert(entropy)
    }

    /// `DataStatsU8_getDeltaEntropy`.
    pub(crate) fn delta_entropy(&mut self) -> f64 {
        if let Some(entropy) = self.delta_entropy {
            return entropy;
        }
        let total = self.total_elements();
        let entropy = calculate_entropy_u8(self.delta_histogram(), total);
        *self.delta_entropy.get_or_insert(entropy)
    }

    /// `DataStatsU8_estimateHuffmanSize`: the size of the symbols, the table
    /// and 4 bytes of framing. The reference adds the table writer's error
    /// code to the sum when it fails; that wrapped value is reproduced.
    fn estimate_huffman_size(&mut self, delta: bool) -> usize {
        if self.src.len() <= 4 {
            return 4;
        }
        let src_size = self.src.len();
        let hist = if delta {
            self.delta_histogram()
        } else {
            self.histogram()
        };
        let huff_log = huf::optimal_table_log(fse::MAX_TABLELOG, src_size, 255);
        let table = match huf::build_ctable(&hist[..], 255, huff_log) {
            Ok(table) => table,
            // Unreachable: the log is at most 12 and the alphabet 256 symbols.
            Err(_) => return 4,
        };
        let estimate_encoded_size = huf::estimate_compressed_size(&table, &hist[..], 255);
        let header = huf::write_ctable(&table, 255, table.table_log(), huf::CTABLE_BOUND);
        let estimate_header_size = match header {
            Ok(header) => header.len(),
            Err(error) => (error.code()).wrapping_neg(),
        };
        estimate_encoded_size
            .wrapping_add(estimate_header_size)
            .wrapping_add(4)
    }

    /// `DataStatsU8_getHuffmanSize`: an estimate of the Huffman size,
    /// header included.
    pub(crate) fn huffman_size(&mut self) -> usize {
        if let Some(size) = self.huffman_size {
            return size;
        }
        let size = self.estimate_huffman_size(false);
        *self.huffman_size.get_or_insert(size)
    }

    /// `DataStatsU8_getDeltaHuffmanSize`.
    pub(crate) fn delta_huffman_size(&mut self) -> usize {
        if let Some(size) = self.delta_huffman_size {
            return size;
        }
        let size = self.estimate_huffman_size(true);
        *self.delta_huffman_size.get_or_insert(size)
    }

    /// `DataStatsU8_estimateHuffmanSizeFast`: the entropy-based estimate of
    /// the Huffman size, with a 9-byte header allowance and the cheaper of
    /// two table-size estimates.
    pub(crate) fn estimate_huffman_size_fast(&mut self, delta: bool) -> usize {
        let mut entropy = if delta {
            self.delta_entropy()
        } else {
            self.entropy()
        };
        // At least one bit when there is more than one symbol.
        if entropy < 1.0 {
            entropy = 1.0;
        }
        let symbols_size = ((entropy * self.src.len() as f64) as usize) / 8;
        let header_size = 9;
        let table_bits_c = 5 * self.cardinality();
        let table_bits_m = 4 * (usize::from(self.max_elt()) + 1);
        let table_size = table_bits_c.min(table_bits_m).div_ceil(8);
        header_size + table_size + symbols_size
    }

    /// `DataStatsU8_getBitpackedSize`: bytes when every value takes
    /// `nextPow2(max + 1)` bits.
    pub(crate) fn bitpacked_size(&mut self) -> usize {
        if let Some(size) = self.bitpacked_size {
            return size;
        }
        let size = if self.src.is_empty() {
            0
        } else {
            let nb_bits_per_elt = next_pow2(u64::from(self.max_elt()) + 1) as usize;
            (self.src.len() * nb_bits_per_elt).div_ceil(8)
        };
        *self.bitpacked_size.get_or_insert(size)
    }

    /// `DataStatsU8_getFlatpackedSize`: bytes of the alphabet-indexed form.
    pub(crate) fn flatpacked_size(&mut self) -> usize {
        if let Some(size) = self.flatpacked_size {
            return size;
        }
        let nb_elts = self.cardinality();
        let nb_bits_per_elt = next_pow2(nb_elts as u64) as usize;
        let size = (self.src.len() * nb_bits_per_elt).div_ceil(8) + nb_elts;
        *self.flatpacked_size.get_or_insert(size)
    }

    /// `DataStatsU8_getConstantSize`: the size of the constant form, or
    /// `usize::MAX` when the data is not a single repeated value.
    pub(crate) fn constant_size(&mut self) -> usize {
        if let Some(size) = self.constant_size {
            return size;
        }
        let size = if self.cardinality() == 1 {
            1 + varint::encode(self.total_elements() as u64).1
        } else {
            usize::MAX
        };
        *self.constant_size.get_or_insert(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn exact_entropy(count: &[u32], total: usize) -> f64 {
        count
            .iter()
            .filter(|&&c| c > 0)
            .map(|&c| {
                let p = f64::from(c) / total as f64;
                -p * p.log2()
            })
            .sum()
    }

    fn sample(len: usize, modulo: u32) -> Vec<u8> {
        let mut state = 99u32;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((state >> 12) % modulo) as u8
            })
            .collect()
    }

    #[test]
    fn the_integer_entropy_tracks_the_real_entropy() {
        for modulo in [2u32, 7, 50, 256] {
            let src = sample(20_000, modulo);
            let mut stats = DataStatsU8::new(&src);
            let estimated = stats.entropy();
            let exact = exact_entropy(stats.histogram(), src.len());
            assert!(
                (estimated - exact).abs() < 0.01 * exact.max(1.0),
                "{modulo}: {estimated} vs {exact}"
            );
        }
        assert_eq!(calculate_entropy(&[0; 4], 3, 0), 0.0);
        // A constant stream has no entropy.
        let mut stats = DataStatsU8::new(&[9; 100]);
        assert!(stats.entropy() < 0.01);
    }

    #[test]
    fn the_log_table_matches_its_generator() {
        for (i, &entry) in LOG2_TABLE.iter().enumerate() {
            let value = ((256 + i) as f64).log2();
            let expected = ((value * f64::from(1 << 16)) as i64) ^ 0x80000;
            assert!((i64::from(entry) - expected).abs() <= 1, "entry {i}");
        }
    }

    #[test]
    fn counts_and_extremes_are_computed_lazily() {
        let src = vec![3u8, 3, 9, 200, 3];
        let mut stats = DataStatsU8::new(&src);
        assert_eq!(stats.total_elements(), 5);
        assert_eq!(stats.max_elt(), 200);
        assert_eq!(stats.cardinality(), 3);
        assert_eq!(stats.histogram()[3], 3);
        assert_eq!(stats.max_elt(), 200);
        assert_eq!(stats.cardinality(), 3);
        // Asking for the extremes first computes them from the histogram.
        let mut other = DataStatsU8::new(&src);
        assert_eq!(other.cardinality(), 3);
        let mut empty = DataStatsU8::new(&[]);
        assert_eq!(empty.max_elt(), 0);
        assert_eq!(empty.cardinality(), 0);
        assert_eq!(empty.bitpacked_size(), 0);
        assert_eq!(empty.flatpacked_size(), 0);
        assert_eq!(empty.constant_size(), usize::MAX);
    }

    #[test]
    fn delta_statistics_use_wrapping_differences() {
        let src = vec![250u8, 3, 3, 4];
        let mut stats = DataStatsU8::new(&src);
        let delta = stats.delta_histogram();
        assert_eq!(delta[250], 1);
        assert_eq!(delta[9], 1); // 3 - 250 wraps to 9
        assert_eq!(delta[0], 1);
        assert_eq!(delta[1], 1);
        let first = stats.delta_entropy();
        assert_eq!(first, stats.delta_entropy());
        let src = sample(5000, 20);
        let mut stats = DataStatsU8::new(&src);
        assert!(stats.delta_huffman_size() > 0);
        assert!(stats.huffman_size() > 0);
        assert_eq!(stats.huffman_size(), stats.huffman_size());
        assert_eq!(stats.delta_huffman_size(), stats.delta_huffman_size());
        assert!(stats.estimate_huffman_size_fast(true) > 0);
    }

    #[test]
    fn size_estimates_follow_the_reference_formulas() {
        // 100 bytes over 4 values 0..=3.
        let src: Vec<u8> = (0..100u8).map(|i| i % 4).collect();
        let mut stats = DataStatsU8::new(&src);
        assert_eq!(stats.bitpacked_size(), (100usize * 2).div_ceil(8));
        assert_eq!(stats.flatpacked_size(), (100usize * 2).div_ceil(8) + 4);
        assert_eq!(stats.constant_size(), usize::MAX);
        // Entropy of a uniform 4-symbol source is 2 bits.
        let fast = stats.estimate_huffman_size_fast(false);
        assert!((35..=36).contains(&fast), "{fast}");
        let mut constant = DataStatsU8::new(&[7; 300]);
        assert_eq!(constant.constant_size(), 1 + 2);
        // A tiny source has the fixed estimate.
        assert_eq!(DataStatsU8::new(&[1, 2, 3]).huffman_size(), 4);
        // A one-symbol source still yields a size.
        assert!(constant.huffman_size() > 0);
    }

    #[test]
    fn next_pow2_rounds_up() {
        assert_eq!(next_pow2(0), 0);
        assert_eq!(next_pow2(1), 0);
        assert_eq!(next_pow2(2), 1);
        assert_eq!(next_pow2(3), 2);
        assert_eq!(next_pow2(256), 8);
        assert_eq!(next_pow2(257), 9);
    }
}
