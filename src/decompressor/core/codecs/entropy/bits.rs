//! The backward bit stream of FSE and Huffman (`BIT_DStream_t`,
//! `fse/bitstream.h`).
//!
//! The writer packs bits least significant first and closes the stream with
//! a single 1 bit above the last payload bit; the reader starts under that
//! end mark and walks towards the first byte, so the first value it returns
//! is the last one written. Reads past the start return zero bits, like the
//! reference's shifted-out container, and leave [`Reverse::remaining`]
//! negative so callers can tell an over-read from an exact end.

use crate::decompressor::core::data::corrupt;
use crate::decompressor::error::DecodeError;

/// A backward bit reader over one stream (`BIT_initDStream`).
#[derive(Debug)]
pub(super) struct Reverse<'a> {
    data: &'a [u8],
    /// Payload bits not consumed yet; negative after an over-read.
    remaining: i64,
}

impl<'a> Reverse<'a> {
    /// Starts reading `data`.
    ///
    /// # Errors
    ///
    /// A corrupted stream when `data` is empty or its last byte, which
    /// carries the end mark, is zero.
    pub(super) fn new(data: &'a [u8]) -> Result<Self, DecodeError> {
        let last = *data.last().ok_or_else(corrupt)?;
        if last == 0 {
            return Err(corrupt());
        }
        let payload = i64::from(7 - last.leading_zeros());
        // A slice is at most `isize::MAX` bytes; saturate rather than wrap.
        let whole = (data.len() as i64 - 1).saturating_mul(8);
        Ok(Self {
            data,
            remaining: whole.saturating_add(payload),
        })
    }

    /// Payload bits left; negative once more bits were consumed than the
    /// stream holds.
    pub(super) const fn remaining(&self) -> i64 {
        self.remaining
    }

    /// The `n` bits after the cursor (at most 25) without consuming them,
    /// zero-filled below the start of the stream.
    #[inline]
    pub(super) fn peek(&self, n: u32) -> u32 {
        let low = self.remaining - i64::from(n);
        if low >= 0 {
            self.window(low as u64, n)
        } else if self.remaining > 0 {
            self.window(0, self.remaining as u32) << (-low)
        } else {
            0
        }
    }

    /// Consumes `n` bits.
    #[inline]
    pub(super) const fn skip(&mut self, n: u32) {
        self.remaining -= n as i64;
    }

    /// Consumes and returns `n` bits (at most 25).
    #[inline]
    pub(super) fn read(&mut self, n: u32) -> u32 {
        let value = self.peek(n);
        self.skip(n);
        value
    }

    /// Bits `[low, low + n)` of the stream, counted from its first bit.
    #[inline]
    fn window(&self, low: u64, n: u32) -> u32 {
        let byte = (low >> 3) as usize;
        let word = match self.data.get(byte..).and_then(<[u8]>::first_chunk::<8>) {
            Some(chunk) => u64::from_le_bytes(*chunk),
            None => {
                let mut chunk = [0u8; 8];
                let tail = self.data.get(byte..).unwrap_or_default();
                chunk[..tail.len().min(8)].copy_from_slice(&tail[..tail.len().min(8)]);
                u64::from_le_bytes(chunk)
            }
        };
        ((word >> (low & 7)) & ((1u64 << n) - 1)) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_marks_are_required() {
        assert_eq!(Reverse::new(&[]).unwrap_err(), corrupt());
        assert_eq!(Reverse::new(&[1, 0]).unwrap_err(), corrupt());
        assert_eq!(Reverse::new(&[0xff, 1]).unwrap().remaining(), 8);
    }

    #[test]
    fn values_come_back_in_reverse_order() {
        // Written: 0b101 (3 bits), 0b01 (2 bits), 0xAB (8 bits), then the mark.
        let value = 0b101 | (0b01 << 3) | (0xAB << 5) | (1 << 13);
        let bytes = [value as u8, (value >> 8) as u8];
        let mut bits = Reverse::new(&bytes).unwrap();
        assert_eq!(bits.remaining(), 13);
        assert_eq!(bits.read(8), 0xAB);
        assert_eq!(bits.read(2), 0b01);
        assert_eq!(bits.peek(3), 0b101);
        bits.skip(3);
        assert_eq!(bits.remaining(), 0);
    }

    #[test]
    fn over_reads_are_zero_filled_and_visible() {
        let mut bits = Reverse::new(&[0b101]).unwrap();
        assert_eq!(bits.remaining(), 2);
        // Two payload bits (01) then zeros below the stream.
        assert_eq!(bits.peek(4), 0b0100);
        assert_eq!(bits.read(3), 0b010);
        assert_eq!(bits.remaining(), -1);
        assert_eq!(bits.peek(3), 0);
    }

    #[test]
    fn long_streams_read_across_word_boundaries() {
        let mut data: alloc::vec::Vec<u8> = (0..40u8).map(|i| i.wrapping_mul(37) | 1).collect();
        data[39] = 0x80;
        let mut bits = Reverse::new(&data).unwrap();
        assert_eq!(bits.remaining(), 39 * 8 + 7);
        let mut total = 0;
        while bits.remaining() > 12 {
            bits.read(12);
            total += 12;
        }
        assert!(total > 300);
        // Reading the last bits of a stream shorter than a word.
        let mut short = Reverse::new(&[0xff, 0xff, 0xff]).unwrap();
        assert_eq!(short.read(7), 0x7f);
        assert_eq!(short.read(16), 0xffff);
        assert_eq!(short.remaining(), 0);
    }
}
