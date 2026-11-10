//! The row-based hash table of `common/row_table.h`: sixteen candidates per
//! hash bucket, found by a tag compare.
//!
//! The reference lays each row out as one 128-byte struct and compares the
//! tags with SIMD. This port keeps the same layout; the candidates are
//! visited in the same order, so the matches found are identical.

use alloc::boxed::Box;
use alloc::vec;

use fearless_simd::{Simd, SimdBase, SimdMask, u8x16};

use super::count::{count_after, count_back};
use super::hash::{hash_ptr, read_u64};

/// `ZS_RowTable_kRowLog`.
pub(crate) const ROW_LOG: u32 = 4;
/// `ZS_RowTable_kRowSize`.
const ROW_SIZE: usize = 1 << ROW_LOG;
/// `ZS_RowTable_kRowMask`.
const ROW_MASK: u32 = ROW_SIZE as u32 - 1;
/// Bytes of one row: sixteen tags, the head, sixteen positions, padding
/// (`ZS_RowTable_Row`, 128 bytes).
const ROW_BYTES: usize = 128;
/// Offset of the head byte in a row.
const HEAD_AT: usize = ROW_SIZE;
/// An unused byte of the first cache line, written by [`RowTable::touch`].
#[cfg(any(feature = "field-lz", test))]
const TOUCH_AT: usize = HEAD_AT + 1;
/// Offset of the first position in a row.
const POSITIONS_AT: usize = 20;

/// The most bytes any supported hash reads (`ZL_hashPtr` of 16 bytes).
const HASH_READ: usize = 16;

/// One row of the table, as bytes so that a fresh table is a zeroed
/// allocation: tags at 0, head at 16, little-endian positions at 64.
type Row = [u8; ROW_BYTES];

/// A [`Row`] aligned to a cache line, so a row spans two lines, not three.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C, align(64))]
struct Aligned(Row);

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
/// `data[i - BIAS]` (`BIAS` is 1 for the `field_lz` window, whose base
/// pointer sits one byte before the source, and 0 for the `lz` codec).
///
/// Like the reference, each row is one 128-byte block holding the tags,
/// the head and the positions, so a lookup touches one row (two cache
/// lines) and the tags are compared in a single vector operation. The
/// candidates are visited in the same order, so the matches found are
/// identical.
#[derive(Debug)]
pub(crate) struct RowTable<const BIAS: usize, const MIN: u32 = 0> {
    rows: Box<[Aligned]>,
    /// `tableLog - ZS_RowTable_kRowLog`.
    log: u32,
    min_match: u32,
    /// The first index not yet inserted by [`Self::fill`].
    pub(crate) next_to_fill: u32,
    /// Sum of the head bytes read ahead to warm the cache; see
    /// [`Self::warmed`].
    warmed: u8,
}

/// The position `slot` of `row`.
#[inline(always)]
fn position(row: &Row, slot: usize) -> u32 {
    let at = POSITIONS_AT + 4 * (slot & ROW_MASK as usize);
    u32::from_le_bytes([row[at], row[at + 1], row[at + 2], row[at + 3]])
}

/// `ZS_RowTable_nextIndex` followed by the insertion.
#[inline(always)]
fn insert(row: &mut Row, tag: u8, pos: u32) {
    // Bytes throughout: the head is below 16, so the masked index needs no
    // bounds check and no widening.
    let next = row[HEAD_AT].wrapping_sub(1) & ROW_MASK as u8;
    row[HEAD_AT] = next;
    row[usize::from(next)] = tag;
    let at = POSITIONS_AT + 4 * usize::from(next);
    row[at..at + 4].copy_from_slice(&pos.to_le_bytes());
}

/// `ZS_RowTable_matchMask`: bit `i` is set when tag `i` equals `tag`.
#[inline(always)]
fn match_mask<S: Simd>(simd: S, row: &Row, tag: u8) -> u16 {
    let tags = row.first_chunk::<ROW_SIZE>().copied().unwrap_or_default();
    let haystack = u8x16::from_slice(simd, &tags);
    haystack.simd_eq(u8x16::splat(simd, tag)).to_bitmask() as u16
}

impl<const BIAS: usize, const MIN: u32> RowTable<BIAS, MIN> {
    /// `ZS_RowTable_init`: an empty table of `2^table_log` candidates.
    ///
    /// `table_log` must be at least [`ROW_LOG`].
    pub(crate) fn new(table_log: u32, min_match: u32) -> Self {
        let rows = 1usize << (table_log - ROW_LOG);
        Self {
            rows: vec![Aligned([0; ROW_BYTES]); rows].into_boxed_slice(),
            log: table_log - ROW_LOG,
            min_match,
            next_to_fill: 0,
            warmed: 0,
        }
    }

    /// The sum of the head bytes [`Self::best_match`] reads ahead of the
    /// search position to bring rows into the cache. Callers pass it to
    /// [`core::hint::black_box`] when a parse ends so the early reads stay.
    pub(crate) const fn warmed(&self) -> u8 {
        self.warmed
    }

    /// Starts loading the row for the bytes at index `ahead`: a store to an
    /// unused byte of the row's first cache line makes the core fetch the
    /// line without the pipeline waiting for it, as a prefetch would (safe
    /// Rust has no prefetch, and a load that misses stalls retirement).
    #[cfg(any(feature = "field-lz", test))]
    #[inline(always)]
    pub(crate) fn touch(&mut self, data: &[u8], ahead: u32) {
        if (ahead as usize) < data.len() + BIAS {
            let (row, _) = self.locate(data, ahead);
            self.rows[row].0[TOUCH_AT] = 0;
        }
    }

    /// The number of bytes hashed: `MIN` when it is not 0, a compile-time
    /// constant that lets the hash be selected at compile time; otherwise
    /// the length given to [`Self::new`].
    #[inline(always)]
    const fn hash_len(&self) -> u32 {
        if MIN != 0 { MIN } else { self.min_match }
    }

    /// The row and tag of the bytes at index `pos`.
    #[inline(always)]
    fn locate(&self, data: &[u8], pos: u32) -> (usize, u8) {
        let pair = hash_ptr(data, pos as usize - BIAS, self.log + 8, self.hash_len());
        (pair >> 8, (pair & 0xFF) as u8)
    }

    /// `ZS_RowTable_putT`: adds index `pos`.
    #[cfg(test)]
    pub(crate) fn put(&mut self, data: &[u8], pos: u32) {
        let (row, tag) = self.locate(data, pos);
        insert(&mut self.rows[row].0, tag, pos);
    }

    /// `ZS_RowTable_fillT`: adds every index from `next_to_fill` up to, but
    /// not including, `end`, one per field of `FIELD` bytes.
    ///
    /// The positions whose hashed bytes all lie inside `data` run over a
    /// sliding window of the source, so they read without bounds checks;
    /// the few at the very end read zero padding like [`Self::locate`].
    #[inline(always)]
    pub(crate) fn fill<const FIELD: u32>(&mut self, data: &[u8], end: u32) {
        let mut pos = self.next_to_fill;
        if pos < end {
            let mut at = pos as usize - BIAS;
            while pos < end
                && let Some(window) = data.get(at..).and_then(<[u8]>::first_chunk::<HASH_READ>)
            {
                let pair = hash_ptr(window, 0, self.log + 8, self.hash_len());
                insert(&mut self.rows[pair >> 8].0, (pair & 0xFF) as u8, pos);
                pos = pos.wrapping_add(FIELD);
                at += FIELD as usize;
            }
            while pos < end {
                let (row, tag) = self.locate(data, pos);
                insert(&mut self.rows[row].0, tag, pos);
                pos = pos.wrapping_add(FIELD);
            }
        }
        self.next_to_fill = end;
    }

    /// `ZS_RowTable_getBestMatchAndUpdateT`: the best match of at least
    /// `min_length` bytes for the bytes at index `pos`, extended backward
    /// down to `anchor` (a data offset), then adds `pos`. `end` is the data
    /// offset the match may extend to.
    ///
    /// The reference gathers the candidates (prefetching each) and then
    /// scores them; the candidate set is fixed before the new position is
    /// inserted, so scoring each as it is read gives the same match without
    /// the intermediate arrays.
    ///
    /// `OUTLINE` keeps the length counting behind calls; the parsers pick
    /// whichever gives the tighter search loop.
    ///
    /// `total` is `min_length - 1` when nothing was found.
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors ZS_RowTable_getBestMatchAndUpdateT"
    )]
    #[inline(always)]
    pub(crate) fn best_match<S: Simd, const FIELD: u32, const OUTLINE: bool>(
        &mut self,
        simd: S,
        data: &[u8],
        anchor: usize,
        low_limit: u32,
        pos: u32,
        end: usize,
        mut searches: usize,
        min_length: usize,
    ) -> Match {
        let field_mask = FIELD as usize - 1;
        let ptr = pos as usize - BIAS;
        let (row_at, tag) = self.locate(data, pos);
        let mut best = 0u32;
        let mut best_length = min_length - 1;
        let mut back_length = 0usize;
        let row = &mut self.rows[row_at].0;
        let head = u32::from(row[HEAD_AT]);
        // `ZS_RowTable_rotateMaskRight`: the newest candidate is bit 0.
        let mut mask = u32::from(match_mask(simd, row, tag).rotate_right(head));
        if mask > 0 {
            let here = read_u64(data, ptr);
            // `ZS_countBack` is clamped to `ptr - anchor`, so this bounds
            // the backward length of any candidate.
            let max_back = ptr - anchor;
            let low = low_limit as usize - BIAS;
            if OUTLINE {
                // Gather first, score after, like the reference: the reads
                // of the candidates' first words do not depend on one
                // another, so their cache misses overlap.
                let mut candidates = [0u32; ROW_SIZE];
                let mut firsts = [0u64; ROW_SIZE];
                let mut found = 0;
                while mask > 0 && searches > 0 {
                    let candidate =
                        position(row, ((head + mask.trailing_zeros()) & ROW_MASK) as usize);
                    if candidate < low_limit {
                        break;
                    }
                    searches -= 1;
                    mask &= mask - 1;
                    candidates[found & (ROW_SIZE - 1)] = candidate;
                    firsts[found & (ROW_SIZE - 1)] =
                        read_u64(data, candidate as usize - BIAS) ^ here;
                    found += 1;
                }
                for i in 0..found {
                    let candidate = candidates[i & (ROW_SIZE - 1)];
                    let first = firsts[i & (ROW_SIZE - 1)];
                    let source = candidate as usize - BIAS;
                    let length =
                        count_after::<OUTLINE>(data, ptr, source, end, first) & !field_mask;
                    if length < min_length || length + max_back <= back_length + best_length {
                        continue;
                    }
                    let back = count_back::<OUTLINE>(data, ptr, source, anchor, low) & !field_mask;
                    if back + length > back_length + best_length {
                        best = candidate;
                        best_length = length;
                        back_length = back;
                        if best_length >= end - ptr {
                            break;
                        }
                    }
                }
            } else {
                while mask > 0 && searches > 0 {
                    let candidate =
                        position(row, ((head + mask.trailing_zeros()) & ROW_MASK) as usize);
                    if candidate < low_limit {
                        break;
                    }
                    searches -= 1;
                    mask &= mask - 1;
                    let source = candidate as usize - BIAS;
                    let first = read_u64(data, source);
                    let length =
                        count_after::<OUTLINE>(data, ptr, source, end, first ^ here) & !field_mask;
                    if length < min_length || length + max_back <= back_length + best_length {
                        continue;
                    }
                    let back = count_back::<OUTLINE>(data, ptr, source, anchor, low) & !field_mask;
                    if back + length > back_length + best_length {
                        best = candidate;
                        best_length = length;
                        back_length = back;
                        if best_length >= end - ptr {
                            break;
                        }
                    }
                }
            }
        }
        insert(row, tag, pos);
        self.next_to_fill = self.next_to_fill.wrapping_add(FIELD);
        if !OUTLINE {
            // `ZL_PREFETCH_L1` of the row of a position a few fields ahead:
            // an early read of its head byte brings the row into the cache.
            let ahead = pos.wrapping_add(4 * FIELD);
            if (ahead as usize) < data.len() + BIAS {
                let (row, _) = self.locate(data, ahead);
                self.warmed = self.warmed.wrapping_add(self.rows[row].0[HEAD_AT]);
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
    use crate::Backend;
    use alloc::vec::Vec;
    use fearless_simd::dispatch;

    /// A repeating text of period 31.
    fn data() -> Vec<u8> {
        (0..400u32)
            .map(|i| b"the quick brown fox jumps over."[(i % 31) as usize])
            .collect()
    }

    fn fill_inserts_every_field_and_best_match_finds_the_period<S: Simd>(simd: S) {
        let data = data();
        let mut table = RowTable::<0>::new(10, 4);
        table.next_to_fill = 1;
        table.fill::<1>(&data, 100);
        assert_eq!(table.next_to_fill, 100);
        let found = table.best_match::<_, 1, false>(simd, &data, 90, 1, 100, data.len(), 8, 4);
        assert_eq!(found.match_idx, 69, "the newest period start");
        assert_eq!(found.forward, data.len() - 100);
        assert_eq!(found.backward, 10, "extends back to the anchor");
        assert_eq!(found.total, found.forward + found.backward);
        assert_eq!(table.next_to_fill, 101);
    }

    fn no_candidate_reports_a_total_below_the_minimum<S: Simd>(simd: S) {
        let data: Vec<u8> = (0..200u32).map(|i| (i * 7 + i / 3) as u8).collect();
        let mut table = RowTable::<0>::new(10, 4);
        let found = table.best_match::<_, 1, false>(simd, &data, 0, 1, 50, data.len(), 8, 4);
        assert_eq!(found.total, 3);
        assert_eq!(found.match_idx, 0);
    }

    fn candidates_below_the_low_limit_stop_the_search<S: Simd>(simd: S) {
        let data = data();
        let mut table = RowTable::<0>::new(10, 4);
        table.next_to_fill = 1;
        table.fill::<1>(&data, 100);
        let found = table.best_match::<_, 1, false>(simd, &data, 90, 90, 100, data.len(), 8, 4);
        assert_eq!(found.total, 3, "every candidate is out of the window");
    }

    fn a_bias_shifts_the_index_of_every_byte<S: Simd>(simd: S) {
        let data = data();
        let mut plain = RowTable::<0>::new(10, 4);
        let mut biased = RowTable::<1>::new(10, 4);
        plain.next_to_fill = 1;
        biased.next_to_fill = 2;
        plain.fill::<1>(&data, 100);
        biased.fill::<1>(&data, 101);
        let a = plain.best_match::<_, 1, false>(simd, &data, 90, 1, 100, data.len(), 8, 4);
        let b = biased.best_match::<_, 1, false>(simd, &data, 90, 2, 101, data.len(), 8, 4);
        assert_eq!(b.match_idx, a.match_idx + 1);
        assert_eq!((a.forward, a.backward), (b.forward, b.backward));
    }

    fn the_field_size_aligns_lengths<S: Simd>(simd: S) {
        let data = data();
        let mut table = RowTable::<0>::new(10, 4);
        table.next_to_fill = 4;
        table.fill::<4>(&data, 200);
        // The period (31) is odd: only every fourth period lines up with a
        // field boundary, at distance 124.
        let found = table.best_match::<_, 4, false>(simd, &data, 188, 4, 200, data.len(), 8, 4);
        assert_eq!(found.match_idx, 76);
        assert_eq!(found.forward % 4, 0);
        assert_eq!(found.backward % 4, 0);
        assert!(found.total >= 4);
    }

    fn put_adds_a_single_position<S: Simd>(simd: S) {
        let data = data();
        let mut table = RowTable::<0>::new(10, 4);
        table.put(&data, 31);
        let found = table.best_match::<_, 1, false>(simd, &data, 0, 1, 62, data.len(), 8, 4);
        assert_eq!(found.match_idx, 31);
    }

    fn touching_rows_and_a_compile_time_hash_length_change_nothing<S: Simd>(simd: S) {
        let data = data();
        let mut plain = RowTable::<0>::new(10, 4);
        let mut fixed = RowTable::<0, 4>::new(10, 4);
        plain.next_to_fill = 1;
        fixed.next_to_fill = 1;
        plain.fill::<1>(&data, 100);
        fixed.fill::<1>(&data, 100);
        // Rows ahead, and a position past the end (a no-op).
        for ahead in [101, 150, 300, data.len() as u32, data.len() as u32 + 50] {
            fixed.touch(&data, ahead);
        }
        let a = plain.best_match::<_, 1, false>(simd, &data, 90, 1, 100, data.len(), 8, 4);
        let b = fixed.best_match::<_, 1, true>(simd, &data, 90, 1, 100, data.len(), 8, 4);
        assert_eq!(a, b);
        assert_eq!(plain.rows, fixed.rows);
    }

    /// Filling to the very end reads the zero padding like `put` does.
    #[test]
    fn fill_near_the_end_of_the_data_equals_single_puts() {
        for field in [1usize, 2, 4] {
            let data: Vec<u8> = (0..70u32).map(|i| (i % 9) as u8).collect();
            let end = data.len() as u32;
            let mut filled = RowTable::<0>::new(10, 8);
            let mut put = RowTable::<0>::new(10, 8);
            filled.next_to_fill = field as u32;
            match field {
                1 => filled.fill::<1>(&data, end),
                2 => filled.fill::<2>(&data, end),
                _ => filled.fill::<4>(&data, end),
            }
            for pos in (field as u32..end).step_by(field) {
                put.put(&data, pos);
            }
            assert_eq!(filled.rows, put.rows, "field size {field}");
            assert_eq!(filled.next_to_fill, end);
        }
    }

    #[test]
    fn every_simd_level_finds_the_same_matches() {
        for backend in Backend::available() {
            dispatch!(backend.0, simd => fill_inserts_every_field_and_best_match_finds_the_period(simd));
            dispatch!(backend.0, simd => no_candidate_reports_a_total_below_the_minimum(simd));
            dispatch!(backend.0, simd => candidates_below_the_low_limit_stop_the_search(simd));
            dispatch!(backend.0, simd => a_bias_shifts_the_index_of_every_byte(simd));
            dispatch!(backend.0, simd => the_field_size_aligns_lengths(simd));
            dispatch!(backend.0, simd => put_adds_a_single_position(simd));
            dispatch!(backend.0, simd => touching_rows_and_a_compile_time_hash_length_change_nothing(simd));
        }
    }
}
