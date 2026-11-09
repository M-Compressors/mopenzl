//! `LZ4_compress_fast` (lz4.c, `LZ4_compress_generic_validated`) for the
//! case OpenZL uses: a fresh stream, no dictionary, and an output buffer of
//! `LZ4_compressBound` bytes (`notLimited`).
//!
//! Inputs below `LZ4_64Klimit` use the `byU16` table (13-bit hash of four
//! bytes), larger ones the `byU32` table (12-bit hash of five bytes). The
//! reference's `byPtr` table exists only on 32-bit hosts and is not ported.

use alloc::vec;
use alloc::vec::Vec;

use super::block::{
    DISTANCE_MAX, LAST_LITERALS, MF_LIMIT, MIN_LENGTH, MIN_MATCH, compress_bound, count,
    push_last_literals, push_literals, push_match_length, read32, read64,
};

/// `LZ4_64Klimit`.
const LIMIT_64K: usize = 64 * 1024 + (MF_LIMIT - 1);
/// `LZ4_skipTrigger`.
const SKIP_TRIGGER: u32 = 6;
/// `LZ4_ACCELERATION_MAX`.
const ACCELERATION_MAX: i32 = 65_537;
/// `LZ4_HASHLOG`.
const HASH_LOG: u32 = 12;

/// `LZ4_hash4` for `byU16`.
const fn hash4(sequence: u32) -> usize {
    (sequence.wrapping_mul(2_654_435_761) >> (MIN_MATCH as u32 * 8 - (HASH_LOG + 1))) as usize
}

/// `LZ4_hash5` for `byU32`, little-endian.
const fn hash5(sequence: u64) -> usize {
    const PRIME_5_BYTES: u64 = 889_523_592_379;
    ((sequence << 24).wrapping_mul(PRIME_5_BYTES) >> (64 - HASH_LOG)) as usize
}

/// `LZ4_hashPosition`.
fn hash_position(src: &[u8], at: usize, by_u16: bool) -> usize {
    if by_u16 {
        hash4(read32(src, at))
    } else {
        hash5(read64(src, at))
    }
}

/// `LZ4_compress_fast` with the given acceleration.
pub(super) fn compress(src: &[u8], acceleration: i32) -> Vec<u8> {
    let acceleration = acceleration.clamp(1, ACCELERATION_MAX);
    let n = src.len();
    let mut out = Vec::with_capacity(compress_bound(n));
    if n == 0 {
        out.push(0);
        return out;
    }
    let by_u16 = n < LIMIT_64K;
    let mut table = vec![0u32; if by_u16 { 2 << HASH_LOG } else { 1 << HASH_LOG }];
    let mut anchor = 0usize;
    if n >= MIN_LENGTH {
        let mflimit_plus_one = n - MF_LIMIT + 1;
        let matchlimit = n - LAST_LITERALS;
        table[hash_position(src, 0, by_u16)] = 0;
        let mut ip = 1usize;
        let mut forward_h = hash_position(src, ip, by_u16);
        'blocks: loop {
            // Find a match.
            let mut forward_ip = ip;
            let mut step = 1usize;
            let mut search_match_nb = acceleration << SKIP_TRIGGER;
            let mut matched;
            loop {
                let h = forward_h;
                let current = forward_ip;
                matched = table[h] as usize;
                ip = forward_ip;
                forward_ip += step;
                step = (search_match_nb >> SKIP_TRIGGER) as usize;
                search_match_nb += 1;
                if forward_ip > mflimit_plus_one {
                    break 'blocks;
                }
                forward_h = hash_position(src, forward_ip, by_u16);
                table[h] = current as u32;
                if !by_u16 && matched + DISTANCE_MAX < current {
                    continue;
                }
                if read32(src, matched) == read32(src, ip) {
                    break;
                }
            }
            // Catch up.
            if matched > 0 && src[ip - 1] == src[matched - 1] {
                loop {
                    ip -= 1;
                    matched -= 1;
                    if !(ip > anchor && matched > 0 && src[ip - 1] == src[matched - 1]) {
                        break;
                    }
                }
            }
            let mut token = push_literals(&mut out, src, anchor, ip);
            loop {
                // `_next_match`.
                out.extend_from_slice(&((ip - matched) as u16).to_le_bytes());
                let code = count(src, ip + MIN_MATCH, matched + MIN_MATCH, matchlimit);
                ip += code + MIN_MATCH;
                push_match_length(&mut out, token, code);
                anchor = ip;
                if ip >= mflimit_plus_one {
                    break 'blocks;
                }
                // Fill table.
                table[hash_position(src, ip - 2, by_u16)] = (ip - 2) as u32;
                // Test next position.
                let h = hash_position(src, ip, by_u16);
                let candidate = table[h] as usize;
                table[h] = ip as u32;
                if (by_u16 || candidate + DISTANCE_MAX >= ip)
                    && read32(src, candidate) == read32(src, ip)
                {
                    token = out.len();
                    out.push(0);
                    matched = candidate;
                    continue;
                }
                break;
            }
            ip += 1;
            forward_h = hash_position(src, ip, by_u16);
        }
    }
    push_last_literals(&mut out, src, anchor);
    out
}
