//! The greedy match finder of `field_lz` (`ZS_greedyTokenLzMatchFinder`,
//! `encode_match_finder_greedy_field_lz.c`), used at levels 4 and above.
//!
//! Two row tables (a hash of `min_match` and of `2 * min_match` bytes) give
//! up to eight candidates per position; the longer hash is tried first.
//! After a match a second, lazy search near its end may replace it with a
//! longer one. Positions are offsets into the source and the tables hold
//! indices (offset plus one), as in [`super::fast`].

use super::{SeqStore, Sequence, check_match, count_fields};
use crate::compressor::core::codecs::lz_common::row_table::{Match, RowTable};

/// The index bias of the single-segment window.
const BIAS: usize = 1;
/// `nbSearches`.
const SEARCHES: usize = 8;

/// `minMatchLength`.
const fn min_match_length(field_size: u32) -> u32 {
    if field_size < 4 { 4 } else { field_size }
}

/// The state of one parse.
struct Finder<'a> {
    src: &'a [u8],
    field_bits: u32,
    min_match: u32,
    /// The table hashing `min_match` bytes.
    table: RowTable,
    /// The table hashing `2 * min_match` bytes.
    table2: RowTable,
    /// The lowest valid table index (`window->lowLimit`).
    low_limit: u32,
}

impl Finder<'_> {
    /// The sequence for a table match found at `ip`, and the position of
    /// the match source (rolled back over the backward extension).
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

    /// `findMatchGreedy`: the long hash first, then the short one; both
    /// tables learn `ip`.
    fn find_greedy(&mut self, anchor: isize, ip: isize) -> Option<(isize, Sequence)> {
        let pos = (ip + 1) as u32;
        let end = self.src.len();
        let min_length = self.min_match as usize;
        let found = self.table2.best_match(
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
        let found = self.table.best_match(
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
    fn find_lazy(&mut self, anchor: isize, ip: isize) -> Option<(isize, Sequence)> {
        let pos = (ip + 1) as u32;
        self.table2.fill(self.src, pos);
        let min_length = 2 * self.min_match as usize;
        let found = self.table2.best_match(
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
pub(super) fn parse(src: &[u8], field_size: u32, table_log: u32, low_limit: u32) -> SeqStore {
    let min_match = min_match_length(field_size);
    let mut finder = Finder {
        src,
        field_bits: field_size.trailing_zeros(),
        min_match,
        table: RowTable::new(table_log, field_size, min_match, BIAS),
        table2: RowTable::new(table_log, field_size, min_match * 2, BIAS),
        low_limit,
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
            for (r, &distance) in rep.iter().enumerate() {
                let matched = ip - distance as isize;
                if check_match(src, ip, matched, min_match) {
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
            finder.table.fill(src, stop);
            finder.table2.fill(src, stop);
        }
        ip += length;
        anchor = ip;
    }
    store.store_last_literals(src, anchor as usize);
    store
}
