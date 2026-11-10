//! The multiplicative hashes of `shared/hash.h` (`ZL_hashPtr`).
//!
//! Every function hashes the first `len` bytes at an offset of a slice into
//! `log` bits. Reads past the end of the slice see zero bytes: the reference
//! never reads there, so the padding only keeps the port panic free.

const PRIME_2: u32 = 506_832_829;
const PRIME_3: u32 = 506_832_829;
const PRIME_4: u32 = 2_654_435_761;
const PRIME_5: u64 = 889_523_592_379;
const PRIME_6: u64 = 227_718_039_650_203;
const PRIME_7: u64 = 58_295_818_150_454_627;
const PRIME_8: u64 = 0xCF1B_BCDC_B7A5_6463;

/// `ZL_readLE16` .. `ZL_readLE64` generalised: `N` bytes at `at`, zero
/// padded past the end of `data`.
#[inline(always)]
pub(crate) fn load<const N: usize>(data: &[u8], at: usize) -> [u8; N] {
    if let Some(chunk) = data.get(at..).and_then(<[u8]>::first_chunk::<N>) {
        return *chunk;
    }
    let mut padded = [0u8; N];
    if let Some(rest) = data.get(at..) {
        padded[..rest.len()].copy_from_slice(rest);
    }
    padded
}

/// `ZL_readLE64`.
#[inline(always)]
pub(crate) fn read_u64(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(load::<8>(data, at))
}

/// `ZL_readLE32`.
#[inline(always)]
pub(crate) fn read_u32(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(load::<4>(data, at))
}

/// `ZL_hash8`.
#[inline(always)]
const fn hash8(value: u64, log: u32) -> usize {
    (value.wrapping_mul(PRIME_8) >> (64 - log)) as usize
}

/// `ZL_hashPtr`: hashes `len` bytes (2 to 8, 12 or 16) at `data[at..]` to
/// `log` bits. Other lengths hash like 8 (the reference asserts).
#[inline(always)]
pub(crate) fn hash_ptr(data: &[u8], at: usize, log: u32, len: u32) -> usize {
    match len {
        2 => {
            let value = u32::from(u16::from_le_bytes(load::<2>(data, at)));
            (value << 16).wrapping_mul(PRIME_2).wrapping_shr(32 - log) as usize
        }
        3 => (read_u32(data, at) << 8)
            .wrapping_mul(PRIME_3)
            .wrapping_shr(32 - log) as usize,
        4 => read_u32(data, at)
            .wrapping_mul(PRIME_4)
            .wrapping_shr(32 - log) as usize,
        5 => ((read_u64(data, at) << 24).wrapping_mul(PRIME_5) >> (64 - log)) as usize,
        6 => ((read_u64(data, at) << 16).wrapping_mul(PRIME_6) >> (64 - log)) as usize,
        7 => ((read_u64(data, at) << 8).wrapping_mul(PRIME_7) >> (64 - log)) as usize,
        12 => hash8(read_u64(data, at) ^ u64::from(read_u32(data, at + 8)), log),
        16 => hash8(read_u64(data, at) ^ read_u64(data, at + 8), log),
        _ => hash8(read_u64(data, at), log),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn loads_pad_with_zeros_past_the_end() {
        let data = [1u8, 2, 3];
        assert_eq!(load::<4>(&data, 1), [2, 3, 0, 0]);
        assert_eq!(load::<2>(&data, 0), [1, 2]);
        assert_eq!(load::<2>(&data, 9), [0, 0]);
        assert_eq!(read_u32(&data, 0), 0x0003_0201);
        assert_eq!(read_u64(&data, 2), 3);
    }

    #[test]
    fn hashes_follow_the_reference_formulas() {
        let data: Vec<u8> = (1..=32).collect();
        let v64 = u64::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8]);
        let v32 = 0x0403_0201u32;
        assert_eq!(
            hash_ptr(&data, 0, 12, 2),
            ((0x0201u32 << 16).wrapping_mul(506_832_829) >> 20) as usize
        );
        assert_eq!(
            hash_ptr(&data, 0, 12, 3),
            ((v32 << 8).wrapping_mul(506_832_829) >> 20) as usize
        );
        assert_eq!(
            hash_ptr(&data, 0, 12, 4),
            (v32.wrapping_mul(2_654_435_761) >> 20) as usize
        );
        for (len, shift, prime) in [
            (5u32, 24, PRIME_5),
            (6, 16, PRIME_6),
            (7, 8, PRIME_7),
            (8, 0, PRIME_8),
        ] {
            assert_eq!(
                hash_ptr(&data, 0, 20, len),
                ((v64 << shift).wrapping_mul(prime) >> 44) as usize
            );
        }
        let v12 = v64 ^ u64::from(u32::from_le_bytes([9, 10, 11, 12]));
        assert_eq!(hash_ptr(&data, 0, 20, 12), hash8(v12, 20));
        let v16 = v64 ^ u64::from_le_bytes([9, 10, 11, 12, 13, 14, 15, 16]);
        assert_eq!(hash_ptr(&data, 0, 20, 16), hash8(v16, 20));
        // Unsupported lengths hash like 8.
        assert_eq!(hash_ptr(&data, 0, 20, 9), hash_ptr(&data, 0, 20, 8));
    }
}
