//! `LZ4_compress_HC` (lz4hc.c) for a fresh stream without a dictionary and
//! an output buffer of `LZ4_compressBound` bytes (`notLimited`).
//!
//! The level selects the strategy of `k_clTable`: level 2 is `lz4mid`
//! (two hash tables), 3 to 9 `lz4hc` (hash chains), 10 to 12 `lz4opt` (the
//! optimal parser in [`super::optimal`]). Positions are offsets into the
//! source; the reference's indices start at `LZ4HC_init_internal`'s 64 KiB
//! (`PREFIX_IDX`) and wrap as `U32`.
//!
//! Branches the reference reserves for an external dictionary or a
//! dictionary context are dead for a fresh stream (`dictStart == prefixStart`,
//! `lowLimit == dictLimit`) and are not ported, nor are `limitedOutput` and
//! `fillOutput`. `favorDecSpeed` is always off.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::block::{
    DISTANCE_MAX, FIELD_MAX, LAST_LITERALS, MF_LIMIT, MIN_LENGTH, MIN_MATCH, compress_bound, count,
    push_last_literals, push_length, read32, read64,
};
use super::optimal;

/// `LZ4HC_HASH_LOG`.
const HASH_LOG: u32 = 15;
/// `LZ4MID_HASHLOG`.
const MID_HASH_LOG: u32 = HASH_LOG - 1;
/// `LZ4MID_HASHSIZE`.
const MID_HASH_SIZE: usize = 8;
/// `LZ4HC_CLEVEL_DEFAULT`.
const LEVEL_DEFAULT: i32 = 9;
/// `LZ4HC_CLEVEL_MAX`.
const LEVEL_MAX: i32 = 12;
/// `dictLimit` and `lowLimit` after `LZ4HC_init_internal` on a fresh state.
const PREFIX_IDX: u32 = 64 * 1024;
/// `OPTIMAL_ML`.
const OPTIMAL_ML: isize = (FIELD_MAX as isize - 1) + MIN_MATCH as isize;

/// `lz4hc_strat_e`.
#[derive(Copy, Clone, Eq, PartialEq)]
pub(super) enum Strategy {
    Mid,
    Chain,
    Optimal,
}

/// `cParams_t`.
#[derive(Copy, Clone)]
pub(super) struct LevelParams {
    pub(super) strategy: Strategy,
    pub(super) searches: i32,
    pub(super) target_length: usize,
}

/// `LZ4HC_getCLevelParams` (`k_clTable`).
pub(super) const fn level_params(level: i32) -> LevelParams {
    let level = if level < 1 { LEVEL_DEFAULT } else { level };
    let level = if level > LEVEL_MAX { LEVEL_MAX } else { level };
    let (strategy, searches, target_length) = match level {
        0..=2 => (Strategy::Mid, 2, 16),
        3 => (Strategy::Chain, 4, 16),
        4 => (Strategy::Chain, 8, 16),
        5 => (Strategy::Chain, 16, 16),
        6 => (Strategy::Chain, 32, 16),
        7 => (Strategy::Chain, 64, 16),
        8 => (Strategy::Chain, 128, 16),
        9 => (Strategy::Chain, 256, 16),
        10 => (Strategy::Optimal, 96, 64),
        11 => (Strategy::Optimal, 512, 128),
        _ => (Strategy::Optimal, 16_384, optimal::OPT_NUM),
    };
    LevelParams {
        strategy,
        searches,
        target_length,
    }
}

/// `LZ4HC_match_t`.
#[derive(Copy, Clone, Default)]
pub(super) struct Match {
    pub(super) off: i32,
    pub(super) len: i32,
    /// Negative: bytes the match extends before the search position.
    pub(super) back: i32,
}

/// `repeat_state_e`.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Repeat {
    Untested,
    Not,
    Confirmed,
}

/// `LZ4HC_hashPtr` of the four bytes `sequence`.
const fn hash_ptr(sequence: u32) -> usize {
    (sequence.wrapping_mul(2_654_435_761) >> (MIN_MATCH as u32 * 8 - HASH_LOG)) as usize
}

/// `LZ4MID_hash4`.
const fn mid_hash4(sequence: u32) -> usize {
    (sequence.wrapping_mul(2_654_435_761) >> (32 - MID_HASH_LOG)) as usize
}

/// `LZ4MID_hash7`: hashes the low 56 bits.
const fn mid_hash7(value: u64) -> usize {
    ((value << 8).wrapping_mul(58_295_818_150_454_627) >> (64 - MID_HASH_LOG)) as usize
}

/// Two bytes at `at` little-endian as a non-negative value, `-2` outside the
/// source (never equal to [`head16`]'s `-1`, nor to a byte pair).
#[inline(always)]
fn peek16(src: &[u8], at: usize) -> i32 {
    // One comparison against a loop-invariant bound; a wrapped (negative)
    // position is above every bound.
    match src.len().checked_sub(2) {
        Some(last) if at <= last => match src[at..at + 2] {
            [low, high] => i32::from(u16::from_le_bytes([low, high])),
            _ => -2,
        },
        _ => -2,
    }
}

/// The two bytes of the best match so far that a candidate must repeat
/// (`LZ4_read16(iLowLimit + longest - 1)`), `-1` outside the source.
#[inline(always)]
fn head16(src: &[u8], i_low: usize, longest: i32) -> i32 {
    match peek16(src, i_low + longest as usize - 1) {
        -2 => -1,
        value => value,
    }
}

/// The length of the match of `ip` and `match_ptr` (four bytes known equal)
/// extended forward to `i_high` and back to `i_low`, and the count of bytes
/// it extends back (non-positive).
#[inline(always)]
fn extend(
    src: &[u8],
    ip: usize,
    match_ptr: usize,
    i_low: usize,
    i_high: usize,
    look_back: i32,
) -> (i32, i32) {
    let back = if look_back != 0 {
        count_back(src, ip, match_ptr, i_low, 0)
    } else {
        0
    };
    let length =
        MIN_MATCH as i32 + count(src, ip + MIN_MATCH, match_ptr + MIN_MATCH, i_high) as i32;
    (length - back, back)
}

/// `LZ4HC_countBack`: a non-positive count of equal bytes before `ip` and
/// `matched`, not passing `i_min` or `m_min`.
#[inline]
fn count_back(src: &[u8], ip: usize, matched: usize, i_min: usize, m_min: usize) -> i32 {
    // The most bytes that may be counted.
    let cap = ip.saturating_sub(i_min).min(matched.saturating_sub(m_min));
    if cap == 0 {
        return 0;
    }
    // Most differences lie within eight bytes: one read settles them. The
    // equal bytes before the first difference are the high-order zero bytes
    // of the xor.
    if ip >= 8
        && matched >= 8
        && matched <= ip
        && ip <= src.len()
        && let (Ok(a), Ok(b)) = (
            <[u8; 8]>::try_from(&src[ip - 8..ip]),
            <[u8; 8]>::try_from(&src[matched - 8..matched]),
        )
    {
        let value = u64::from_le_bytes(a) ^ u64::from_le_bytes(b);
        let equal = (value.leading_zeros() >> 3) as usize;
        if equal < 8 || cap <= 8 {
            return -(equal.min(cap) as i32);
        }
    }
    count_back_long(src, ip, matched, cap)
}

/// [`count_back`] for a difference beyond the first eight bytes or a source
/// too short for a whole read.
#[inline(never)]
fn count_back_long(src: &[u8], ip: usize, matched: usize, cap: usize) -> i32 {
    let mut done = 0usize;
    while done < cap {
        let (a, b) = (ip - done, matched - done);
        if a < 8 || b < 8 {
            // Too close to the start of the source for a whole read.
            while done < cap && src[ip - done - 1] == src[matched - done - 1] {
                done += 1;
            }
            break;
        }
        let value = read64(src, a - 8) ^ read64(src, b - 8);
        let equal = (value.leading_zeros() >> 3) as usize;
        done += equal.min(cap - done);
        if equal < 8 {
            break;
        }
    }
    -(done as i32)
}

/// `LZ4HC_countPattern`: bytes from `ip` repeating the four-byte pattern.
fn count_pattern(src: &[u8], mut ip: usize, end: usize, pattern32: u32) -> usize {
    let start = ip;
    let pattern = u64::from(pattern32) + (u64::from(pattern32) << 32);
    while ip + 7 < end {
        let diff = read64(src, ip) ^ pattern;
        if diff == 0 {
            ip += 8;
            continue;
        }
        ip += (diff.trailing_zeros() >> 3) as usize;
        return ip - start;
    }
    let mut pattern_byte = pattern;
    while ip < end && src[ip] == pattern_byte as u8 {
        ip += 1;
        pattern_byte >>= 8;
    }
    ip - start
}

/// `LZ4HC_reverseCountPattern`: bytes before `ip` repeating the pattern.
fn reverse_count_pattern(src: &[u8], mut ip: usize, low: usize, pattern: u32) -> usize {
    let start = ip;
    while ip >= low + 4 {
        if read32(src, ip - 4) != pattern {
            break;
        }
        ip -= 4;
    }
    let bytes = pattern.to_le_bytes();
    let mut index = 3usize;
    while ip > low {
        if src[ip - 1] != bytes[index] {
            break;
        }
        ip -= 1;
        // The reference walks a pointer here; at most three bytes match
        // once the four-byte loop has stopped.
        index = index.saturating_sub(1);
    }
    start - ip
}

/// `LZ4HC_protectDictEnd`.
const fn protect_dict_end(dict_limit: u32, match_index: u32) -> bool {
    dict_limit.wrapping_sub(1).wrapping_sub(match_index) >= 3
}

/// `LZ4HC_encodeSequence` with `notLimited`: appends a sequence and returns
/// nothing; the caller advances `ip` and `anchor`.
pub(super) fn encode_sequence(
    out: &mut Vec<u8>,
    src: &[u8],
    ip: &mut usize,
    anchor: &mut usize,
    match_length: i32,
    offset: i32,
) {
    let length = *ip - *anchor;
    let token = out.len();
    if length >= FIELD_MAX {
        out.push((FIELD_MAX << 4) as u8);
        push_length(out, length - FIELD_MAX);
    } else {
        out.push((length << 4) as u8);
    }
    out.extend_from_slice(&src[*anchor..*ip]);
    out.extend_from_slice(&(offset as u16).to_le_bytes());
    let mut length = (match_length as usize) - MIN_MATCH;
    if length >= FIELD_MAX {
        out[token] += FIELD_MAX as u8;
        length -= FIELD_MAX;
        while length >= 510 {
            out.push(255);
            out.push(255);
            length -= 510;
        }
        if length >= 255 {
            length -= 255;
            out.push(255);
        }
        out.push(length as u8);
    } else {
        out[token] += length as u8;
    }
    *ip += match_length as usize;
    *anchor = *ip;
}

/// `LZ4HC_CCtx_internal` for a fresh stream.
pub(super) struct Hc<'a> {
    pub(super) src: &'a [u8],
    hash_table: Box<[u32; 1 << HASH_LOG]>,
    chain_table: Box<[u16; 1 << 16]>,
    next_to_update: u32,
}

impl<'a> Hc<'a> {
    /// `LZ4_initStreamHC` followed by `LZ4HC_init_internal`.
    pub(super) fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            hash_table: crate::shared::fixed::fixed_table(0),
            chain_table: crate::shared::fixed::fixed_table(0),
            next_to_update: PREFIX_IDX,
        }
    }

    /// `LZ4HC_Insert`: chains every position before `ip`.
    fn insert(&mut self, ip: usize) {
        let target = ip as u32 + PREFIX_IDX;
        let idx = self.next_to_update;
        if idx < target {
            let first = (idx - PREFIX_IDX) as usize;
            let count = (target - idx) as usize;
            // Position `p` hashes the four bytes at `p`, so the windows of
            // `count` positions span `count + 3` bytes.
            let windows = self.src.get(first..first + count + 3).unwrap_or_default();
            for (window, idx) in windows.windows(MIN_MATCH).zip(idx..) {
                let h = hash_ptr(window.first_chunk().map_or(0, |w| u32::from_le_bytes(*w)));
                let mut delta = idx.wrapping_sub(self.hash_table[h]) as usize;
                if delta > DISTANCE_MAX {
                    delta = DISTANCE_MAX;
                }
                self.chain_table[usize::from(idx as u16)] = delta as u16;
                self.hash_table[h] = idx;
            }
        }
        self.next_to_update = target;
    }

    /// `LZ4HC_InsertAndGetWiderMatch`.
    ///
    /// `PATTERN` is the reference's `patternAnalysis` and `SWAP` its
    /// `chainSwap`; both are fixed per caller, so each combination is
    /// compiled without the other's branches.
    #[inline(always)]
    pub(super) fn insert_and_get_wider_match<const PATTERN: bool, const SWAP: bool>(
        &mut self,
        ip: usize,
        i_low: usize,
        i_high: usize,
        mut longest: i32,
        max_attempts: i32,
    ) -> Match {
        let src = self.src;
        let ip_index = ip as u32 + PREFIX_IDX;
        let within_start_distance = PREFIX_IDX + (DISTANCE_MAX as u32 + 1) > ip_index;
        let lowest_match_index = if within_start_distance {
            PREFIX_IDX
        } else {
            ip_index - DISTANCE_MAX as u32
        };
        let look_back = (ip - i_low) as i32;
        let mut nb_attempts = max_attempts;
        let mut match_chain_pos = 0u32;
        let pattern = read32(src, ip);
        let mut repeat = Repeat::Untested;
        let mut src_pattern_length = 0usize;
        let mut offset = 0i32;
        let mut s_back = 0i32;

        self.insert(ip);
        let mut match_index = self.hash_table[hash_ptr(pattern)];
        let chain: &[u16; 1 << 16] = &self.chain_table;
        let delta = |position: u32| u32::from(chain[usize::from(position as u16)]);
        // The two bytes a candidate must share with the best match so far
        // sit at `i_low + longest - 1` and, for the candidate, at
        // `match_ptr + adjust`; both only change with `longest`.
        let mut head = head16(src, i_low, longest);
        let mut adjust = (longest as usize - 1).wrapping_sub(look_back as usize);

        while match_index >= lowest_match_index && nb_attempts > 0 {
            let mut match_length = 0i32;
            nb_attempts -= 1;
            // The match is always within the prefix of a fresh stream.
            let match_ptr = (match_index - PREFIX_IDX) as usize;
            // A negative position wraps to one `peek16` rejects.
            if head == peek16(src, match_ptr.wrapping_add(adjust))
                && read32(src, match_ptr) == pattern
            {
                let (length, back) = extend(src, ip, match_ptr, i_low, i_high, look_back);
                match_length = length;
                if match_length > longest {
                    longest = match_length;
                    offset = (ip_index - match_index) as i32;
                    s_back = back;
                    head = head16(src, i_low, longest);
                    adjust = (longest as usize - 1).wrapping_sub(look_back as usize);
                }
            }

            if SWAP && match_length == longest && match_index + longest as u32 <= ip_index {
                let mut distance_to_next_match = 1u32;
                let end = longest - MIN_MATCH as i32 + 1;
                let mut step;
                let mut accel = 1i32 << 4;
                let mut pos = 0i32;
                while pos < end {
                    let candidate_dist = delta(match_index.wrapping_add(pos as u32));
                    step = accel >> 4;
                    accel += 1;
                    if candidate_dist > distance_to_next_match {
                        distance_to_next_match = candidate_dist;
                        match_chain_pos = pos as u32;
                        accel = 1 << 4;
                    }
                    pos += step;
                }
                if distance_to_next_match > 1 {
                    if distance_to_next_match > match_index {
                        break;
                    }
                    match_index -= distance_to_next_match;
                    continue;
                }
            }

            if PATTERN && delta(match_index) == 1 && match_chain_pos == 0 {
                let match_candidate_idx = match_index - 1;
                if repeat == Repeat::Untested {
                    if ((pattern & 0xFFFF) == (pattern >> 16))
                        & ((pattern & 0xFF) == (pattern >> 24))
                    {
                        repeat = Repeat::Confirmed;
                        src_pattern_length = count_pattern(src, ip + 4, i_high, pattern) + 4;
                    } else {
                        repeat = Repeat::Not;
                    }
                }
                if repeat == Repeat::Confirmed
                    && match_candidate_idx >= lowest_match_index
                    && protect_dict_end(PREFIX_IDX, match_candidate_idx)
                {
                    let match_ptr = (match_candidate_idx - PREFIX_IDX) as usize;
                    if read32(src, match_ptr) == pattern {
                        let forward_pattern_length =
                            count_pattern(src, match_ptr + 4, i_high, pattern) + 4;
                        let mut back_length = reverse_count_pattern(src, match_ptr, 0, pattern);
                        back_length = (match_candidate_idx
                            - match_candidate_idx
                                .wrapping_sub(back_length as u32)
                                .max(lowest_match_index))
                            as usize;
                        let current_segment_length = back_length + forward_pattern_length;
                        if current_segment_length >= src_pattern_length
                            && forward_pattern_length <= src_pattern_length
                        {
                            let new_match_index = match_candidate_idx
                                .wrapping_add(forward_pattern_length as u32)
                                .wrapping_sub(src_pattern_length as u32);
                            match_index = if protect_dict_end(PREFIX_IDX, new_match_index) {
                                new_match_index
                            } else {
                                PREFIX_IDX
                            };
                        } else {
                            let new_match_index =
                                match_candidate_idx.wrapping_sub(back_length as u32);
                            if protect_dict_end(PREFIX_IDX, new_match_index) {
                                match_index = new_match_index;
                                if look_back == 0 {
                                    let max_ml = current_segment_length.min(src_pattern_length);
                                    if (longest as usize) < max_ml {
                                        if ip + PREFIX_IDX as usize - match_index as usize
                                            > DISTANCE_MAX
                                        {
                                            break;
                                        }
                                        longest = max_ml as i32;
                                        offset = (ip_index - match_index) as i32;
                                        head = head16(src, i_low, longest);
                                        adjust =
                                            (longest as usize - 1).wrapping_sub(look_back as usize);
                                    }
                                    let dist_to_next_pattern = delta(match_index);
                                    if dist_to_next_pattern > match_index {
                                        break;
                                    }
                                    match_index -= dist_to_next_pattern;
                                }
                            } else {
                                match_index = PREFIX_IDX;
                            }
                        }
                        continue;
                    }
                }
            }

            match_index -= delta(match_index.wrapping_add(match_chain_pos));
        }

        Match {
            len: longest,
            off: offset,
            back: s_back,
        }
    }

    /// `LZ4HC_InsertAndFindBestMatch`.
    fn find_best_match<const PATTERN: bool>(
        &mut self,
        ip: usize,
        limit: usize,
        attempts: i32,
    ) -> Match {
        self.insert_and_get_wider_match::<PATTERN, false>(
            ip,
            ip,
            limit,
            MIN_MATCH as i32 - 1,
            attempts,
        )
    }

    /// `LZ4HC_compress_hashChain`: `patternAnalysis` is on from 128 attempts
    /// up (levels 8 and 9).
    fn compress_hash_chain(&mut self, out: &mut Vec<u8>, max_attempts: i32) {
        if max_attempts > 128 {
            self.compress_hash_chain_with::<true>(out, max_attempts);
        } else {
            self.compress_hash_chain_with::<false>(out, max_attempts);
        }
    }

    /// `LZ4HC_compress_hashChain` with `patternAnalysis` fixed.
    fn compress_hash_chain_with<const PATTERN: bool>(
        &mut self,
        out: &mut Vec<u8>,
        max_attempts: i32,
    ) {
        let src = self.src;
        let n = src.len();
        let mut ip = 0isize;
        let mut anchor = 0usize;
        if n >= MIN_LENGTH {
            let mflimit = (n - MF_LIMIT) as isize;
            let matchlimit = n - LAST_LITERALS;
            let nomatch = Match::default();
            let mut start0;
            let mut start2 = 0isize;
            let mut start3;
            let mut m0;
            let mut m1;
            let mut m2;
            let mut m3;
            // Local helpers over `usize` positions.
            macro_rules! encode {
                ($len:expr, $off:expr) => {{
                    let mut p = ip as usize;
                    encode_sequence(out, src, &mut p, &mut anchor, $len, $off);
                    ip = p as isize;
                }};
            }
            // Like `encode!` for a call followed by an assignment to `ip`.
            macro_rules! encode_then_jump {
                ($len:expr, $off:expr) => {{
                    let mut p = ip as usize;
                    encode_sequence(out, src, &mut p, &mut anchor, $len, $off);
                }};
            }
            'main: while ip <= mflimit {
                m1 = self.find_best_match::<PATTERN>(ip as usize, matchlimit, max_attempts);
                if m1.len < MIN_MATCH as i32 {
                    ip += 1;
                    continue;
                }
                start0 = ip;
                m0 = m1;
                'search2: loop {
                    if ip + m1.len as isize <= mflimit {
                        start2 = ip + m1.len as isize - 2;
                        m2 = self.insert_and_get_wider_match::<PATTERN, false>(
                            start2 as usize,
                            ip as usize,
                            matchlimit,
                            m1.len,
                            max_attempts,
                        );
                        start2 += m2.back as isize;
                    } else {
                        m2 = nomatch;
                    }
                    if m2.len <= m1.len {
                        encode!(m1.len, m1.off);
                        continue 'main;
                    }
                    if start0 < ip && start2 < ip + m0.len as isize {
                        ip = start0;
                        m1 = m0;
                    }
                    if start2 - ip < 3 {
                        ip = start2;
                        m1 = m2;
                        continue 'search2;
                    }
                    'search3: loop {
                        if start2 - ip < OPTIMAL_ML {
                            let mut new_ml = m1.len as isize;
                            if new_ml > OPTIMAL_ML {
                                new_ml = OPTIMAL_ML;
                            }
                            if ip + new_ml > start2 + m2.len as isize - MIN_MATCH as isize {
                                new_ml = start2 - ip + m2.len as isize - MIN_MATCH as isize;
                            }
                            let correction = new_ml - (start2 - ip);
                            if correction > 0 {
                                start2 += correction;
                                m2.len -= correction as i32;
                            }
                        }
                        if start2 + m2.len as isize <= mflimit {
                            start3 = start2 + m2.len as isize - 3;
                            m3 = self.insert_and_get_wider_match::<PATTERN, false>(
                                start3 as usize,
                                start2 as usize,
                                matchlimit,
                                m2.len,
                                max_attempts,
                            );
                            start3 += m3.back as isize;
                        } else {
                            m3 = nomatch;
                            start3 = 0;
                        }
                        if m3.len <= m2.len {
                            if start2 < ip + m1.len as isize {
                                m1.len = (start2 - ip) as i32;
                            }
                            encode_then_jump!(m1.len, m1.off);
                            ip = start2;
                            encode!(m2.len, m2.off);
                            continue 'main;
                        }
                        if start3 < ip + m1.len as isize + 3 {
                            if start3 >= ip + m1.len as isize {
                                if start2 < ip + m1.len as isize {
                                    let correction = (ip + m1.len as isize - start2) as i32;
                                    start2 += correction as isize;
                                    m2.len -= correction;
                                    if m2.len < MIN_MATCH as i32 {
                                        start2 = start3;
                                        m2 = m3;
                                    }
                                }
                                encode_then_jump!(m1.len, m1.off);
                                ip = start3;
                                m1 = m3;
                                start0 = start2;
                                m0 = m2;
                                continue 'search2;
                            }
                            start2 = start3;
                            m2 = m3;
                            continue 'search3;
                        }
                        if start2 < ip + m1.len as isize {
                            if start2 - ip < OPTIMAL_ML {
                                if m1.len as isize > OPTIMAL_ML {
                                    m1.len = OPTIMAL_ML as i32;
                                }
                                if ip + m1.len as isize
                                    > start2 + m2.len as isize - MIN_MATCH as isize
                                {
                                    m1.len = (start2 - ip) as i32 + m2.len - MIN_MATCH as i32;
                                }
                                let correction = m1.len - (start2 - ip) as i32;
                                if correction > 0 {
                                    start2 += correction as isize;
                                    m2.len -= correction;
                                }
                            } else {
                                m1.len = (start2 - ip) as i32;
                            }
                        }
                        encode_then_jump!(m1.len, m1.off);
                        ip = start2;
                        m1 = m2;
                        start2 = start3;
                        m2 = m3;
                        continue 'search3;
                    }
                }
            }
        }
        push_last_literals(out, src, anchor);
    }

    /// `LZ4MID_compress`.
    fn compress_mid(&mut self, out: &mut Vec<u8>) {
        let src = self.src;
        let n = src.len();
        let mut anchor = 0usize;
        if n >= MIN_LENGTH {
            let mut ip = 0usize;
            let mflimit = n - MF_LIMIT;
            let matchlimit = n - LAST_LITERALS;
            let ilimit_idx = ((n - MID_HASH_SIZE) as u32).wrapping_add(PREFIX_IDX);
            let half = 1usize << MID_HASH_LOG;
            // hash4 table: [0, half), hash8 table: [half, 2 * half).
            macro_rules! add8 {
                ($at:expr, $idx:expr) => {
                    self.hash_table[half + mid_hash7(read64(src, $at))] = $idx
                };
            }
            macro_rules! add4 {
                ($at:expr, $idx:expr) => {
                    self.hash_table[mid_hash4(read32(src, $at))] = $idx
                };
            }
            while ip <= mflimit {
                let ip_index = ip as u32 + PREFIX_IDX;
                let mut match_length = 0usize;
                let mut match_distance = 0usize;
                let mut found = false;
                // Search a long match.
                {
                    let h8 = mid_hash7(read64(src, ip));
                    let pos8 = self.hash_table[half + h8];
                    self.hash_table[half + h8] = ip_index;
                    if (ip_index.wrapping_sub(pos8) as usize) <= DISTANCE_MAX && pos8 >= PREFIX_IDX
                    {
                        let match_ptr = (pos8 - PREFIX_IDX) as usize;
                        match_length = count(src, ip, match_ptr, matchlimit);
                        if match_length >= MIN_MATCH {
                            match_distance = (ip_index - pos8) as usize;
                            found = true;
                        }
                    }
                }
                // Search a short match.
                if !found {
                    let h4 = mid_hash4(read32(src, ip));
                    let pos4 = self.hash_table[h4];
                    self.hash_table[h4] = ip_index;
                    if (ip_index.wrapping_sub(pos4) as usize) <= DISTANCE_MAX && pos4 >= PREFIX_IDX
                    {
                        let match_ptr = (pos4 - PREFIX_IDX) as usize;
                        match_length = count(src, ip, match_ptr, matchlimit);
                        if match_length >= MIN_MATCH {
                            // Short match found, check ip + 1 for a longer one.
                            let h8 = mid_hash7(read64(src, ip + 1));
                            let pos8 = self.hash_table[half + h8];
                            let m2_distance = (ip_index + 1).wrapping_sub(pos8) as usize;
                            match_distance = (ip_index - pos4) as usize;
                            if m2_distance <= DISTANCE_MAX && pos8 >= PREFIX_IDX && ip < mflimit {
                                let m2_ptr = (pos8 - PREFIX_IDX) as usize;
                                let ml2 = count(src, ip + 1, m2_ptr, matchlimit);
                                if ml2 > match_length {
                                    self.hash_table[half + h8] = ip_index + 1;
                                    ip += 1;
                                    match_length = ml2;
                                    match_distance = m2_distance;
                                }
                            }
                            found = true;
                        }
                    }
                }
                if !found {
                    ip += 1 + ((ip - anchor) >> 9);
                    continue;
                }
                // Catch back.
                while ip > anchor
                    && ip > match_distance
                    && src[ip - 1] == src[ip - match_distance - 1]
                {
                    ip -= 1;
                    match_length += 1;
                }
                // Fill the table with the beginning of the match.
                add8!(ip + 1, ip_index + 1);
                add8!(ip + 2, ip_index + 2);
                add4!(ip + 1, ip_index + 1);
                encode_sequence(
                    out,
                    src,
                    &mut ip,
                    &mut anchor,
                    match_length as i32,
                    match_distance as i32,
                );
                // Fill the table with the end of the match.
                let end_match_idx = ip as u32 + PREFIX_IDX;
                let pos_m2 = end_match_idx - 2;
                if pos_m2 < ilimit_idx {
                    if ip > 5 {
                        add8!(ip - 5, end_match_idx - 5);
                    }
                    add8!(ip - 3, end_match_idx - 3);
                    add8!(ip - 2, end_match_idx - 2);
                    add4!(ip - 2, end_match_idx - 2);
                    add4!(ip - 1, end_match_idx - 1);
                }
            }
        }
        push_last_literals(out, src, anchor);
    }
}

/// `LZ4_compress_HC` with an output buffer of `LZ4_compressBound` bytes.
pub(super) fn compress(src: &[u8], level: i32) -> Vec<u8> {
    let params = level_params(level);
    let mut out = Vec::with_capacity(compress_bound(src.len()));
    let mut hc = Hc::new(src);
    match params.strategy {
        Strategy::Mid => hc.compress_mid(&mut out),
        Strategy::Chain => hc.compress_hash_chain(&mut out, params.searches),
        Strategy::Optimal => {
            optimal::compress(&mut hc, &mut out, params, level >= LEVEL_MAX);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{count_back, count_pattern, head16, peek16, reverse_count_pattern};

    /// The equal bytes before `ip` and `matched`, one at a time.
    fn naive_back(src: &[u8], ip: usize, matched: usize, i_min: usize, m_min: usize) -> i32 {
        let mut equal = 0;
        while ip - equal > i_min
            && matched - equal > m_min
            && src[ip - equal - 1] == src[matched - equal - 1]
        {
            equal += 1;
        }
        -(equal as i32)
    }

    fn sample(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut data: Vec<u8> = Vec::with_capacity(len);
        while data.len() < len {
            if data.len() > 40 && next() % 3 == 0 {
                let from = (next() as usize) % (data.len() - 20);
                let run = 1 + (next() as usize) % 20;
                data.extend_from_within(from..from + run);
            } else {
                data.push((next() % 3) as u8);
            }
        }
        data.truncate(len);
        data
    }

    #[test]
    fn count_back_matches_a_byte_loop() {
        let len = if cfg!(miri) { 90 } else { 300 };
        let src = sample(len, 0xbac4);
        let step = if cfg!(miri) { 5 } else { 1 };
        for ip in (0..=len).step_by(step) {
            for matched in (0..ip).step_by(step * 2) {
                for i_min in [
                    0,
                    ip.saturating_sub(3),
                    ip.saturating_sub(8),
                    ip.saturating_sub(9),
                    ip.saturating_sub(40),
                ] {
                    for m_min in [0, matched.saturating_sub(5)] {
                        assert_eq!(
                            count_back(&src, ip, matched, i_min, m_min),
                            naive_back(&src, ip, matched, i_min, m_min),
                            "{ip} {matched} {i_min} {m_min}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn count_back_of_a_long_run_continues_past_eight_bytes() {
        let src = [3u8; 64];
        assert_eq!(count_back(&src, 40, 20, 0, 0), -20);
        assert_eq!(count_back(&src, 40, 20, 35, 0), -5);
        assert_eq!(count_back(&src, 40, 20, 10, 0), -20);
        assert_eq!(count_back(&src, 6, 3, 0, 0), -3);
    }

    #[test]
    fn peeks_report_a_missing_read_as_a_distinct_value() {
        let src = [1u8, 2, 3, 4];
        assert_eq!(peek16(&src, 0), 0x0201);
        assert_eq!(peek16(&src, 2), 0x0403);
        assert_eq!(peek16(&src, 3), -2);
        assert_eq!(peek16(&src, usize::MAX), -2);
        // `longest - 1` into the source, and the miss value `-1`.
        assert_eq!(head16(&src, 0, 3), 0x0403);
        assert_eq!(head16(&src, 0, 4), -1);
    }

    #[test]
    fn pattern_counts_follow_the_repeated_word() {
        let pattern = u32::from_le_bytes([1, 2, 3, 4]);
        let mut src: Vec<u8> = [1u8, 2, 3, 4].iter().copied().cycle().take(37).collect();
        src.extend_from_slice(&[9, 9]);
        assert_eq!(count_pattern(&src, 0, src.len(), pattern), 37);
        assert_eq!(count_pattern(&src, 0, 20, pattern), 20);
        assert_eq!(reverse_count_pattern(&src, 36, 0, pattern), 36);
        assert_eq!(reverse_count_pattern(&src, 36, 8, pattern), 28);
    }
}
