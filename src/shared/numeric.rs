//! Numeric helpers shared by the codecs (`shared/numeric_operations.c`).
//!
//! Numeric streams are little-endian bytes, so every helper here reads and
//! writes elements of 1, 2, 4 or 8 bytes through `from_le_bytes`-style loops
//! instead of the reference's pointer casts. The `NUMOP_*` functions keep
//! their reference semantics; the element accessors ([`read_le`],
//! [`push_le`], [`elements`]) are the Rust spelling of the casts and
//! `ZL_readN` / `ZL_writeN`.
//!
//! Functions are gated by the codec features that call them, because the
//! crate treats an unused item as an error. A function marked
//! `#[cfg(test)]` is ported and tested but has no caller yet: the package
//! that first needs it deletes the attribute.

#[cfg(any(
    test,
    feature = "delta",
    feature = "zigzag",
    feature = "range-pack",
    feature = "divide-by",
    feature = "merge-sorted",
    feature = "sparse-num",
    feature = "sentinel",
    feature = "bitsplit",
    feature = "float-deconstruct"
))]
use alloc::vec::Vec;

/// `ZL_maxValueForWidth`: the largest unsigned value of a `width`-byte
/// element.
#[cfg(any(test, feature = "sentinel"))]
pub(crate) const fn max_value_for_width(width: usize) -> u64 {
    if width >= 8 {
        u64::MAX
    } else {
        (1u64 << (8 * width)) - 1
    }
}

/// Reads a little-endian unsigned number of up to eight bytes (`ZL_readN`
/// and the width casts of the reference kernels).
#[cfg(any(
    test,
    feature = "delta",
    feature = "zigzag",
    feature = "range-pack",
    feature = "divide-by",
    feature = "merge-sorted",
    feature = "partition",
    feature = "sparse-num",
    feature = "sentinel",
    feature = "bitsplit",
    feature = "float-deconstruct"
))]
pub(crate) fn read_le(bytes: &[u8]) -> u64 {
    let mut value = [0u8; 8];
    for (to, from) in value.iter_mut().zip(bytes) {
        *to = *from;
    }
    u64::from_le_bytes(value)
}

/// The elements of a numeric stream of `width` bytes as unsigned integers.
///
/// A trailing partial element is ignored, as a stream never has one.
#[cfg(any(
    test,
    feature = "delta",
    feature = "zigzag",
    feature = "range-pack",
    feature = "divide-by",
    feature = "merge-sorted",
    feature = "partition",
    feature = "sparse-num",
    feature = "sentinel",
    feature = "bitsplit",
    feature = "float-deconstruct"
))]
pub(crate) fn elements(bytes: &[u8], width: usize) -> impl Iterator<Item = u64> + Clone + '_ {
    bytes.chunks_exact(width.max(1)).map(read_le)
}

/// Appends the low `width` bytes of `value`, little-endian (`ZL_writeN`,
/// `ZL_writeLE64_N`).
#[cfg(any(
    test,
    feature = "delta",
    feature = "zigzag",
    feature = "range-pack",
    feature = "divide-by",
    feature = "merge-sorted",
    feature = "sparse-num",
    feature = "sentinel",
    feature = "bitsplit",
    feature = "float-deconstruct"
))]
pub(crate) fn push_le(out: &mut Vec<u8>, value: u64, width: usize) {
    out.extend_from_slice(&value.to_le_bytes()[..width.min(8)]);
}

/// `NUMOP_numericWidthForValue`: the smallest element width that holds
/// `max_value` (1, 2, 4 or 8).
#[cfg(any(test, feature = "range-pack"))]
pub(crate) const fn width_for_value(max_value: u64) -> usize {
    if max_value < 256 {
        1
    } else if max_value < 65_536 {
        2
    } else if max_value <= u32::MAX as u64 {
        4
    } else {
        8
    }
}

/// `NUMOP_findMaxU8`: the largest byte, 0 for an empty array.
#[cfg(any(test, feature = "partition"))]
pub(crate) fn find_max_u8(array: &[u8]) -> u8 {
    array.iter().copied().max().unwrap_or(0)
}

/// `NUMOP_findMaxST`: the largest value, 0 for an empty array.
#[cfg(test)]
pub(crate) fn find_max_st(array: &[usize]) -> usize {
    array.iter().copied().max().unwrap_or(0)
}

/// `NUMOP_findMaxArr32`: the largest value, 0 for an empty array.
#[cfg(test)]
pub(crate) fn find_max_u32(array: &[u32]) -> u32 {
    array.iter().copied().max().unwrap_or(0)
}

/// `NUMOP_sumArray32`: the sum, which cannot overflow 64 bits.
#[cfg(test)]
pub(crate) fn sum_array_u32(array: &[u32]) -> u64 {
    array.iter().map(|&value| u64::from(value)).sum()
}

/// `NUMOP_sumArrayST`: the sum, wrapping like the reference's unchecked
/// release build.
#[cfg(test)]
pub(crate) fn sum_array_st(array: &[usize]) -> usize {
    array
        .iter()
        .fold(0, |total, &value| total.wrapping_add(value))
}

/// `NUMOP_underLimit`: whether every value is below `limit`.
#[cfg(test)]
pub(crate) fn under_limit(array: &[u32], limit: u32) -> bool {
    array.iter().all(|&value| value < limit)
}

/// `NUMOP_underLimitU8`: whether every byte is below `limit`.
#[cfg(test)]
pub(crate) fn under_limit_u8(array: &[u8], limit: u32) -> bool {
    array.iter().all(|&value| u32::from(value) < limit)
}

/// `NUMOP_underLimitU16`: whether every value is below `limit`.
#[cfg(test)]
pub(crate) fn under_limit_u16(array: &[u16], limit: u32) -> bool {
    array.iter().all(|&value| u32::from(value) < limit)
}

/// `NUMOP_numericWidthForArrayST`.
#[cfg(test)]
pub(crate) fn width_for_array_st(array: &[usize]) -> usize {
    width_for_value(find_max_st(array) as u64)
}

/// `NUMOP_numericWidthForArray32`.
#[cfg(test)]
pub(crate) fn width_for_array_u32(array: &[u32]) -> usize {
    width_for_value(u64::from(find_max_u32(array)))
}

/// `NUMOP_writeNumerics_fromST`: `values` as `width`-byte elements; a value
/// wider than `width` is truncated (the reference asserts in debug builds).
#[cfg(test)]
pub(crate) fn write_numerics_from_st(out: &mut Vec<u8>, width: usize, values: &[usize]) {
    for &value in values {
        push_le(out, value as u64, width);
    }
}

/// `NUMOP_writeNumerics_fromU32`: `values` as `width`-byte elements.
#[cfg(test)]
pub(crate) fn write_numerics_from_u32(out: &mut Vec<u8>, width: usize, values: &[u32]) {
    for &value in values {
        push_le(out, u64::from(value), width);
    }
}

/// `NUMOP_writeST_fromNumerics`: the elements of a numeric stream as
/// `usize` (the reference is 64-bit only).
#[cfg(test)]
pub(crate) fn write_st_from_numerics(bytes: &[u8], width: usize) -> Vec<usize> {
    elements(bytes, width).map(|value| value as usize).collect()
}

/// `NUMOP_write32_fromNumerics`: the elements as `u32`; `None` when one does
/// not fit (`integerOverflow` in the reference).
#[cfg(test)]
pub(crate) fn write32_from_numerics(bytes: &[u8], width: usize) -> Option<Vec<u32>> {
    elements(bytes, width)
        .map(|value| u32::try_from(value).ok())
        .collect()
}

/// `NUMOP_byteswap`: `bytes` with every `width`-byte element reversed.
#[cfg(test)]
pub(crate) fn byteswap(bytes: &[u8], width: usize) -> Vec<u8> {
    let mut out = bytes.to_vec();
    out.chunks_exact_mut(width.max(1)).for_each(<[u8]>::reverse);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn widths_follow_the_largest_value() {
        assert_eq!(
            [0, 255, 256, 65_535, 65_536, u64::from(u32::MAX), 1 << 32].map(width_for_value),
            [1, 1, 2, 2, 4, 4, 8]
        );
        assert_eq!(width_for_array_u32(&[]), 1);
        assert_eq!(width_for_array_u32(&[3, 70_000]), 4);
        assert_eq!(width_for_array_st(&[300, 2]), 2);
        assert_eq!(max_value_for_width(1), 255);
        assert_eq!(max_value_for_width(4), u64::from(u32::MAX));
        assert_eq!(max_value_for_width(8), u64::MAX);
    }

    #[test]
    fn extremes_sums_and_limits_match_the_reference() {
        assert_eq!(find_max_u8(&[]), 0);
        assert_eq!(find_max_u8(&[3, 9, 4]), 9);
        assert_eq!(find_max_st(&[1, 7, 2]), 7);
        assert_eq!(find_max_u32(&[]), 0);
        assert_eq!(
            sum_array_u32(&[u32::MAX, u32::MAX]),
            2 * u64::from(u32::MAX)
        );
        assert_eq!(sum_array_st(&[usize::MAX, 2]), 1);
        assert!(under_limit(&[1, 2], 3) && !under_limit(&[1, 3], 3));
        assert!(under_limit_u8(&[], 0) && !under_limit_u8(&[200], 200));
        assert!(under_limit_u16(&[255, 256], 257) && !under_limit_u16(&[256], 256));
    }

    #[test]
    fn elements_round_trip_through_every_width() {
        for width in [1usize, 2, 4, 8] {
            let values = [0u64, 1, 0x7f, 0xff];
            let mut bytes = Vec::new();
            for &value in &values {
                push_le(&mut bytes, value, width);
            }
            assert_eq!(bytes.len(), width * values.len());
            assert_eq!(elements(&bytes, width).collect::<Vec<_>>(), values);
        }
        assert_eq!(read_le(&[0x34, 0x12]), 0x1234);
        let mut bytes = Vec::new();
        push_le(&mut bytes, u64::MAX, 3);
        assert_eq!(bytes, [0xff; 3]);
    }

    #[test]
    fn numeric_conversions_match_the_reference() {
        let mut bytes = Vec::new();
        write_numerics_from_st(&mut bytes, 2, &[1, 0x1_0002]);
        assert_eq!(bytes, [1, 0, 2, 0]);
        let mut bytes32 = Vec::new();
        write_numerics_from_u32(&mut bytes32, 8, &[5]);
        assert_eq!(bytes32, [5, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(write_st_from_numerics(&[1, 0, 2, 0], 2), vec![1, 2]);
        assert_eq!(write32_from_numerics(&[1, 0, 2, 0], 2), Some(vec![1, 2]));
        assert_eq!(write32_from_numerics(&[0, 0, 0, 0, 1, 0, 0, 0], 8), None);
        assert_eq!(write32_from_numerics(&[], 8), Some(vec![]));
    }

    #[test]
    fn byteswap_reverses_each_element() {
        assert_eq!(byteswap(&[1, 2, 3, 4], 1), [1, 2, 3, 4]);
        assert_eq!(byteswap(&[1, 2, 3, 4], 2), [2, 1, 4, 3]);
        assert_eq!(byteswap(&[1, 2, 3, 4], 4), [4, 3, 2, 1]);
        assert_eq!(
            byteswap(&[1, 2, 3, 4, 5, 6, 7, 8], 8),
            [8, 7, 6, 5, 4, 3, 2, 1]
        );
    }
}
