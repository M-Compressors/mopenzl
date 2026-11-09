//! Helpers shared by the structure codecs (tokenize, split, dispatch, ...).
//!
//! The reference reads local parameters with `ZL_Encoder_getLocalParam`,
//! which looks at the reference parameters first and falls back to the copy
//! parameters, handing back the raw bytes either way. [`local_param`]
//! reproduces that two-step lookup.

#[cfg(any(
    feature = "split",
    feature = "split-by-struct",
    feature = "tokenize",
    feature = "dispatch"
))]
use alloc::vec::Vec;
#[cfg(any(feature = "split", feature = "dispatch", feature = "parse-int"))]
use core::any::Any;

#[cfg(any(feature = "split", feature = "dispatch", feature = "parse-int"))]
use crate::compressor::core::exec::NodeCx;

/// What `ZL_Encoder_getLocalParam` found.
#[cfg(any(feature = "split", feature = "dispatch", feature = "parse-int"))]
pub(crate) enum Param<'p> {
    /// A reference parameter: an object to downcast.
    Object(&'p (dyn Any + Send + Sync)),
    /// A copy parameter: raw bytes.
    Bytes(&'p [u8]),
}

/// `ZL_Encoder_getLocalParam`: the reference parameter `id`, else the copy
/// parameter `id`.
#[cfg(any(feature = "split", feature = "dispatch", feature = "parse-int"))]
pub(crate) fn local_param<'p>(cx: &'p NodeCx<'_, '_>, id: i32) -> Option<Param<'p>> {
    if let Some(object) = cx.ref_param(id) {
        return Some(Param::Object(object));
    }
    cx.copy_param(id).map(Param::Bytes)
}

/// Reads up to eight little-endian bytes as an unsigned integer.
#[cfg(any(feature = "split", feature = "tokenize"))]
pub(crate) fn read_le(bytes: &[u8]) -> u64 {
    let mut word = [0u8; 8];
    word[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(word)
}

/// Appends the low `width` bytes of `value`, little endian.
#[cfg(any(feature = "tokenize", feature = "dispatch"))]
pub(crate) fn push_le(out: &mut Vec<u8>, value: u64, width: usize) {
    out.extend_from_slice(&value.to_le_bytes()[..width]);
}

/// `NUMOP_numericWidthForValue`: the smallest numeric width that holds
/// `max_value`.
#[cfg(feature = "dispatch")]
pub(crate) const fn width_for(max_value: u64) -> usize {
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

/// The `size_t` array a copy parameter holds: native-endian words, with a
/// trailing partial word dropped, as in the reference
/// (`paramSize / sizeof(size_t)` elements).
#[cfg(any(feature = "split", feature = "split-by-struct"))]
pub(crate) fn usize_words(bytes: &[u8]) -> Vec<usize> {
    bytes
        .as_chunks::<{ core::mem::size_of::<usize>() }>()
        .0
        .iter()
        .map(|chunk| usize::from_ne_bytes(*chunk))
        .collect()
}

/// A `size_t` array parameter: a `Vec<usize>` object, or the native-endian
/// bytes of a copy parameter ([`usize_words`]).
///
/// Returns `None` when the parameter holds neither.
#[cfg(feature = "split")]
pub(crate) fn usize_array(param: &Param<'_>) -> Option<Vec<usize>> {
    match param {
        Param::Object(object) => object.downcast_ref::<Vec<usize>>().cloned(),
        Param::Bytes(bytes) => Some(usize_words(bytes)),
    }
}

#[cfg(all(
    test,
    feature = "split",
    feature = "split-by-struct",
    feature = "tokenize",
    feature = "dispatch",
    feature = "parse-int"
))]
mod tests {
    use super::*;

    #[test]
    fn little_endian_helpers_round_trip() {
        let mut out = Vec::new();
        push_le(&mut out, 0x0102_0304, 3);
        assert_eq!(out, [4, 3, 2]);
        assert_eq!(read_le(&out), 0x0002_0304);
        assert_eq!(read_le(&[]), 0);
    }

    #[test]
    fn widths_follow_the_value_ranges() {
        assert_eq!(
            [0, 255, 256, 65_535, 65_536, u64::from(u32::MAX), 1 << 32].map(width_for),
            [1, 1, 2, 2, 4, 4, 8]
        );
    }

    #[test]
    fn size_arrays_come_from_objects_or_bytes() {
        let object: Vec<usize> = alloc::vec![3, 5];
        let found = usize_array(&Param::Object(&object)).unwrap();
        assert_eq!(found, [3, 5]);
        let wrong = 7u8;
        assert!(usize_array(&Param::Object(&wrong)).is_none());
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&9usize.to_ne_bytes());
        bytes.extend_from_slice(&[1, 2, 3]);
        assert_eq!(usize_array(&Param::Bytes(&bytes)).unwrap(), [9]);
    }
}
