//! Output primitives shared by the LZ decoders (`lz`, `field_lz`).
//!
//! The decoders write into a buffer that is sized to the declared output up
//! front, so a literal run or a match is a block copy into a slice instead
//! of a `Vec::extend_from_slice` (capacity check plus a call to `memcpy` per
//! sequence). Short runs, which dominate real streams, are copied as one
//! fixed 16-byte block: the bytes past the run are scratch that the next
//! write covers, or that the final truncation drops. This is the safe
//! counterpart of the reference's `wildcopy`.
//!
//! Scratch bytes are never read as data: a match source always lies before
//! the write cursor, and everything before the cursor is final.

/// The block size of the short-run fast paths.
const BLOCK: usize = 16;
/// The longest match copied block by block; longer ones use `copy_within`.
const BLOCK_LOOP_MAX: usize = 64;

/// Writes `literals[..count]` at `buf[pos..]`.
///
/// `pos + count <= buf.len()` and `count <= literals.len()` hold.
#[inline(always)]
pub(super) fn copy_literals(buf: &mut [u8], pos: usize, literals: &[u8], count: usize) {
    if count <= BLOCK
        && let (Some(from), Some(to)) = (
            literals.first_chunk::<BLOCK>(),
            buf.get_mut(pos..)
                .and_then(<[u8]>::first_chunk_mut::<BLOCK>),
        )
    {
        *to = *from;
        return;
    }
    buf[pos..pos + count].copy_from_slice(&literals[..count]);
}

/// Appends the `length`-byte match `offset` bytes back to the output
/// written up to `pos`; an offset below `length` repeats the pattern.
///
/// `1 <= offset <= pos` and `pos + length <= buf.len()` hold.
#[inline(always)]
pub(super) fn copy_match(buf: &mut [u8], pos: usize, offset: usize, length: usize) {
    let from = pos - offset;
    if offset >= length {
        // The source ends before the destination starts.
        if length <= BLOCK
            && let Some(block) = buf
                .get(from..)
                .and_then(<[u8]>::first_chunk::<BLOCK>)
                .copied()
            && let Some(to) = buf
                .get_mut(pos..)
                .and_then(<[u8]>::first_chunk_mut::<BLOCK>)
        {
            *to = block;
            return;
        }
        if offset >= BLOCK
            && length <= BLOCK_LOOP_MAX
            && pos + length.next_multiple_of(BLOCK) <= buf.len()
        {
            // Every block's source lies at least 16 bytes behind its
            // destination, so it is final when read.
            let mut done = 0;
            while done < length {
                let block: [u8; BLOCK] = buf[from + done..from + done + BLOCK]
                    .try_into()
                    .unwrap_or([0; BLOCK]);
                buf[pos + done..pos + done + BLOCK].copy_from_slice(&block);
                done += BLOCK;
            }
            return;
        }
        buf.copy_within(from..from + length, pos);
        return;
    }
    if offset == 1 {
        let byte = buf[from];
        buf[pos..pos + length].fill(byte);
        return;
    }
    // The bytes from `from` repeat with period `offset`: each step copies
    // the finished prefix, a multiple of the period, after itself.
    let mut done = 0;
    let mut chunk = offset;
    while done < length {
        let n = chunk.min(length - done);
        buf.copy_within(from..from + n, pos + done);
        done += n;
        chunk = done;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// The match as the reference writes it, byte by byte.
    fn naive(prefix: &[u8], offset: usize, length: usize) -> Vec<u8> {
        let mut out = prefix.to_vec();
        for _ in 0..length {
            out.push(out[out.len() - offset]);
        }
        out
    }

    #[test]
    fn matches_equal_the_byte_by_byte_copy_for_every_shape() {
        let prefix: Vec<u8> = (0..200u32).map(|i| (i * 7 + i / 5) as u8).collect();
        let lengths: &[usize] = if cfg!(miri) {
            &[1, 5, 16, 17, 70, 150]
        } else {
            &[1, 2, 3, 15, 16, 17, 31, 32, 33, 63, 64, 65, 80, 150, 400]
        };
        for offset in [1usize, 2, 3, 5, 8, 15, 16, 17, 33, 100, 200] {
            for &length in lengths {
                // With and without room for block overshoot.
                for slack in [0usize, 40] {
                    let mut buf = prefix.clone();
                    buf.resize(prefix.len() + length + slack, 0xEE);
                    copy_match(&mut buf, prefix.len(), offset, length);
                    let expected = naive(&prefix, offset, length);
                    assert_eq!(
                        &buf[..prefix.len() + length],
                        &expected[..],
                        "offset {offset}, length {length}, slack {slack}"
                    );
                }
            }
        }
    }

    #[test]
    fn literals_copy_exactly_the_run_and_may_overshoot_into_scratch() {
        let literals: Vec<u8> = (1..=40).collect();
        for count in [0usize, 1, 15, 16, 17, 40] {
            // Room for a block and exactly no more room than the run.
            for room in [count, count + 20] {
                let mut buf = vec![0xEEu8; 5 + room];
                copy_literals(&mut buf, 5, &literals, count);
                assert_eq!(&buf[5..5 + count], &literals[..count]);
                assert_eq!(&buf[..5], &[0xEE; 5]);
            }
        }
        // A short remainder of literals cannot be block copied.
        let mut buf = vec![0u8; 32];
        copy_literals(&mut buf, 0, &literals[..3], 3);
        assert_eq!(&buf[..3], &[1, 2, 3]);
        assert_eq!(&buf[3..], &[0; 29]);
    }
}
