//! Little-endian bit packing (`ZS_bitpackEncode32` / `ZS_bitpackDecode32`).
//!
//! Values are laid out from the least significant bit of the first byte
//! onwards, `bits` per value, and the stream is padded with zero bits to a
//! whole byte. The chunk header packs its per-codec flags and its stream
//! distances this way.

#[cfg(feature = "compression")]
use alloc::vec::Vec;

/// Bytes needed for `count` values of `bits` bits.
#[cfg(feature = "decompression")]
pub(crate) const fn packed_len(count: usize, bits: u32) -> usize {
    (count * bits as usize).div_ceil(8)
}

/// Appends `values`, `bits` each, to `out`.
///
/// Each value must fit in `bits` bits; `bits` is at most 32.
#[cfg(feature = "compression")]
pub(crate) fn pack(out: &mut Vec<u8>, values: impl IntoIterator<Item = u32>, bits: u32) {
    if bits == 0 {
        return;
    }
    let mut buffer = 0u64;
    let mut filled = 0u32;
    for value in values {
        debug_assert!(bits == 32 || value >> bits == 0);
        buffer |= u64::from(value) << filled;
        filled += bits;
        while filled >= 8 {
            out.push(buffer as u8);
            buffer >>= 8;
            filled -= 8;
        }
    }
    if filled > 0 {
        out.push(buffer as u8);
    }
}

/// Reads `count` values of `bits` bits from the front of `input`, calling
/// `sink` with each in order, and returns the bytes consumed; `None` when
/// `input` is shorter than [`packed_len`].
#[cfg(feature = "decompression")]
pub(crate) fn unpack(
    input: &[u8],
    count: usize,
    bits: u32,
    mut sink: impl FnMut(u32),
) -> Option<usize> {
    debug_assert!(bits <= 32);
    let len = packed_len(count, bits);
    let bytes = input.get(..len)?;
    if bits == 0 {
        (0..count).for_each(|_| sink(0));
        return Some(0);
    }
    let mask = if bits == 32 {
        u64::from(u32::MAX)
    } else {
        (1u64 << bits) - 1
    };
    let mut buffer = 0u64;
    let mut filled = 0u32;
    let mut next = bytes.iter();
    for _ in 0..count {
        while filled < bits {
            let byte = next.next().copied().unwrap_or(0);
            buffer |= u64::from(byte) << filled;
            filled += 8;
        }
        sink((buffer & mask) as u32);
        buffer >>= bits;
        filled -= bits;
    }
    Some(len)
}

/// `ZL_nextPow2`: the bits needed to represent every value below `bound`.
pub(crate) const fn bits_for(bound: u64) -> u32 {
    if bound <= 1 {
        0
    } else {
        64 - (bound - 1).leading_zeros()
    }
}

#[cfg(all(test, feature = "compression", feature = "decompression"))]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn packing_round_trips_every_width() {
        for bits in 0..=32u32 {
            let values: Vec<u32> = (0..37u32)
                .map(|i| {
                    if bits == 0 {
                        0
                    } else {
                        i.wrapping_mul(0x9E37_79B1) >> (32 - bits)
                    }
                })
                .collect();
            let mut out = Vec::new();
            pack(&mut out, values.iter().copied(), bits);
            assert_eq!(out.len(), packed_len(values.len(), bits));
            let mut back = Vec::new();
            assert_eq!(
                unpack(&out, values.len(), bits, |v| back.push(v)),
                Some(out.len())
            );
            assert_eq!(back, values, "{bits} bits");
        }
    }

    #[test]
    fn bits_are_little_endian_from_the_first_byte() {
        let mut out = Vec::new();
        pack(&mut out, [1, 0, 1, 1, 0, 0, 0, 0, 1], 1);
        assert_eq!(out, [0b0000_1101, 0b0000_0001]);
        let mut out = Vec::new();
        pack(&mut out, [22], 7);
        assert_eq!(out, [0x16]);
    }

    #[test]
    fn short_input_is_reported() {
        assert_eq!(unpack(&[0], 9, 1, drop), None);
        let mut zeros = 0;
        assert_eq!(unpack(&[], 4, 0, |value| zeros += 1 + value), Some(0));
        assert_eq!(zeros, 4);
    }

    #[test]
    fn bit_widths_follow_next_pow2() {
        assert_eq!(bits_for(0), 0);
        assert_eq!(bits_for(1), 0);
        assert_eq!(bits_for(2), 1);
        assert_eq!(bits_for(64), 6);
        assert_eq!(bits_for(65), 7);
        assert_eq!(bits_for(128), 7);
    }
}
