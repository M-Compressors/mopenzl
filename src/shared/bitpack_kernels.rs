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

#[cfg(any(feature = "bitunpack", test))]
use alloc::vec;
use alloc::vec::Vec;

/// `ZS_bitpackEncodeBound`: the bytes `nb_elts` fields of `nb_bits` take.
#[cfg(any(feature = "bitpack", test))]
pub(crate) const fn encode_bound(nb_elts: usize, nb_bits: u32) -> usize {
    (nb_elts * nb_bits as usize).div_ceil(8)
}

/// `ZS_bitpackEncode`: packs the little-endian elements of `width` bytes in
/// `src` into fields of `nb_bits` bits.
///
/// Every element must fit in `nb_bits` bits (`ZS_bitpackEncodeVerify`),
/// `nb_bits` is at most `8 * width` and `width` is 1, 2, 4 or 8; the
/// function does not check it. Zero fields (`nb_bits == 0`) or no elements
/// pack to nothing.
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
