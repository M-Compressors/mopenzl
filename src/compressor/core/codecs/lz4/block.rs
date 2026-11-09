//! Constants and helpers shared by the LZ4 compressors (`lz4.c` common
//! definitions, `LZ4_count`, the literal-run and match-length encoders).

/// `MINMATCH`.
pub(super) const MIN_MATCH: usize = 4;
/// `LASTLITERALS`: the last bytes of a block are literals.
pub(super) const LAST_LITERALS: usize = 5;
/// `MFLIMIT`: no match starts in the last bytes of a block.
pub(super) const MF_LIMIT: usize = 12;
/// `LZ4_minLength`: inputs shorter than this are all literals.
pub(super) const MIN_LENGTH: usize = MF_LIMIT + 1;
/// `ML_MASK` and `RUN_MASK`: a saturated four-bit length field.
pub(super) const FIELD_MAX: usize = 15;
/// `LZ4_DISTANCE_MAX`.
pub(super) const DISTANCE_MAX: usize = 65_535;
/// `LZ4_MAX_INPUT_SIZE`.
pub(crate) const MAX_INPUT_SIZE: usize = 0x7E00_0000;

/// `LZ4_COMPRESSBOUND` for a valid size.
pub(super) const fn compress_bound(size: usize) -> usize {
    size + size / 255 + 16
}

/// `LZ4_read16`, little-endian.
pub(super) fn read16(src: &[u8], at: usize) -> u16 {
    let bytes = &src[at..at + 2];
    u16::from_le_bytes([bytes[0], bytes[1]])
}

/// `LZ4_read32`, little-endian.
pub(super) fn read32(src: &[u8], at: usize) -> u32 {
    let bytes = &src[at..at + 4];
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// `LZ4_read_ARCH` on a 64-bit little-endian host.
pub(super) fn read64(src: &[u8], at: usize) -> u64 {
    let bytes = &src[at..at + 8];
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

/// `LZ4_count`: the number of equal bytes at `p_in` and `p_match`, reading
/// `src` before `limit` only.
pub(super) fn count(src: &[u8], mut p_in: usize, mut p_match: usize, limit: usize) -> usize {
    let start = p_in;
    while p_in + 7 < limit {
        let diff = read64(src, p_match) ^ read64(src, p_in);
        if diff == 0 {
            p_in += 8;
            p_match += 8;
            continue;
        }
        // `LZ4_NbCommonBytes`: little-endian, trailing zero bytes.
        return p_in + (diff.trailing_zeros() >> 3) as usize - start;
    }
    if p_in + 3 < limit && read32(src, p_match) == read32(src, p_in) {
        p_in += 4;
        p_match += 4;
    }
    if p_in + 1 < limit && read16(src, p_match) == read16(src, p_in) {
        p_in += 2;
        p_match += 2;
    }
    if p_in < limit && src[p_match] == src[p_in] {
        p_in += 1;
    }
    p_in - start
}

/// Appends the extension bytes of a saturated length: 255 for every full
/// 255 and the remainder.
pub(super) fn push_length(out: &mut alloc::vec::Vec<u8>, mut len: usize) {
    while len >= 255 {
        out.push(255);
        len -= 255;
    }
    out.push(len as u8);
}

/// The token and literal run of a sequence: returns the token's position
/// (its low four bits, the match length, are still zero).
pub(super) fn push_literals(
    out: &mut alloc::vec::Vec<u8>,
    src: &[u8],
    anchor: usize,
    ip: usize,
) -> usize {
    let lit_len = ip - anchor;
    let token = out.len();
    if lit_len >= FIELD_MAX {
        out.push((FIELD_MAX << 4) as u8);
        push_length(out, lit_len - FIELD_MAX);
    } else {
        out.push((lit_len << 4) as u8);
    }
    out.extend_from_slice(&src[anchor..ip]);
    token
}

/// The match-length part of a sequence (`matchCode` is the length minus
/// `MINMATCH`) added to the token at `token`.
pub(super) fn push_match_length(out: &mut alloc::vec::Vec<u8>, token: usize, code: usize) {
    if code >= FIELD_MAX {
        out[token] += FIELD_MAX as u8;
        push_length(out, code - FIELD_MAX);
    } else {
        out[token] += code as u8;
    }
}

/// `_last_literals`: the final literal run of the block.
pub(super) fn push_last_literals(out: &mut alloc::vec::Vec<u8>, src: &[u8], anchor: usize) {
    let last_run = src.len() - anchor;
    if last_run >= FIELD_MAX {
        out.push((FIELD_MAX << 4) as u8);
        push_length(out, last_run - FIELD_MAX);
    } else {
        out.push((last_run << 4) as u8);
    }
    out.extend_from_slice(&src[anchor..]);
}
