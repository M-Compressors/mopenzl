//! OpenZL's multi-state FSE (`fse/decompress/fse_decompress.c`,
//! `fse/common/entropy_common.c`).
//!
//! The table build and the state machine are Zstandard's, but the stream
//! layout differs: two or four interleaved states read one backward stream
//! (see [`decode`] for the tail), and the normalised counts travel in their
//! own stream.

use alloc::boxed::Box;
use alloc::vec::Vec;

use fearless_simd::{Level, dispatch};

use super::bits::{Reverse, refill};
use crate::decompressor::core::data::corrupt;
use crate::decompressor::error::DecodeError;

/// `FSE_MIN_TABLELOG`.
const MIN_LOG: u32 = 5;
/// `FSE_MAX_TABLELOG`: the largest table the binding builds.
const MAX_LOG: u32 = 12;
/// `FSE_TABLELOG_ABSOLUTE_MAX`: the largest log a count header may name.
const ABSOLUTE_MAX_LOG: u32 = 15;
/// The cells of the largest table: a constant so that indexing with a masked
/// state needs no bounds check.
const CELLS: usize = 1 << MAX_LOG;
/// `FSE_MAX_SYMBOL_VALUE + 1`.
pub(super) const MAX_SYMBOLS: usize = 256;

/// One decoding cell (`FSE_decode_t`).
#[derive(Copy, Clone, Debug, Default)]
struct Cell {
    symbol: u8,
    bits: u8,
    next: u16,
}

/// A decoding table (`FSE_DTable`).
#[derive(Debug)]
pub(super) struct Table {
    log: u32,
    /// Whether every cell reads at least one bit, i.e. no symbol owns more
    /// than half of the table (the reference's `FSE_decompress_usingDTable_fast`
    /// condition).
    fast: bool,
    cells: Box<[Cell; CELLS]>,
}

/// The index of the highest set bit (`ZSTD_highbit32`); `value` is not 0.
const fn high_bit(value: u32) -> u32 {
    31 - value.leading_zeros()
}

impl Table {
    /// Builds the table of normalised counts `counts` as the binding does
    /// (`buildFSEDTable` + `FSE_buildDTable`): at least two and at most 256
    /// symbols, counts of -1 (one cell) or more that add up to a power of
    /// two between 32 and 4096.
    ///
    /// # Errors
    ///
    /// A corrupted stream for counts that break those rules.
    pub(super) fn new(counts: &[i16]) -> Result<Self, DecodeError> {
        if counts.len() < 2 || counts.len() > MAX_SYMBOLS {
            return Err(corrupt());
        }
        let mut sum = 0u32;
        let mut largest = 0u32;
        for &count in counts {
            if count < -1 {
                return Err(corrupt());
            }
            sum += if count == -1 { 1 } else { count as u32 };
            largest = largest.max(count.max(0) as u32);
        }
        if sum == 0 || !sum.is_power_of_two() {
            return Err(corrupt());
        }
        let log = high_bit(sum);
        if !(MIN_LOG..=MAX_LOG).contains(&log) {
            return Err(corrupt());
        }
        let size = 1usize << log;
        let mask = size - 1;
        let step = (size >> 1) + (size >> 3) + 3;
        let mut cells: Box<[Cell; CELLS]> = crate::shared::fixed::fixed_table(Cell::default());
        let mut next = [0u32; MAX_SYMBOLS];
        // Symbols of probability below one cell go to the top of the table.
        let mut high = size;
        for (symbol, &count) in counts.iter().enumerate() {
            if count == -1 {
                high -= 1;
                cells[high].symbol = symbol as u8;
                next[symbol] = 1;
            } else {
                next[symbol] = count as u32;
            }
        }
        // Spread the others with the golden step, skipping the top.
        let mut position = 0usize;
        if high == size {
            // No symbol of low probability: the symbols laid down in order,
            // each owning a run, are scattered by the step with no cell to
            // skip (`FSE_buildDTable_internal`'s fast spread, which writes
            // the same cells).
            let mut spread = [0u8; CELLS];
            let mut at = 0usize;
            for (symbol, &count) in counts.iter().enumerate() {
                let run = count.max(0) as usize;
                spread[at..at + run].fill(symbol as u8);
                at += run;
            }
            for &symbol in &spread[..size] {
                cells[position].symbol = symbol;
                position = (position + step) & mask;
            }
        } else {
            for (symbol, &count) in counts.iter().enumerate() {
                for _ in 0..count.max(0) {
                    cells[position].symbol = symbol as u8;
                    position = (position + step) & mask;
                    while position >= high {
                        position = (position + step) & mask;
                    }
                }
            }
        }
        if position != 0 {
            return Err(corrupt());
        }
        for cell in &mut cells[..size] {
            let state = next[usize::from(cell.symbol)];
            next[usize::from(cell.symbol)] += 1;
            let bits = log - high_bit(state);
            cell.bits = bits as u8;
            cell.next = ((state << bits) - size as u32) as u16;
        }
        // A symbol with more than half of the cells reaches states past the
        // table, which then read no bits.
        let fast = 2 * largest <= size as u32;
        Ok(Self { log, fast, cells })
    }
}

/// Decodes `count` bytes from `src` with `states` interleaved states
/// (`FSE_decompress_usingDTable`), appending them to `out`.
///
/// `states` is 2 or 4 and `count` at least 2. The states read their initial
/// values in order, then decode round-robin until the stream is exhausted;
/// the tail then drains the states that no longer need bits and finally
/// emits the last symbol of each remaining state without reading.
///
/// While at least 56 bits remain and four symbols are due, one 64-bit
/// refill feeds four symbols (a state reads at most 12 bits per step); the
/// last few symbols take the bit-exact path of [`Reverse`].
///
/// # Errors
///
/// A corrupted stream for a stream that is empty, ends without its mark,
/// runs out of bits early or has bits left over, or does not hold exactly
/// `count` symbols.
pub(super) fn decode(
    level: Level,
    table: &Table,
    src: &[u8],
    states: usize,
    count: usize,
    out: &mut Vec<u8>,
) -> Result<(), DecodeError> {
    // The kernel is inlined into the dispatch target, so it is compiled with
    // the features of `level` (BMI2 shifts on x86-64-v3).
    dispatch!(level, _simd => decode_kernel(table, src, states, count, out))
}

/// [`decode`] under the dispatched level.
#[inline(always)]
fn decode_kernel(
    table: &Table,
    src: &[u8],
    states: usize,
    count: usize,
    out: &mut Vec<u8>,
) -> Result<(), DecodeError> {
    let start = out.len();
    out.resize(start + count, 0);
    let dst = &mut out[start..];
    let mut bits = Reverse::new(src)?;
    let mut state = [0usize; 4];
    for slot in &mut state[..states] {
        *slot = bits.read(table.log) as usize;
    }
    let mut pos = bits.remaining();
    // Four symbols per refill: states 0..4 in turn, or 0, 1, 0, 1.
    let mut written = if !table.fast {
        generic_quads(table, src, dst, &mut state, states, &mut pos)
    } else if states == 4 {
        fast_quads::<4>(table, src, dst, &mut state, &mut pos)
    } else {
        fast_quads::<2>(table, src, dst, &mut state, &mut pos)
    };
    let mut bits = Reverse::at(src, pos);
    let mut index = 0usize;
    // Decode while the stream has bits and the output room.
    while bits.remaining() > 0 && written < count {
        let cell = table.cells[state[index]];
        dst[written] = cell.symbol;
        written += 1;
        state[index] = usize::from(cell.next) + bits.read(u32::from(cell.bits)) as usize;
        index = (index + 1) % states;
    }
    // An over-read is corruption; bits left with the output full mean the
    // output is too small.
    if bits.remaining() != 0 {
        return Err(corrupt());
    }
    // The tail: keep emitting while states need no bits.
    while written < count {
        let cell = table.cells[state[index]];
        dst[written] = cell.symbol;
        written += 1;
        state[index] = usize::from(cell.next);
        index = (index + 1) % states;
        if cell.bits != 0 {
            break;
        }
    }
    // Every other state still holds a symbol to flush.
    if count - written < states - 1 {
        return Err(corrupt());
    }
    for _ in 1..states {
        dst[written] = table.cells[state[index]].symbol;
        written += 1;
        index = (index + 1) % states;
    }
    if written != count {
        return Err(corrupt());
    }
    Ok(())
}

/// Decodes quads of symbols while at least 56 bits remain, for a table where
/// a state may read no bits: a masked read (`rotate` then mask) that is also
/// right for zero bits. Returns the symbols written; `state` and `pos` advance.
#[inline(always)]
fn generic_quads(
    table: &Table,
    src: &[u8],
    dst: &mut [u8],
    state: &mut [usize; 4],
    states: usize,
    pos: &mut i64,
) -> usize {
    let lanes = if states == 4 {
        [0, 1, 2, 3]
    } else {
        [0, 1, 0, 1]
    };
    let mut written = 0usize;
    for out in dst.as_chunks_mut::<4>().0 {
        let Some(mut word) = refill(src, *pos) else {
            break;
        };
        for (symbol, &lane) in out.iter_mut().zip(&lanes) {
            let cell = table.cells[state[lane] & (CELLS - 1)];
            let nb_bits = u32::from(cell.bits);
            let value = word.rotate_left(nb_bits) & ((1u64 << nb_bits) - 1);
            word <<= nb_bits;
            *pos -= i64::from(nb_bits);
            state[lane] = usize::from(cell.next) + value as usize;
            *symbol = cell.symbol;
        }
        written += 4;
    }
    written
}

/// [`generic_quads`] for a table where every state reads at least one bit
/// (`FSE_decompress_usingDTable_fast`), with `STATES` (2 or 4) states kept in
/// locals: the bits of a state are the top `bits` of the word, which a single
/// shift extracts.
#[inline(always)]
fn fast_quads<const STATES: usize>(
    table: &Table,
    src: &[u8],
    dst: &mut [u8],
    state: &mut [usize; 4],
    pos: &mut i64,
) -> usize {
    let cells = &*table.cells;
    let [mut s0, mut s1, mut s2, mut s3] = *state;
    let mut cursor = *pos;
    let mut written = 0usize;
    for out in dst.as_chunks_mut::<4>().0 {
        // At least 56 bits remain, so the offset is not negative.
        if cursor < 56 {
            break;
        }
        let Some(chunk) = src
            .get((cursor >> 3) as usize - 7..)
            .and_then(<[u8]>::first_chunk::<8>)
        else {
            break;
        };
        let mut word = u64::from_le_bytes(*chunk) << (8 - (cursor & 7));
        let mut used = 0u32;
        // One symbol of state `$state`; the last of a quad leaves the word.
        macro_rules! step {
            ($state:ident, $index:expr, $nb:ident => $after:block) => {
                let cell = cells[$state & (CELLS - 1)];
                let $nb = u32::from(cell.bits);
                let low = word.wrapping_shr(64 - $nb);
                used += $nb;
                $state = usize::from(cell.next) + low as usize;
                out[$index] = cell.symbol;
                $after
            };
            ($state:ident, $index:expr) => {
                step!($state, $index, nb => { word = word.wrapping_shl(nb); });
            };
            (last $state:ident, $index:expr) => {
                step!($state, $index, nb => {});
            };
        }
        if STATES == 4 {
            step!(s0, 0);
            step!(s1, 1);
            step!(s2, 2);
            step!(last s3, 3);
        } else {
            step!(s0, 0);
            step!(s1, 1);
            step!(s0, 2);
            step!(last s1, 3);
        }
        cursor -= i64::from(used);
        written += 4;
    }
    *state = [s0, s1, s2, s3];
    *pos = cursor;
    written
}

/// Reads a normalised-count header (`FSE_readNCount`) of up to 256
/// symbols and returns the counts and the header's length in bytes.
///
/// The header is a little-endian bit stream: the table log minus 5 in four
/// bits, then one variable-width count per symbol, where a zero count is
/// followed by a run length of the zero symbols after it.
///
/// # Errors
///
/// A corrupted stream for a log above 15, counts that do not add up to the
/// table, more than 256 symbols, or a header longer than `src`.
pub(super) fn read_ncount(src: &[u8]) -> Result<(Vec<i16>, usize), DecodeError> {
    let bit_len = src.len() * 8;
    let peek = |position: usize, n: u32| -> u32 {
        let mut chunk = [0u8; 4];
        let tail = src.get(position >> 3..).unwrap_or_default();
        chunk[..tail.len().min(4)].copy_from_slice(&tail[..tail.len().min(4)]);
        (u32::from_le_bytes(chunk) >> (position & 7)) & ((1u32 << n) - 1)
    };
    let log = peek(0, 4) + MIN_LOG;
    if log > ABSOLUTE_MAX_LOG {
        return Err(corrupt());
    }
    let mut position = 4usize;
    let mut remaining = (1i32 << log) + 1;
    let mut threshold = 1i32 << log;
    let mut width = log + 1;
    let mut counts = Vec::new();
    let mut after_zero = false;
    loop {
        if after_zero {
            // Runs of zero counts: 2-bit codes, 3 meaning "three more".
            loop {
                let code = peek(position, 2) as usize;
                position += 2;
                counts.resize(counts.len() + code, 0);
                if code != 3 {
                    break;
                }
            }
            if counts.len() >= MAX_SYMBOLS {
                return Err(corrupt());
            }
        }
        let max = (2 * threshold - 1) - remaining;
        let low = peek(position, width) as i32;
        let mut count;
        if (low & (threshold - 1)) < max {
            count = low & (threshold - 1);
            position += width as usize - 1;
        } else {
            count = low & (2 * threshold - 1);
            if count >= threshold {
                count -= max;
            }
            position += width as usize;
        }
        count -= 1;
        remaining -= count.abs();
        counts.push(count as i16);
        after_zero = count == 0;
        if remaining < threshold {
            if remaining <= 1 {
                break;
            }
            width = high_bit(remaining as u32) + 1;
            threshold = 1 << (width - 1);
        }
        if counts.len() >= MAX_SYMBOLS {
            break;
        }
    }
    if remaining != 1 || counts.len() > MAX_SYMBOLS || position > bit_len {
        return Err(corrupt());
    }
    Ok((counts, position.div_ceil(8)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Backend;
    use alloc::vec;

    fn level() -> Level {
        Backend::default().0
    }

    /// Counts for a 32-cell table over four symbols.
    const COUNTS: [i16; 4] = [16, 8, 7, -1];

    #[test]
    fn tables_validate_their_counts() {
        let table = Table::new(&COUNTS).unwrap();
        assert_eq!(table.log, 5);
        // The low-probability symbol sits in the last cell and resets fully.
        assert_eq!(table.cells[31].symbol, 3);
        assert_eq!((table.cells[31].bits, table.cells[31].next), (5, 0));
        for bad in [
            &[32][..],
            &[16, 15][..],
            &[-2, 33][..],
            &[0, 0][..],
            &[8, 8][..],
            &[4096, 1][..],
        ] {
            assert_eq!(Table::new(bad).unwrap_err(), corrupt());
        }
        assert_eq!(Table::new(&[0i16; 257]).unwrap_err(), corrupt());
        // A table made only of low-probability cells, and one symbol that
        // owns the whole table.
        assert!(Table::new(&[-1i16; 32]).is_ok());
        assert!(Table::new(&[32, 0]).is_ok());
    }

    #[test]
    fn a_table_that_never_needs_bits_leaves_no_room_to_flush() {
        // Symbol 0 owns every cell, so no state ever reads: the tail emits
        // every requested symbol and has none left for the other states.
        let table = Table::new(&[32, 0]).unwrap();
        let mut out = Vec::new();
        assert!(decode(level(), &table, &[0, 0b100], 2, 5, &mut out).is_err());
        assert_eq!(out, [0; 5]);
    }

    #[test]
    fn streams_with_the_wrong_length_are_rejected() {
        let table = Table::new(&[32, 0]).unwrap();
        let mut out = Vec::new();
        // Leftover payload: 18 bits for 2 states of 5 bits cannot be drained.
        assert!(decode(level(), &table, &[0, 0, 0b100], 2, 4, &mut out).is_err());
        // An over-read: one payload byte but two 5-bit states.
        assert!(decode(level(), &table, &[0b1], 2, 4, &mut out).is_err());
        // No mark.
        assert!(decode(level(), &table, &[0], 2, 2, &mut out).is_err());
        assert!(decode(level(), &table, &[], 2, 2, &mut out).is_err());
        // Too small an output for the flush.
        assert!(decode(level(), &table, &[0, 0b100], 2, 1, &mut out).is_err());
    }

    #[test]
    fn count_headers_decode_hand_encoded_examples() {
        // Log 5, one symbol owning the table: the 6-bit code 63 reads as 32.
        assert_eq!(read_ncount(&[0xf0, 0x03]), Ok((vec![32], 2)));
        // All-zero bits read as 32 low-probability symbols of a 32-cell table.
        assert_eq!(read_ncount(&[0; 64]), Ok((vec![-1; 32], 15)));
        // Two halves: a 5-bit code, then a 5-bit code in the larger range.
        assert_eq!(read_ncount(&[0x10, 0x3f]), Ok((vec![16, 16], 2)));
        // A zero count followed by a repeat code for one more zero.
        assert_eq!(
            read_ncount(&[0x10, 0xa3, 0x0f]),
            Ok((vec![16, 0, 0, 16], 3))
        );
    }

    #[test]
    fn count_headers_reject_malformed_input() {
        // A table log of 20 is beyond 15.
        assert!(read_ncount(&[0x0f]).is_err());
        // Empty input parses as zeros and runs out of bits.
        assert!(read_ncount(&[]).is_err());
        // Counts that never add up: ones keep asking for more than remains.
        assert!(read_ncount(&[0xff; 64]).is_err());
        // A header cut short of its last bits.
        assert!(read_ncount(&[0x10]).is_err());
        // A zero count followed by repeat codes that run past 256 symbols.
        let mut endless = vec![0x10, 0xfe];
        endless.resize(120, 0xff);
        assert!(read_ncount(&endless).is_err());
        // More than 256 symbols of counts: 257 symbols of weight 1 never
        // reach the end of the table.
        let mut many = vec![0x1f];
        many.resize(200, 0x55);
        assert!(read_ncount(&many).is_err());
    }

    #[test]
    fn only_tables_without_a_dominant_symbol_take_the_fast_loop() {
        // Half of the table is the most a symbol can own.
        assert!(Table::new(&[16, 8, 4, 4]).unwrap().fast);
        assert!(Table::new(&[-1i16; 32]).unwrap().fast);
        assert!(!Table::new(&[17, 8, 4, 3]).unwrap().fast);
        assert!(!Table::new(&[32, 0]).unwrap().fast);
    }

    #[cfg(feature = "compression")]
    #[test]
    fn the_fast_and_the_generic_loop_decode_alike() {
        use crate::shared::entropy::fse as encoder;
        let lengths: &[usize] = if cfg!(miri) {
            &[5, 77, 300]
        } else {
            &[5, 6, 77, 1000, 4099, 20_000]
        };
        // Counts with no dominant symbol (fast), with one at exactly half
        // (fast) and with one above half (generic only).
        for counts in [
            &[8i16, 8, 8, 8][..],
            &[16, 8, 4, 4][..],
            &[20, 6, 4, 2][..],
            &[-1, -1, 15, 15][..],
        ] {
            let max = counts.len() as u32 - 1;
            let log = 5;
            let ctable = encoder::CTable::build(counts, max, log).unwrap();
            let fast_table = Table::new(counts).unwrap();
            let expect_fast = counts.iter().all(|&count| 2 * i32::from(count) <= 32);
            assert_eq!(fast_table.fast, expect_fast, "{counts:?}");
            let mut generic_table = Table::new(counts).unwrap();
            generic_table.fast = false;
            for &len in lengths {
                let mut seed = len as u64 | 1;
                let src: Vec<u8> = (0..len)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        // Skewed towards the low symbols, but all of them.
                        ((seed >> 33) % (max as u64 + 1)).min((seed >> 45) % (max as u64 + 1)) as u8
                    })
                    .collect();
                for states in [2usize, 4] {
                    let Some(payload) = encoder::compress_using_ctable(
                        level(),
                        &src,
                        &ctable,
                        states,
                        encoder::compress_bound(len),
                    ) else {
                        continue;
                    };
                    for backend in Backend::available() {
                        for table in [&fast_table, &generic_table] {
                            let mut out = Vec::new();
                            decode(backend.0, table, &payload, states, len, &mut out).unwrap();
                            assert_eq!(out, src, "{counts:?}, {len} symbols, {states} states");
                        }
                    }
                }
            }
        }
    }

    #[cfg(feature = "compression")]
    #[test]
    fn decoding_inverts_the_encoder_with_two_and_four_states_on_every_level() {
        use crate::shared::entropy::fse as encoder;
        let lengths: &[usize] = if cfg!(miri) {
            &[5, 40, 700]
        } else {
            &[5, 6, 7, 40, 1000, 4097, 60_000]
        };
        for &len in lengths {
            let mut state = len as u64 | 1;
            let src: Vec<u8> = (0..len)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (state.trailing_zeros() as u8) * 5 + (state >> 40) as u8 % 3
                })
                .collect();
            let mut count = vec![0u32; 256];
            for &byte in &src {
                count[usize::from(byte)] += 1;
            }
            let max = count.iter().rposition(|&c| c != 0).unwrap() as u32;
            let log = encoder::optimal_table_log(encoder::DEFAULT_TABLELOG, len, max);
            let mut norm = vec![0i16; max as usize + 1];
            encoder::normalize_count(&mut norm, log, &count, len, max, true).unwrap();
            let ctable = encoder::CTable::build(&norm, max, log).unwrap();
            let table = Table::new(&norm).unwrap();
            for states in [2usize, 4] {
                let Some(payload) = encoder::compress_using_ctable(
                    level(),
                    &src,
                    &ctable,
                    states,
                    encoder::compress_bound(len),
                ) else {
                    continue;
                };
                for backend in Backend::available() {
                    let mut out = Vec::new();
                    decode(backend.0, &table, &payload, states, len, &mut out).unwrap();
                    assert_eq!(out, src, "{backend}, {len} symbols, {states} states");
                }
            }
        }
    }
}
