//! XXH3-64 with the default secret and seed 0, OpenZL's checksum.
//!
//! Every OpenZL checksum is derived from `XXH3_64bits`: the frame header
//! check byte keeps its low 8 bits, the per-chunk content and compressed
//! checksums keep the low 32. Inputs of up to 240 bytes take the reference's
//! short paths, which are scalar. Longer inputs run the stripe accumulator,
//! the only part that touches every byte, as a SIMD kernel at the codec's
//! selected [`Level`]; [`scalar_long`] is the independent baseline it is
//! tested against.

use fearless_simd::{Bytes, Level, Simd, SimdBase, SimdCombine, SimdSplit, dispatch};
use fearless_simd::{u8x64, u32x8, u32x16, u64x8};

const PRIME32_1: u64 = 0x9E37_79B1;
const PRIME32_2: u64 = 0x85EB_CA77;
const PRIME32_3: u64 = 0xC2B2_AE3D;
const PRIME64_1: u64 = 0x9E37_79B1_85EB_CA87;
const PRIME64_2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const PRIME64_3: u64 = 0x1656_67B1_9E37_79F9;
const PRIME64_4: u64 = 0x85EB_CA77_C2B2_AE63;
const PRIME64_5: u64 = 0x27D4_EB2F_1656_67C5;
const PRIME_MX1: u64 = 0x1656_6791_9E37_79F9;
const PRIME_MX2: u64 = 0x9FB2_1C65_1E98_DF25;

/// `XXH3_kSecret`.
const SECRET: [u8; 192] = [
    0xb8, 0xfe, 0x6c, 0x39, 0x23, 0xa4, 0x4b, 0xbe, 0x7c, 0x01, 0x81, 0x2c, 0xf7, 0x21, 0xad, 0x1c,
    0xde, 0xd4, 0x6d, 0xe9, 0x83, 0x90, 0x97, 0xdb, 0x72, 0x40, 0xa4, 0xa4, 0xb7, 0xb3, 0x67, 0x1f,
    0xcb, 0x79, 0xe6, 0x4e, 0xcc, 0xc0, 0xe5, 0x78, 0x82, 0x5a, 0xd0, 0x7d, 0xcc, 0xff, 0x72, 0x21,
    0xb8, 0x08, 0x46, 0x74, 0xf7, 0x43, 0x24, 0x8e, 0xe0, 0x35, 0x90, 0xe6, 0x81, 0x3a, 0x26, 0x4c,
    0x3c, 0x28, 0x52, 0xbb, 0x91, 0xc3, 0x00, 0xcb, 0x88, 0xd0, 0x65, 0x8b, 0x1b, 0x53, 0x2e, 0xa3,
    0x71, 0x64, 0x48, 0x97, 0xa2, 0x0d, 0xf9, 0x4e, 0x38, 0x19, 0xef, 0x46, 0xa9, 0xde, 0xac, 0xd8,
    0xa8, 0xfa, 0x76, 0x3f, 0xe3, 0x9c, 0x34, 0x3f, 0xf9, 0xdc, 0xbb, 0xc7, 0xc7, 0x0b, 0x4f, 0x1d,
    0x8a, 0x51, 0xe0, 0x4b, 0xcd, 0xb4, 0x59, 0x31, 0xc8, 0x9f, 0x7e, 0xc9, 0xd9, 0x78, 0x73, 0x64,
    0xea, 0xc5, 0xac, 0x83, 0x34, 0xd3, 0xeb, 0xc3, 0xc5, 0x81, 0xa0, 0xff, 0xfa, 0x13, 0x63, 0xeb,
    0x17, 0x0d, 0xdd, 0x51, 0xb7, 0xf0, 0xda, 0x49, 0xd3, 0x16, 0x55, 0x26, 0x29, 0xd4, 0x68, 0x9e,
    0x2b, 0x16, 0xbe, 0x58, 0x7d, 0x47, 0xa1, 0xfc, 0x8f, 0xf8, 0xb8, 0xd1, 0x7a, 0xd0, 0x31, 0xce,
    0x45, 0xcb, 0x3a, 0x8f, 0x95, 0x16, 0x04, 0x28, 0xaf, 0xd7, 0xfb, 0xca, 0xbb, 0x4b, 0x40, 0x7e,
];

/// Bytes hashed by one accumulation step.
const STRIPE: usize = 64;
/// Secret bytes advanced per stripe.
const CONSUME_RATE: usize = 8;
/// Stripes between two scrambles: `(192 - 64) / 8`.
const STRIPES_PER_BLOCK: usize = (SECRET.len() - STRIPE) / CONSUME_RATE;
/// Bytes between two scrambles.
const BLOCK: usize = STRIPE * STRIPES_PER_BLOCK;
/// Offset of the scramble key in the secret.
const SCRAMBLE_KEY: usize = SECRET.len() - STRIPE;
/// Offset of the last stripe's key: `XXH_SECRET_LASTACC_START`.
const LAST_STRIPE_KEY: usize = SECRET.len() - STRIPE - 7;
/// Offset of the merge key: `XXH_SECRET_MERGEACCS_START`.
const MERGE_KEY: usize = 11;
/// Largest input hashed without the stripe accumulator.
const MIDSIZE_MAX: usize = 240;

/// The accumulator's initial value, `XXH3_INIT_ACC`.
const INIT_ACC: [u64; 8] = [
    PRIME32_3, PRIME64_1, PRIME64_2, PRIME64_3, PRIME64_4, PRIME32_2, PRIME64_5, PRIME32_1,
];

#[inline(always)]
fn read64(bytes: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(word)
}

#[inline(always)]
fn read32(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(word)
}

#[inline(always)]
const fn fold64(lhs: u64, rhs: u64) -> u64 {
    let product = (lhs as u128).wrapping_mul(rhs as u128);
    (product as u64) ^ ((product >> 64) as u64)
}

#[inline(always)]
const fn xxh64_avalanche(mut hash: u64) -> u64 {
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(PRIME64_2);
    hash ^= hash >> 29;
    hash = hash.wrapping_mul(PRIME64_3);
    hash ^ (hash >> 32)
}

#[inline(always)]
const fn avalanche(mut hash: u64) -> u64 {
    hash ^= hash >> 37;
    hash = hash.wrapping_mul(PRIME_MX1);
    hash ^ (hash >> 32)
}

#[inline(always)]
const fn rrmxmx(mut hash: u64, len: u64) -> u64 {
    hash ^= hash.rotate_left(49) ^ hash.rotate_left(24);
    hash = hash.wrapping_mul(PRIME_MX2);
    hash ^= (hash >> 35).wrapping_add(len);
    hash = hash.wrapping_mul(PRIME_MX2);
    hash ^ (hash >> 28)
}

#[inline(always)]
fn mix16(input: &[u8], at: usize, secret_at: usize) -> u64 {
    fold64(
        read64(input, at) ^ read64(&SECRET, secret_at),
        read64(input, at + 8) ^ read64(&SECRET, secret_at + 8),
    )
}

/// `XXH3_len_0to16_64b`.
fn hash_0_to_16(input: &[u8]) -> u64 {
    let len = input.len();
    if len > 8 {
        let flip_lo = read64(&SECRET, 24) ^ read64(&SECRET, 32);
        let flip_hi = read64(&SECRET, 40) ^ read64(&SECRET, 48);
        let lo = read64(input, 0) ^ flip_lo;
        let hi = read64(input, len - 8) ^ flip_hi;
        let acc = (len as u64)
            .wrapping_add(lo.swap_bytes())
            .wrapping_add(hi)
            .wrapping_add(fold64(lo, hi));
        return avalanche(acc);
    }
    if len >= 4 {
        let first = u64::from(read32(input, 0));
        let last = u64::from(read32(input, len - 4));
        let flip = read64(&SECRET, 8) ^ read64(&SECRET, 16);
        let keyed = (last.wrapping_add(first << 32)) ^ flip;
        return rrmxmx(keyed, len as u64);
    }
    if len > 0 {
        let combined = (u32::from(input[0]) << 16)
            | (u32::from(input[len >> 1]) << 24)
            | u32::from(input[len - 1])
            | ((len as u32) << 8);
        let flip = u64::from(read32(&SECRET, 0) ^ read32(&SECRET, 4));
        return xxh64_avalanche(u64::from(combined) ^ flip);
    }
    xxh64_avalanche(read64(&SECRET, 56) ^ read64(&SECRET, 64))
}

/// `XXH3_len_17to128_64b`.
fn hash_17_to_128(input: &[u8]) -> u64 {
    let len = input.len();
    let mut acc = (len as u64).wrapping_mul(PRIME64_1);
    let rounds = (len - 1) / 32;
    for i in (0..=rounds).rev() {
        acc = acc.wrapping_add(mix16(input, 16 * i, 32 * i));
        acc = acc.wrapping_add(mix16(input, len - 16 * (i + 1), 32 * i + 16));
    }
    avalanche(acc)
}

/// `XXH3_len_129to240_64b`.
fn hash_129_to_240(input: &[u8]) -> u64 {
    const START_OFFSET: usize = 3;
    const LAST_OFFSET: usize = 17;
    let len = input.len();
    let mut acc = (len as u64).wrapping_mul(PRIME64_1);
    for i in 0..8 {
        acc = acc.wrapping_add(mix16(input, 16 * i, 16 * i));
    }
    let mut acc_end = mix16(input, len - 16, 136 - LAST_OFFSET);
    acc = avalanche(acc);
    for i in 8..len / 16 {
        acc_end = acc_end.wrapping_add(mix16(input, 16 * i, 16 * (i - 8) + START_OFFSET));
    }
    avalanche(acc.wrapping_add(acc_end))
}

/// `XXH3_mergeAccs` with the default secret.
fn merge(acc: &[u64; 8], len: usize) -> u64 {
    let mut result = (len as u64).wrapping_mul(PRIME64_1);
    for i in 0..4 {
        result = result.wrapping_add(fold64(
            acc[2 * i] ^ read64(&SECRET, MERGE_KEY + 16 * i),
            acc[2 * i + 1] ^ read64(&SECRET, MERGE_KEY + 16 * i + 8),
        ));
    }
    avalanche(result)
}

/// `XXH3_accumulate_512` for one stripe, lane by lane.
#[inline(always)]
fn scalar_stripe(acc: &mut [u64; 8], stripe: &[u8], key: usize) {
    for i in 0..8 {
        let data = read64(stripe, 8 * i);
        let keyed = data ^ read64(&SECRET, key + 8 * i);
        acc[i ^ 1] = acc[i ^ 1].wrapping_add(data);
        acc[i] = acc[i].wrapping_add((keyed & 0xFFFF_FFFF).wrapping_mul(keyed >> 32));
    }
}

/// `XXH3_scrambleAcc`, lane by lane.
#[inline(always)]
fn scalar_scramble(acc: &mut [u64; 8]) {
    for (i, lane) in acc.iter_mut().enumerate() {
        let mut value = *lane;
        value ^= value >> 47;
        value ^= read64(&SECRET, SCRAMBLE_KEY + 8 * i);
        *lane = value.wrapping_mul(PRIME32_1);
    }
}

/// The accumulator of an input longer than 240 bytes, lane by lane: the
/// reference's blocks, partial block and overlapping last stripe.
pub(crate) fn scalar_long(input: &[u8]) -> [u64; 8] {
    let mut acc = INIT_ACC;
    let len = input.len();
    let blocks = (len - 1) / BLOCK;
    let (block_data, tail) = input.split_at(blocks * BLOCK);
    for block in block_data.as_chunks::<BLOCK>().0 {
        for (n, data) in block.as_chunks::<STRIPE>().0.iter().enumerate() {
            scalar_stripe(&mut acc, data, n * CONSUME_RATE);
        }
        scalar_scramble(&mut acc);
    }
    let stripes = (len - 1 - blocks * BLOCK) / STRIPE;
    for (n, data) in tail
        .as_chunks::<STRIPE>()
        .0
        .iter()
        .take(stripes)
        .enumerate()
    {
        scalar_stripe(&mut acc, data, n * CONSUME_RATE);
    }
    scalar_stripe(&mut acc, &input[len - STRIPE..], LAST_STRIPE_KEY);
    acc
}

/// One stripe in vector registers: `acc + swap_pairs(data) + lo32 * hi32`
/// of the keyed data.
#[inline(always)]
fn simd_stripe<S: Simd>(
    simd: S,
    acc: u64x8<S>,
    stripe: &[u8; STRIPE],
    key: &[u8; STRIPE],
) -> u64x8<S> {
    // Little-endian lanes: the byte view and the integer views coincide.
    let bytes = u8x64::load_array_ref(simd, stripe);
    let data: u64x8<S> = bytes.bitcast();
    let keyed: u32x16<S> =
        (data ^ u8x64::load_array_ref(simd, key).bitcast::<u64x8<S>>()).bitcast();
    let (front, back): (u32x8<S>, u32x8<S>) = keyed.split();
    let lows = front.unzip_low(back);
    let highs = front.unzip_high(back);
    let (first, second) = simd.widening_mul_u32x8(lows, highs);
    let products = first.combine(second);
    // Exchange the two 64-bit halves of every 128-bit block.
    let swapped: u64x8<S> = bytes
        .swizzle_dyn_within_blocks(u8x64::from_slice(simd, &SWAP_HALVES))
        .bitcast();
    acc + swapped + products
}

/// Byte indices that exchange the 8-byte halves of each 16-byte block.
const SWAP_HALVES: [u8; 64] = {
    let mut indices = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        indices[i] = ((i % 16) ^ 8) as u8;
        i += 1;
    }
    indices
};

/// The accumulator of an input longer than 240 bytes, eight lanes at once.
#[inline(always)]
fn simd_long<S: Simd>(simd: S, input: &[u8]) -> [u64; 8] {
    let mut acc = u64x8::from_slice(simd, &INIT_ACC);
    let len = input.len();
    let blocks = (len - 1) / BLOCK;
    let key = |offset: usize| -> &[u8; STRIPE] {
        SECRET[offset..offset + STRIPE]
            .first_chunk::<STRIPE>()
            .expect("secret offsets stay inside the secret")
    };
    let (block_data, tail) = input.split_at(blocks * BLOCK);
    for block in block_data.as_chunks::<BLOCK>().0 {
        let (stripes, _) = block.as_chunks::<STRIPE>();
        for (n, data) in stripes.iter().enumerate() {
            acc = simd_stripe(simd, acc, data, key(n * CONSUME_RATE));
        }
        let mut lanes = [0u64; 8];
        acc.store_slice(&mut lanes);
        scalar_scramble(&mut lanes);
        acc = u64x8::from_slice(simd, &lanes);
    }
    let stripes = (len - 1 - blocks * BLOCK) / STRIPE;
    let (tail_stripes, _) = tail.as_chunks::<STRIPE>();
    for (n, data) in tail_stripes.iter().take(stripes).enumerate() {
        acc = simd_stripe(simd, acc, data, key(n * CONSUME_RATE));
    }
    let last = input[len - STRIPE..]
        .first_chunk::<STRIPE>()
        .expect("long inputs hold a full last stripe");
    acc = simd_stripe(simd, acc, last, key(LAST_STRIPE_KEY));
    let mut lanes = [0u64; 8];
    acc.store_slice(&mut lanes);
    lanes
}

/// `XXH3_64bits(input)`, with the long-input accumulator run at `level`.
#[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
pub(crate) fn hash64(level: Level, input: &[u8]) -> u64 {
    match input.len() {
        0..=16 => hash_0_to_16(input),
        17..=128 => hash_17_to_128(input),
        129..=MIDSIZE_MAX => hash_129_to_240(input),
        len => {
            let acc = if cfg!(target_endian = "little") {
                dispatch!(level, simd => simd_long(simd, input))
            } else {
                scalar_long(input)
            };
            merge(&acc, len)
        }
    }
}

/// The low 32 bits of [`hash64`]: OpenZL's chunk checksums.
pub(crate) fn hash32(level: Level, input: &[u8]) -> u32 {
    hash64(level, input) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn sample(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (i.wrapping_mul(0x9E37_79B1) >> 11) as u8)
            .collect()
    }

    #[test]
    fn every_length_class_matches_the_reference_library() {
        let data = sample(if cfg!(miri) { 1100 } else { 9000 });
        let lengths: Vec<usize> = if cfg!(miri) {
            [
                0, 1, 3, 4, 8, 9, 16, 17, 128, 129, 240, 241, 1024, 1025, 1100,
            ]
            .to_vec()
        } else {
            (0..=2100).chain([4095, 4096, 4097, 8191, 9000]).collect()
        };
        for len in lengths {
            let expected = openzl_ffi::xxh3_64(&data[..len]);
            for backend in Backend::available() {
                assert_eq!(
                    hash64(backend.0, &data[..len]),
                    expected,
                    "{backend}, length {len}"
                );
            }
        }
    }

    #[test]
    fn simd_accumulator_matches_the_scalar_baseline_at_every_level() {
        let data = sample(if cfg!(miri) { 1300 } else { 5000 });
        for len in [241, 255, 256, 1023, 1024, 1025, 1088, 1089, data.len()] {
            let expected = scalar_long(&data[..len]);
            for backend in Backend::available() {
                let got = dispatch!(backend.0, simd => simd_long(simd, &data[..len]));
                assert_eq!(got, expected, "{backend}, length {len}");
            }
        }
    }

    #[test]
    fn checksum_keeps_the_low_bits() {
        let data = sample(300);
        let level = Backend::default().0;
        assert_eq!(
            u64::from(hash32(level, &data)),
            hash64(level, &data) & 0xFFFF_FFFF
        );
    }
}
