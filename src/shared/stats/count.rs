//! Match-length counting (`codecs/common/count.h`).
//!
//! The reference compares a machine word at a time and counts the common
//! prefix with a trailing-zero count. These functions return the same
//! lengths over a single buffer addressed by positions: `in_pos` and
//! `match_pos` are offsets into `buf` and every limit is an offset too.

const WORD: usize = core::mem::size_of::<usize>();

/// `ZS_nbCommonBytes`: the number of equal low-order bytes of a non-zero
/// XOR of two little-endian words.
pub(crate) const fn nb_common_bytes(val: usize) -> usize {
    (val.trailing_zeros() >> 3) as usize
}

/// `ZS_nbCommonBytesBack`: the number of equal high-order bytes.
pub(crate) const fn nb_common_bytes_back(val: usize) -> usize {
    (val.leading_zeros() >> 3) as usize
}

fn read_word(buf: &[u8], pos: usize) -> usize {
    let mut bytes = [0u8; WORD];
    bytes.copy_from_slice(&buf[pos..pos + WORD]);
    usize::from_le_bytes(bytes)
}

/// `ZS_count`: the length of the common prefix of `buf[in_pos..in_limit]`
/// and `buf[match_pos..]`.
pub(crate) fn count(buf: &[u8], in_pos: usize, match_pos: usize, in_limit: usize) -> usize {
    let start = in_pos;
    let mut p_in = in_pos;
    let mut p_match = match_pos;
    let loop_limit = in_limit.saturating_sub(WORD - 1);
    if p_in < loop_limit {
        let diff = read_word(buf, p_match) ^ read_word(buf, p_in);
        if diff != 0 {
            return nb_common_bytes(diff);
        }
        p_in += WORD;
        p_match += WORD;
        while p_in < loop_limit {
            let diff = read_word(buf, p_match) ^ read_word(buf, p_in);
            if diff == 0 {
                p_in += WORD;
                p_match += WORD;
                continue;
            }
            p_in += nb_common_bytes(diff);
            return p_in - start;
        }
    }
    if cfg!(target_pointer_width = "64")
        && p_in + 3 < in_limit
        && buf[p_match..p_match + 4] == buf[p_in..p_in + 4]
    {
        p_in += 4;
        p_match += 4;
    }
    if p_in + 1 < in_limit && buf[p_match..p_match + 2] == buf[p_in..p_in + 2] {
        p_in += 2;
        p_match += 2;
    }
    if p_in < in_limit && buf[p_match] == buf[p_in] {
        p_in += 1;
    }
    p_in - start
}

/// `ZS_countBack`: how many bytes before `in_pos` and `match_pos` agree,
/// going backwards, without crossing `in_limit` on the input side or
/// `low_limit` on the match side.
pub(crate) fn count_back(
    buf: &[u8],
    in_pos: usize,
    match_pos: usize,
    in_limit: usize,
    low_limit: usize,
) -> usize {
    debug_assert!(match_pos < in_pos);
    let max_length = in_pos - in_limit;
    let mut k = 0usize;
    // Word-wise while both sides have a full word below them.
    while k + WORD <= max_length && match_pos - k >= low_limit + WORD {
        let diff = read_word(buf, match_pos - k - WORD) ^ read_word(buf, in_pos - k - WORD);
        if diff != 0 {
            return (k + nb_common_bytes_back(diff)).min(max_length);
        }
        k += WORD;
    }
    while k < max_length
        && match_pos - k > low_limit
        && buf[match_pos - k - 1] == buf[in_pos - k - 1]
    {
        k += 1;
    }
    k.min(max_length)
}

/// `ZS_count2segments`: [`count`] where the match runs through `m_end` and
/// continues from `i_start`.
pub(crate) fn count_2segments(
    buf: &[u8],
    ip: usize,
    match_pos: usize,
    i_end: usize,
    m_end: usize,
    i_start: usize,
) -> usize {
    let v_end = (ip + (m_end - match_pos)).min(i_end);
    let match_length = count(buf, ip, match_pos, v_end);
    if match_pos + match_length != m_end {
        return match_length;
    }
    match_length + count(buf, ip + match_length, i_start, i_end)
}

/// `ZS_countBound`: [`count`] capped at `p_bound`, reading no further than
/// `p_end`.
pub(crate) fn count_bound(
    buf: &[u8],
    p_in: usize,
    p_match: usize,
    p_bound: usize,
    p_end: usize,
) -> usize {
    debug_assert!(p_in >= p_match && p_end >= p_bound && p_bound >= p_in);
    let mut in_next = p_in;
    let mut match_next = p_match;
    let end_loop_limit = p_end.saturating_sub(WORD - 1).min(p_bound);
    if in_next < end_loop_limit {
        let diff = read_word(buf, in_next) ^ read_word(buf, match_next);
        if diff != 0 {
            return nb_common_bytes(diff).min(p_bound - p_in);
        }
        in_next += WORD;
        match_next += WORD;
        while in_next < end_loop_limit {
            let diff = read_word(buf, in_next) ^ read_word(buf, match_next);
            if diff == 0 {
                in_next += WORD;
                match_next += WORD;
                continue;
            }
            in_next += nb_common_bytes(diff);
            return (p_bound - p_in).min(in_next - p_in);
        }
    }
    if cfg!(target_pointer_width = "64")
        && in_next + 3 < p_bound
        && buf[in_next..in_next + 4] == buf[match_next..match_next + 4]
    {
        in_next += 4;
        match_next += 4;
    }
    if in_next + 1 < p_bound && buf[in_next..in_next + 2] == buf[match_next..match_next + 2] {
        in_next += 2;
        match_next += 2;
    }
    if in_next < p_bound && buf[in_next] == buf[match_next] {
        in_next += 1;
    }
    (in_next - p_in).min(p_bound - p_in)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn pattern(len: usize) -> Vec<u8> {
        let mut state = 7u32;
        (0..len)
            .map(|i| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
                // Long repeats with occasional differences.
                if i % 97 == 50 {
                    (state >> 20) as u8
                } else {
                    (i % 5) as u8
                }
            })
            .collect()
    }

    fn naive_count(buf: &[u8], p_in: usize, p_match: usize, limit: usize) -> usize {
        let mut n = 0;
        while p_in + n < limit && buf[p_in + n] == buf[p_match + n] {
            n += 1;
        }
        n
    }

    #[test]
    fn counting_forward_equals_the_naive_loop() {
        let buf = pattern(600);
        for match_pos in [0usize, 3, 5, 10, 47] {
            for in_pos in [match_pos + 5, match_pos + 10, match_pos + 100] {
                for limit in [in_pos, in_pos + 1, in_pos + 7, in_pos + 8, in_pos + 40, 600] {
                    let limit = limit.min(600);
                    assert_eq!(
                        count(&buf, in_pos, match_pos, limit),
                        naive_count(&buf, in_pos, match_pos, limit),
                        "in {in_pos} match {match_pos} limit {limit}"
                    );
                }
            }
        }
    }

    #[test]
    fn counting_backward_equals_the_naive_loop() {
        let buf = pattern(600);
        for (in_pos, match_pos) in [
            (100usize, 95usize),
            (300, 5),
            (299, 4),
            (500, 480),
            (64, 59),
        ] {
            for in_limit in [0usize, 10, 90, in_pos - 3] {
                for low in [0usize, 1, 3, 20] {
                    if low > match_pos || in_limit > in_pos {
                        continue;
                    }
                    let max = in_pos - in_limit;
                    let mut naive = 0;
                    while naive < max
                        && match_pos - naive > low
                        && buf[match_pos - naive - 1] == buf[in_pos - naive - 1]
                    {
                        naive += 1;
                    }
                    assert_eq!(
                        count_back(&buf, in_pos, match_pos, in_limit, low),
                        naive,
                        "in {in_pos} match {match_pos} limit {in_limit} low {low}"
                    );
                }
            }
        }
    }

    #[test]
    fn bounded_counting_stops_at_the_bound() {
        let buf = pattern(600);
        for (p_in, p_match) in [(50usize, 45usize), (100, 0), (200, 195), (590, 580)] {
            for bound in [p_in, p_in + 1, p_in + 9, p_in + 30, p_in + 400] {
                let bound = bound.min(600);
                for end in [bound, 600] {
                    let expected = naive_count(&buf, p_in, p_match, bound);
                    assert_eq!(
                        count_bound(&buf, p_in, p_match, bound, end),
                        expected,
                        "in {p_in} match {p_match} bound {bound} end {end}"
                    );
                }
            }
        }
    }

    #[test]
    fn two_segment_counting_continues_from_the_start() {
        // match runs [10, 20) then continues from index 0.
        let mut buf = alloc::vec![0u8; 100];
        for (i, byte) in buf.iter_mut().enumerate() {
            *byte = (i % 7) as u8;
        }
        // Make the continuation agree: bytes at ip.. equal match then start.
        let ip = 50usize;
        for n in 0..10 {
            buf[ip + n] = buf[10 + n];
        }
        for n in 0..8 {
            buf[ip + 10 + n] = buf[n];
        }
        let total = count_2segments(&buf, ip, 10, 100, 20, 0);
        assert!(total >= 18, "{total}");
        // A mismatch before the segment ends returns early.
        buf[ip + 3] ^= 0xff;
        assert_eq!(count_2segments(&buf, ip, 10, 100, 20, 0), 3);
    }

    #[test]
    fn common_byte_counts_use_the_word_order() {
        assert_eq!(nb_common_bytes(0x0100), 1);
        assert_eq!(nb_common_bytes(1), 0);
        assert_eq!(nb_common_bytes_back(1 << (usize::BITS - 1)), 0);
        assert_eq!(nb_common_bytes_back(1), WORD - 1);
    }
}
