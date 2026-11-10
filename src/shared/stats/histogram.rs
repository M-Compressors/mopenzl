//! Histograms of byte and 16-bit streams (`shared/histogram.c`).

use alloc::vec;
use alloc::vec::Vec;

/// A histogram with the summary the entropy codecs read (`ZL_Histogram`).
///
/// `count` covers symbols `0..=bound` where `bound` is the static upper
/// bound given to [`Histogram::new`] (255 for bytes, 65535 for 16-bit
/// symbols); after a build [`Histogram::max_symbol`] is the largest symbol
/// actually present.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Histogram {
    /// Number of elements counted.
    pub(crate) total: u32,
    /// The largest symbol present (0 for an empty input).
    pub(crate) max_symbol: u32,
    /// The count of the most frequent symbol.
    pub(crate) largest_count: u32,
    /// The element width in bytes (0 before a build).
    pub(crate) element_size: u32,
    /// The number of distinct symbols.
    pub(crate) cardinality: u32,
    /// The counts, `bound + 1` entries.
    pub(crate) count: Vec<u32>,
}

impl Histogram {
    /// `ZL_Histogram_init` over a fresh table for symbols up to `bound`.
    pub(crate) fn new(bound: u32) -> Self {
        Self {
            total: 0,
            max_symbol: bound,
            largest_count: 0,
            element_size: 0,
            cardinality: 0,
            count: vec![0; bound as usize + 1],
        }
    }

    /// `ZL_Histogram_build` for bytes (`eltWidth == 1`).
    pub(crate) fn build8(&mut self, src: &[u8]) {
        self.element_size = 1;
        self.total = src.len() as u32;
        let mut max_symbol = 255;
        let (largest, cardinality) = count8(&mut self.count, &mut max_symbol, src);
        self.max_symbol = max_symbol;
        self.cardinality = cardinality;
        self.largest_count = largest;
    }

    /// `ZL_Histogram_build` for little-endian 16-bit symbols
    /// (`eltWidth == 2`); a trailing odd byte is ignored.
    #[cfg_attr(
        not(any(test, feature = "entropy")),
        expect(
            dead_code,
            reason = "only the 16-bit Huffman graphs count wide symbols"
        )
    )]
    pub(crate) fn build16(&mut self, src: &[u8]) {
        self.element_size = 2;
        self.total = (src.len() / 2) as u32;
        let (largest, max_symbol, cardinality) = count16(&mut self.count, src);
        self.max_symbol = max_symbol;
        self.cardinality = cardinality;
        self.largest_count = largest;
    }
}

/// `ZL_Histogram_count` / `HIST_countFast` for bytes: zeroes
/// `count[..=*max_symbol_value]`, fills it and returns the largest count and
/// the cardinality; `max_symbol_value` becomes the largest symbol present.
pub(crate) fn count8(count: &mut [u32], max_symbol_value: &mut u32, src: &[u8]) -> (u32, u32) {
    count[..=*max_symbol_value as usize].fill(0);
    if src.is_empty() {
        *max_symbol_value = 0;
        return (0, 0);
    }
    if src.len() < 1500 {
        for &b in src {
            count[usize::from(b)] += 1;
        }
    } else {
        // Four interleaved tables, recombined at the end (HIST_count_parallel_wksp).
        let mut tables = [[0u32; 256]; 4];
        let (chunks, tail) = src.as_chunks::<8>();
        let [t0, t1, t2, t3] = &mut tables;
        for chunk in chunks {
            // One 8-byte load per eight symbols, two to each table.
            let word = u64::from_le_bytes(*chunk);
            t0[usize::from(word as u8)] += 1;
            t1[usize::from((word >> 8) as u8)] += 1;
            t2[usize::from((word >> 16) as u8)] += 1;
            t3[usize::from((word >> 24) as u8)] += 1;
            t0[usize::from((word >> 32) as u8)] += 1;
            t1[usize::from((word >> 40) as u8)] += 1;
            t2[usize::from((word >> 48) as u8)] += 1;
            t3[usize::from((word >> 56) as u8)] += 1;
        }
        for &b in tail {
            tables[0][usize::from(b)] += 1;
        }
        for (s, slot) in count[..256].iter_mut().enumerate() {
            *slot = tables[0][s] + tables[1][s] + tables[2][s] + tables[3][s];
        }
    }
    let mut max = *max_symbol_value;
    while count[max as usize] == 0 {
        max -= 1;
    }
    *max_symbol_value = max;
    let mut largest = 0;
    let mut cardinality = 0;
    for &c in &count[..=max as usize] {
        largest = largest.max(c);
        cardinality += u32::from(c > 0);
    }
    (largest, cardinality)
}

/// `ZS_Histogram_count16`: zeroes the whole table, counts the symbols and
/// returns the largest count, the largest symbol and the cardinality.
#[cfg_attr(
    not(any(test, feature = "entropy")),
    expect(
        dead_code,
        reason = "only the 16-bit Huffman graphs count wide symbols"
    )
)]
fn count16(count: &mut [u32], src: &[u8]) -> (u32, u32, u32) {
    count.fill(0);
    let symbols = src.as_chunks::<2>().0;
    if symbols.is_empty() {
        return (0, 0, 0);
    }
    let mut max = 0usize;
    for &pair in symbols {
        let symbol = usize::from(u16::from_le_bytes(pair));
        count[symbol] += 1;
        max = max.max(symbol);
    }
    if count[max] as usize == symbols.len() {
        return (count[max], max as u32, 1);
    }
    let mut largest = 0;
    let mut cardinality = 0;
    for &c in &count[..=max] {
        largest = largest.max(c);
        cardinality += u32::from(c > 0);
    }
    (largest, max as u32, cardinality)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(len: usize, modulo: u32) -> Vec<u8> {
        let mut state = 12345u32;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
                ((state >> 16) % modulo) as u8
            })
            .collect()
    }

    #[test]
    fn byte_histograms_summarise_small_and_large_inputs() {
        for len in [1usize, 10, 1499, 1500, 1501, 5003] {
            let src = lcg(len, 37);
            let mut hist = Histogram::new(255);
            hist.build8(&src);
            let mut expected = vec![0u32; 256];
            for &b in &src {
                expected[usize::from(b)] += 1;
            }
            assert_eq!(hist.count, expected, "len {len}");
            assert_eq!(hist.total as usize, len);
            assert_eq!(hist.element_size, 1);
            assert_eq!(hist.largest_count, *expected.iter().max().unwrap());
            assert_eq!(
                hist.cardinality as usize,
                expected.iter().filter(|&&c| c > 0).count()
            );
            assert_eq!(
                hist.max_symbol as usize,
                expected.iter().rposition(|&c| c > 0).unwrap()
            );
        }
    }

    #[test]
    fn an_empty_byte_input_has_no_symbols() {
        let mut hist = Histogram::new(255);
        assert_eq!(hist.max_symbol, 255);
        hist.build8(&[]);
        assert_eq!(
            (
                hist.total,
                hist.max_symbol,
                hist.largest_count,
                hist.cardinality
            ),
            (0, 0, 0, 0)
        );
    }

    #[test]
    fn sixteen_bit_histograms_cover_the_full_range() {
        let mut hist = Histogram::new(65535);
        let symbols: Vec<u16> = vec![5, 700, 5, 65535, 5, 700];
        let src: Vec<u8> = symbols.iter().flat_map(|s| s.to_le_bytes()).collect();
        hist.build16(&src);
        assert_eq!(hist.total, 6);
        assert_eq!(hist.element_size, 2);
        assert_eq!(hist.max_symbol, 65535);
        assert_eq!(hist.cardinality, 3);
        assert_eq!(hist.largest_count, 3);
        assert_eq!(hist.count[5], 3);
        assert_eq!(hist.count[700], 2);
        // Rebuilding clears the previous counts.
        hist.build16(&[1, 0, 1, 0]);
        assert_eq!(hist.count[5], 0);
        assert_eq!(
            (hist.max_symbol, hist.cardinality, hist.largest_count),
            (1, 1, 2)
        );
        hist.build16(&[]);
        assert_eq!((hist.total, hist.max_symbol, hist.cardinality), (0, 0, 0));
    }
}
