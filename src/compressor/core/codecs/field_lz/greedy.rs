//! The greedy match finder of `field_lz` (`ZS_greedyTokenLzMatchFinder`,
//! `encode_match_finder_greedy_field_lz.c`), used at levels 4 and above.
//!
//! Two row tables (a hash of `min_match` and of `2 * min_match` bytes) give
//! up to eight candidates per position; the longer hash is tried first.
//! After a match a second, lazy search near its end may replace it with a
//! longer one. Positions are offsets into the source and the tables hold
//! indices (offset plus one), as in [`super::fast`].

use fearless_simd::Simd;

use super::{SeqStore, Sequence, check_match, count_fields};
use crate::compressor::core::codecs::lz_common::hash::{read_u32, read_u64};
use crate::compressor::core::codecs::lz_common::row_table::{Match, RowTable};

/// The index bias of the single-segment window.
const BIAS: usize = 1;
/// `nbSearches`.
const SEARCHES: usize = 8;
/// The positions [`Finder::warm`] warms at a time.
const WARM_BATCH: u32 = 8;

/// The `K` (4 or 8) bytes at `at` as a number, zero padded past the end of
/// `src`: the repcode test compares two of them.
#[inline(always)]
fn read_word<const K: u32>(src: &[u8], at: usize) -> u64 {
    if K == 4 {
        u64::from(read_u32(src, at))
    } else {
        read_u64(src, at)
    }
}

/// `minMatchLength`.
const fn min_match_length(field_size: u32) -> u32 {
    if field_size < 4 { 4 } else { field_size }
}

/// The state of one parse.
struct Finder<'a, S: Simd, const FIELD: u32, const MIN: u32, const LONG: u32> {
    simd: S,
    src: &'a [u8],
    field_bits: u32,
    min_match: u32,
    /// The table hashing `min_match` bytes.
    table: RowTable<BIAS, MIN>,
    /// The table hashing `2 * min_match` bytes.
    table2: RowTable<BIAS, LONG>,
    /// The lowest valid table index (`window->lowLimit`).
    low_limit: u32,
    /// The position from which [`Self::warm`] runs next.
    warm_next: u32,
}

impl<S: Simd, const FIELD: u32, const MIN: u32, const LONG: u32> Finder<'_, S, FIELD, MIN, LONG> {
    /// The sequence for a table match found at `ip`, and the position of
    /// the match source (rolled back over the backward extension).
    #[inline(always)]
    fn sequence(&self, found: Match, anchor: isize, ip: isize) -> (isize, Sequence) {
        let pos = (ip + 1) as u32;
        let offset = pos - found.match_idx;
        (
            found.match_idx as isize - 1 - found.backward as isize,
            Sequence {
                rep: false,
                match_code: offset >> self.field_bits,
                match_length: found.total as u32,
                literal_length: ((ip - found.backward as isize) - anchor) as u32,
            },
        )
    }

    /// Warms the rows of both tables for the next [`WARM_BATCH`] positions,
    /// starting four fields after `pos`, in one batch: the loads are
    /// independent, so their cache misses overlap, where one load per
    /// search would stall the pipeline on every position. Safe Rust has no
    /// prefetch instruction (`ZL_PREFETCH_L1` in the reference).
    #[inline(never)]
    fn warm(&mut self, pos: u32) {
        for k in 0..WARM_BATCH {
            let ahead = pos.wrapping_add((4 + k) * FIELD);
            self.table.touch(self.src, ahead);
            self.table2.touch(self.src, ahead);
        }
        self.warm_next = pos.wrapping_add(WARM_BATCH * FIELD);
    }

    /// `findMatchGreedy`: the long hash first, then the short one; both
    /// tables learn `ip`.
    #[inline(always)]
    fn find_greedy(&mut self, anchor: isize, ip: isize) -> Option<(isize, Sequence)> {
        let pos = (ip + 1) as u32;
        if pos >= self.warm_next {
            self.warm(pos);
        }
        let end = self.src.len();
        let min_length = self.min_match as usize;
        let found = self.table2.best_match::<_, FIELD, true>(
            self.simd,
            self.src,
            anchor as usize,
            self.low_limit,
            pos,
            end,
            SEARCHES,
            min_length,
        );
        if found.total >= min_length {
            return Some(self.sequence(found, anchor, ip));
        }
        let found = self.table.best_match::<_, FIELD, true>(
            self.simd,
            self.src,
            anchor as usize,
            self.low_limit,
            pos,
            end,
            SEARCHES,
            min_length,
        );
        if found.total >= min_length {
            return Some(self.sequence(found, anchor, ip));
        }
        None
    }

    /// `findMatchLazy`: a long-hash search at `ip` after catching the
    /// table up.
    #[inline(always)]
    fn find_lazy(&mut self, anchor: isize, ip: isize) -> Option<(isize, Sequence)> {
        let pos = (ip + 1) as u32;
        self.table2.fill::<FIELD>(self.src, pos);
        let min_length = 2 * self.min_match as usize;
        let found = self.table2.best_match::<_, FIELD, true>(
            self.simd,
            self.src,
            anchor as usize,
            self.low_limit,
            pos,
            self.src.len(),
            SEARCHES,
            min_length,
        );
        if found.total >= min_length {
            return Some(self.sequence(found, anchor, ip));
        }
        None
    }
}

/// `ZS_greedyTokenLzMatchFinder_parseT`: parses `src`, a whole number of
/// fields of `field_size` (2, 4 or 8) bytes, into sequences and literals.
#[inline(always)]
pub(super) fn parse<S: Simd, const FIELD: u32, const MIN: u32, const LONG: u32>(
    simd: S,
    src: &[u8],
    table_log: u32,
    low_limit: u32,
) -> SeqStore {
    let field_size = FIELD;
    let min_match = min_match_length(field_size);
    let mut finder: Finder<'_, S, FIELD, MIN, LONG> = Finder {
        simd,
        src,
        field_bits: field_size.trailing_zeros(),
        min_match,
        table: RowTable::new(table_log, min_match),
        table2: RowTable::new(table_log, min_match * 2),
        low_limit,
        warm_next: 0,
    };
    let mut store = SeqStore::default();
    let fs = field_size as isize;
    let min = min_match as isize;
    let end = src.len() as isize;
    let limit = end - (2 * fs).max(16);
    let mut anchor: isize = 0;
    let mut rep = [field_size, 2 * field_size];
    let mut ip = rep[1] as isize;
    finder.table.next_to_fill = (ip + 1) as u32;
    finder.table2.next_to_fill = (ip + 1) as u32;

    while ip < limit {
        // The first match, and a second one found near its end.
        let mut first: Option<(isize, Sequence)> = None;
        let mut second: Option<(isize, Sequence)> = None;
        loop {
            let here = read_word::<MIN>(src, ip as usize);
            for (r, &distance) in rep.iter().enumerate() {
                let matched = ip - distance as isize;
                if matched >= 0 && read_word::<MIN>(src, matched as usize) == here {
                    first = Some((
                        matched,
                        Sequence {
                            rep: true,
                            match_code: r as u32,
                            literal_length: (ip - anchor) as u32,
                            match_length: count_fields(src, ip, matched, field_size) as u32,
                        },
                    ));
                    break;
                }
            }
            if first.is_none() {
                first = finder.find_greedy(anchor, ip);
                if let Some((_, seq)) = &first {
                    // A second search `min` bytes before the end of the
                    // match, for a longer one.
                    let match_end =
                        anchor + seq.literal_length as isize + seq.match_length as isize;
                    if match_end > ip + min && match_end < limit {
                        second = finder.find_lazy(anchor, match_end - min);
                    }
                }
            }
            if first.is_some() {
                break;
            }
            ip += fs;
            if ip >= limit {
                break;
            }
        }
        let Some((mut matched, mut seq)) = first else {
            continue;
        };

        ip = anchor + seq.literal_length as isize;
        // Roll the match back; only repcode matches have anything left.
        while ip > anchor && matched > fs - 1 && check_match(src, ip - fs, matched - fs, field_size)
        {
            ip -= fs;
            matched -= fs;
            seq.match_length += field_size;
            seq.literal_length -= field_size;
        }

        if let Some((_, later)) = second {
            // The second match ends after the first: with no more literals
            // it is also the longer one.
            if later.literal_length <= seq.literal_length {
                seq = later;
                ip = anchor + seq.literal_length as isize;
            }
        }

        if seq.rep {
            if seq.match_code != 0 {
                let code = seq.match_code as usize;
                let offset = rep[code];
                rep.copy_within(0..code, 1);
                rep[0] = offset;
            }
        } else {
            rep[1] = rep[0];
            rep[0] = seq.match_code << finder.field_bits;
        }
        store.store(src, anchor as usize, seq);

        let length = seq.match_length as isize;
        if ip + length <= limit {
            let stop = (ip + length + 1) as u32;
            finder.table.fill::<FIELD>(src, stop);
            finder.table2.fill::<FIELD>(src, stop);
        }
        ip += length;
        anchor = ip;
    }
    store.store_last_literals(src, anchor as usize);
    core::hint::black_box((finder.table.warmed(), finder.table2.warmed()));
    store
}
