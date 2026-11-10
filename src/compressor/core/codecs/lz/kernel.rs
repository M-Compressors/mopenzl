//! The LZ77 kernels of the `lz` codec (`encode_lz_kernel.c`): parameters,
//! and the fast, double-fast and row-table (greedy, lazy, lazy2) parsers.
//!
//! The parsers produce sequences of `(literal length, match length,
//! offset)`; a match is at least 4 bytes except when a length overflowing
//! 16 bits is split across sequences. Positions are signed offsets into the
//! source, as in the reference (`ptrdiff_t`).

use alloc::vec::Vec;

use fearless_simd::{Level, Simd, dispatch};

use crate::compressor::core::codecs::lz_common::fast_table::FastTable;
use crate::compressor::core::codecs::lz_common::hash::read_u32;
use crate::compressor::core::codecs::lz_common::hash::read_u64;
use crate::compressor::core::codecs::lz_common::row_table::{Match, ROW_LOG, RowTable};

/// `ZL_LzStrategy_fast`.
pub(super) const FAST: u32 = 1;
/// `ZL_LzStrategy_doubleFast`.
pub(super) const DOUBLE_FAST: u32 = 2;
/// `ZL_LzStrategy_greedy`.
pub(super) const GREEDY: u32 = 3;
/// `ZL_LzStrategy_lazy`.
pub(super) const LAZY: u32 = 4;
/// `ZL_LzStrategy_lazy2`.
pub(super) const LAZY2: u32 = 5;

/// The hash length of every row-table preset in `kDefaultParams`.
const DEFAULT_HASH_LENGTH: u32 = 5;
/// `ZL_LZ_MIN_MATCH`.
pub(super) const MIN_MATCH: usize = 4;
/// `ZL_LZ_MAX_LENGTH`: lengths are 16-bit.
const MAX_LENGTH: isize = 65_535;
/// `ZL_LZ_MAX_OFFSET_U32` (`ZL_PARTITION_MAX_PARTITION_SIZE_FOR_UNROLL2`).
const MAX_OFFSET_U32: isize = 1 << 28;
/// `ZL_LZ_MAX_OFFSET_U16`.
const MAX_OFFSET_U16: isize = 1 << 16;
/// `kSrcOverLength`: `max(ZL_LZ_LIT_OVER_LENGTH, ZL_LZ_MATCH_OVER_LENGTH)`.
const SRC_OVER_LENGTH: isize = 32;
/// `ZL_LZ_SEARCH_STRENGTH`.
const SEARCH_STRENGTH: u32 = 8;

/// `ZL_LzParameters`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Parameters {
    pub(super) strategy: u32,
    pub(super) window_log: u32,
    pub(super) hash_log1: u32,
    pub(super) hash_log2: u32,
    pub(super) hash_length: u32,
    pub(super) acceleration: u32,
    pub(super) search_log: u32,
}

/// One row of `kDefaultParams`: `(strategy, W, H1, H2, HL, S)`; the
/// acceleration of a preset is 1.
macro_rules! preset {
    ($strategy:expr, $window_log:expr, $hash_log1:expr, $hash_log2:expr, $hash_length:expr, $search_log:expr) => {
        Parameters {
            strategy: $strategy,
            window_log: $window_log,
            hash_log1: $hash_log1,
            hash_log2: $hash_log2,
            hash_length: $hash_length,
            acceleration: 1,
            search_log: $search_log,
        }
    };
}

/// `kDefaultParams`.
const DEFAULTS: [Parameters; 8] = [
    preset!(FAST, 19, 13, 0, 7, 0),
    preset!(FAST, 19, 14, 0, 7, 0),
    preset!(FAST, 19, 16, 0, 6, 0),
    preset!(DOUBLE_FAST, 21, 17, 16, 5, 0),
    preset!(DOUBLE_FAST, 21, 18, 18, 5, 0),
    preset!(GREEDY, 22, 18, 0, 5, 2),
    preset!(LAZY, 22, 18, 0, 5, 3),
    preset!(LAZY2, 22, 19, 0, 5, 4),
];

impl Parameters {
    /// `ZL_LzParameters_default`.
    pub(super) fn for_level(level: i32) -> Self {
        let row = if level == 0 {
            1
        } else {
            level.clamp(0, 7) as usize
        };
        let mut params = DEFAULTS[row];
        if level < 0 {
            params.acceleration = level.wrapping_neg() as u32;
        }
        params
    }

    /// Whether the strategy searches a [`RowTable`].
    const fn uses_row_table(&self) -> bool {
        matches!(self.strategy, GREEDY | LAZY | LAZY2)
    }

    /// `ZL_LzParameters_adjust`: clamps every parameter to its range and
    /// shrinks the window and tables for a source of `src_size` bytes.
    pub(super) fn adjust(&mut self, src_size: usize) {
        self.strategy = self.strategy.clamp(FAST, LAZY2);
        self.window_log = self.window_log.clamp(10, 28);
        self.hash_log1 = self.hash_log1.clamp(10, 23);
        self.hash_log2 = self.hash_log2.clamp(10, 23);
        self.hash_length = self.hash_length.clamp(4, 7);
        self.acceleration = self.acceleration.clamp(1, 10_000);
        self.search_log = self.search_log.clamp(1, 4);
        // `ZL_nextPow2`: the least n with 2^n >= src_size.
        let src_log = if src_size <= 1 {
            0
        } else {
            (src_size - 1).ilog2() + 1
        };
        self.window_log = self.window_log.min(src_log);
        self.hash_log1 = self.hash_log1.min(self.window_log + 1);
        self.hash_log2 = self.hash_log2.min(self.window_log + 1);
        if self.strategy == FAST || self.uses_row_table() {
            self.hash_log2 = 0;
        }
        if self.uses_row_table() {
            self.hash_log1 = self.hash_log1.max(ROW_LOG);
        }
    }

    /// The width in bytes of an offset: 16 bits when the window fits.
    pub(super) const fn offset_width(&self) -> usize {
        if self.window_log > 16 { 4 } else { 2 }
    }
}

/// `ZL_Lz_OutSequences`, as vectors.
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct Sequences {
    pub(super) literals: Vec<u8>,
    pub(super) offsets: Vec<u32>,
    pub(super) literal_lengths: Vec<u16>,
    pub(super) match_lengths: Vec<u16>,
}

impl Sequences {
    /// `storeSequence`: the `literal_length` bytes at `lit_start`, then the
    /// sequence(s) for the lengths and offset, splitting a literal run or a
    /// match that does not fit 16 bits.
    fn store(
        &mut self,
        src: &[u8],
        lit_start: isize,
        mut literal_length: isize,
        match_length: isize,
        offset: isize,
    ) {
        let from = lit_start as usize;
        self.literals
            .extend_from_slice(&src[from..from + literal_length as usize]);
        // A literal run too long for one sequence is split into sequences
        // with no match and offset 1.
        while literal_length > MAX_LENGTH {
            self.push(MAX_LENGTH, 0, 1);
            literal_length -= MAX_LENGTH;
        }
        if match_length <= MAX_LENGTH {
            self.push(literal_length, match_length, offset);
            return;
        }
        // The final piece of a split match may be shorter than the minimum.
        let mut remaining = match_length;
        while remaining > 0 {
            let piece = remaining.min(MAX_LENGTH);
            self.push(literal_length, piece, offset);
            literal_length = 0;
            remaining -= piece;
        }
    }

    fn push(&mut self, literal_length: isize, match_length: isize, offset: isize) {
        self.literal_lengths.push(literal_length as u16);
        self.match_lengths.push(match_length as u16);
        self.offsets.push(offset as u32);
    }

    /// The trailing literals.
    fn finish(&mut self, src: &[u8], lit_start: isize) {
        self.literals.extend_from_slice(&src[lit_start as usize..]);
    }
}

/// `getMaxOffset`.
fn max_offset(offset_width: usize, window_log: u32) -> isize {
    let limit = if offset_width == 4 {
        MAX_OFFSET_U32
    } else {
        MAX_OFFSET_U16
    };
    limit.min(1 << window_log)
}

/// `matchLength`: the equal bytes at `a` and `b`, up to `end`.
fn match_length(src: &[u8], a: isize, b: isize, end: isize) -> isize {
    let (a, b, end) = (a as usize, b as usize, end as usize);
    let mut length = 0;
    while a + length + 8 <= end {
        let diff = read_u64(src, a + length) ^ read_u64(src, b + length);
        if diff != 0 {
            return (length + (diff.trailing_zeros() >> 3) as usize) as isize;
        }
        length += 8;
    }
    while a + length < end && src[a + length] == src[b + length] {
        length += 1;
    }
    length as isize
}

/// The parameters every parser needs.
struct Cursor {
    max_offset: isize,
    limit: isize,
    first_step: isize,
}

impl Cursor {
    fn new(params: &Parameters, offset_width: usize, src_size: usize) -> Self {
        Self {
            max_offset: max_offset(offset_width, params.window_log),
            limit: src_size as isize - SRC_OVER_LENGTH,
            first_step: params.acceleration.max(1) as isize,
        }
    }
}

/// `ZL_Lz_encodeFastImpl`.
fn encode_fast(src: &[u8], params: &Parameters, offset_width: usize, hash_len: u32) -> Sequences {
    let mut out = Sequences::default();
    let cursor = Cursor::new(params, offset_width, src.len());
    let mut table = FastTable::new(params.hash_log1, hash_len);
    let end = src.len() as isize;
    let mut lit_start: isize = 0;
    let mut pos: isize = 1;
    let step_increment: isize = 1 << SEARCH_STRENGTH;
    let mut step = cursor.first_step;
    let mut next_step = pos + step_increment;

    while pos <= cursor.limit {
        let mut matched = table.get_and_update(src, pos as usize, pos as u32) as isize;
        let distance = pos - matched;
        if read_u32(src, matched as usize) == read_u32(src, pos as usize)
            && distance < cursor.max_offset
        {
            let mut length = 4 + match_length(src, pos + 4, matched + 4, end);
            // Walk the match backwards.
            while matched > 0
                && pos > lit_start
                && src[matched as usize - 1] == src[pos as usize - 1]
            {
                matched -= 1;
                pos -= 1;
                length += 1;
            }
            out.store(src, lit_start, pos - lit_start, length, distance);
            table.put(src, pos as usize + 2, pos as u32 + 2);
            pos += length;
            if pos <= cursor.limit {
                table.put(src, pos as usize - 2, pos as u32 - 2);
            }
            lit_start = pos;
            step = cursor.first_step;
            next_step = pos + step_increment;
        } else {
            pos += step;
            // Skip incompressible data faster the longer it lasts.
            if pos >= next_step {
                step += 1;
                next_step += step_increment;
            }
        }
    }
    out.finish(src, lit_start);
    out
}

/// `ZL_Lz_encodeDoubleFastImpl`.
fn encode_double_fast(
    src: &[u8],
    params: &Parameters,
    offset_width: usize,
    hash_len: u32,
) -> Sequences {
    let mut out = Sequences::default();
    let cursor = Cursor::new(params, offset_width, src.len());
    let mut long = FastTable::new(params.hash_log1, 8);
    let mut short = FastTable::new(params.hash_log2, hash_len);
    let end = src.len() as isize;
    let mut lit_start: isize = 0;
    let mut pos: isize = 1;
    let step_increment: isize = 1 << SEARCH_STRENGTH;
    let mut step = cursor.first_step;
    let mut next_step = pos + step_increment;

    loop {
        let mut distance = 0;
        let mut matched = 0;
        let mut length = 0;
        let mut pos1 = pos + step;
        while pos1 <= cursor.limit {
            let at = pos as usize;
            let long_match = long.get_and_update(src, at, pos as u32) as isize;
            let short_match = short.get_and_update(src, at, pos as u32) as isize;
            let long_distance = pos - long_match;
            let short_distance = pos - short_match;
            if read_u64(src, long_match as usize) == read_u64(src, at)
                && long_distance < cursor.max_offset
            {
                matched = long_match;
                length = 8 + match_length(src, pos + 8, long_match + 8, end);
                distance = long_distance;
                break;
            } else if read_u32(src, short_match as usize) == read_u32(src, at)
                && short_distance < cursor.max_offset
            {
                matched = short_match;
                length = 4 + match_length(src, pos + 4, short_match + 4, end);
                distance = short_distance;
                // Look for a longer match at the next probe. It is only safe
                // to store that position when the step is below 4: the
                // short match already skips at least 4.
                let next =
                    long.get_and_conditional_update(src, pos1 as usize, pos1 as u32, step < 4)
                        as isize;
                let next_distance = pos1 - next;
                if read_u64(src, next as usize) == read_u64(src, pos1 as usize)
                    && next_distance < cursor.max_offset
                {
                    let next_length = 8 + match_length(src, pos1 + 8, next + 8, end);
                    if next_length > length {
                        pos = pos1;
                        matched = next;
                        length = next_length;
                        distance = next_distance;
                    }
                }
                break;
            }
            pos += step;
            pos1 = pos + step;
            if pos >= next_step {
                step += 1;
                next_step += step_increment;
            }
        }
        // The search only ends without a match when it passes the limit.
        if pos1 > cursor.limit {
            break;
        }

        while matched > 0 && pos > lit_start && src[matched as usize - 1] == src[pos as usize - 1] {
            matched -= 1;
            pos -= 1;
            length += 1;
        }
        out.store(src, lit_start, pos - lit_start, length, distance);
        long.put(src, pos as usize + 2, pos as u32 + 2);
        short.put(src, pos as usize + 2, pos as u32 + 2);
        pos += length;
        if pos <= cursor.limit {
            long.put(src, pos as usize - 2, pos as u32 - 2);
            short.put(src, pos as usize - 2, pos as u32 - 2);
        }
        lit_start = pos;
        step = cursor.first_step;
        next_step = pos + step_increment;
    }
    out.finish(src, lit_start);
    out
}

/// `rowSearch`: fills the positions the parser skipped, then searches the
/// row table for the best match at `pos`.
#[inline(always)]
fn row_search<S: Simd>(
    simd: S,
    table: &mut RowTable<0>,
    src: &[u8],
    anchor: isize,
    pos: isize,
    max_offset: isize,
    searches: usize,
) -> Match {
    // Bounding the search by the window also rejects the never-written
    // entries, which read back as position 0.
    let low_limit = (pos - max_offset + 1).max(1) as u32;
    table.fill::<1>(src, pos as u32);
    table.best_match::<_, 1, false>(
        simd,
        src,
        anchor as usize,
        low_limit,
        pos as u32,
        src.len(),
        searches,
        MIN_MATCH,
    )
}

/// `ZL_Lz_encodeLazyImpl`: greedy (`depth` 0), lazy (1) and lazy2 (2).
#[inline(always)]
fn encode_lazy<S: Simd, const DEPTH: u32, const HASH: u32>(
    simd: S,
    src: &[u8],
    params: &Parameters,
    offset_width: usize,
    hash_len: u32,
) -> Sequences {
    // A non-zero `HASH` is the hash length known at compile time, which
    // folds the hash function's length dispatch.
    let hash_len = if HASH == 0 { hash_len } else { HASH };
    let depth = DEPTH;
    let mut out = Sequences::default();
    let cursor = Cursor::new(params, offset_width, src.len());
    let acceleration = params.acceleration.max(1) as isize;
    let searches = 1usize << params.search_log;
    let mut table = RowTable::<0>::new(params.hash_log1, hash_len);
    // Position 0 is indistinguishable from an empty entry: never insert it.
    let mut lit_start: isize = 0;
    let mut pos: isize = 1;
    table.next_to_fill = 1;

    while pos < cursor.limit {
        let found = row_search(
            simd,
            &mut table,
            src,
            lit_start,
            pos,
            cursor.max_offset,
            searches,
        );
        if found.total < MIN_MATCH {
            pos += ((pos - lit_start) >> SEARCH_STRENGTH) + acceleration;
            continue;
        }
        let mut start = pos - found.backward as isize;
        let mut length = found.total as isize;
        let mut distance = pos - found.match_idx as isize;

        // Look one (lazy2: two) byte later for a better match, which costs
        // a literal but may pay for itself.
        let mut level = 0;
        while level < depth && pos + 1 < cursor.limit {
            pos += 1;
            let next = row_search(
                simd,
                &mut table,
                src,
                lit_start,
                pos,
                cursor.max_offset,
                searches,
            );
            if next.total < MIN_MATCH {
                break;
            }
            let next_distance = pos - next.match_idx as isize;
            // The gain heuristic of zstd_lazy.c.
            let gain_next = (next.total * 4) as i32 - (next_distance as u32).ilog2() as i32;
            let gain_current = (length * 4) as i32 - (distance as u32).ilog2() as i32
                + (4 * (level + 1) - 1) as i32;
            if gain_next <= gain_current {
                break;
            }
            start = pos - next.backward as isize;
            length = next.total as isize;
            distance = next_distance;
            level += 1;
        }

        out.store(src, lit_start, start - lit_start, length, distance);
        pos = start + length;
        lit_start = pos;
    }
    out.finish(src, lit_start);
    core::hint::black_box(table.warmed());
    out
}

/// [`encode_lazy`] at `level` with the hash length of the presets
/// (`kDefaultParams`) compiled in; other lengths run the general code.
fn lazy<const DEPTH: u32>(
    level: Level,
    src: &[u8],
    params: &Parameters,
    offset_width: usize,
    hash_len: u32,
) -> Sequences {
    if hash_len == DEFAULT_HASH_LENGTH {
        dispatch!(level, simd => encode_lazy::<_, DEPTH, DEFAULT_HASH_LENGTH>(simd, src, params, offset_width, hash_len))
    } else {
        dispatch!(level, simd => encode_lazy::<_, DEPTH, 0>(simd, src, params, offset_width, hash_len))
    }
}

/// `ZL_Lz_encode`: parses `src` with the strategy of `params`.
///
/// `params` must have been [adjusted](Parameters::adjust).
pub(super) fn encode(level: Level, src: &[u8], params: &Parameters) -> Sequences {
    if src.is_empty() {
        return Sequences::default();
    }
    let offset_width = params.offset_width();
    // `hashLen <= 4`, 5, 6 or the rest.
    let hash_len = params.hash_length.clamp(4, 7);
    match params.strategy {
        GREEDY => lazy::<0>(level, src, params, offset_width, hash_len),
        LAZY => lazy::<1>(level, src, params, offset_width, hash_len),
        LAZY2 => lazy::<2>(level, src, params, offset_width, hash_len),
        DOUBLE_FAST => encode_double_fast(src, params, offset_width, hash_len),
        _ => encode_fast(src, params, offset_width, hash_len),
    }
}

/// `ZL_Lz_maxNumSequences`.
pub(super) const fn max_sequences(src_size: usize) -> usize {
    if src_size == 0 {
        0
    } else {
        src_size / MIN_MATCH + src_size / MAX_LENGTH as usize + 2
    }
}
