//! The row-based hash table of `common/row_table.h`: sixteen candidates per
//! hash bucket, found by a tag compare.
//!
//! The reference lays each row out as one 128-byte struct and compares the
//! tags with SIMD. This port keeps the rows in three parallel arrays (tags,
//! heads, positions); the candidates are visited in the same order, so the
//! matches found are identical.

use alloc::vec;
use alloc::vec::Vec;

use super::count::{count, count_back};
use super::hash::hash_ptr;

/// `ZS_RowTable_kRowLog`.
pub(crate) const ROW_LOG: u32 = 4;
/// `ZS_RowTable_kRowSize`.
const ROW_SIZE: usize = 1 << ROW_LOG;
/// `ZS_RowTable_kRowMask`.
const ROW_MASK: u32 = ROW_SIZE as u32 - 1;

/// `ZS_RowTable_Match`: a match and how far it extends either way.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Match {
    /// The table index of the match source.
    pub(crate) match_idx: u32,
    /// Bytes matched forward from the search position.
    pub(crate) forward: usize,
    /// Bytes matched backward from the search position.
    pub(crate) backward: usize,
    /// `forward + backward`.
    pub(crate) total: usize,
}

/// `ZS_RowTable`.
///
/// Positions stored in the table are indices; the byte of index `i` is
/// `data[i - bias]` (`bias` is 1 for the `field_lz` window, whose base
/// pointer sits one byte before the source, and 0 for the `lz` codec).
#[derive(Debug)]
pub(crate) struct RowTable {
    tags: Vec<u8>,
    heads: Vec<u8>,
    positions: Vec<u32>,
    /// `tableLog - ZS_RowTable_kRowLog`.
    log: u32,
    field_size: u32,
    min_match: u32,
    bias: usize,
    /// The first index not yet inserted by [`Self::fill`].
    pub(crate) next_to_fill: u32,
}

impl RowTable {
    /// `ZS_RowTable_init`: an empty table of `2^table_log` candidates.
    ///
    /// `table_log` must be at least [`ROW_LOG`].
    pub(crate) fn new(table_log: u32, field_size: u32, min_match: u32, bias: usize) -> Self {
        let rows = 1usize << (table_log - ROW_LOG);
        Self {
            tags: vec![0; rows * ROW_SIZE],
            heads: vec![0; rows],
            positions: vec![0; rows * ROW_SIZE],
            log: table_log - ROW_LOG,
            field_size,
            min_match,
            bias,
            next_to_fill: 0,
        }
    }

    /// The row and tag of the bytes at index `pos`.
    fn locate(&self, data: &[u8], pos: u32) -> (usize, u8) {
        let pair = hash_ptr(data, pos as usize - self.bias, self.log + 8, self.min_match);
        (pair >> 8, (pair & 0xFF) as u8)
    }

    /// `ZS_RowTable_nextIndex` followed by the insertion.
    fn insert(&mut self, row: usize, tag: u8, pos: u32) {
        let next = (u32::from(self.heads[row]).wrapping_sub(1)) & ROW_MASK;
        self.heads[row] = next as u8;
        let slot = row * ROW_SIZE + next as usize;
        self.tags[slot] = tag;
        self.positions[slot] = pos;
    }

    /// `ZS_RowTable_putT`: adds index `pos`.
    #[cfg(test)]
    pub(crate) fn put(&mut self, data: &[u8], pos: u32) {
        let (row, tag) = self.locate(data, pos);
        self.insert(row, tag, pos);
    }

    /// `ZS_RowTable_fillT`: adds every index from `next_to_fill` up to, but
    /// not including, `end`, one per field.
    pub(crate) fn fill(&mut self, data: &[u8], end: u32) {
        let mut pos = self.next_to_fill;
        while pos < end {
            let (row, tag) = self.locate(data, pos);
            self.insert(row, tag, pos);
            pos = pos.wrapping_add(self.field_size);
        }
        self.next_to_fill = end;
    }

    /// `ZS_RowTable_getAndUpdateT`: the candidates at `pos` (newest first,
    /// stopping at the first one below `low_limit`), then adds `pos`.
    fn get_and_update(
        &mut self,
        data: &[u8],
        low_limit: u32,
        pos: u32,
        mut searches: usize,
        matches: &mut [u32; ROW_SIZE],
    ) -> usize {
        let (row, tag) = self.locate(data, pos);
        let head = u32::from(self.heads[row]);
        let base = row * ROW_SIZE;
        let tags = &self.tags[base..base + ROW_SIZE];
        let mut mask = 0u16;
        for (i, &t) in tags.iter().enumerate() {
            mask |= u16::from(t == tag) << i;
        }
        // `ZS_RowTable_rotateMaskRight`: the newest candidate is bit 0.
        let mut mask = u32::from(mask.rotate_right(head));
        let mut found = 0;
        while mask > 0 && searches > 0 {
            let slot = (head + mask.trailing_zeros()) & ROW_MASK;
            let index = self.positions[base + slot as usize];
            if index < low_limit {
                break;
            }
            matches[found] = index;
            found += 1;
            searches -= 1;
            mask &= mask - 1;
        }
        self.insert(row, tag, pos);
        self.next_to_fill = self.next_to_fill.wrapping_add(self.field_size);
        found
    }

    /// `ZS_RowTable_getBestMatchAndUpdateT`: the best match of at least
    /// `min_length` bytes for the bytes at index `pos`, extended backward
    /// down to `anchor` (a data offset), then adds `pos`. `end` is the data
    /// offset the match may extend to.
    ///
    /// `total` is `min_length - 1` when nothing was found.
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors ZS_RowTable_getBestMatchAndUpdateT"
    )]
    pub(crate) fn best_match(
        &mut self,
        data: &[u8],
        anchor: usize,
        low_limit: u32,
        pos: u32,
        end: usize,
        searches: usize,
        min_length: usize,
    ) -> Match {
        let field_mask = self.field_size as usize - 1;
        let ptr = pos as usize - self.bias;
        let mut matches = [0u32; ROW_SIZE];
        let found = self.get_and_update(data, low_limit, pos, searches, &mut matches);
        let mut best = 0u32;
        let mut best_length = min_length - 1;
        let mut back_length = 0usize;
        let max_back = ptr - anchor;
        let low = low_limit as usize - self.bias;
        for &candidate in &matches[..found] {
            let source = candidate as usize - self.bias;
            let length = count(data, ptr, source, end) & !field_mask;
            if length < min_length || length + max_back <= back_length + best_length {
                continue;
            }
            let back = count_back(data, ptr, source, anchor, low) & !field_mask;
            if back + length > back_length + best_length {
                best = candidate;
                best_length = length;
                back_length = back;
                if best_length >= end - ptr {
                    break;
                }
            }
        }
        Match {
            match_idx: best,
            forward: best_length,
            backward: back_length,
            total: back_length + best_length,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// A repeating text of period 31.
    fn data() -> Vec<u8> {
        (0..400u32)
            .map(|i| b"the quick brown fox jumps over."[(i % 31) as usize])
            .collect()
    }

    #[test]
    fn fill_inserts_every_field_and_best_match_finds_the_period() {
        let data = data();
        let mut table = RowTable::new(10, 1, 4, 0);
        table.next_to_fill = 1;
        table.fill(&data, 100);
        assert_eq!(table.next_to_fill, 100);
        let found = table.best_match(&data, 90, 1, 100, data.len(), 8, 4);
        assert_eq!(found.match_idx, 69, "the newest period start");
        assert_eq!(found.forward, data.len() - 100);
        assert_eq!(found.backward, 10, "extends back to the anchor");
        assert_eq!(found.total, found.forward + found.backward);
        assert_eq!(table.next_to_fill, 101);
    }

    #[test]
    fn no_candidate_reports_a_total_below_the_minimum() {
        let data: Vec<u8> = (0..200u32).map(|i| (i * 7 + i / 3) as u8).collect();
        let mut table = RowTable::new(10, 1, 4, 0);
        let found = table.best_match(&data, 0, 1, 50, data.len(), 8, 4);
        assert_eq!(found.total, 3);
        assert_eq!(found.match_idx, 0);
    }

    #[test]
    fn candidates_below_the_low_limit_stop_the_search() {
        let data = data();
        let mut table = RowTable::new(10, 1, 4, 0);
        table.next_to_fill = 1;
        table.fill(&data, 100);
        let found = table.best_match(&data, 90, 90, 100, data.len(), 8, 4);
        assert_eq!(found.total, 3, "every candidate is out of the window");
    }

    #[test]
    fn a_bias_shifts_the_index_of_every_byte() {
        let data = data();
        let mut plain = RowTable::new(10, 1, 4, 0);
        let mut biased = RowTable::new(10, 1, 4, 1);
        plain.next_to_fill = 1;
        biased.next_to_fill = 2;
        plain.fill(&data, 100);
        biased.fill(&data, 101);
        let a = plain.best_match(&data, 90, 1, 100, data.len(), 8, 4);
        let b = biased.best_match(&data, 90, 2, 101, data.len(), 8, 4);
        assert_eq!(b.match_idx, a.match_idx + 1);
        assert_eq!((a.forward, a.backward), (b.forward, b.backward));
    }

    #[test]
    fn the_field_size_aligns_lengths() {
        let data = data();
        let mut table = RowTable::new(10, 4, 4, 0);
        table.next_to_fill = 4;
        table.fill(&data, 200);
        // The period (31) is odd: only every fourth period lines up with a
        // field boundary, at distance 124.
        let found = table.best_match(&data, 188, 4, 200, data.len(), 8, 4);
        assert_eq!(found.match_idx, 76);
        assert_eq!(found.forward % 4, 0);
        assert_eq!(found.backward % 4, 0);
        assert!(found.total >= 4);
    }

    #[test]
    fn put_adds_a_single_position() {
        let data = data();
        let mut table = RowTable::new(10, 1, 4, 0);
        table.put(&data, 31);
        let found = table.best_match(&data, 0, 1, 62, data.len(), 8, 4);
        assert_eq!(found.match_idx, 31);
    }
}
