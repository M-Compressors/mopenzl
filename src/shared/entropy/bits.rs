//! The forward bit writer of FSE and Huffman (`BIT_CStream_t`, `fse/bitstream.h`).
//!
//! Values are appended least significant bit first; the stream ends with a
//! one-bit end mark so the (backward-reading) decoder finds where the data
//! stops. The reference flushes in unrolled batches into a fixed buffer
//! whose last eight bytes are slop: a stream that does not fit makes
//! `BIT_closeCStream` return 0. [`BitWriter::close`] reproduces exactly that
//! decision, so callers fail in the same cases.

use alloc::vec::Vec;

/// The size of the reference's bit container: both the minimum useful
/// capacity and the slop kept free at the end of the buffer.
const CONTAINER_BYTES: usize = 8;

/// A bit container writing into a growing buffer.
#[derive(Debug, Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    container: u64,
    nb_bits: u32,
}

impl BitWriter {
    /// An empty writer with room for `capacity` output bytes.
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity.min(1 << 20)),
            container: 0,
            nb_bits: 0,
        }
    }

    /// `BIT_addBits`: appends the low `nb_bits` bits of `value`
    /// (`nb_bits < 32`).
    #[inline]
    pub(crate) fn add_bits(&mut self, value: u64, nb_bits: u32) {
        debug_assert!(nb_bits < 32);
        let mask = (1u64 << nb_bits) - 1;
        self.container |= (value & mask) << self.nb_bits;
        self.nb_bits += nb_bits;
        if self.nb_bits >= 32 {
            self.bytes
                .extend_from_slice(&(self.container as u32).to_le_bytes());
            self.container >>= 32;
            self.nb_bits -= 32;
        }
    }

    /// `BIT_closeCStream`: adds the end mark and returns the stream, or
    /// `None` where the reference returns 0: a destination of at most eight
    /// bytes (`BIT_initCStream` fails) or one whose flushed bytes reach the
    /// last eight (the overflow `BIT_closeCStream` detects).
    pub(crate) fn close(mut self, dst_capacity: usize) -> Option<Vec<u8>> {
        self.add_bits(1, 1);
        if dst_capacity <= CONTAINER_BYTES {
            return None;
        }
        let total_bits = self.bytes.len() * 8 + self.nb_bits as usize;
        if total_bits / 8 >= dst_capacity - CONTAINER_BYTES {
            return None;
        }
        let tail = (self.nb_bits as usize).div_ceil(8);
        self.bytes
            .extend_from_slice(&self.container.to_le_bytes()[..tail]);
        Some(self.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_are_packed_least_significant_first_with_an_end_mark() {
        let mut writer = BitWriter::with_capacity(16);
        writer.add_bits(0b101, 3);
        writer.add_bits(0b11, 2);
        // 11101 then the end mark at bit 5.
        assert_eq!(writer.close(64), Some(alloc::vec![0b11_1101]));
    }

    #[test]
    fn an_empty_stream_is_just_the_end_mark() {
        assert_eq!(BitWriter::default().close(64), Some(alloc::vec![1]));
    }

    #[test]
    fn long_streams_flush_whole_words() {
        let mut writer = BitWriter::with_capacity(0);
        for _ in 0..5 {
            writer.add_bits(0xffff_ffff, 31);
        }
        let out = writer.close(1024).unwrap();
        // 155 bits of ones plus the mark: 156 bits is 20 bytes.
        assert_eq!(out.len(), 20);
        assert!(out[..19].iter().all(|&byte| byte == 0xff));
        assert_eq!(out[19], 0x0f);
    }

    #[test]
    fn capacity_failures_follow_the_reference() {
        // A destination of eight bytes or fewer cannot even start.
        assert!(BitWriter::default().close(8).is_none());
        assert!(BitWriter::default().close(9).is_some());
        // Flushed bytes must stay below capacity - 8.
        let make = || {
            let mut writer = BitWriter::default();
            for _ in 0..4 {
                writer.add_bits(0xff, 8);
            }
            writer
        };
        // 4 full bytes plus the mark: 4 flushed bytes (the mark opens a fifth).
        assert!(make().close(12).is_none());
        assert!(make().close(13).is_some());
    }
}
