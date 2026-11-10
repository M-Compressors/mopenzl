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

/// `LZ4_read32`, little-endian.
#[inline(always)]
pub(super) fn read32(src: &[u8], at: usize) -> u32 {
    src.get(at..at.wrapping_add(4))
        .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
        .map_or_else(|| out_of_range(at), u32::from_le_bytes)
}

/// `LZ4_read_ARCH` on a 64-bit little-endian host.
#[inline(always)]
pub(super) fn read64(src: &[u8], at: usize) -> u64 {
    src.get(at..at.wrapping_add(8))
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .map_or_else(|| out_of_range(at), u64::from_le_bytes)
}

/// The reference never reads outside the source; a read that would is a
/// bug of the port.
#[cold]
#[inline(never)]
fn out_of_range(at: usize) -> ! {
    panic!("LZ4 read at {at} is outside the source")
}

/// `LZ4_count`: the number of equal bytes at `p_in` and `p_match`, reading
/// `src` before `limit` only.
#[inline(always)]
pub(super) fn count(src: &[u8], p_in: usize, p_match: usize, limit: usize) -> usize {
    // Most matches end within the first eight bytes: one read settles them.
    // `p_match < p_in`, so the three comparisons bound both reads.
    if limit <= src.len()
        && p_match < p_in
        && p_in + 8 <= limit
        && let (Ok(a), Ok(b)) = (
            <[u8; 8]>::try_from(&src[p_in..p_in + 8]),
            <[u8; 8]>::try_from(&src[p_match..p_match + 8]),
        )
    {
        let diff = u64::from_le_bytes(a) ^ u64::from_le_bytes(b);
        if diff != 0 {
            // `LZ4_NbCommonBytes`: little-endian, trailing zero bytes.
            return (diff.trailing_zeros() >> 3) as usize;
        }
    }
    count_long(src, p_in, p_match, limit)
}

/// [`count`] for a match beyond the first eight bytes or a window shorter
/// than eight bytes.
#[inline(never)]
fn count_long(src: &[u8], p_in: usize, p_match: usize, limit: usize) -> usize {
    // `p_match < p_in`, so the window at `p_match` is at least as long.
    let (Some(a), Some(b)) = (src.get(p_in..limit.max(p_in)), src.get(p_match..)) else {
        out_of_range(p_in)
    };
    let b = &b[..a.len().min(b.len())];
    let (a8, mut a) = a.as_chunks::<8>();
    let (b8, mut b) = b.as_chunks::<8>();
    let mut done = 0;
    for (x, y) in a8.iter().zip(b8) {
        let diff = u64::from_le_bytes(*x) ^ u64::from_le_bytes(*y);
        if diff != 0 {
            return done + (diff.trailing_zeros() >> 3) as usize;
        }
        done += 8;
    }
    if a.len() >= 4 && a[..4] == b[..4] {
        done += 4;
        (a, b) = (&a[4..], &b[4..]);
    }
    if a.len() >= 2 && a[..2] == b[..2] {
        done += 2;
        (a, b) = (&a[2..], &b[2..]);
    }
    if !a.is_empty() && a[0] == b[0] {
        done += 1;
    }
    done
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

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::count;

    /// The equal bytes at `a` and `b`, one at a time, before `limit`.
    fn naive(src: &[u8], a: usize, b: usize, limit: usize) -> usize {
        let mut equal = 0;
        while a + equal < limit && src[a + equal] == src[b + equal] {
            equal += 1;
        }
        equal
    }

    /// Data of a tiny alphabet with long repeats, so that matches end at
    /// every distance from the start.
    fn sample(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut data: Vec<u8> = Vec::with_capacity(len);
        while data.len() < len {
            if data.len() > 40 && next() % 3 == 0 {
                let from = (next() as usize) % (data.len() - 20);
                let run = 1 + (next() as usize) % 20;
                data.extend_from_within(from..from + run);
            } else {
                data.push((next() % 3) as u8);
            }
        }
        data.truncate(len);
        data
    }

    #[test]
    fn count_matches_a_byte_loop() {
        let len = if cfg!(miri) { 120 } else { 400 };
        let src = sample(len, 0x5eed);
        let step = if cfg!(miri) { 7 } else { 1 };
        for p_in in (1..len).step_by(step) {
            for p_match in (0..p_in).step_by(step * 3) {
                for limit in [p_in, p_in + 1, p_in + 7, p_in + 8, p_in + 9, p_in + 40, len] {
                    let limit = limit.min(len);
                    assert_eq!(
                        count(&src, p_in, p_match, limit),
                        naive(&src, p_in, p_match, limit),
                        "{p_in} {p_match} {limit}"
                    );
                }
            }
        }
    }

    #[test]
    #[should_panic(expected = "outside the source")]
    fn count_beyond_the_source_is_a_port_bug() {
        count(&[0u8; 16], 8, 0, 100);
    }

    #[test]
    #[should_panic(expected = "outside the source")]
    fn read32_beyond_the_source_is_a_port_bug() {
        super::read32(&[0u8; 3], 0);
    }

    #[test]
    #[should_panic(expected = "outside the source")]
    fn read64_beyond_the_source_is_a_port_bug() {
        super::read64(&[0u8; 7], 0);
    }

    #[test]
    fn count_of_a_long_run_spans_chunks() {
        let src = [7u8; 100];
        assert_eq!(count(&src, 50, 0, 95), 45);
        assert_eq!(count(&src, 50, 49, 100), 50);
        assert_eq!(count(&src, 50, 49, 50), 0);
    }
}
