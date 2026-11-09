//! OpenZL's LEB128 varints (`ZL_varintEncode` / `ZL_varintDecode`).
//!
//! Seven bits per byte, least significant group first, the high bit set on
//! every byte but the last. Decoding follows the reference's lenient reader:
//! it accepts redundant groups (`0x80 0x00` is 0), reads at most ten bytes,
//! and takes only bit 0 of a tenth byte.

/// The longest encoding of a `u64`.
pub(crate) const MAX_LEN: usize = 10;

/// Why a varint could not be read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg(feature = "decompression")]
pub(crate) enum VarintError {
    /// The input ended inside the varint.
    Truncated,
    /// A tenth byte still had its continuation bit set.
    Overlong,
}

/// The encoding of `value`, and how many of its bytes are used.
#[cfg(feature = "compression")]
pub(crate) const fn encode(mut value: u64) -> ([u8; MAX_LEN], usize) {
    let mut bytes = [0u8; MAX_LEN];
    let mut len = 0;
    while value >= 0x80 {
        bytes[len] = (value as u8 & 0x7f) | 0x80;
        value >>= 7;
        len += 1;
    }
    bytes[len] = value as u8;
    (bytes, len + 1)
}

/// Appends the encoding of `value` to `out`.
#[cfg(feature = "compression")]
pub(crate) fn push(out: &mut alloc::vec::Vec<u8>, value: u64) {
    let (bytes, len) = encode(value);
    out.extend_from_slice(&bytes[..len]);
}

/// Reads one varint from the front of `input`: its value and length.
#[cfg(feature = "decompression")]
pub(crate) fn decode(input: &[u8]) -> Result<(u64, usize), VarintError> {
    let mut value = 0u64;
    for (index, &byte) in input.iter().take(MAX_LEN).enumerate() {
        if index == MAX_LEN - 1 {
            if byte & 0x80 != 0 {
                return Err(VarintError::Overlong);
            }
            value |= u64::from(byte & 1) << 63;
            return Ok((value, MAX_LEN));
        }
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
    }
    Err(VarintError::Truncated)
}

#[cfg(all(test, feature = "compression", feature = "decompression"))]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn values_round_trip() {
        for value in [
            0,
            1,
            127,
            128,
            300,
            16_383,
            16_384,
            u64::from(u32::MAX),
            u64::MAX,
        ] {
            let (bytes, len) = encode(value);
            assert_eq!(decode(&bytes[..len]), Ok((value, len)));
            let mut out = Vec::new();
            push(&mut out, value);
            assert_eq!(out, &bytes[..len]);
        }
    }

    #[test]
    fn decoding_matches_the_reference_leniency() {
        assert_eq!(decode(&[0x80, 0x00]), Ok((0, 2)));
        assert_eq!(decode(&[0xad, 0x02, 0xff]), Ok((301, 2)));
        let mut ten = [0xffu8; 10];
        ten[9] = 0x7f;
        assert_eq!(decode(&ten), Ok((u64::MAX, 10)));
    }

    #[test]
    fn malformed_varints_are_rejected() {
        assert_eq!(decode(&[]), Err(VarintError::Truncated));
        assert_eq!(decode(&[0x80, 0x80]), Err(VarintError::Truncated));
        assert_eq!(decode(&[0xff; 10]), Err(VarintError::Overlong));
    }
}
