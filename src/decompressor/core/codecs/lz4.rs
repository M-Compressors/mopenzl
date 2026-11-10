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
/// Input a fast-path sequence may touch: a token, an extension byte, 269
/// literals read as whole 16-byte chunks, an offset and an extension byte.
const IN_WINDOW: usize = 288;
/// Output a fast-path sequence may touch: 269 literals and a match of 273
/// bytes written as whole chunks, with the end-of-block rules to spare.
const OUT_WINDOW: usize = 640;
/// The longest run copied by chunks rather than by `memmove`.
const WILD_LIMIT: usize = 64;
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
    // The bytes read are the 255s and the byte after them; the cursor,
    // which only grows, may not pass `limit` after the last one.
    let rest = src.get(*ip..).ok_or_else(corrupt)?;
    let run = rest
        .iter()
        .position(|&byte| byte != 255)
        .ok_or_else(corrupt)?;
    *ip += run + 1;
    if *ip as isize > limit {
        return Err(corrupt());
    }
    Ok(run * 255 + usize::from(rest[run]))
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
        (ip, op) = fast_sequences(src, dst, ip, op)?;
        (ip, op) = far_sequences(src, dst, ip, op)?;
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
        copy_literals(src, ip, dst, op, length);
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

/// The longest offset a match can have (the two offset bytes are a `u16`).
const MAX_OFFSET: usize = 65535;
/// Output a far sequence may touch: the longest offset back from the
/// position, then the sequence window.
const FAR_WINDOW: usize = MAX_OFFSET + OUT_WINDOW;

/// Decodes sequences like `fast_sequences`, once at least `MAX_OFFSET`
/// bytes of output exist, so that every offset is valid and every byte a
/// sequence reads or writes lies in one fixed-size window of the output:
/// `MAX_OFFSET` bytes before the position and `OUT_WINDOW` from it. A short
/// match is then three copies at constant lengths and computed positions
/// whose bounds the compiler proves, and the surplus of whole-chunk copies is
/// overwritten by what follows.
#[inline(never)]
fn far_sequences(
    src: &[u8],
    dst: &mut [u8],
    ip: usize,
    mut op: usize,
) -> Result<(usize, usize), DecodeError> {
    let Some(last_op) = dst.len().checked_sub(OUT_WINDOW) else {
        return Ok((ip, op));
    };
    let mut rest = src.get(ip..).unwrap_or_default();
    while op >= MAX_OFFSET
        && op <= last_op
        && let Some(window) = rest.first_chunk::<IN_WINDOW>()
        && let Some(span) = dst[op - MAX_OFFSET..].first_chunk_mut::<FAR_WINDOW>()
    {
        let token = usize::from(window[0]);
        let mut literals = token >> 4;
        let mut at = 1;
        if literals == FIELD_MAX {
            let extra = usize::from(window[1]);
            if extra == 255 {
                break;
            }
            literals += extra;
            at = 2;
        }
        let tail = at + literals;
        let offset = usize::from(u16::from_le_bytes([window[tail], window[tail + 1]]));
        let mut length = (token & FIELD_MAX) + MIN_MATCH;
        let mut next = tail + 2;
        if length == FIELD_MAX + MIN_MATCH {
            let extra = usize::from(window[next]);
            if extra == 255 {
                break;
            }
            length += extra;
            next += 1;
        }
        if offset == 0 {
            return Err(corrupt());
        }
        // The literals are copied in whole chunks; the surplus is
        // overwritten by what follows.
        let to = MAX_OFFSET;
        if literals <= 16 {
            span[to..to + 16].copy_from_slice(&window[at..at + 16]);
        } else {
            let rounded = (literals + 15) & !15;
            let (to, _) = span[to..to + rounded].as_chunks_mut::<16>();
            let (from, _) = window[at..at + rounded].as_chunks::<16>();
            for (to, from) in to.iter_mut().zip(from) {
                *to = *from;
            }
        }
        rest = &rest[next..];
        let target = to + literals;
        let source = target - offset;
        if length <= 18 && offset >= 8 {
            // As the reference: 8 + 8 + 2 bytes in order, which is right
            // for any offset of 8 or more.
            span.copy_within(source..source + 8, target);
            span.copy_within(source + 8..source + 16, target + 8);
            span.copy_within(source + 16..source + 18, target + 16);
        } else if offset >= 32 {
            chunked::<32>(span, source, target, length);
        } else if offset >= 16 {
            chunked::<16>(span, source, target, length);
        } else if offset >= 8 {
            chunked::<8>(span, source, target, length);
        } else {
            copy_long_match(dst, op + literals, offset, length);
        }
        op += literals + length;
    }
    Ok((src.len() - rest.len(), op))
}

/// Copies `length` bytes from `source` to `target` in `N`-byte chunks, in
/// order; the caller guarantees `target - source >= N`, so each chunk reads
/// only bytes that are already final and the chunks reproduce an overlapping
/// copy. The surplus of the last chunk is overwritten by what follows.
#[inline(always)]
fn chunked<const N: usize>(
    span: &mut [u8; FAR_WINDOW],
    source: usize,
    target: usize,
    length: usize,
) {
    let mut done = 0;
    while done < length {
        span.copy_within(source + done..source + done + N, target + done);
        done += N;
    }
}

/// Decodes sequences far from both ends of the block with whole-chunk
/// copies, and returns the position of the first sequence it leaves to the
/// general path: one with a length extension of more than one byte, or too
/// close to an end. A literal run or a match may carry a single extension
/// byte (below 255), so a run is at most 269 bytes and a match 273.
///
/// The margins prove every check of the general path (the end-of-block rules
/// and the extension limits), and an offset the reference could not resolve
/// is the same error. A sequence that fails a margin is left untouched.
fn fast_sequences(
    src: &[u8],
    dst: &mut [u8],
    ip: usize,
    mut op: usize,
) -> Result<(usize, usize), DecodeError> {
    // The unread input is a shrinking slice rather than a position, which
    // keeps one value less live across the loop-carried dependency chain
    // (the next sequence starts where this one ends). A sequence with a
    // whole window of input and output left is a fast one.
    let Some(last_op) = dst.len().checked_sub(OUT_WINDOW) else {
        return Ok((ip, op));
    };
    let mut rest = src.get(ip..).unwrap_or_default();
    while op <= last_op
        && op < MAX_OFFSET
        && let Some(window) = rest.first_chunk::<IN_WINDOW>()
    {
        let (head, room) = dst.split_at_mut(op);
        let Some(room) = room.first_chunk_mut::<OUT_WINDOW>() else {
            break;
        };
        let token = usize::from(window[0]);
        let mut literals = token >> 4;
        let mut at = 1;
        if literals == FIELD_MAX {
            let extra = usize::from(window[1]);
            if extra == 255 {
                break;
            }
            literals += extra;
            at = 2;
        }
        // The literals are copied in whole chunks; the surplus is
        // overwritten by what follows.
        if literals <= 16 {
            room[..16].copy_from_slice(&window[at..at + 16]);
        } else {
            let rounded = (literals + 15) & !15;
            let (to, _) = room[..rounded].as_chunks_mut::<16>();
            let (from, _) = window[at..at + rounded].as_chunks::<16>();
            for (to, from) in to.iter_mut().zip(from) {
                *to = *from;
            }
        }
        let tail = at + literals;
        let offset = usize::from(u16::from_le_bytes([window[tail], window[tail + 1]]));
        let mut length = (token & FIELD_MAX) + MIN_MATCH;
        let mut next = tail + 2;
        if length == FIELD_MAX + MIN_MATCH {
            let extra = usize::from(window[next]);
            if extra == 255 {
                break;
            }
            length += extra;
            next += 1;
        }
        // The next sequence starts here; advancing before the copies keeps
        // the loop-carried chain (token, lengths, position) free of them.
        rest = &rest[next..];
        let written = op + literals;
        if offset == 0 || offset > written {
            return Err(corrupt());
        }
        let from = written - offset;
        // A match of at most 16 bytes whose source lies wholly before the
        // literals is one 16-byte copy, the surplus overwritten later.
        // (`head` ends where the output so far ends, so a whole chunk at
        // `from` also means the chunk ends before the literals.)
        if length <= 16
            && let Some(source) = head.get(from..).and_then(<[u8]>::first_chunk::<16>)
        {
            room[literals..literals + 16].copy_from_slice(source);
        } else {
            copy_long_match(dst, written, offset, length);
        }
        op = written + length;
    }
    Ok((src.len() - rest.len(), op))
}

/// Copies the match of the fast loop that is not one 16-byte copy: `length`
/// bytes from `offset` bytes back to `dst[written..]`, in whole chunks whose
/// surplus the next sequence overwrites. The caller guarantees `offset` is
/// in `1..=written` and that `written` has `OUT_WINDOW - 269` bytes of room
/// in `dst`. Kept out of line so the fast loop's common case keeps its
/// registers.
#[inline(never)]
fn copy_long_match(dst: &mut [u8], written: usize, offset: usize, length: usize) {
    let from = written - offset;
    let (head, room) = dst.split_at_mut(written);
    // A match whose first 32 source bytes lie wholly before `written` starts
    // as one 32-byte copy.
    if let Some(source) = head.get(from..).and_then(<[u8]>::first_chunk::<32>)
        && let Some(target) = room.first_chunk_mut::<32>()
    {
        *target = *source;
        if length > 32 {
            let rounded = (length + 31) & !31;
            if offset >= rounded
                && let Some(source) = head.get(from + 32..from + rounded)
                && let Some(target) = room.get_mut(32..rounded)
            {
                // The source lies wholly before `written`: a run of 32-byte
                // chunks that overlap nothing they read.
                let (to, _) = target.as_chunks_mut::<32>();
                let (from, _) = source.as_chunks::<32>();
                for (to, from) in to.iter_mut().zip(from) {
                    *to = *from;
                }
            } else {
                // Each 16-byte chunk reads only bytes that are already
                // final, so the order of the chunks reproduces an
                // overlapping copy.
                let region = &mut dst[from..written + rounded];
                let mut done = 32;
                while done < rounded {
                    region.copy_within(done..done + 16, offset + done);
                    done += 16;
                }
            }
        }
    } else if offset >= 16 {
        let rounded = (length + 15) & !15;
        let region = &mut dst[from..written + rounded];
        let mut done = 0;
        while done < rounded {
            region.copy_within(done..done + 16, offset + done);
            done += 16;
        }
    } else {
        copy_match(dst, written, offset, length);
    }
}

/// Copies the literal run `src[ip..ip + length]` to `dst[op..]`; the caller
/// has checked that both ranges exist.
///
/// A short run is copied in whole 16-byte chunks when both buffers have room
/// for the surplus, which the following sequence overwrites.
fn copy_literals(src: &[u8], ip: usize, dst: &mut [u8], op: usize, length: usize) {
    if length <= WILD_LIMIT && ip + length + 16 <= src.len() && op + length + 16 <= dst.len() {
        let from = &src[ip..ip + length + 16];
        let to = &mut dst[op..op + length + 16];
        let mut done = 0;
        while done < length {
            if let (Some(chunk), Some(target)) = (
                from[done..].first_chunk::<16>(),
                to[done..].first_chunk_mut::<16>(),
            ) {
                *target = *chunk;
            }
            done += 16;
        }
    } else {
        dst[op..op + length].copy_from_slice(&src[ip..ip + length]);
    }
}

/// Copies the `N` bytes at `from` to `to` within `buf`, loading them before
/// storing, so that the ranges may overlap. Out-of-range positions copy
/// nothing; the callers have sized `buf` for them.
#[inline(always)]
fn copy_chunk<const N: usize>(buf: &mut [u8], from: usize, to: usize) {
    if let Some(chunk) = buf[from..].first_chunk::<N>().copied()
        && let Some(target) = buf[to..].first_chunk_mut::<N>()
    {
        *target = chunk;
    }
}

/// Copies `length` bytes from `offset` bytes back, which may overlap the
/// destination (a short offset repeats a pattern).
fn copy_match(dst: &mut [u8], op: usize, offset: usize, length: usize) {
    let from = op - offset;
    if offset >= length && length > WILD_LIMIT {
        dst.copy_within(from..from + length, op);
    } else if offset >= 16 && op + length + 16 <= dst.len() {
        // Each 16-byte chunk reads only bytes that are already final, so the
        // order of the chunks reproduces an overlapping copy.
        let region = &mut dst[from..][..offset + length + 16];
        let mut done = 0;
        while done < length {
            copy_chunk::<16>(region, done, offset + done);
            done += 16;
        }
    } else if offset >= 8 && op + length + 8 <= dst.len() {
        let region = &mut dst[from..][..offset + length + 8];
        let mut done = 0;
        while done < length {
            copy_chunk::<8>(region, done, offset + done);
            done += 8;
        }
    } else if offset == 1 {
        let byte = dst[from];
        dst[op..op + length].fill(byte);
    } else if length <= WILD_LIMIT {
        for at in op..op + length {
            dst[at] = dst[at - offset];
        }
    } else {
        // The pattern `dst[from..op]` repeats: each pass copies everything
        // written so far, a whole number of patterns, so it never reads a
        // byte it has not written.
        let mut done = 0;
        while done < length {
            let chunk = (offset + done).min(length - done);
            dst.copy_within(from..from + chunk, op + done);
            done += chunk;
        }
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

    /// A short offset right at the start of a block, where fewer than eight
    /// bytes precede the match, still decodes inside the fast-path margins.
    #[test]
    fn short_matches_at_the_start_of_a_block_decode() {
        let mut block = vec![0x3e, b'a', b'b', b'c', 2, 0, 0xf0, 45];
        block.extend(core::iter::repeat_n(b'x', 60));
        let mut expected = b"abc".to_vec();
        for _ in 0..18 {
            expected.push(expected[expected.len() - 2]);
        }
        expected.extend(core::iter::repeat_n(b'x', 60));
        assert_eq!(run(&[expected.len() as u8], &block, 128).unwrap(), expected);
        assert_eq!(reference(&block, expected.len()).unwrap(), expected);
    }

    /// Encoder output over periodic data of many periods: every offset class
    /// of the fast path (below, at and above a chunk) decodes like C.
    #[test]
    fn encoder_blocks_decode_like_the_reference() {
        let mut next = rng(0x1234_5678_9abc_def1);
        let size = if cfg!(miri) { 600 } else { 6000 };
        for period in [1usize, 2, 3, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 100, 1000] {
            let pattern: Vec<u8> = (0..period).map(|_| next() as u8).collect();
            let mut data: Vec<u8> = pattern.iter().copied().cycle().take(size).collect();
            for _ in 0..size / 50 {
                let at = next() as usize % size;
                data[at] = next() as u8;
            }
            for level in [1, 6, 10] {
                let Some(block) =
                    crate::compressor::core::codecs::lz4::compress_block(&data, level)
                else {
                    panic!("block too large")
                };
                let mut out = vec![0u8; data.len()];
                assert_eq!(super::block(&block, &mut out), Ok(data.len()));
                assert_eq!(out, data, "period {period} level {level}");
                assert_eq!(reference(&block, data.len()).unwrap(), data);
            }
        }
    }

    /// Encoder output over data with short and long literal runs, short and
    /// long matches at near and far offsets: every sequence shape of the fast
    /// loop decodes like C, and a block cut anywhere is judged like C.
    #[test]
    fn mixed_encoder_blocks_decode_like_the_reference() {
        let mut next = rng(0x0bad_cafe_f00d_1234);
        let size = if cfg!(miri) { 3000 } else { 120_000 };
        let mut data: Vec<u8> = Vec::with_capacity(size);
        while data.len() < size {
            match next() % 5 {
                // Fresh bytes: literal runs of every length.
                0 => data.extend((0..next() % 400).map(|_| next() as u8)),
                // A near repeat, possibly overlapping itself.
                1 if data.len() > 40 => {
                    let back = 1 + next() as usize % 40;
                    let run = 4 + next() as usize % 300;
                    for _ in 0..run {
                        data.push(data[data.len() - back]);
                    }
                }
                // A far repeat.
                _ if data.len() > 70_000 => {
                    let from = next() as usize % (data.len() - 300);
                    let run = 4 + next() as usize % 290;
                    data.extend_from_within(from..from + run);
                }
                _ => data.extend((0..next() % 30).map(|_| (next() % 4) as u8)),
            }
        }
        data.truncate(size);
        for level in [1, 6, 9] {
            let Some(block) = crate::compressor::core::codecs::lz4::compress_block(&data, level)
            else {
                panic!("block too large")
            };
            let mut out = vec![0u8; data.len()];
            assert_eq!(super::block(&block, &mut out), Ok(data.len()));
            assert_eq!(out, data, "level {level}");
            // Truncations and a corrupted byte agree with C.
            for cut in [block.len() - 1, block.len() / 2, block.len() / 3] {
                let mut out = vec![0u8; data.len()];
                let ours =
                    matches!(super::block(&block[..cut], &mut out), Ok(n) if n == data.len());
                assert_eq!(
                    ours,
                    reference(&block[..cut], data.len()).is_some(),
                    "level {level} cut {cut}"
                );
            }
        }
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

    /// A valid block of sequences with literal runs, offsets and match
    /// lengths from every class of the copy paths, and the data it decodes
    /// to. The output reaches past the margins of the fast paths.
    fn generate_large(rng: &mut impl FnMut() -> u64) -> (Vec<u8>, Vec<u8>) {
        const LENGTHS: [usize; 12] = [4, 9, 18, 19, 40, 64, 65, 66, 100, 273, 274, 700];
        let mut data: Vec<u8> = Vec::new();
        let mut block = Vec::new();
        let mut block_data_len = 0;
        let sequences = 4 + rng() % 40;
        for _ in 0..sequences {
            let literals = match rng() % 6 {
                0 => 15 + (rng() % 300) as usize,
                1 => 0,
                _ => (rng() % 15) as usize,
            };
            let lits: Vec<u8> = (0..literals).map(|_| (rng() % 5) as u8).collect();
            let available = data.len() + literals;
            let offset = match rng() % 6 {
                0 => 1,
                1 => 2 + (rng() % 6) as usize,
                2 => 8 + (rng() % 8) as usize,
                3 => 16 + (rng() % 40) as usize,
                4 => 1 + (rng() % 700) as usize,
                _ => 1 + (rng() % 5000) as usize,
            };
            if offset > available || (data.is_empty() && literals == 0) {
                continue;
            }
            let length = LENGTHS[(rng() % LENGTHS.len() as u64) as usize] + (rng() % 3) as usize;
            data.extend_from_slice(&lits);
            block_data_len = data.len();
            let from = data.len() - offset;
            for at in 0..length {
                data.push(data[from + at]);
            }
            block.push((literals.min(15) << 4 | (length - MIN_MATCH).min(15)) as u8);
            if literals >= 15 {
                extend(&mut block, literals - 15);
            }
            block.extend_from_slice(&lits);
            block.extend_from_slice(&(offset as u16).to_le_bytes());
            if length - MIN_MATCH >= 15 {
                extend(&mut block, length - MIN_MATCH - 15);
            }
        }
        // Few final literals put the last match against the end of the output.
        // The last match starts at least `MF_LIMIT` bytes before the end.
        let last_match = data.len() - block_data_len;
        let last = [5usize, 6, 8, 12, 40, 400][(rng() % 6) as usize]
            .max(MF_LIMIT.saturating_sub(last_match));
        block.push((last.min(15) << 4) as u8);
        if last >= 15 {
            extend(&mut block, last - 15);
        }
        let tail: Vec<u8> = (0..last).map(|_| (rng() % 5) as u8).collect();
        block.extend_from_slice(&tail);
        data.extend_from_slice(&tail);
        (block, data)
    }

    /// Blocks of every copy class decode to their data and like C, whole or
    /// with a corrupted byte.
    #[test]
    fn large_blocks_decode_like_the_reference() {
        let rounds = if cfg!(miri) { 12 } else { 3000 };
        let mut next = rng(0xdead_beef_0123_4567);
        for round in 0..rounds {
            let (mut block, data) = generate_large(&mut next);
            let mut out = vec![0u8; data.len()];
            assert_eq!(super::block(&block, &mut out), Ok(data.len()), "{round}");
            assert_eq!(out, data, "round {round}");
            assert_eq!(reference(&block, data.len()).as_deref(), Some(&data[..]));
            // A corrupted copy is judged as C does (offset zero apart).
            let at = next() as usize % block.len();
            block[at] = next() as u8;
            let theirs = reference(&block, data.len());
            let mut out = vec![0u8; data.len()];
            let ours = super::block(&block, &mut out)
                .ok()
                .filter(|&n| n == data.len());
            match (theirs, ours) {
                (Some(theirs), Some(_)) => assert_eq!(theirs, out, "round {round}"),
                (None, None) => {}
                (Some(_), None) => assert!(has_zero_offset(&block), "round {round}"),
                (None, Some(_)) => panic!("accepted round {round}"),
            }
        }
    }

    /// A block of sequences over a long first literal run, so that most of
    /// them start beyond `MAX_OFFSET` bytes of output and take the far path;
    /// the decoded bytes come with it. Offsets and lengths hit every copy
    /// class of the far loop.
    fn generate_far(rng: &mut impl FnMut() -> u64, sequences: usize) -> (Vec<u8>, Vec<u8>) {
        let mut out: Vec<u8> = Vec::new();
        let mut block = Vec::new();
        let mut sequence = |out: &mut Vec<u8>, literals: usize, matched: Option<(usize, usize)>| {
            let lits: Vec<u8> = (0..literals).map(|i| (i % 251) as u8 ^ 0x5a).collect();
            let (l, m) = match matched {
                Some((_, length)) => (literals.min(15), (length - MIN_MATCH).min(15)),
                None => (literals.min(15), 0),
            };
            block.push((l << 4 | m) as u8);
            if literals >= 15 {
                extend(&mut block, literals - 15);
            }
            block.extend_from_slice(&lits);
            out.extend_from_slice(&lits);
            if let Some((offset, length)) = matched {
                block.extend_from_slice(&(offset as u16).to_le_bytes());
                if length - MIN_MATCH >= 15 {
                    extend(&mut block, length - MIN_MATCH - 15);
                }
                let from = out.len() - offset;
                for i in 0..length {
                    out.push(out[from + i]);
                }
            }
        };
        let first = MAX_OFFSET + 1 + (rng() % 700) as usize;
        sequence(
            &mut out,
            first,
            Some((1 + (rng() % 40) as usize, 4 + (rng() % 20) as usize)),
        );
        for _ in 0..sequences {
            let literals = match rng() % 4 {
                0 => 0,
                1 => (rng() % 15) as usize,
                2 => 15 + (rng() % 255) as usize,
                _ => 270 + (rng() % 300) as usize,
            };
            let offset = match rng() % 7 {
                0 => 1 + (rng() % 7) as usize,
                1 => 8 + (rng() % 8) as usize,
                2 => 16 + (rng() % 16) as usize,
                3 => 32 + (rng() % 40) as usize,
                4 => 1 + (rng() % 2000) as usize,
                5 => MAX_OFFSET - (rng() % 3) as usize,
                _ => 1 + (rng() % MAX_OFFSET as u64) as usize,
            }
            .min(out.len() + literals);
            let length = match rng() % 5 {
                0 => 4 + (rng() % 4) as usize,
                1 => 8 + (rng() % 11) as usize,
                2 => 19 + (rng() % 40) as usize,
                3 => 59 + (rng() % 200) as usize,
                _ => 259 + (rng() % 600) as usize,
            };
            sequence(&mut out, literals, Some((offset, length)));
        }
        // The last sequence is literals only.
        let last = 12 + (rng() % 40) as usize;
        block.push((last.min(15) << 4) as u8);
        extend_if(&mut block, last);
        let tail: Vec<u8> = (0..last).map(|i| i as u8).collect();
        block.extend_from_slice(&tail);
        out.extend_from_slice(&tail);
        (block, out)
    }

    fn extend_if(block: &mut Vec<u8>, literals: usize) {
        if literals >= 15 {
            extend(block, literals - 15);
        }
    }

    #[test]
    fn far_blocks_decode_like_the_reference() {
        let mut next = rng(0x1234_5678_9abc_def1);
        for _ in 0..if cfg!(miri) { 1 } else { 12 } {
            let (block, expected) = generate_far(&mut next, if cfg!(miri) { 8 } else { 400 });
            assert_eq!(reference(&block, expected.len()).unwrap(), expected);
            let mut out = vec![0u8; expected.len()];
            assert_eq!(super::block(&block, &mut out).unwrap(), expected.len());
            assert_eq!(out, expected);
            // Off by one either way is rejected by both.
            for capacity in [expected.len() - 1, expected.len() + 1] {
                let theirs = reference(&block, capacity);
                let mut out = vec![0u8; capacity];
                let ours = super::block(&block, &mut out)
                    .ok()
                    .filter(|&n| n == capacity);
                assert_eq!(theirs.is_some(), ours.is_some());
            }
        }
    }

    #[test]
    fn mutated_far_blocks_are_judged_like_the_reference() {
        let mut next = rng(0x0bad_cafe_f00d_1234);
        let rounds = if cfg!(miri) { 2 } else { 300 };
        let (block, expected) = generate_far(&mut next, if cfg!(miri) { 6 } else { 60 });
        for _ in 0..rounds {
            let mut block = block.clone();
            for _ in 0..1 + next() % 3 {
                let at = next() as usize % block.len();
                block[at] = next() as u8;
            }
            if next().is_multiple_of(4) {
                block.truncate(next() as usize % block.len());
            }
            let capacity = expected.len();
            let theirs = reference(&block, capacity);
            let mut out = vec![0u8; capacity];
            let ours = super::block(&block, &mut out)
                .ok()
                .filter(|&n| n == capacity);
            match (theirs, ours) {
                (Some(theirs), Some(_)) => assert_eq!(theirs, out),
                (None, None) => {}
                (Some(_), None) => assert!(has_zero_offset(&block)),
                (None, Some(_)) => panic!("accepted a block the reference rejects"),
            }
        }
    }

    /// Whether some sequence of `block` has offset zero (a loose scan: any
    /// two zero bytes after a token).
    fn has_zero_offset(block: &[u8]) -> bool {
        block.windows(2).any(|pair| pair == [0, 0])
    }
}
