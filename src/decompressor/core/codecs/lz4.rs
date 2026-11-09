//! The LZ4 codec (`DI_lz4`), feature `lz4`.
//!
//! Ports `codecs/lz4/decode_lz4_binding.c` and the safe path of
//! `LZ4_decompress_safe` (lz4 1.10.0, `LZ4_decompress_generic` with the
//! default `decode_full_block` and no dictionary). One serial stream in, one
//! LZ4 block; the codec header is the decoded size as a varint. The block is
//! decoded by a safe-Rust port of the reference loop rather than a third-party
//! crate, because the C decoder rejects blocks other decoders accept (a match
//! ending in the last five bytes, a non-final sequence starting in the last
//! twelve), and frames must be judged identically.
//!
//! Format versions before 23 have no LZ4 codec.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;
use crate::shared::varint;

/// `ZL_StandardTransformID_lz4`.
pub(crate) const LZ4: Desc = Desc {
    id: 62,
    min_version: 23,
    regens: &[Type::Serial],
    variable_regens: false,
    inputs: &[Type::Serial],
    variable_inputs: 0,
    decode,
};

/// `INT_MAX`: the C binding passes sizes to LZ4 as `int`.
const INT_MAX: u64 = i32::MAX as u64;

/// A match is at least this long (`MINMATCH`).
const MIN_MATCH: usize = 4;
/// The last bytes of a block are literals (`LASTLITERALS`).
const LAST_LITERALS: usize = 5;
/// A sequence starting this close to the end of the output is the last one
/// (`MFLIMIT`).
const MF_LIMIT: usize = 12;
/// Extra bytes a sequence needs after its literals: an offset, a match
/// length byte and the trailing literals (`2 + 1 + LASTLITERALS`).
const SEQUENCE_TAIL: usize = 2 + 1 + LAST_LITERALS;
/// The value of a saturated four-bit length field (`RUN_MASK`, `ML_MASK`).
const FIELD_MAX: usize = 15;

fn decode(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let input = inputs[0].bytes;
    if input.len() as u64 > INT_MAX {
        return Err(corrupt());
    }
    // The reference demands a header and ignores bytes after the size.
    let header = ctx.header;
    let (size, _) = varint::decode(header).map_err(|_| corrupt())?;
    if size > INT_MAX {
        return Err(corrupt());
    }
    let size = usize::try_from(size).unwrap_or(usize::MAX);
    let out = regens[0].fixed(1, size)?;
    out.resize(size, 0);
    if block(input, out)? != size {
        return Err(corrupt());
    }
    Ok(())
}

/// Reads an extended length (`read_variable_length`): bytes added until one
/// is not 255. The cursor may not pass `limit` (a signed position, since the
/// reference computes it from the end of the block and it can precede the
/// start); `initial_check` also rejects a cursor already at it.
fn extended(
    src: &[u8],
    ip: &mut usize,
    limit: isize,
    initial_check: bool,
) -> Result<usize, DecodeError> {
    if initial_check && *ip as isize >= limit {
        return Err(corrupt());
    }
    let mut length = 0usize;
    loop {
        let byte = *src.get(*ip).ok_or_else(corrupt)?;
        *ip += 1;
        length += usize::from(byte);
        if *ip as isize > limit {
            return Err(corrupt());
        }
        if byte != 255 {
            return Ok(length);
        }
    }
}

/// Decodes the LZ4 block `src` into `dst` (`LZ4_decompress_safe`): the
/// number of bytes written. `dst` is the whole expected output; a block that
/// ends before filling it succeeds with a smaller count, as in C.
///
/// # Errors
///
/// A corrupted stream for every block the reference rejects.
fn block(src: &[u8], dst: &mut [u8]) -> Result<usize, DecodeError> {
    let iend = src.len();
    let oend = dst.len();
    if oend == 0 {
        return if src == [0] { Ok(0) } else { Err(corrupt()) };
    }
    if iend == 0 {
        return Err(corrupt());
    }
    let (mut ip, mut op) = (0usize, 0usize);
    loop {
        let token = usize::from(*src.get(ip).ok_or_else(corrupt)?);
        ip += 1;
        let mut length = token >> 4;
        if length == FIELD_MAX {
            length += extended(src, &mut ip, iend as isize - FIELD_MAX as isize, true)?;
        }
        let cpy = op.checked_add(length).ok_or_else(corrupt)?;
        let lit_end = ip.checked_add(length).ok_or_else(corrupt)?;
        if cpy as isize > oend as isize - MF_LIMIT as isize
            || lit_end as isize > iend as isize - SEQUENCE_TAIL as isize
        {
            // Only the last sequence may come this close to either end, and
            // it has no match.
            if lit_end != iend || cpy > oend {
                return Err(corrupt());
            }
            dst[op..cpy].copy_from_slice(&src[ip..lit_end]);
            return Ok(cpy);
        }
        dst[op..cpy].copy_from_slice(&src[ip..lit_end]);
        ip = lit_end;
        op = cpy;

        let offset = usize::from(u16::from_le_bytes([
            *src.get(ip).ok_or_else(corrupt)?,
            *src.get(ip + 1).ok_or_else(corrupt)?,
        ]));
        ip += 2;
        // The reference accepts offset 0 and copies bytes it has not written
        // yet (undefined output); the specification forbids it, so it is
        // rejected here.
        if offset == 0 || offset > op {
            return Err(corrupt());
        }
        let mut length = token & FIELD_MAX;
        if length == FIELD_MAX {
            length += extended(
                src,
                &mut ip,
                iend as isize - (LAST_LITERALS as isize - 1),
                false,
            )?;
        }
        length += MIN_MATCH;
        let cpy = op.checked_add(length).ok_or_else(corrupt)?;
        if cpy as isize > oend as isize - LAST_LITERALS as isize {
            return Err(corrupt());
        }
        copy_match(dst, op, offset, length);
        op = cpy;
    }
}

/// Copies `length` bytes from `offset` bytes back, which may overlap the
/// destination (a short offset repeats a pattern).
fn copy_match(dst: &mut [u8], op: usize, offset: usize, length: usize) {
    let from = op - offset;
    if offset >= length {
        dst.copy_within(from..from + length, op);
        return;
    }
    // The pattern `dst[from..op]` repeats: copy it whole, then again from
    // the same start, so each pass is a non-overlapping copy.
    let mut done = 0;
    while done < length {
        let chunk = offset.min(length - done);
        dst.copy_within(from..from + chunk, op + done);
        done += chunk;
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::super::State;
    use super::*;
    use crate::Backend;

    /// Runs the codec on `block` with `header`, expecting `size` bytes.
    fn run(header: &[u8], block: &[u8], budget: usize) -> Result<Vec<u8>, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, header);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::Serial, budget);
        decode(&mut ctx, &[Input::serial(block)], &mut regens)?;
        regens[0].validate()?;
        Ok(regens[0].bytes().to_vec())
    }

    /// An LZ4 block of random sequences, valid or nearly so.
    fn generate(rng: &mut impl FnMut() -> u64) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let mut block = Vec::new();
        for _ in 0..(rng() % 6) {
            let literals = (rng() % 40) as usize;
            let lits: Vec<u8> = (0..literals).map(|_| (rng() % 7) as u8).collect();
            let offset = (1 + rng() % 30) as usize;
            let length = MIN_MATCH + (rng() % 300) as usize;
            if offset > out.len() + literals {
                continue;
            }
            out.extend_from_slice(&lits);
            let from = out.len() - offset;
            for i in 0..length {
                out.push(out[from + i]);
            }
            let (l, m) = (literals.min(15), (length - MIN_MATCH).min(15));
            block.push((l << 4 | m) as u8);
            if literals >= 15 {
                extend(&mut block, literals - 15);
            }
            block.extend_from_slice(&lits);
            block.extend_from_slice(&(offset as u16).to_le_bytes());
            if length - MIN_MATCH >= 15 {
                extend(&mut block, length - MIN_MATCH - 15);
            }
        }
        let last = (rng() % 30) as usize;
        block.push((last.min(15) << 4) as u8);
        if last >= 15 {
            extend(&mut block, last - 15);
        }
        block.extend((0..last).map(|_| (rng() % 7) as u8));
        block
    }

    fn extend(block: &mut Vec<u8>, mut rest: usize) {
        while rest >= 255 {
            block.push(255);
            rest -= 255;
        }
        block.push(rest as u8);
    }

    fn rng(seed: u64) -> impl FnMut() -> u64 {
        let mut state = seed;
        move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        }
    }

    /// C's verdict on a block that must fill `capacity` bytes exactly.
    fn reference(block: &[u8], capacity: usize) -> Option<Vec<u8>> {
        openzl_ffi::lz::lz4_decompress_safe(block, capacity).filter(|out| out.len() == capacity)
    }

    #[test]
    fn literal_only_and_empty_blocks() {
        assert_eq!(run(&[0], &[0], 16).unwrap(), b"");
        assert_eq!(run(&[0], &[], 16), Err(corrupt()));
        assert_eq!(run(&[0], &[1], 16), Err(corrupt()));
        assert_eq!(run(&[0], &[0, 0], 16), Err(corrupt()));
        let mut block = vec![0x70];
        block.extend_from_slice(b"1234567");
        assert_eq!(run(&[7], &block, 16).unwrap(), b"1234567");
        // A block that stops short of the declared size.
        assert_eq!(run(&[9], &block, 16), Err(corrupt()));
        // A block that outgrows it.
        assert_eq!(run(&[6], &block, 16), Err(corrupt()));
    }

    #[test]
    fn matches_repeat_overlapping_patterns() {
        // "ab" then a 18-byte match at offset 2, then 6 trailing literals.
        let mut block = vec![0x2e, b'a', b'b', 2, 0];
        block.push(0x60);
        block.extend_from_slice(b"abcdef");
        // 14 + 4 = 18 matched bytes.
        let mut expected = b"ab".to_vec();
        expected.extend(b"ab".iter().cycle().take(18));
        expected.extend_from_slice(b"abcdef");
        assert_eq!(run(&[26], &block, 64).unwrap(), expected);
        assert_eq!(reference(&block, 26).unwrap(), expected);
        // A match reaching just into its own output.
        let mut block = vec![0xc6];
        block.extend_from_slice(b"0123456789ab");
        block.extend_from_slice(&[8, 0]);
        block.push(0x60);
        block.extend_from_slice(b"012345");
        let expected = b"0123456789ab456789ab45012345";
        assert_eq!(run(&[28], &block, 64).unwrap(), expected);
        assert_eq!(reference(&block, 28).unwrap(), expected);
    }

    #[test]
    fn lengths_extend_through_255_bytes() {
        let literals = 15 + 255 + 255 + 3;
        let mut block = vec![0xf0];
        extend(&mut block, literals - 15);
        block.extend(core::iter::repeat_n(9u8, literals));
        assert_eq!(run(&[0x90, 0x04], &block, 4096).unwrap(), vec![9; literals]);
        assert_eq!(reference(&block, literals).unwrap(), vec![9; literals]);
        // A truncated extension.
        assert_eq!(run(&[0x90, 0x04], &[0xf0, 255, 255], 4096), Err(corrupt()));
        assert_eq!(run(&[0x90, 0x04], &[0xf0], 4096), Err(corrupt()));
    }

    #[test]
    fn malformed_blocks_are_rejected() {
        let tail = |mut block: Vec<u8>| {
            block.push(0x50);
            block.extend_from_slice(b"12345");
            block
        };
        // Offset beyond the output so far.
        let block = tail(vec![0x10, b'x', 2, 0]);
        assert_eq!(run(&[16], &block, 64), Err(corrupt()));
        // Offset zero.
        let block = tail(vec![0x10, b'x', 0, 0]);
        assert_eq!(run(&[16], &block, 64), Err(corrupt()));
        // Missing offset bytes.
        assert_eq!(run(&[16], &[0x10, b'x', 1], 64), Err(corrupt()));
        // A match running into the last five bytes: valid for 25 bytes,
        // whose last five are literals, but not for 24.
        let mut block = vec![0x1f, b'x', 1, 0, 0];
        block.push(0x50);
        block.extend_from_slice(b"12345");
        assert_eq!(run(&[25], &block, 64).unwrap().len(), 25);
        assert!(reference(&block, 25).is_some());
        assert_eq!(run(&[24], &block, 64), Err(corrupt()));
        assert!(reference(&block, 24).is_none());
        // Trailing bytes after the final literals.
        assert_eq!(run(&[5], &[0x50, 1, 2, 3, 4, 5, 6], 64), Err(corrupt()));
        // Final literals past the declared output.
        assert_eq!(run(&[2], &[0x50, 1, 2, 3, 4, 5], 64), Err(corrupt()));
        // A match length extension that never ends.
        assert_eq!(
            run(&[40], &[0x1f, b'x', 1, 0, 255, 255, 255], 64),
            Err(corrupt())
        );
        // An input with no final literal run.
        assert_eq!(run(&[8], &[0x10, b'x', 1, 0], 64), Err(corrupt()));
    }

    #[test]
    fn headers_and_sizes_are_checked() {
        let block = [0x30, 1, 2, 3];
        assert_eq!(run(&[3], &block, 64).unwrap(), [1, 2, 3]);
        // No header, a truncated varint, an overlong one.
        assert_eq!(run(&[], &block, 64), Err(corrupt()));
        assert_eq!(run(&[0x80], &block, 64), Err(corrupt()));
        assert_eq!(run(&[0xff; 10], &block, 64), Err(corrupt()));
        // Bytes after the size are ignored, as in the reference.
        assert_eq!(run(&[3, 99], &block, 64).unwrap(), [1, 2, 3]);
        // A size beyond `INT_MAX`, and beyond the budget.
        assert_eq!(
            run(&[0x80, 0x80, 0x80, 0x80, 0x08], &block, usize::MAX),
            Err(corrupt())
        );
        assert_eq!(run(&[200, 1], &block, 64), Err(corrupt()));
    }

    #[test]
    fn blocks_are_judged_like_the_reference() {
        let rounds = if cfg!(miri) { 20 } else { 20_000 };
        let mut next = rng(0x9e37_79b9_7f4a_7c15);
        let mut accepted = 0;
        for round in 0..rounds {
            let mut block = if round % 4 == 0 {
                (0..next() % 40).map(|_| next() as u8).collect()
            } else {
                generate(&mut next)
            };
            for _ in 0..next() % 3 {
                if !block.is_empty() {
                    let at = next() as usize % block.len();
                    block[at] = next() as u8;
                }
            }
            // Try the exact size C produces and a few off by one.
            let reference_size = (0..400).find(|&n| reference(&block, n).is_some());
            let sizes = match reference_size {
                Some(n) => vec![n, n + 1, n.saturating_sub(1)],
                None => vec![(next() % 300) as usize],
            };
            let zero_offset = has_zero_offset(&block);
            for capacity in sizes {
                let theirs = reference(&block, capacity);
                let mut out = vec![0u8; capacity];
                let ours = super::block(&block, &mut out)
                    .ok()
                    .filter(|&n| n == capacity);
                match (theirs, ours) {
                    (Some(theirs), Some(_)) => {
                        accepted += 1;
                        assert_eq!(theirs, out, "{block:02x?}");
                    }
                    (None, None) => {}
                    // Offset 0 is the one deliberate difference.
                    (Some(_), None) => {
                        assert!(zero_offset, "{block:02x?} / {capacity}")
                    }
                    (None, Some(_)) => panic!("accepted {block:02x?} / {capacity}"),
                }
            }
        }
        assert!(accepted > rounds / 20, "{accepted}");
    }

    /// Whether some sequence of `block` has offset zero (a loose scan: any
    /// two zero bytes after a token).
    fn has_zero_offset(block: &[u8]) -> bool {
        block.windows(2).any(|pair| pair == [0, 0])
    }
}
