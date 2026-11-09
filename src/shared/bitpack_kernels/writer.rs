//! `ZS_BitCStreamFF` (`codecs/common/bitstream/ff_bitstream.h`): the forward
//! bit-writer.
//!
//! Nothing outside the tests uses it yet: the partition and pivco encoders
//! will. The expectation below fails the build once they do, as a reminder
//! to delete it.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the partition and pivco encoders")
)]

use alloc::vec::Vec;

/// `ZS_BITSTREAM_WRITE_MAX_BITS`.
pub(crate) const WRITE_MAX_BITS: usize = usize::BITS as usize - 1;

/// Why a [`BitWriter`] could not finish.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct TooSmall;

/// `ZS_BitCStreamFF`: a forward bit-writer over a buffer of fixed capacity.
///
/// Values are packed least significant bit first into a word-sized
/// container; [`flush`](Self::flush) moves whole bytes out of it. The
/// caller flushes often enough that the container never holds more than
/// [`WRITE_MAX_BITS`] bits, exactly as with the reference, whose flush is a
/// no-op once the write position reaches the buffer's safety limit
/// (`capacity - 7`): [`finish`](Self::finish) then writes what is left and
/// fails if the capacity is too small.
#[derive(Debug)]
pub(crate) struct BitWriter {
    container: usize,
    nb_bits: usize,
    /// The bytes written so far; `ptr` is the write position within them.
    buffer: Vec<u8>,
    ptr: usize,
    limit: usize,
    end: usize,
}

impl BitWriter {
    /// `ZS_BitCStreamFF_init` over `capacity` bytes.
    pub(crate) fn new(capacity: usize) -> Self {
        let word = size_of::<usize>();
        Self {
            container: 0,
            nb_bits: 0,
            buffer: Vec::with_capacity(capacity),
            ptr: 0,
            limit: if capacity < word {
                0
            } else {
                capacity - word + 1
            },
            end: capacity,
        }
    }

    /// Stores `bytes` at the write position, replacing what is there.
    fn put_bytes(&mut self, bytes: &[u8]) {
        let overlap = (self.buffer.len() - self.ptr).min(bytes.len());
        self.buffer[self.ptr..self.ptr + overlap].copy_from_slice(&bytes[..overlap]);
        self.buffer.extend_from_slice(&bytes[overlap..]);
    }

    /// `ZS_BitCStreamFF_write`: appends the low `nb_bits` bits of `value`.
    pub(crate) fn write(&mut self, value: usize, nb_bits: usize) {
        debug_assert!(self.nb_bits + nb_bits <= WRITE_MAX_BITS);
        let mask = 1usize.wrapping_shl(nb_bits as u32).wrapping_sub(1);
        self.container |= (value & mask).wrapping_shl(self.nb_bits as u32);
        self.nb_bits += nb_bits;
    }

    /// `ZS_BitCStreamFF_flush`: writes the whole bytes of the container.
    pub(crate) fn flush(&mut self) {
        if self.ptr >= self.limit {
            return;
        }
        let nb_bytes = self.nb_bits >> 3;
        let bytes = self.container.to_le_bytes();
        self.put_bytes(&bytes[..nb_bytes]);
        self.ptr += nb_bytes;
        self.nb_bits &= 7;
        self.container = self.container.wrapping_shr((nb_bytes << 3) as u32);
    }

    /// `ZS_BitCStreamFF_finish`: writes the remaining bits and returns the
    /// stream.
    ///
    /// # Errors
    ///
    /// [`TooSmall`] when the capacity cannot hold the trailing bits.
    pub(crate) fn finish(mut self) -> Result<Vec<u8>, TooSmall> {
        let to_write = self.nb_bits.div_ceil(8);
        if self.end - self.ptr < to_write {
            return Err(TooSmall);
        }
        let bytes = self.container.to_le_bytes();
        self.put_bytes(&bytes[..to_write.min(bytes.len())]);
        let total = self.ptr + to_write;
        self.buffer.truncate(total);
        Ok(self.buffer)
    }

    /// `ZS_BitCStreamFF_writeExpGolomb`: an order-`order` exponential-Golomb
    /// code.
    pub(crate) fn write_exp_golomb(&mut self, mut value: u32, order: usize) {
        if order > 0 {
            self.write(value as usize, order);
            value >>= order;
        }
        let nb_bits = value.wrapping_add(1).checked_ilog2().unwrap_or(0) as usize;
        self.write(1usize << nb_bits, nb_bits + 1);
        self.write(value.wrapping_add(1) as usize, nb_bits);
    }

    /// `ZS_BitCStreamFF_reserveAlignedBits`: flushes to a byte boundary and
    /// reserves `ceil(nb_bits / 8)` bytes for direct writes, returned as a
    /// range of the final stream; fill it with
    /// [`reserved_mut`](Self::reserved_mut) and call
    /// [`commit_reserved_bits`](Self::commit_reserved_bits).
    pub(crate) fn reserve_aligned_bits(
        &mut self,
        nb_bits: usize,
    ) -> Option<core::ops::Range<usize>> {
        if nb_bits > usize::MAX - 7 {
            return None;
        }
        let buffered = self.nb_bits.div_ceil(8);
        if buffered > self.end - self.ptr {
            return None;
        }
        let nb_bytes = nb_bits.div_ceil(8);
        if nb_bytes > self.end - self.ptr - buffered {
            return None;
        }
        if buffered != 0 {
            let bytes = self.container.to_le_bytes();
            self.put_bytes(&bytes[..buffered]);
            self.ptr += buffered;
            self.container = 0;
            self.nb_bits = 0;
        }
        let start = self.ptr;
        // The reserved bytes exist (zeroed) in the buffer from now on.
        let wanted = start + nb_bytes;
        if self.buffer.len() < wanted {
            self.buffer.resize(wanted, 0);
        }
        self.ptr = start + nb_bits / 8;
        self.nb_bits = nb_bits & 7;
        self.container = 0;
        Some(start..wanted)
    }

    /// The bytes of a reserved range.
    pub(crate) fn reserved_mut(&mut self, range: core::ops::Range<usize>) -> &mut [u8] {
        &mut self.buffer[range]
    }

    /// `ZS_BitCStreamFF_commitReservedBits`: resumes bit writes after a
    /// reserved region.
    pub(crate) fn commit_reserved_bits(&mut self) {
        if self.nb_bits == 0 {
            self.container = 0;
            return;
        }
        let mask = (1usize << self.nb_bits) - 1;
        self.container = usize::from(self.buffer[self.ptr]) & mask;
    }
}
