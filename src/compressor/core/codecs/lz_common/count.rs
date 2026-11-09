//! Match length counting (`ZS_count`, `ZS_countBack` of `common/count.h`).
//!
//! The reference compares a machine word at a time and falls back to 4, 2
//! and 1 byte steps at the end; the result is always the exact length of
//! the common run, which is what these functions compute.

use super::hash::read_u64;

/// `ZS_count`: the number of equal bytes starting at `ptr` and `matched`,
/// stopping at `limit` (a bound on `ptr`).
pub(crate) fn count(data: &[u8], ptr: usize, matched: usize, limit: usize) -> usize {
    let mut length = 0;
    while ptr + length + 8 <= limit {
        let diff = read_u64(data, matched + length) ^ read_u64(data, ptr + length);
        if diff != 0 {
            return length + (diff.trailing_zeros() >> 3) as usize;
        }
        length += 8;
    }
    while ptr + length < limit && data[matched + length] == data[ptr + length] {
        length += 1;
    }
    length
}

/// `ZS_countBack`: the number of equal bytes before `ptr` and `matched`,
/// at most `ptr - anchor` and at most `matched - low`.
pub(crate) fn count_back(
    data: &[u8],
    ptr: usize,
    matched: usize,
    anchor: usize,
    low: usize,
) -> usize {
    let max = ptr.saturating_sub(anchor).min(matched.saturating_sub(low));
    let mut length = 0;
    while length + 8 <= max {
        let diff = read_u64(data, matched - length - 8) ^ read_u64(data, ptr - length - 8);
        if diff != 0 {
            return length + (diff.leading_zeros() >> 3) as usize;
        }
        length += 8;
    }
    while length < max && data[matched - length - 1] == data[ptr - length - 1] {
        length += 1;
    }
    length
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

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
    fn counts_backward_runs_with_both_bounds() {
        let mut data = vec![7u8; 80];
        for run in [0usize, 1, 5, 8, 9, 17] {
            data.fill(7);
            data[10 - 1 - run.min(9)] = 9;
            let expected = run.min(9);
            assert_eq!(count_back(&data, 50, 10, 0, 0), expected);
        }
        data.fill(7);
        assert_eq!(count_back(&data, 50, 30, 45, 0), 5, "bounded by the anchor");
        assert_eq!(count_back(&data, 50, 30, 0, 27), 3, "bounded by the window");
        assert_eq!(
            count_back(&data, 50, 30, 60, 0),
            0,
            "anchor after the pointer"
        );
        data[9] = 1;
        assert_eq!(count_back(&data, 50, 30, 0, 0), 20, "stops at a difference");
    }
}
