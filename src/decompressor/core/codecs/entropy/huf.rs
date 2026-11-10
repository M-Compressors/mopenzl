//! OpenZL's Huffman decoders: the byte coder on Zstandard's Huffman layout
//! (`fse/decompress/huf_decompress.c`, X1 flavours) and the 16-bit "large"
//! coder (`codecs/entropy/decode_huffman_kernel.c`).
//!
//! Both rebuild a canonical code from per-symbol weights: a symbol of weight
//! `w` owns `2^(w-1)` cells of a `2^tableLog` table and has a code of
//! `tableLog + 1 - w` bits; weight 0 means absent. Cells are handed out in
//! ascending weight (longest codes first), ascending symbol order. The
//! reference's double-symbol (X2) decoders only speed up the same code, so a
//! single table walk decodes everything.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use fearless_simd::{Level, dispatch};

use super::bits::{Reverse, refill};
use crate::decompressor::core::data::corrupt;
use crate::decompressor::error::DecodeError;

/// `HUF_TABLELOG_MAX`.
const BYTE_MAX_LOG: u32 = 12;
/// `ZS_kLargeHuffmanMaxNbBits`.
const WIDE_MAX_LOG: u32 = 20;
/// The cells of a byte table: `2^BYTE_MAX_LOG`, a constant so that indexing
/// with a masked prefix needs no bounds check.
const BYTE_CELLS: usize = 1 << BYTE_MAX_LOG;
/// Symbols a byte stream decodes per refill: a refill leaves at least 56
/// valid bits and `4 * 12` fit in them.
const BYTE_BATCH: usize = 4;
/// Bytes of input one pair iteration steps back over, at most: the 7 bits
/// the refill may leave unread in its first byte, plus the lookups.
const fn pair_bytes(lookups: usize, log: u32) -> usize {
    (lookups * log as usize).div_ceil(8)
}
/// Outputs shorter than this decode with the single-symbol table: building
/// the pair table costs about as much as decoding this many symbols.
const PAIR_MIN_OUTPUT: usize = 1 << 13;
/// Symbols a 16-bit stream decodes per refill: `2 * 20` bits fit in 56.
const WIDE_BATCH: usize = 2;

/// The shape of a canonical code, before any cell is written.
struct Layout {
    log: u32,
    /// First cell of each weight's run.
    start: [usize; WIDE_MAX_LOG as usize + 2],
}

impl Layout {
    /// Validates `weights` (`buildHUFDTable`, `buildHUF16DTable`): each at
    /// most `max_log`, at least two non-zero, and `sum(2^(w-1))` a power of
    /// two with log at most `max_log`.
    fn new(weights: &[u8], max_log: u32) -> Result<Self, DecodeError> {
        let mut sum = 0u64;
        let mut present = 0usize;
        let mut counts = [0usize; WIDE_MAX_LOG as usize + 1];
        for &weight in weights {
            if u32::from(weight) > max_log {
                return Err(corrupt());
            }
            sum += (1u64 << weight) >> 1;
            counts[usize::from(weight)] += 1;
            present += usize::from(weight != 0);
        }
        if present < 2 || !sum.is_power_of_two() {
            return Err(corrupt());
        }
        let log = sum.trailing_zeros();
        if log > max_log {
            return Err(corrupt());
        }
        let mut start = [0usize; WIDE_MAX_LOG as usize + 2];
        for weight in 1..=log as usize {
            start[weight + 1] = start[weight] + (counts[weight] << (weight - 1));
        }
        Ok(Self { log, start })
    }

    /// Writes `pack(bits, symbol)` into the cells of `cells`: a symbol of
    /// weight `w` owns `2^(w-1)` cells and a code of `log + 1 - w` bits,
    /// handed out in ascending weight, then ascending symbol.
    fn fill<T: Copy>(
        mut self,
        weights: &[u8],
        cells: &mut [T],
        pack: impl Fn(u32, usize) -> T,
    ) -> Result<(), DecodeError> {
        for (symbol, &weight) in weights.iter().enumerate() {
            if weight == 0 {
                continue;
            }
            let weight = usize::from(weight);
            let length = 1usize << (weight - 1);
            let first = self.start[weight];
            self.start[weight] += length;
            // Present weights are at most `log`, since two or more cells'
            // worth of weight add up to the table.
            let bits = self.log + 1 - weight as u32;
            cells
                .get_mut(first..first + length)
                .ok_or_else(corrupt)?
                .fill(pack(bits, symbol));
        }
        Ok(())
    }
}

/// Checks how a stream ended: no over-read, and, when `exact`, no bits left.
const fn check_end(bits: &Reverse<'_>, exact: bool) -> Result<(), DecodeError> {
    if bits.remaining() < 0 || (exact && bits.remaining() != 0) {
        return Err(corrupt());
    }
    Ok(())
}

/// One of the four streams of a pair decode: its unread bits and cursors.
///
/// `bits` holds the unread bits left-aligned, closed by a single 1 bit just
/// below them (so the bits left are `63 - bits.trailing_zeros()`); `ip` is
/// the offset in the shared body of the 8 bytes `bits` was loaded from, and
/// the stream cursor is `8 * ip + bits left`.
struct PairLane {
    bits: u64,
    ip: usize,
}

impl PairLane {
    /// Starts a lane at the cursor `pos` (at least 56 payload bits) of the
    /// stream that begins at `start` in `body`.
    fn new(body: &[u8], start: usize, pos: i64) -> Option<Self> {
        let ip = start + usize::try_from((pos >> 3) - 7).ok()?;
        let chunk = body.get(ip..)?.first_chunk::<8>()?;
        let skip = 8 - (pos & 7) as u32;
        let bits = (u64::from_le_bytes(*chunk) << skip) | (1 << (skip - 1));
        Some(Self { bits, ip })
    }

    /// The stream cursor, for a lane whose stream begins at `start`.
    const fn cursor(&self, start: usize) -> i64 {
        ((self.ip - start) * 8) as i64 + 63 - self.bits.trailing_zeros() as i64
    }
}

/// A decoding table of bytes: a cell per `log`-bit prefix holding the symbol
/// and the length of its code, packed as `bits << 8 | symbol`.
#[derive(Debug)]
pub(super) struct ByteTable {
    log: u32,
    cells: Box<[u16; BYTE_CELLS]>,
}

impl ByteTable {
    /// The table of a byte coder's weights (`buildHUFDTable`): 2 to 256
    /// symbols, table log at most 12.
    ///
    /// # Errors
    ///
    /// A corrupted stream for weights that do not describe a code.
    pub(super) fn new(weights: &[u8]) -> Result<Self, DecodeError> {
        if weights.len() < 2 || weights.len() > 256 {
            return Err(corrupt());
        }
        let layout = Layout::new(weights, BYTE_MAX_LOG)?;
        let log = layout.log;
        let mut cells: Box<[u16; BYTE_CELLS]> = crate::shared::fixed::fixed_table(0);
        layout.fill(weights, &mut cells[..], |bits, symbol| {
            ((bits as u16) << 8) | symbol as u16
        })?;
        Ok(Self { log, cells })
    }

    /// Decodes [`BYTE_BATCH`] symbols per lane at once, one refill per lane
    /// and batch, while every lane can refill and has room for a batch.
    /// `pos` is each lane's cursor, advanced; returns the symbols written to
    /// the front of every output.
    #[inline(always)]
    fn batches<const N: usize>(
        &self,
        srcs: [&[u8]; N],
        pos: &mut [i64; N],
        outs: [&mut [u8]; N],
    ) -> usize {
        let shift = 64 - self.log;
        let limit = outs.iter().map(|out| out.len()).min().unwrap_or(0);
        let mut done = 0usize;
        // Cursors live in registers while the loop runs.
        let mut cursor = *pos;
        while done + BYTE_BATCH <= limit {
            let mut words = [0u64; N];
            let mut ready = true;
            for lane in 0..N {
                match refill(srcs[lane], cursor[lane]) {
                    Some(word) => words[lane] = word,
                    None => ready = false,
                }
            }
            if !ready {
                break;
            }
            for lane in 0..N {
                let mut word = words[lane];
                let mut left = cursor[lane];
                let mut packed = 0u32;
                for step in 0..BYTE_BATCH {
                    let cell = self.cells[(word >> shift) as usize & (BYTE_CELLS - 1)];
                    let bits = u32::from(cell >> 8);
                    word <<= bits;
                    left -= i64::from(bits);
                    packed |= u32::from(cell as u8) << (8 * step);
                }
                cursor[lane] = left;
                outs[lane][done..done + BYTE_BATCH].copy_from_slice(&packed.to_le_bytes());
            }
            done += BYTE_BATCH;
        }
        *pos = cursor;
        done
    }

    /// Decodes the rest of one stream from its cursor `pos`, the first
    /// `done` symbols of `out` being written already: batches while 56 bits
    /// remain, then one symbol at a time with zero-fill below the start.
    #[inline(always)]
    fn finish(
        &self,
        src: &[u8],
        pos: i64,
        out: &mut [u8],
        done: usize,
        exact: bool,
    ) -> Result<(), DecodeError> {
        let mut cursor = [pos];
        let more = self.batches([src], &mut cursor, [&mut out[done..]]);
        let mut bits = Reverse::at(src, cursor[0]);
        for slot in &mut out[done + more..] {
            let cell = self.cells[bits.peek(self.log) as usize & (BYTE_CELLS - 1)];
            bits.skip(u32::from(cell >> 8));
            *slot = cell as u8;
        }
        check_end(&bits, exact)
    }

    /// The table of prefixes that decode two symbols at once, indexed by the
    /// first `log` bits of the stream (the cells past `2^log` are untouched).
    /// Each cell packs
    /// `bits | symbols << 8 | count << 30`: the bits both codes take, the
    /// first symbol and the second (if any) as a 16-bit value, and the
    /// symbol count, 2 where both codes fit in the `log` bits of the prefix
    /// and 1 where only the first does. This is the content of the
    /// reference's double-symbol (`X2`) table for the codes it covers; both
    /// decode the same symbols. `bits` sits in the low byte so that a shift
    /// by the cell is a shift by the bits.
    ///
    /// The table is written in runs, as the reference's is. A symbol of
    /// `bits` bits owns a run of `2^(log - bits)` prefixes, which all start
    /// with its code; the rest of a prefix `base + t` of the run is `t`
    /// shifted up by `bits`, which is the code of the second symbol when it
    /// lies in a run of the base table of at least `2^bits` prefixes, i.e.
    /// when both codes fit. So a run is filled with its first symbol alone,
    /// and then, for every second symbol whose code is short enough, the
    /// `t` that start with that code get the pair. The base table lists the
    /// longest codes first, so those second symbols are a suffix of it.
    fn pairs(&self, pairs: &mut [u32; BYTE_CELLS]) -> Result<(), DecodeError> {
        let size = 1usize << self.log;
        // The runs of the base table, in cell order: start, bits, symbol.
        let mut runs = [(0u16, 0u8, 0u8); 256];
        let mut count = 0usize;
        let mut base = 0usize;
        while base < size {
            let cell = self.cells[base];
            let bits = u32::from(cell >> 8);
            let slot = runs.get_mut(count).ok_or_else(corrupt)?;
            *slot = (base as u16, bits as u8, cell as u8);
            count += 1;
            base += 1usize << self.log.saturating_sub(bits);
        }
        let runs = &runs[..count];
        for &(start, bits, symbol) in runs {
            let first_bits = u32::from(bits);
            let run = 1usize << self.log.saturating_sub(first_bits);
            let start = usize::from(start);
            let head = first_bits | (u32::from(symbol) << 8) | (1 << 30);
            pairs
                .get_mut(start..start + run)
                .ok_or_else(corrupt)?
                .fill(head);
            let room = self.log.saturating_sub(first_bits);
            for &(second_start, second_bits, second) in runs.iter().rev() {
                if u32::from(second_bits) > room {
                    break;
                }
                // Both bounds are multiples of `2^first_bits`: the second
                // run is at least that long and aligned to its length.
                let second_run = 1usize << (self.log - u32::from(second_bits));
                let from = start + (usize::from(second_start) >> first_bits);
                let to = start + ((usize::from(second_start) + second_run) >> first_bits);
                pairs
                    .get_mut(from..to)
                    .ok_or_else(corrupt)?
                    .fill(head + (u32::from(second) << 16) + (1 << 30) + u32::from(second_bits));
            }
        }
        Ok(())
    }

    /// Decodes four streams of `body` at once with the pair table, as long
    /// as every lane can refill and has room for an iteration. `starts`
    /// place the streams in `body`, `bounds` the quarters in `out`; `pos` is
    /// each lane's cursor, advanced. Returns the symbols written to the
    /// front of every quarter. Lookups per refill: as many as fit in the 56
    /// bits a refill guarantees.
    #[inline(always)]
    fn pair_lanes(
        &self,
        body: &[u8],
        starts: [usize; 4],
        pos: &mut [i64; 4],
        out: &mut [u8],
        bounds: [usize; 5],
    ) -> Result<[usize; 4], DecodeError> {
        // On the stack, the table needs no register for its address.
        let mut pairs = [0u32; BYTE_CELLS];
        self.pairs(&mut pairs)?;
        if pos.iter().any(|&cursor| cursor < 56) {
            return Ok([0; 4]);
        }
        match self.log {
            ..=9 => Self::run_lanes::<6, 18>(&pairs, self.log, body, starts, pos, out, bounds),
            10 | 11 => Self::run_lanes::<5, 16>(&pairs, self.log, body, starts, pos, out, bounds),
            _ => Self::run_lanes::<4, 16>(&pairs, self.log, body, starts, pos, out, bounds),
        }
    }

    /// [`ByteTable::pair_lanes`] with `LOOKUPS` lookups per refill and the
    /// matching output window (`3 * LOOKUPS - 1`: the compiler sees a cell
    /// advance the output by up to 3).
    ///
    /// The lanes advance in lockstep, lookup `j` of every lane before lookup
    /// `j + 1` of any: each lane's lookups form a chain of dependent loads,
    /// and this order puts the four independent chains next to each other in
    /// the instruction stream, where the processor finds them without
    /// looking far ahead. The output is split into its four quarters, so the
    /// four windows are disjoint borrows.
    #[inline(always)]
    fn run_lanes<const LOOKUPS: usize, const WINDOW: usize>(
        pairs: &[u32; BYTE_CELLS],
        log: u32,
        body: &[u8],
        starts: [usize; 4],
        pos: &mut [i64; 4],
        out: &mut [u8],
        bounds: [usize; 5],
    ) -> Result<[usize; 4], DecodeError> {
        let (Some(l0), Some(l1), Some(l2), Some(l3)) = (
            PairLane::new(body, starts[0], pos[0]),
            PairLane::new(body, starts[1], pos[1]),
            PairLane::new(body, starts[2], pos[2]),
            PairLane::new(body, starts[3], pos[3]),
        ) else {
            return Err(corrupt());
        };
        let (q0, rest) = out.split_at_mut(bounds[1]);
        let (q1, rest) = rest.split_at_mut(bounds[2] - bounds[1]);
        let (q2, q3) = rest.split_at_mut(bounds[3] - bounds[2]);
        let (mut b0, mut b1, mut b2, mut b3) = (l0.bits, l1.bits, l2.bits, l3.bits);
        // Each lane's input is the prefix of `body` that ends with the 8 bytes
        // its bits were loaded from, so a refill steps back by shortening it.
        let (Some(mut h0), Some(mut h1), Some(mut h2), Some(mut h3)) = (
            body.get(..l0.ip + 8),
            body.get(..l1.ip + 8),
            body.get(..l2.ip + 8),
            body.get(..l3.ip + 8),
        ) else {
            return Err(corrupt());
        };
        // What is left of each quarter to write: a window is taken from its
        // front and it shrinks by the symbols written.
        let lens = [q0.len(), q1.len(), q2.len(), q3.len()];
        let (mut r0, mut r1, mut r2, mut r3) = (q0, q1, q2, q3);
        // A shift of at least 52 leaves 12 bits, so the index needs no check;
        // the clamp is a no-op for a table log of 1 to 12.
        let shift = (u64::BITS - log).clamp(u64::BITS - BYTE_MAX_LOG, 63);
        let bytes = pair_bytes(LOOKUPS, log);
        // Iterations of `LOOKUPS` lookups lane `index` can run before its
        // input or its quarter of `len` bytes ends.
        let room = |hi: &[u8], rest: &[u8], index: usize| {
            let by_input = (hi.len().saturating_sub(8 + starts[index])) / bytes;
            let by_output = match rest.len().checked_sub(WINDOW) {
                Some(room) => room / (2 * LOOKUPS) + 1,
                None => 0,
            };
            by_input.min(by_output)
        };
        loop {
            let count = room(h0, r0, 0)
                .min(room(h1, r1, 1))
                .min(room(h2, r2, 2))
                .min(room(h3, r3, 3));
            if count == 0 {
                break;
            }
            for _ in 0..count {
                let (Some(w0), Some(w1), Some(w2), Some(w3)) = (
                    r0.first_chunk_mut::<WINDOW>(),
                    r1.first_chunk_mut::<WINDOW>(),
                    r2.first_chunk_mut::<WINDOW>(),
                    r3.first_chunk_mut::<WINDOW>(),
                ) else {
                    return Err(corrupt());
                };
                // Phase 1: the dependent chains only.
                let mut cells = [[0u32; 4]; LOOKUPS];
                for row in &mut cells {
                    macro_rules! lookup {
                        ($bits:ident, $lane:expr) => {
                            let cell = pairs[($bits >> shift) as usize];
                            $bits = $bits.wrapping_shl(cell);
                            row[$lane] = cell;
                        };
                    }
                    lookup!(b0, 0);
                    lookup!(b1, 1);
                    lookup!(b2, 2);
                    lookup!(b3, 3);
                }
                // Phase 2: the writes.
                macro_rules! write {
                    ($window:ident, $lane:expr) => {{
                        let mut at = 0usize;
                        for row in &cells {
                            let cell = row[$lane];
                            $window[at..at + 2]
                                .copy_from_slice(&((cell >> 8) as u16).to_le_bytes());
                            at += (cell >> 30) as usize;
                        }
                        at
                    }};
                }
                let a0 = write!(w0, 0);
                let a1 = write!(w1, 1);
                let a2 = write!(w2, 2);
                let a3 = write!(w3, 3);
                // The refill: the bits eaten since the load tell how far to
                // step back; the 1 bit below the stream is put back under
                // the new bits.
                macro_rules! refill {
                    ($bits:ident, $hi:ident, $rest:ident, $at:ident) => {
                        let Some((_, tail)) = core::mem::take(&mut $rest).split_at_mut_checked($at)
                        else {
                            return Err(corrupt());
                        };
                        $rest = tail;
                        let eaten = $bits.trailing_zeros();
                        // One check: the step is at most 8 bytes, so the
                        // shortened prefix still holds a chunk.
                        let step = (eaten >> 3) as usize;
                        if $hi.len() < step + 8 {
                            return Err(corrupt());
                        }
                        $hi = &$hi[..$hi.len() - step];
                        let Some(chunk) = $hi.last_chunk::<8>() else {
                            return Err(corrupt());
                        };
                        $bits = ((u64::from_le_bytes(*chunk) << 1) | 1) << (eaten & 7);
                    };
                }
                refill!(b0, h0, r0, a0);
                refill!(b1, h1, r1, a1);
                refill!(b2, h2, r2, a2);
                refill!(b3, h3, r3, a3);
            }
        }
        let lanes = [
            (b0, h0.len() - 8, lens[0] - r0.len()),
            (b1, h1.len() - 8, lens[1] - r1.len()),
            (b2, h2.len() - 8, lens[2] - r2.len()),
            (b3, h3.len() - 8, lens[3] - r3.len()),
        ];
        let mut done = [0usize; 4];
        for (index, (bits, ip, op)) in lanes.into_iter().enumerate() {
            pos[index] = PairLane { bits, ip }.cursor(starts[index]);
            done[index] = op;
        }
        Ok(done)
    }

    /// Decodes `out.len()` bytes from one stream (`HUF_decompress1X1_usingDTable`).
    ///
    /// # Errors
    ///
    /// A corrupted stream when it lacks its end mark or is not consumed
    /// exactly.
    pub(super) fn single(
        &self,
        level: Level,
        src: &[u8],
        out: &mut [u8],
    ) -> Result<(), DecodeError> {
        // The kernel is inlined into the dispatch target, so it is compiled
        // with the features of `level` (BMI2 shifts on x86-64-v3).
        dispatch!(level, _simd => {
            let bits = Reverse::new(src)?;
            self.finish(src, bits.remaining(), out, 0, true)
        })
    }

    /// Decodes `out.len()` bytes from four streams behind a 6-byte table of
    /// three little-endian 16-bit lengths (`HUF_decompress4X1_usingDTable`).
    /// Each of the first three streams holds `ceil(count / 4)` symbols, the
    /// fourth the rest.
    ///
    /// # Errors
    ///
    /// A corrupted stream for a short source, lengths beyond it, a `count`
    /// the quarter layout cannot hold, or streams not consumed exactly.
    pub(super) fn quad(&self, level: Level, src: &[u8], out: &mut [u8]) -> Result<(), DecodeError> {
        dispatch!(level, _simd => self.quad_kernel(src, out))
    }

    /// [`ByteTable::quad`] under the dispatched level.
    #[inline(always)]
    fn quad_kernel(&self, src: &[u8], out: &mut [u8]) -> Result<(), DecodeError> {
        if src.len() < 10 {
            return Err(corrupt());
        }
        let length = |at: usize| usize::from(u16::from_le_bytes([src[at], src[at + 1]]));
        let (first, second, third) = (length(0), length(2), length(4));
        let body = &src[6..];
        if body.len() < first + second + third {
            return Err(corrupt());
        }
        let count = out.len();
        let segment = count.div_ceil(4);
        if segment * 3 > count {
            return Err(corrupt());
        }
        let starts = [0, first, first + second, first + second + third];
        let (part0, rest) = body.split_at(first);
        let (part1, rest) = rest.split_at(second);
        let (part2, part3) = rest.split_at(third);
        let srcs = [part0, part1, part2, part3];
        let mut pos = [0i64; 4];
        for (cursor, part) in pos.iter_mut().zip(srcs) {
            *cursor = Reverse::new(part)?.remaining();
        }
        let bounds = [0, segment, 2 * segment, 3 * segment, count];
        let done = if count >= PAIR_MIN_OUTPUT {
            self.pair_lanes(body, starts, &mut pos, out, bounds)?
        } else {
            let (out0, rest) = out.split_at_mut(segment);
            let (out1, rest) = rest.split_at_mut(segment);
            let (out2, out3) = rest.split_at_mut(segment);
            let common = self.batches(
                srcs,
                &mut pos,
                [&mut out0[..], &mut out1[..], &mut out2[..], &mut out3[..]],
            );
            [common; 4]
        };
        let (out0, rest) = out.split_at_mut(segment);
        let (out1, rest) = rest.split_at_mut(segment);
        let (out2, out3) = rest.split_at_mut(segment);
        let outs = [out0, out1, out2, out3];
        for (((part, cursor), out), done) in srcs.into_iter().zip(pos).zip(outs).zip(done) {
            self.finish(part, cursor, out, done, true)?;
        }
        Ok(())
    }
}

/// A decoding table of 16-bit symbols: a cell per `log`-bit prefix holding
/// the symbol and the length of its code, packed as `bits << 16 | symbol`.
#[derive(Debug)]
pub(super) struct WideTable {
    log: u32,
    cells: Vec<u32>,
}

impl WideTable {
    /// The table of a 16-bit coder's weights (`buildHUF16DTable`): 2 to
    /// 65536 symbols, table log at most 20.
    ///
    /// # Errors
    ///
    /// A corrupted stream for weights that do not describe a code.
    pub(super) fn new(weights: &[u8]) -> Result<Self, DecodeError> {
        if weights.len() < 2 || weights.len() > 65536 {
            return Err(corrupt());
        }
        let layout = Layout::new(weights, WIDE_MAX_LOG)?;
        let log = layout.log;
        let mut cells = vec![0u32; 1usize << log];
        layout.fill(weights, &mut cells, |bits, symbol| {
            (bits << 16) | symbol as u32
        })?;
        Ok(Self { log, cells })
    }

    /// Decodes [`WIDE_BATCH`] symbols per lane at once, one refill per lane
    /// and batch, while every lane can refill and has room for a batch. The
    /// lanes advance in lockstep (symbol `j` of every lane before symbol
    /// `j + 1` of any), so their independent chains of lookups overlap.
    /// `pos` is each lane's cursor, advanced; returns the symbols written to
    /// the front of every output.
    #[inline(always)]
    fn batches<const N: usize>(
        &self,
        srcs: [&[u8]; N],
        pos: &mut [i64; N],
        outs: [&mut [u8]; N],
    ) -> usize {
        let shift = 64 - self.log;
        let batch = 2 * WIDE_BATCH;
        let limit = outs.iter().map(|out| out.len()).min().unwrap_or(0);
        let mut done = 0usize;
        let mut cursor = *pos;
        'batches: while done + batch <= limit {
            let mut words = [0u64; N];
            for lane in 0..N {
                match refill(srcs[lane], cursor[lane]) {
                    Some(word) => words[lane] = word,
                    None => break 'batches,
                }
            }
            for step in 0..WIDE_BATCH {
                for lane in 0..N {
                    let cell = self.cells[(words[lane] >> shift) as usize];
                    let used = cell >> 16;
                    words[lane] <<= used;
                    cursor[lane] -= i64::from(used);
                    outs[lane][done + 2 * step..done + 2 * step + 2]
                        .copy_from_slice(&(cell as u16).to_le_bytes());
                }
            }
            done += batch;
        }
        *pos = cursor;
        done
    }

    /// Decodes the rest of one stream from its cursor `pos`, the first `done`
    /// symbols' bytes of `out` being written already: batches while 56 bits
    /// remain, then one symbol at a time with zero-fill below the start. The
    /// stream need not be consumed to its last bit; over-reads are errors.
    #[inline(always)]
    fn finish(&self, src: &[u8], pos: i64, out: &mut [u8], done: usize) -> Result<(), DecodeError> {
        let mut cursor = [pos];
        let more = self.batches([src], &mut cursor, [&mut out[done..]]);
        let mut bits = Reverse::at(src, cursor[0]);
        for slot in out[done + more..].as_chunks_mut::<2>().0 {
            let cell = self.cells[bits.peek(self.log) as usize];
            bits.skip(cell >> 16);
            *slot = (cell as u16).to_le_bytes();
        }
        check_end(&bits, false)
    }

    /// Decodes `out.len() / 2` symbols of `src` into `out` as 16-bit
    /// little-endian values.
    #[inline(always)]
    fn stream(&self, src: &[u8], out: &mut [u8]) -> Result<(), DecodeError> {
        let pos = Reverse::new(src)?.remaining();
        self.finish(src, pos, out, 0)
    }

    /// Decodes the four blocks of an `x4` payload at once: the headers give
    /// every block's symbols and bytes, so the four streams decode in
    /// lockstep into four disjoint parts of `out`.
    #[inline(always)]
    fn quad(&self, mut src: &[u8], count: usize, out: &mut Vec<u8>) -> Result<(), DecodeError> {
        let mut payloads: [&[u8]; 4] = [&[]; 4];
        let mut counts = [0usize; 4];
        let mut decoded = 0usize;
        for (payload, block) in payloads.iter_mut().zip(&mut counts) {
            let (head, rest) = src.split_first_chunk::<8>().ok_or_else(corrupt)?;
            let symbols = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
            let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
            if size > rest.len() || symbols > count - decoded {
                return Err(corrupt());
            }
            (*payload, src) = rest.split_at(size);
            *block = symbols;
            decoded += symbols;
        }
        if decoded != count || !src.is_empty() {
            return Err(corrupt());
        }
        let start = out.len();
        out.resize(start + 2 * count, 0);
        let (out0, rest) = out[start..].split_at_mut(2 * counts[0]);
        let (out1, rest) = rest.split_at_mut(2 * counts[1]);
        let (out2, out3) = rest.split_at_mut(2 * counts[2]);
        let mut pos = [0i64; 4];
        for (cursor, payload) in pos.iter_mut().zip(payloads) {
            *cursor = Reverse::new(payload)?.remaining();
        }
        let done = self.batches(
            payloads,
            &mut pos,
            [&mut *out0, &mut *out1, &mut *out2, &mut *out3],
        );
        for ((payload, cursor), out) in payloads.into_iter().zip(pos).zip([out0, out1, out2, out3])
        {
            self.finish(payload, cursor, out, done)?;
        }
        Ok(())
    }

    /// Decodes 16-bit symbols from one `[count u32][size u32][bits]` block at
    /// the front of `src` (`ZS_largeHuffmanDecodeUsingDTable`), appending
    /// them to `out` and returning the rest of `src`.
    #[inline(always)]
    fn block<'a>(
        &self,
        src: &'a [u8],
        room: usize,
        out: &mut Vec<u8>,
    ) -> Result<(&'a [u8], usize), DecodeError> {
        let (head, rest) = src.split_first_chunk::<8>().ok_or_else(corrupt)?;
        let count = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
        if size > rest.len() || count > room {
            return Err(corrupt());
        }
        let (payload, rest) = rest.split_at(size);
        let start = out.len();
        out.resize(start + 2 * count, 0);
        self.stream(payload, &mut out[start..])?;
        Ok((rest, count))
    }

    /// Decodes `count` 16-bit symbols from a large-Huffman payload of one
    /// block (`x4` false) or four blocks (`ZS_largeHuffmanDecodeUsingDTable`
    /// and `...X4`), appending little-endian symbols to `out`.
    ///
    /// # Errors
    ///
    /// A corrupted stream for malformed blocks, blocks that do not hold
    /// exactly `count` symbols, or bytes left in `src`.
    pub(super) fn blocks(
        &self,
        level: Level,
        src: &[u8],
        count: usize,
        x4: bool,
        out: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        dispatch!(level, _simd => self.blocks_kernel(src, count, x4, out))
    }

    /// [`WideTable::blocks`] under the dispatched level.
    #[inline(always)]
    fn blocks_kernel(
        &self,
        src: &[u8],
        count: usize,
        x4: bool,
        out: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        if x4 {
            return self.quad(src, count, out);
        }
        let (rest, block) = self.block(src, count, out)?;
        if block != count || !rest.is_empty() {
            return Err(corrupt());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Backend;

    fn level() -> Level {
        Backend::default().0
    }

    /// Weights of a complete code: one 1-bit and two 2-bit symbols.
    const WEIGHTS: [u8; 3] = [3, 2, 2];

    #[test]
    fn canonical_codes_follow_ascending_weight_then_symbol() {
        let table = ByteTable::new(&WEIGHTS).unwrap();
        assert_eq!(table.log, 3);
        // Weight 2 (2-bit codes) comes first: symbols 1 and 2, then symbol 0.
        let cells = &table.cells[..8];
        let symbols: Vec<u8> = cells.iter().map(|&cell| cell as u8).collect();
        let lengths: Vec<u16> = cells.iter().map(|&cell| cell >> 8).collect();
        assert_eq!(symbols, [1, 1, 2, 2, 0, 0, 0, 0]);
        assert_eq!(lengths, [2, 2, 2, 2, 1, 1, 1, 1]);
    }

    #[test]
    fn weights_are_validated() {
        for bad in [
            &[1u8][..],
            &[1, 0][..],
            &[13, 12][..],
            &[1, 1, 1][..],
            &[0, 0][..],
            &[12, 12, 12][..],
        ] {
            assert_eq!(ByteTable::new(bad).unwrap_err(), corrupt());
        }
        assert!(ByteTable::new(&[1, 1]).is_ok());
        assert!(ByteTable::new(&[0; 257]).is_err());
        assert_eq!(WideTable::new(&[20, 20, 0, 1]).unwrap_err(), corrupt());
        assert!(WideTable::new(&[0; 65537]).is_err());
        assert!(WideTable::new(&[21, 21]).is_err());
        // Weight 20 twice makes a 20-bit table of two 1-bit codes.
        assert_eq!(WideTable::new(&[20, 20]).unwrap().log, 20);
    }

    /// Bits written first end up last: a stream of `codes` (value, width).
    fn pack(codes: &[(u32, u32)]) -> Vec<u8> {
        let mut acc = 0u64;
        let mut nbits = 0;
        for &(value, width) in codes {
            acc |= u64::from(value) << nbits;
            nbits += width;
        }
        acc |= 1 << nbits;
        nbits += 1;
        acc.to_le_bytes()[..(nbits as usize).div_ceil(8)].to_vec()
    }

    #[test]
    fn single_streams_must_end_exactly() {
        let table = ByteTable::new(&WEIGHTS).unwrap();
        // Codes: symbol 0 = '1' (cells 2, 3 -> prefix 1x), 1 = 00, 2 = 01.
        // The reader returns the last written first, so write in reverse.
        let reversed = pack(&[(0b01, 2), (0b00, 2), (0b1, 1)]);
        let mut out = [0u8; 3];
        table.single(level(), &reversed, &mut out).unwrap();
        assert_eq!(out, [0, 1, 2]);
        // Asking for fewer leaves bits behind, more over-reads.
        assert!(table.single(level(), &reversed, &mut [0u8; 2]).is_err());
        assert!(table.single(level(), &reversed, &mut [0u8; 4]).is_err());
        assert!(table.single(level(), &[], &mut [0u8; 1]).is_err());
    }

    #[test]
    fn quad_streams_split_the_output_in_quarters() {
        let table = ByteTable::new(&WEIGHTS).unwrap();
        // Four streams of one symbol each (symbol 0 is the 1-bit code).
        let one = pack(&[(0b1, 1)]);
        let mut framed = Vec::new();
        for _ in 0..3 {
            framed.extend_from_slice(&(one.len() as u16).to_le_bytes());
        }
        for _ in 0..4 {
            framed.extend_from_slice(&one);
        }
        let mut out = [9u8; 4];
        table.quad(level(), &framed, &mut out).unwrap();
        assert_eq!(out, [0, 0, 0, 0]);
        // A count below 4 cannot fill three quarters.
        assert!(table.quad(level(), &framed, &mut [0u8; 2]).is_err());
        // Lengths beyond the source, and a source below the minimum.
        let mut bad = framed.clone();
        bad[0] = 0xff;
        assert!(table.quad(level(), &bad, &mut out).is_err());
        assert!(table.quad(level(), &framed[..9], &mut out).is_err());
        // A stream without its end mark.
        let mut unmarked = framed.clone();
        *unmarked.last_mut().unwrap() = 0;
        assert!(table.quad(level(), &unmarked, &mut out).is_err());
    }

    #[test]
    fn wide_blocks_decode_sixteen_bit_symbols() {
        let table = WideTable::new(&[1, 1]).unwrap();
        // Two symbols with one-bit codes 0 and 1; written in reverse.
        let bits = pack(&[(1, 1), (0, 1), (1, 1)]);
        let mut block = Vec::new();
        block.extend_from_slice(&3u32.to_le_bytes());
        block.extend_from_slice(&(bits.len() as u32).to_le_bytes());
        block.extend_from_slice(&bits);
        let mut out = Vec::new();
        table.blocks(level(), &block, 3, false, &mut out).unwrap();
        assert_eq!(out, [1, 0, 0, 0, 1, 0]);
        // Four blocks, two of them empty.
        let mut quad = block.clone();
        for _ in 0..3 {
            quad.extend_from_slice(&0u32.to_le_bytes());
            quad.extend_from_slice(&1u32.to_le_bytes());
            quad.push(1);
        }
        out.clear();
        table.blocks(level(), &quad, 3, true, &mut out).unwrap();
        assert_eq!(out.len(), 6);
        // Wrong counts, trailing bytes, short headers and payloads.
        assert!(table.blocks(level(), &block, 4, false, &mut out).is_err());
        assert!(table.blocks(level(), &block, 2, false, &mut out).is_err());
        let mut trailing = block.clone();
        trailing.push(0);
        assert!(
            table
                .blocks(level(), &trailing, 3, false, &mut out)
                .is_err()
        );
        assert!(
            table
                .blocks(level(), &block[..7], 3, false, &mut out)
                .is_err()
        );
        assert!(
            table
                .blocks(level(), &block[..8], 3, false, &mut out)
                .is_err()
        );
        // The same faults in the four-block layout: a count that the blocks
        // do not add up to, a block holding more than the room left, bytes
        // after the last block and fewer than four blocks.
        assert!(table.blocks(level(), &quad, 4, true, &mut out).is_err());
        assert!(table.blocks(level(), &quad, 2, true, &mut out).is_err());
        let mut quad_trailing = quad.clone();
        quad_trailing.push(0);
        assert!(
            table
                .blocks(level(), &quad_trailing, 3, true, &mut out)
                .is_err()
        );
        assert!(
            table
                .blocks(level(), &quad[..quad.len() - 1], 3, true, &mut out)
                .is_err()
        );
        assert!(table.blocks(level(), &block, 3, true, &mut out).is_err());
        // Over-reading the payload.
        let mut starved = Vec::new();
        starved.extend_from_slice(&9u32.to_le_bytes());
        starved.extend_from_slice(&1u32.to_le_bytes());
        starved.push(0b11);
        assert!(table.blocks(level(), &starved, 9, false, &mut out).is_err());
        let mut starved_quad = starved.clone();
        for _ in 0..3 {
            starved_quad.extend_from_slice(&0u32.to_le_bytes());
            starved_quad.extend_from_slice(&1u32.to_le_bytes());
            starved_quad.push(1);
        }
        assert!(
            table
                .blocks(level(), &starved_quad, 9, true, &mut out)
                .is_err()
        );
    }

    /// A skewed sample: a geometric spread of symbols, so that codes are of
    /// every length up to the table limit.
    #[cfg(feature = "compression")]
    fn skewed(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed | 1;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        (0..len)
            .map(|_| {
                let r = next();
                (r.trailing_zeros() as u8) * 9 + (next() % 4) as u8
            })
            .collect()
    }

    /// The weights and the four-stream payload of `src` from the encoder.
    #[cfg(feature = "compression")]
    fn encode_quad(src: &[u8]) -> (Vec<u8>, Vec<u8>) {
        use crate::shared::entropy::huf as encoder;
        let mut count = vec![0u32; 256];
        for &byte in src {
            count[usize::from(byte)] += 1;
        }
        let max = count.iter().rposition(|&c| c != 0).unwrap() as u32;
        let log = encoder::optimal_table_log(encoder::TABLELOG_DEFAULT, src.len(), max);
        let table = encoder::build_ctable(&count, max, log).unwrap();
        let weights = (0..=max as usize)
            .map(|symbol| match table.nb_bits(symbol) {
                0 => 0,
                bits => (table.table_log() + 1 - bits) as u8,
            })
            .collect();
        let payload = encoder::compress4x_using_ctable(
            level(),
            src,
            &table,
            encoder::compress_bound(src.len()),
        )
        .unwrap();
        (weights, payload)
    }

    #[cfg(feature = "compression")]
    #[test]
    fn quad_decoding_agrees_across_paths_and_levels() {
        // Below, at and above the size that switches to the pair table, and
        // sizes that leave the quarters uneven.
        let sizes: &[usize] = if cfg!(miri) {
            &[40, PAIR_MIN_OUTPUT - 1, PAIR_MIN_OUTPUT + 3]
        } else {
            &[
                40,
                1000,
                PAIR_MIN_OUTPUT - 1,
                PAIR_MIN_OUTPUT,
                20_001,
                65_537,
            ]
        };
        for &size in sizes {
            let src = skewed(size, size as u64);
            let (weights, payload) = encode_quad(&src);
            let table = ByteTable::new(&weights).unwrap();
            for backend in Backend::available() {
                let mut out = vec![0u8; size];
                table.quad(backend.0, &payload, &mut out).unwrap();
                assert_eq!(out, src, "{backend}, {size} bytes");
            }
        }
    }

    #[cfg(feature = "compression")]
    #[test]
    fn corrupted_quad_streams_fail_or_decode_alike_on_every_level() {
        let size = PAIR_MIN_OUTPUT + 777;
        let src = skewed(size, 99);
        let (weights, payload) = encode_quad(&src);
        let table = ByteTable::new(&weights).unwrap();
        let decode = |bytes: &[u8], level: Level| {
            let mut out = vec![0u8; size];
            table.quad(level, bytes, &mut out).map(|()| out)
        };
        let mut variants = vec![payload[..payload.len() - 1].to_vec()];
        for at in [
            0,
            6,
            7,
            payload.len() / 3,
            payload.len() / 2,
            payload.len() - 1,
        ] {
            let mut bytes = payload.clone();
            bytes[at] ^= 0x5a;
            variants.push(bytes);
        }
        // Streams whose jump table claims more than the payload holds, and
        // one whose first stream ends early.
        let mut lengths = payload.clone();
        lengths[0] = 0xff;
        lengths[1] = 0xff;
        variants.push(lengths);
        for bytes in &variants {
            let expected = decode(bytes, Backend::default().0).ok();
            for backend in Backend::available() {
                assert_eq!(decode(bytes, backend.0).ok(), expected, "{backend}");
            }
        }
    }

    #[cfg(feature = "compression")]
    #[test]
    fn quad_decoding_covers_every_lookup_width() {
        // Alphabets of 3 to 256 symbols give table logs from 2 to 11, and a
        // skewed 256-symbol one reaches the 12-bit table.
        let size = if cfg!(miri) { PAIR_MIN_OUTPUT } else { 40_000 };
        for alphabet in [3u32, 16, 40, 100, 256] {
            let mut state = u64::from(alphabet) | 1;
            let src: Vec<u8> = (0..size)
                .map(|_| {
                    state = state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    ((state >> 33) as u32 % alphabet) as u8
                })
                .collect();
            let (weights, payload) = encode_quad(&src);
            let table = ByteTable::new(&weights).unwrap();
            for backend in Backend::available() {
                let mut out = vec![0u8; size];
                table.quad(backend.0, &payload, &mut out).unwrap();
                assert_eq!(out, src, "{backend}, alphabet {alphabet}");
            }
        }
    }

    #[cfg(feature = "compression")]
    #[test]
    fn pair_tables_decode_what_single_tables_decode() {
        let src = skewed(30_000, 7);
        let (weights, _) = encode_quad(&src);
        let table = ByteTable::new(&weights).unwrap();
        let mut pairs = [0u32; BYTE_CELLS];
        table.pairs(&mut pairs).unwrap();
        let mask = (1usize << table.log) - 1;
        let mut two = 0;
        // Cells past the table are untouched.
        assert!(pairs[mask + 1..].iter().all(|&cell| cell == 0));
        for (prefix, &cell) in pairs[..=mask].iter().enumerate() {
            let first = table.cells[prefix];
            assert_eq!((cell >> 8) as u8, first as u8);
            if cell >> 30 == 2 {
                two += 1;
                let used = u32::from(first >> 8);
                let second = table.cells[(prefix << used) & mask];
                assert_eq!((cell >> 16) as u8, second as u8);
                assert_eq!(cell & 0xff, used + u32::from(second >> 8));
            } else {
                assert_eq!((cell >> 30, cell & 0xff), (1, u32::from(first >> 8)));
            }
        }
        assert!(two > 0);
    }

    #[cfg(feature = "compression")]
    #[test]
    fn wide_decoding_agrees_across_levels_and_block_counts() {
        use crate::shared::entropy::huf16 as encoder;
        let lengths: &[usize] = if cfg!(miri) {
            &[2, 3, 300]
        } else {
            &[2, 3, 7, 1001, 40_000]
        };
        for &len in lengths {
            // Symbols above 255 make the alphabet 16-bit; the spread of the
            // low byte gives codes of several lengths.
            let src: Vec<u16> = skewed(len, len as u64)
                .into_iter()
                .enumerate()
                .map(|(i, byte)| u16::from(byte) | (((i % 3) as u16) << 8))
                .collect();
            let mut count = vec![0u32; 1 << 16];
            for &symbol in &src {
                count[usize::from(symbol)] += 1;
            }
            let max = count.iter().rposition(|&c| c != 0).unwrap() as u16;
            if count.iter().filter(|&&c| c != 0).count() < 2 {
                continue;
            }
            let (ctable, log) = encoder::build_ctable(&count, max, 0).unwrap();
            let weights: Vec<u8> = ctable[..=usize::from(max)]
                .iter()
                .map(|elt| match i32::from(elt.nb_bits) {
                    0 => 0,
                    bits => (log + 1 - bits) as u8,
                })
                .collect();
            let table = WideTable::new(&weights).unwrap();
            let expected: Vec<u8> = src.iter().flat_map(|symbol| symbol.to_le_bytes()).collect();
            for x4 in [false, true] {
                let mut payload = Vec::new();
                let capacity = 2 * len + 64;
                if x4 {
                    encoder::encode_using_ctable_x4(&mut payload, capacity, &src, &ctable)
                } else {
                    encoder::encode_using_ctable(&mut payload, capacity, &src, &ctable)
                }
                .unwrap();
                for backend in Backend::available() {
                    let mut out = Vec::new();
                    table
                        .blocks(backend.0, &payload, len, x4, &mut out)
                        .unwrap();
                    assert_eq!(out, expected, "{backend}, {len} symbols, x4 {x4}");
                }
            }
        }
    }
}
