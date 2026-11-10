//! Encode-side bit-packing kernels and the forward bit-stream writer.
//!
//! Ports `codecs/bitpack/common_bitpack_kernel.c` (`ZS_bitpackEncode*`,
//! `ZS_bitpackDecode*` as used by `bitunpack`) and the writer half of
//! `codecs/common/bitstream/ff_bitstream.h` (`ZS_BitCStreamFF`).
//!
//! The reference specialises the kernels per element width and bit count
//! (BMI2 `pext`/`pdep`, AVX2 packs) but every variant writes the same
//! bytes: fields of `nbBits` bits, least significant bit first, back to
//! back, the last byte padded with zero bits. The functions here are the
//! scalar baseline of that layout.

#[cfg(any(feature = "bitpack", feature = "bitunpack", test))]
use alloc::vec;
use alloc::vec::Vec;

#[cfg(any(feature = "bitpack", test))]
use fearless_simd::{Level, Simd, SimdBase, dispatch, u8x64};

/// `ZS_bitpackEncodeBound`: the bytes `nb_elts` fields of `nb_bits` take.
#[cfg(any(feature = "bitpack", test))]
pub(crate) const fn encode_bound(nb_elts: usize, nb_bits: u32) -> usize {
    (nb_elts * nb_bits as usize).div_ceil(8)
}

/// `computeNbBits`: the bits of the largest element of `bytes`, which holds
/// little-endian elements of `width` bytes (1, 2, 4 or 8); one when the
/// maximum is zero.
///
/// The highest set bit of the maximum is the highest set bit of the OR of
/// all elements, so the kernel ORs bytes at full vector width and only
/// folds the lanes onto the byte positions of an element at the end.
#[cfg(any(feature = "bitpack", test))]
pub(crate) fn nb_bits(level: Level, bytes: &[u8], width: usize) -> u32 {
    debug_assert!(matches!(width, 1 | 2 | 4 | 8));
    let lanes = dispatch!(level, simd => or_lanes(simd, bytes));
    nb_bits_of_lanes(&lanes, width)
}

/// Folds the OR lanes (lane `i` covers byte `i` of every 64 bytes, so byte
/// `i % width` of an element) into the bits of the largest element.
#[cfg(any(feature = "bitpack", test))]
fn nb_bits_of_lanes(lanes: &[u8; LANES], width: usize) -> u32 {
    let mut folded = [0u8; 8];
    for (position, &lane) in lanes.iter().enumerate() {
        folded[position % width] |= lane;
    }
    1 + u64::from_le_bytes(folded).checked_ilog2().unwrap_or(0)
}

/// Bytes ORed by one vector step.
#[cfg(any(feature = "bitpack", test))]
const LANES: usize = 64;

/// The OR of every 64-byte lane of `bytes`.
#[cfg(any(feature = "bitpack", test))]
#[inline(always)]
fn or_lanes<S: Simd>(simd: S, bytes: &[u8]) -> [u8; LANES] {
    let (blocks, tail) = bytes.as_chunks::<LANES>();
    let mut acc = [u8x64::splat(simd, 0), u8x64::splat(simd, 0)];
    let (pairs, odd) = blocks.as_chunks::<2>();
    for pair in pairs {
        acc[0] |= u8x64::from_slice(simd, &pair[0]);
        acc[1] |= u8x64::from_slice(simd, &pair[1]);
    }
    if let Some(block) = odd.first() {
        acc[0] |= u8x64::from_slice(simd, block);
    }
    let mut lanes = [0u8; LANES];
    (acc[0] | acc[1]).store_slice(&mut lanes);
    for (lane, &byte) in lanes.iter_mut().zip(tail) {
        *lane |= byte;
    }
    lanes
}

/// The scalar baseline of [`nb_bits`].
#[cfg(test)]
pub(crate) fn nb_bits_scalar(bytes: &[u8], width: usize) -> u32 {
    let mut max = 0u64;
    for element in bytes.chunks_exact(width) {
        let mut buffer = [0u8; 8];
        buffer[..width].copy_from_slice(element);
        max = max.max(u64::from_le_bytes(buffer));
    }
    1 + max.checked_ilog2().unwrap_or(0)
}

/// `ZS_bitpackEncode`: packs the little-endian elements of `width` bytes in
/// `src` into fields of `nb_bits` bits.
///
/// Every element must fit in `nb_bits` bits (`ZS_bitpackEncodeVerify`),
/// `nb_bits` is at most `8 * width` and `width` is 1, 2, 4 or 8; the
/// function does not check it. Zero fields (`nb_bits == 0`) or no elements
/// pack to nothing.
///
/// Fields of up to 16 bits are packed eight at a time: eight fields are
/// exactly `nb_bits` bytes, composed in a `u128` with a handful of shifts
/// and stored whole. The result is the same bytes as the one-field-at-a-time
/// baseline [`encode_scalar`].
#[cfg(any(feature = "bitpack", test))]
pub(crate) fn encode(src: &[u8], width: usize, nb_bits: u32) -> Vec<u8> {
    let nb_elts = src.len() / width;
    if nb_elts == 0 || nb_bits == 0 {
        return Vec::new();
    }
    if nb_bits as usize == width * 8 {
        // `ZS_bitpackEncodeEdgeCase`: a plain copy on little-endian hosts.
        return src[..nb_elts * width].to_vec();
    }
    let src = &src[..nb_elts * width];
    match (width, nb_bits) {
        (1, 1..=16) => encode_groups::<1>(src, nb_bits),
        (2, 1..=16) => encode_groups::<2>(src, nb_bits),
        (4, 1..=16) => encode_groups::<4>(src, nb_bits),
        (8, 1..=16) => encode_groups::<8>(src, nb_bits),
        _ => encode_scalar(src, width, nb_bits),
    }
}

/// The element `index` of a group of `W`-byte elements.
#[cfg(any(feature = "bitpack", test))]
#[inline(always)]
fn element<const W: usize>(group: &[u8], index: usize) -> u64 {
    let mut buffer = [0u8; 8];
    buffer[..W].copy_from_slice(&group[index * W..index * W + W]);
    u64::from_le_bytes(buffer)
}

/// Eight fields of at most 16 bits from `group` (eight `W`-byte elements)
/// as one `u128` whose low `8 * nb_bits` bits are the packed fields.
#[cfg(any(feature = "bitpack", test))]
#[inline(always)]
fn compose<const W: usize>(group: &[u8], nb_bits: u32) -> u128 {
    let half = |first: usize| {
        element::<W>(group, first)
            | element::<W>(group, first + 1) << nb_bits
            | element::<W>(group, first + 2) << (2 * nb_bits)
            | element::<W>(group, first + 3) << (3 * nb_bits)
    };
    u128::from(half(0)) | u128::from(half(4)) << (4 * nb_bits)
}

/// [`encode`] for fields of 1 to 15 bits narrower than the `W`-byte element.
#[cfg(any(feature = "bitpack", test))]
fn encode_groups<const W: usize>(src: &[u8], nb_bits: u32) -> Vec<u8> {
    let nb_elts = src.len() / W;
    let size = encode_bound(nb_elts, nb_bits);
    let step = nb_bits as usize;
    // Every group stores 16 bytes but advances `nb_bits`: the last store
    // ends at most 16 - 1 bytes past the packed size.
    let mut out = vec![0u8; size + 16];
    let mut at = 0usize;
    let full = nb_elts / 8 * 8 * W;
    for group in src[..full].chunks_exact(8 * W) {
        let value = compose::<W>(group, nb_bits);
        out[at..at + 16].copy_from_slice(&value.to_le_bytes());
        at += step;
    }
    if full < src.len() {
        // The missing elements are zero fields, which add no bits.
        let mut last = [0u8; 64];
        let rest = &src[full..];
        last[..rest.len()].copy_from_slice(rest);
        let value = compose::<W>(&last[..8 * W], nb_bits);
        out[at..at + 16].copy_from_slice(&value.to_le_bytes());
    }
    out.truncate(size);
    out
}

/// The one-field-at-a-time baseline of [`encode`].
#[cfg(any(feature = "bitpack", test))]
pub(crate) fn encode_scalar(src: &[u8], width: usize, nb_bits: u32) -> Vec<u8> {
    let nb_elts = src.len() / width;
    if nb_elts == 0 || nb_bits == 0 {
        return Vec::new();
    }
    if nb_bits as usize == width * 8 {
        return src[..nb_elts * width].to_vec();
    }
    let mut out = Vec::with_capacity(encode_bound(nb_elts, nb_bits));
    let mut state = 0u64;
    let mut bits = 0u32;
    // At most 7 bits are pending, so a field of up to 56 bits fits the
    // accumulator; wider fields are written in two halves.
    let mut put = |value: u64, count: u32| {
        state |= value << bits;
        bits += count;
        while bits >= 8 {
            out.push(state as u8);
            state >>= 8;
            bits -= 8;
        }
    };
    for element in src.chunks_exact(width) {
        let mut buffer = [0u8; 8];
        buffer[..width].copy_from_slice(element);
        let value = u64::from_le_bytes(buffer);
        if nb_bits > 56 {
            put(value & 0xFFFF_FFFF, 32);
            put(value >> 32, nb_bits - 32);
        } else {
            put(value, nb_bits);
        }
    }
    if bits > 0 {
        out.push(state as u8);
    }
    out
}

/// `ZS_bitpackDecode`: unpacks `nb_elts` fields of `nb_bits` bits from `src`
/// (which must hold `ceil(nb_elts * nb_bits / 8)` bytes) into little-endian
/// elements of `width` bytes, plus the number of bytes consumed.
///
/// `nb_bits` is between 1 and `8 * width`.
#[cfg(any(feature = "bitunpack", test))]
pub(crate) fn decode(src: &[u8], nb_elts: usize, width: usize, nb_bits: u32) -> (Vec<u8>, usize) {
    let used = (nb_elts * nb_bits as usize).div_ceil(8);
    let mut out = vec![0u8; nb_elts * width];
    if nb_elts == 0 {
        return (out, 0);
    }
    let mask = if nb_bits == 64 {
        u64::MAX
    } else {
        (1u64 << nb_bits) - 1
    };
    let mut position = 0usize;
    for element in out.chunks_exact_mut(width) {
        // The field starts `position` bits in and spans at most nine bytes.
        let byte = position / 8;
        let shift = (position % 8) as u32;
        let mut window = [0u8; 16];
        let end = (byte + 9).min(used).min(src.len());
        window[..end - byte].copy_from_slice(&src[byte..end]);
        let value = (u128::from_le_bytes(window) >> shift) as u64 & mask;
        element.copy_from_slice(&value.to_le_bytes()[..width]);
        position += nb_bits as usize;
    }
    (out, used)
}

pub(crate) mod writer;

#[cfg(test)]
mod tests;
