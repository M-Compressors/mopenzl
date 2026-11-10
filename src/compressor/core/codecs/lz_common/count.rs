//! Match length counting (`ZS_count`, `ZS_countBack` of `common/count.h`).
//!
//! The reference compares a machine word at a time and falls back to 4, 2
//! and 1 byte steps at the end; the result is always the exact length of
//! the common run, which is what these functions compute.

use super::hash::read_u64;

/// `ZS_count`: the number of equal bytes starting at `ptr` and `matched`,
/// stopping at `limit` (a bound on `ptr`).
///
/// Both runs are sliced once, so the word loop reads without bounds checks.
/// A run that does not fit `data` (never the case for table candidates,
/// which lie before `ptr`) takes [`count_padded`].
#[inline(always)]
pub(crate) fn count(data: &[u8], ptr: usize, matched: usize, limit: usize) -> usize {
    let n = limit.saturating_sub(ptr);
    let (Some(a), Some(b)) = (
        data.get(ptr..ptr.saturating_add(n)),
        matched
            .checked_add(n)
            .and_then(|end| data.get(matched..end)),
    ) else {
        return count_padded(data, ptr, matched, limit);
    };
    let (a_words, a_rest) = a.as_chunks::<8>();
    let (b_words, b_rest) = b.as_chunks::<8>();
    let mut length = 0;
    for (x, y) in a_words.iter().zip(b_words) {
        let diff = u64::from_le_bytes(*x) ^ u64::from_le_bytes(*y);
        if diff != 0 {
            return length + (diff.trailing_zeros() >> 3) as usize;
        }
        length += 8;
    }
    length
        + a_rest
            .iter()
            .zip(b_rest)
            .take_while(|(x, y)| x == y)
            .count()
}

/// [`count`] behind a call, for parsers where inlining it makes the compiler
/// hoist its slice arithmetic in front of the usual case of no candidate.
#[inline(never)]
fn count_outlined(data: &[u8], ptr: usize, matched: usize, limit: usize) -> usize {
    count(data, ptr, matched, limit)
}

/// [`count`] reading zero padding past the end of `data`, for runs that do
/// not fit it.
#[cold]
fn count_padded(data: &[u8], ptr: usize, matched: usize, limit: usize) -> usize {
    let mut length = 0;
    while ptr + length + 8 <= limit {
        let diff = read_u64(data, matched + length) ^ read_u64(data, ptr + length);
        if diff != 0 {
            return length + (diff.trailing_zeros() >> 3) as usize;
        }
        length += 8;
    }
    while ptr + length < limit
        && data.get(matched + length).copied().unwrap_or(0)
            == data.get(ptr + length).copied().unwrap_or(0)
    {
        length += 1;
    }
    length
}

/// [`count`] when the first word of both runs is known: `first_diff` is the
/// XOR of the 8 bytes at `ptr` and at `matched`, and `ptr + 8 <= limit` or
/// the word is ignored (it then reads the zero padding).
///
/// `OUTLINE` runs the rest of the count behind a call.
#[inline(always)]
pub(crate) fn count_after<const OUTLINE: bool>(
    data: &[u8],
    ptr: usize,
    matched: usize,
    limit: usize,
    first_diff: u64,
) -> usize {
    let rest = |ptr, matched| {
        if OUTLINE {
            count_outlined(data, ptr, matched, limit)
        } else {
            count(data, ptr, matched, limit)
        }
    };
    if ptr + 8 > limit {
        return rest(ptr, matched);
    }
    if first_diff != 0 {
        return (first_diff.trailing_zeros() >> 3) as usize;
    }
    8 + rest(ptr + 8, matched + 8)
}

/// `ZS_countBack`: the number of equal bytes before `ptr` and `matched`,
/// at most `ptr - anchor` and at most `matched - low`.
///
/// Compares the word before each position, so the usual short run of a few
/// literals costs one step; words that start before `anchor` or `low` are
/// read too (they lie inside `data`) and the result is clamped afterwards.
///
/// `OUTLINE` runs it behind a call, like [`count_after`].
#[inline(always)]
pub(crate) fn count_back<const OUTLINE: bool>(
    data: &[u8],
    ptr: usize,
    matched: usize,
    anchor: usize,
    low: usize,
) -> usize {
    if OUTLINE {
        // The usual answer, a difference within the word before both
        // positions, stays inline; longer runs and the window edges take
        // the call.
        if let (Some(a), Some(b)) = (word_before(data, ptr), word_before(data, matched)) {
            let diff = a ^ b;
            if diff != 0 {
                let max = ptr.saturating_sub(anchor).min(matched.saturating_sub(low));
                return ((diff.leading_zeros() >> 3) as usize).min(max);
            }
        }
        count_back_outlined(data, ptr, matched, anchor, low)
    } else {
        count_back_inline(data, ptr, matched, anchor, low)
    }
}

/// [`count_back`] behind a call.
#[inline(never)]
fn count_back_outlined(
    data: &[u8],
    ptr: usize,
    matched: usize,
    anchor: usize,
    low: usize,
) -> usize {
    count_back_inline(data, ptr, matched, anchor, low)
}

/// [`count_back`] inlined.
#[inline(always)]
fn count_back_inline(data: &[u8], ptr: usize, matched: usize, anchor: usize, low: usize) -> usize {
    let max = ptr.saturating_sub(anchor).min(matched.saturating_sub(low));
    let mut length = 0;
    while length < max {
        let (Some(a), Some(b)) = (
            word_before(data, ptr - length),
            word_before(data, matched - length),
        ) else {
            break;
        };
        let diff = a ^ b;
        if diff != 0 {
            return (length + (diff.leading_zeros() >> 3) as usize).min(max);
        }
        length += 8;
    }
    if length >= max {
        return max;
    }
    // Fewer than eight bytes remain before the start of `data`.
    while length < max && data[matched - length - 1] == data[ptr - length - 1] {
        length += 1;
    }
    length
}

/// The little-endian word of the eight bytes before `end`, if there are
/// that many.
#[inline(always)]
fn word_before(data: &[u8], end: usize) -> Option<u64> {
    let start = end.checked_sub(8)?;
    data.get(start..end)
        .and_then(<[u8]>::first_chunk::<8>)
        .map(|word| u64::from_le_bytes(*word))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// `data` with zero padding past its end, as the padded loads see it.
    fn byte(data: &[u8], at: usize) -> u8 {
        data.get(at).copied().unwrap_or(0)
    }

    fn naive_count(data: &[u8], ptr: usize, matched: usize, limit: usize) -> usize {
        let mut length = 0;
        while ptr + length < limit && byte(data, matched + length) == byte(data, ptr + length) {
            length += 1;
        }
        length
    }

    fn naive_count_back(
        data: &[u8],
        ptr: usize,
        matched: usize,
        anchor: usize,
        low: usize,
    ) -> usize {
        let max = ptr.saturating_sub(anchor).min(matched.saturating_sub(low));
        let mut length = 0;
        while length < max && data[matched - length - 1] == data[ptr - length - 1] {
            length += 1;
        }
        length
    }

    /// A buffer made of short repeating runs, so that matches of every
    /// length occur.
    fn runs() -> Vec<u8> {
        (0..96u32).map(|i| ((i / 5) % 3) as u8).collect()
    }

    #[test]
    fn counts_forward_runs_of_every_length() {
        let mut data = vec![7u8; 80];
        for run in [0usize, 1, 3, 7, 8, 9, 15, 16, 29] {
            data.fill(7);
            // `ptr` = 40, `matched` = 10: the two windows differ after `run`.
            data[10 + run] = 9;
            assert_eq!(count(&data, 40, 10, 80), run);
        }
        data.fill(7);
        assert_eq!(count(&data, 40, 10, 80), 40, "bounded by the limit");
        assert_eq!(count(&data, 40, 10, 40), 0);
    }

    #[test]
    fn forward_counts_equal_the_byte_loop_for_every_position_pair() {
        let data = runs();
        for ptr in (0..data.len()).step_by(7) {
            for matched in (0..data.len()).step_by(3) {
                // Limits past the end read zero padding.
                for limit in [ptr, ptr + 5, data.len(), data.len() + 11] {
                    let expected = naive_count(&data, ptr, matched, limit);
                    assert_eq!(count(&data, ptr, matched, limit), expected);
                    assert_eq!(count_outlined(&data, ptr, matched, limit), expected);
                }
            }
        }
    }

    #[test]
    fn counting_after_the_first_word_equals_counting_from_the_start() {
        let mut data = vec![7u8; 80];
        for run in [0usize, 1, 7, 8, 9, 15, 16, 17, 29] {
            for limit in [40usize, 44, 47, 80] {
                data.fill(7);
                data[10 + run] = 9;
                let diff = read_u64(&data, 40) ^ read_u64(&data, 10);
                let expected = count(&data, 40, 10, limit);
                assert_eq!(
                    count_after::<false>(&data, 40, 10, limit, diff),
                    expected,
                    "run {run}, limit {limit}"
                );
                assert_eq!(
                    count_after::<true>(&data, 40, 10, limit, diff),
                    expected,
                    "outlined, run {run}, limit {limit}"
                );
            }
        }
    }

    #[test]
    fn counts_backward_runs_with_both_bounds() {
        let mut data = vec![7u8; 80];
        for run in [0usize, 1, 5, 8, 9, 17] {
            data.fill(7);
            data[10 - 1 - run.min(9)] = 9;
            let expected = run.min(9);
            assert_eq!(count_back::<false>(&data, 50, 10, 0, 0), expected);
            assert_eq!(count_back::<true>(&data, 50, 10, 0, 0), expected);
        }
        data.fill(7);
        assert_eq!(count_back::<false>(&data, 50, 30, 45, 0), 5, "anchor");
        assert_eq!(count_back::<false>(&data, 50, 30, 0, 27), 3, "window");
        assert_eq!(count_back::<true>(&data, 50, 30, 45, 0), 5, "anchor");
        assert_eq!(count_back::<true>(&data, 50, 30, 0, 27), 3, "window");
        assert_eq!(count_back::<false>(&data, 50, 30, 60, 0), 0, "anchor past");
        data[9] = 1;
        assert_eq!(count_back::<false>(&data, 50, 30, 0, 0), 20, "difference");
    }

    #[test]
    fn backward_counts_equal_the_byte_loop_for_every_position_pair() {
        let data = runs();
        for ptr in (0..=data.len()).step_by(5) {
            for matched in (0..=ptr).step_by(3) {
                for anchor in [0, ptr / 2, ptr.saturating_sub(3), ptr] {
                    for low in [0, matched / 2, matched] {
                        let expected = naive_count_back(&data, ptr, matched, anchor, low);
                        assert_eq!(
                            count_back::<false>(&data, ptr, matched, anchor, low),
                            expected
                        );
                        assert_eq!(
                            count_back::<true>(&data, ptr, matched, anchor, low),
                            expected
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_pointer_past_the_data_counts_through_the_padded_path() {
        let data = runs();
        let end = data.len();
        // `ptr` past the end reads zeros against `matched`.
        assert_eq!(count(&data, end + 4, end + 2, end + 20), 16);
        assert_eq!(count_padded(&data, end + 4, end + 2, end + 20), 16);
    }
}
