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

use alloc::vec;
use alloc::vec::Vec;

use super::bits::Reverse;
use crate::decompressor::core::data::corrupt;
use crate::decompressor::error::DecodeError;

/// `HUF_TABLELOG_MAX`.
const BYTE_MAX_LOG: u32 = 12;
/// `ZS_kLargeHuffmanMaxNbBits`.
const WIDE_MAX_LOG: u32 = 20;

/// A decoding table: a cell per `log`-bit prefix holding the symbol and the
/// length of its code, packed as `bits << 16 | symbol`.
#[derive(Debug)]
pub(super) struct Table {
    log: u32,
    cells: Vec<u32>,
}

impl Table {
    /// Builds the table of `weights` (`buildHUFDTable`,
    /// `buildHUF16DTable`): each weight at most `max_log`, at least two
    /// non-zero, and `sum(2^(w-1))` a power of two with log at most
    /// `max_log`.
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
        // First cell of each weight's run.
        let mut start = [0usize; WIDE_MAX_LOG as usize + 2];
        for weight in 1..=log as usize {
            start[weight + 1] = start[weight] + (counts[weight] << (weight - 1));
        }
        let mut cells = vec![0u32; 1usize << log];
        for (symbol, &weight) in weights.iter().enumerate() {
            if weight == 0 {
                continue;
            }
            let weight = usize::from(weight);
            let length = 1usize << (weight - 1);
            let first = start[weight];
            start[weight] += length;
            // Present weights are at most `log`, since two or more cells'
            // worth of weight add up to the table.
            let bits = log + 1 - weight as u32;
            cells
                .get_mut(first..first + length)
                .ok_or_else(corrupt)?
                .fill((bits << 16) | symbol as u32);
        }
        Ok(Self { log, cells })
    }

    /// The table of a byte coder's weights: 2 to 256 symbols, table log at
    /// most 12.
    ///
    /// # Errors
    ///
    /// A corrupted stream for weights that do not describe a code.
    pub(super) fn bytes(weights: &[u8]) -> Result<Self, DecodeError> {
        if weights.len() < 2 || weights.len() > 256 {
            return Err(corrupt());
        }
        Self::new(weights, BYTE_MAX_LOG)
    }

    /// The table of a 16-bit coder's weights: 2 to 65536 symbols, table log
    /// at most 20.
    ///
    /// # Errors
    ///
    /// A corrupted stream for weights that do not describe a code.
    pub(super) fn wide(weights: &[u8]) -> Result<Self, DecodeError> {
        if weights.len() < 2 || weights.len() > 65536 {
            return Err(corrupt());
        }
        Self::new(weights, WIDE_MAX_LOG)
    }

    /// Decodes `count` symbols of `src` into `out`, `WIDE` selecting 16-bit
    /// little-endian symbols over bytes. When `exact`, the stream must be
    /// consumed to its last bit; otherwise only over-reads are errors.
    fn stream<const WIDE: bool>(
        &self,
        src: &[u8],
        count: usize,
        exact: bool,
        out: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        let mut bits = Reverse::new(src)?;
        for _ in 0..count {
            let cell = self.cells[bits.peek(self.log) as usize];
            bits.skip(cell >> 16);
            if WIDE {
                out.extend_from_slice(&(cell as u16).to_le_bytes());
            } else {
                out.push(cell as u8);
            }
        }
        if bits.remaining() < 0 || (exact && bits.remaining() != 0) {
            return Err(corrupt());
        }
        Ok(())
    }

    /// Decodes `count` bytes from one stream (`HUF_decompress1X1_usingDTable`).
    ///
    /// # Errors
    ///
    /// A corrupted stream when it lacks its end mark or is not consumed
    /// exactly.
    pub(super) fn single(
        &self,
        src: &[u8],
        count: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        self.stream::<false>(src, count, true, out)
    }

    /// Decodes `count` bytes from four streams behind a 6-byte table of
    /// three little-endian 16-bit lengths (`HUF_decompress4X1_usingDTable`).
    /// Each of the first three streams holds `ceil(count / 4)` symbols, the
    /// fourth the rest.
    ///
    /// # Errors
    ///
    /// A corrupted stream for a short source, lengths beyond it, a `count`
    /// the quarter layout cannot hold, or streams not consumed exactly.
    pub(super) fn quad(
        &self,
        src: &[u8],
        count: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        if src.len() < 10 {
            return Err(corrupt());
        }
        let length = |at: usize| usize::from(u16::from_le_bytes([src[at], src[at + 1]]));
        let (first, second, third) = (length(0), length(2), length(4));
        let body = &src[6..];
        let fourth = body
            .len()
            .checked_sub(first + second + third)
            .ok_or_else(corrupt)?;
        let segment = count.div_ceil(4);
        if segment * 3 > count {
            return Err(corrupt());
        }
        let mut rest = body;
        for (index, size) in [first, second, third, fourth].into_iter().enumerate() {
            let (part, tail) = rest.split_at(size);
            rest = tail;
            let symbols = if index < 3 {
                segment
            } else {
                count - 3 * segment
            };
            self.stream::<false>(part, symbols, true, out)?;
        }
        Ok(())
    }

    /// Decodes 16-bit symbols from one `[count u32][size u32][bits]` block at
    /// the front of `src` (`ZS_largeHuffmanDecodeUsingDTable`), appending
    /// them to `out` and returning the rest of `src`.
    fn wide_block<'a>(
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
        self.stream::<true>(payload, count, false, out)?;
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
    pub(super) fn wide_blocks(
        &self,
        mut src: &[u8],
        count: usize,
        x4: bool,
        out: &mut Vec<u8>,
    ) -> Result<(), DecodeError> {
        let mut decoded = 0usize;
        for _ in 0..if x4 { 4 } else { 1 } {
            let (rest, block) = self.wide_block(src, count - decoded, out)?;
            src = rest;
            decoded += block;
        }
        if decoded != count || !src.is_empty() {
            return Err(corrupt());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Weights of a complete code: one 1-bit and two 2-bit symbols.
    const WEIGHTS: [u8; 3] = [3, 2, 2];

    #[test]
    fn canonical_codes_follow_ascending_weight_then_symbol() {
        let table = Table::bytes(&WEIGHTS).unwrap();
        assert_eq!(table.log, 3);
        // Weight 2 (2-bit codes) comes first: symbols 1 and 2, then symbol 0.
        let symbols: Vec<u8> = table.cells.iter().map(|&cell| cell as u8).collect();
        let lengths: Vec<u32> = table.cells.iter().map(|&cell| cell >> 16).collect();
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
            assert_eq!(Table::bytes(bad).unwrap_err(), corrupt());
        }
        assert!(Table::bytes(&[1, 1]).is_ok());
        assert!(Table::bytes(&[0; 257]).is_err());
        assert_eq!(Table::wide(&[20, 20, 0, 1]).unwrap_err(), corrupt());
        assert!(Table::wide(&[0; 65537]).is_err());
        assert!(Table::wide(&[21, 21]).is_err());
        // Weight 20 twice makes a 20-bit table of two 1-bit codes.
        assert_eq!(Table::wide(&[20, 20]).unwrap().log, 20);
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
        let table = Table::bytes(&WEIGHTS).unwrap();
        // Codes: symbol 0 = '1' (cells 2, 3 -> prefix 1x), 1 = 00, 2 = 01.
        // The reader returns the last written first, so write in reverse.
        let reversed = pack(&[(0b01, 2), (0b00, 2), (0b1, 1)]);
        let mut out = Vec::new();
        table.single(&reversed, 3, &mut out).unwrap();
        assert_eq!(out, [0, 1, 2]);
        out.clear();
        // Asking for fewer leaves bits behind, more over-reads.
        assert!(table.single(&reversed, 2, &mut out).is_err());
        assert!(table.single(&reversed, 4, &mut out).is_err());
        assert!(table.single(&[], 1, &mut out).is_err());
    }

    #[test]
    fn quad_streams_split_the_output_in_quarters() {
        let table = Table::bytes(&WEIGHTS).unwrap();
        // Four streams of one symbol each (symbol 0 is the 1-bit code).
        let one = pack(&[(0b1, 1)]);
        let mut framed = Vec::new();
        for _ in 0..3 {
            framed.extend_from_slice(&(one.len() as u16).to_le_bytes());
        }
        for _ in 0..4 {
            framed.extend_from_slice(&one);
        }
        let mut out = Vec::new();
        table.quad(&framed, 4, &mut out).unwrap();
        assert_eq!(out, [0, 0, 0, 0]);
        out.clear();
        // A count below 4 cannot fill three quarters.
        assert!(table.quad(&framed, 2, &mut out).is_err());
        // Lengths beyond the source, and a source below the minimum.
        let mut bad = framed.clone();
        bad[0] = 0xff;
        assert!(table.quad(&bad, 4, &mut out).is_err());
        assert!(table.quad(&framed[..9], 4, &mut out).is_err());
    }

    #[test]
    fn wide_blocks_decode_sixteen_bit_symbols() {
        let table = Table::wide(&[1, 1]).unwrap();
        // Two symbols with one-bit codes 0 and 1; written in reverse.
        let bits = pack(&[(1, 1), (0, 1), (1, 1)]);
        let mut block = Vec::new();
        block.extend_from_slice(&3u32.to_le_bytes());
        block.extend_from_slice(&(bits.len() as u32).to_le_bytes());
        block.extend_from_slice(&bits);
        let mut out = Vec::new();
        table.wide_blocks(&block, 3, false, &mut out).unwrap();
        assert_eq!(out, [1, 0, 0, 0, 1, 0]);
        // Four blocks, two of them empty.
        let mut quad = block.clone();
        for _ in 0..3 {
            quad.extend_from_slice(&0u32.to_le_bytes());
            quad.extend_from_slice(&1u32.to_le_bytes());
            quad.push(1);
        }
        out.clear();
        table.wide_blocks(&quad, 3, true, &mut out).unwrap();
        assert_eq!(out.len(), 6);
        // Wrong counts, trailing bytes, short headers and payloads.
        assert!(table.wide_blocks(&block, 4, false, &mut out).is_err());
        assert!(table.wide_blocks(&block, 2, false, &mut out).is_err());
        let mut trailing = block.clone();
        trailing.push(0);
        assert!(table.wide_blocks(&trailing, 3, false, &mut out).is_err());
        assert!(table.wide_blocks(&block[..7], 3, false, &mut out).is_err());
        assert!(table.wide_blocks(&block[..8], 3, false, &mut out).is_err());
        // Over-reading the payload.
        let mut starved = Vec::new();
        starved.extend_from_slice(&9u32.to_le_bytes());
        starved.extend_from_slice(&1u32.to_le_bytes());
        starved.push(0b11);
        assert!(table.wide_blocks(&starved, 9, false, &mut out).is_err());
    }
}
