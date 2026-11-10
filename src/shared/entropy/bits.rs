//! The forward bit writer of FSE and Huffman (`BIT_CStream_t`, `fse/bitstream.h`).
//!
//! Values are appended least significant bit first; the stream ends with a
//! one-bit end mark so the (backward-reading) decoder finds where the data
//! stops. The reference flushes in unrolled batches into a fixed buffer
//! whose last eight bytes are slop: a stream that does not fit makes
//! `BIT_closeCStream` return 0. [`BitWriter::close`] reproduces exactly that
//! decision, so callers fail in the same cases.

use alloc::boxed::Box;
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

/// A bit container that flushes whole words (`BIT_flushBits`), for the hot
/// encoders.
///
/// [`WordWriter::add`] appends without checking for room; the caller flushes
/// before more than 64 bits could pile up (a flush leaves at most 7). The
/// buffer has a fixed size, so a flush has no allocation path and the
/// writer's fields stay in registers inside the encoder loop; a flush that
/// finds no room reports it (a stream that long cannot fit the destination
/// either, which is a failure in the reference too). The bytes are identical
/// to [`BitWriter`]'s and [`WordWriter::close`] applies the same capacity
/// rule.
#[derive(Debug, Default)]
pub(crate) struct WordWriter {
    bytes: Box<[u8]>,
    len: usize,
    container: u64,
    nb_bits: u32,
}

impl WordWriter {
    /// An empty writer with room for `capacity` output bytes (and the slop
    /// of one container that [`WordWriter::flush`] writes past the end of
    /// the data).
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: alloc::vec![0; capacity + 2 * CONTAINER_BYTES].into_boxed_slice(),
            len: 0,
            container: 0,
            nb_bits: 0,
        }
    }

    /// `BIT_addBitsFast`: appends `value`, whose bits above `nb_bits` are
    /// zero. The pending bits plus `nb_bits` must stay below 64.
    #[inline(always)]
    pub(crate) fn add(&mut self, value: u64, nb_bits: u32) {
        self.container |= value << self.nb_bits;
        self.nb_bits += nb_bits;
    }

    /// `BIT_flushBits`: moves the whole bytes of the container to the buffer.
    /// Returns `false` when the buffer has no room for the container.
    #[inline(always)]
    pub(crate) fn flush(&mut self) -> bool {
        let Some(window) = self
            .bytes
            .get_mut(self.len..)
            .and_then(<[u8]>::first_chunk_mut::<CONTAINER_BYTES>)
        else {
            return false;
        };
        *window = self.container.to_le_bytes();
        let whole = self.nb_bits >> 3;
        self.len += whole as usize;
        self.container >>= whole * 8;
        self.nb_bits &= 7;
        true
    }

    /// `BIT_closeCStream`, as [`BitWriter::close`]. `None` too where the
    /// final flush found no room.
    pub(crate) fn close(mut self, dst_capacity: usize) -> Option<Vec<u8>> {
        self.add(1, 1);
        if !self.flush() || dst_capacity <= CONTAINER_BYTES {
            return None;
        }
        let total_bits = self.len * 8 + self.nb_bits as usize;
        if total_bits / 8 >= dst_capacity - CONTAINER_BYTES {
            return None;
        }
        let mut bytes = self.bytes.into_vec();
        bytes.truncate(self.len + (self.nb_bits as usize).div_ceil(8));
        Some(bytes)
    }
}

/// A bit container that keeps the newest bits at its top, like the
/// reference's `HUF_addBits`/`HUF_flushBits`, for the Huffman encoders.
///
/// A symbol is a pair `[count_and_code, code]` built by [`TopWriter::entry`]:
/// the code left-aligned in a word, and the same word with the code length
/// in its low byte. [`TopWriter::add`] is then one shift (by the low byte),
/// one `or` and one add; the pending bit count is the low byte of `count`
/// (the adds of the code bits above it never carry into it while at most 255
/// bits are pending). The caller flushes before 64 bits could pile up (a
/// flush leaves at most 7). The bytes equal [`BitWriter`]'s and
/// [`TopWriter::close`] applies the same capacity rule.
#[derive(Debug)]
pub(crate) struct TopWriter {
    bytes: Vec<u8>,
    len: usize,
    container: u64,
    count: u64,
}

impl TopWriter {
    /// An empty writer with room for `room` stream bytes: [`TopWriter::flush`]
    /// fails once more than that (and the 8 bytes of slop) are written.
    pub(crate) fn with_capacity(room: usize) -> Self {
        Self {
            bytes: alloc::vec![0; room + 2 * CONTAINER_BYTES],
            len: 0,
            container: 0,
            count: 0,
        }
    }

    /// The table entry of a code of `nb_bits` bits (at most 32; zero means
    /// absent) with value `code`.
    pub(crate) const fn entry(code: u32, nb_bits: u32) -> [u64; 2] {
        if nb_bits == 0 {
            return [0, 0];
        }
        let top = (code as u64) << (u64::BITS - nb_bits);
        [top | nb_bits as u64, top]
    }

    /// `HUF_addBitsFast`: appends the code of `entry`. The pending bits
    /// plus the code length must stay below 64.
    #[inline(always)]
    pub(crate) fn add(&mut self, entry: [u64; 2]) {
        self.container = self.container.wrapping_shr(entry[0] as u32) | entry[1];
        self.count = self.count.wrapping_add(entry[0]);
    }

    /// [`TopWriter::add`] of `G` entries in order. Each code is shifted down by
    /// the lengths of the codes that follow it and the old container by all of
    /// them, so the shifts do not depend on one another and the only chain
    /// through the container is one shift and one `or` per group, instead of
    /// per code. The pending bits plus all the code lengths must stay below
    /// 64, as for `G` single adds.
    #[inline(always)]
    pub(crate) fn add_group<const G: usize>(&mut self, entries: [[u64; 2]; G]) {
        let mut shift = 0u64;
        let mut codes = 0u64;
        for entry in entries.iter().rev() {
            codes |= entry[1].wrapping_shr(shift as u32);
            shift = shift.wrapping_add(entry[0]);
        }
        self.container = self.container.wrapping_shr(shift as u32) | codes;
        self.count = self.count.wrapping_add(shift);
    }

    /// `HUF_flushBits`: moves the whole bytes of the container to the
    /// buffer. Returns `false` when the buffer is full, in which case the
    /// reference would fail the stream too.
    #[inline(always)]
    pub(crate) fn flush(&mut self) -> bool {
        let pending = (self.count & 0xff) as u32;
        let Some(window) = self
            .bytes
            .get_mut(self.len..)
            .and_then(<[u8]>::first_chunk_mut::<8>)
        else {
            return false;
        };
        *window = self
            .container
            .wrapping_shr(u64::BITS.wrapping_sub(pending))
            .to_le_bytes();
        self.len += (pending >> 3) as usize;
        self.count = u64::from(pending & 7);
        true
    }

    /// `HUF_closeCStream`, as [`BitWriter::close`].
    pub(crate) fn close(mut self, dst_capacity: usize) -> Option<Vec<u8>> {
        let end = self.finish(dst_capacity)?;
        self.bytes.truncate(end);
        Some(self.bytes)
    }

    /// [`TopWriter::close`] for a writer that is reused: appends the stream
    /// to `out` and starts an empty one. `None` (the writer then unusable
    /// until the next call restarts it) where the reference returns 0.
    pub(crate) fn close_into(&mut self, dst_capacity: usize, out: &mut Vec<u8>) -> Option<()> {
        let end = self.finish(dst_capacity);
        self.len = 0;
        self.container = 0;
        self.count = 0;
        out.extend_from_slice(self.bytes.get(..end?)?);
        Some(())
    }

    /// Adds the end mark and applies the capacity rule; the stream is then
    /// the first returned number of bytes.
    fn finish(&mut self, dst_capacity: usize) -> Option<usize> {
        self.add(Self::entry(1, 1));
        if !self.flush() || dst_capacity <= CONTAINER_BYTES {
            return None;
        }
        let pending = (self.count & 0xff) as usize;
        let total_bits = self.len * 8 + pending;
        if total_bits / 8 >= dst_capacity - CONTAINER_BYTES {
            return None;
        }
        Some(self.len + pending.div_ceil(8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_writers_match_bit_writers() {
        for total in [0usize, 1, 5, 40, 333] {
            let mut slow = BitWriter::with_capacity(0);
            let mut fast = WordWriter::with_capacity(total * 2);
            for i in 0..total {
                let width = (i % 12 + 1) as u32;
                let value = (i as u64 * 0x9e37) & ((1 << width) - 1);
                slow.add_bits(value, width);
                fast.add(value, width);
                if i % 4 == 3 {
                    assert!(fast.flush());
                }
            }
            assert_eq!(slow.close(1 << 16), fast.close(1 << 16), "{total}");
        }
    }

    #[test]
    fn top_writers_match_bit_writers() {
        for total in [0usize, 1, 5, 40, 333] {
            let mut slow = BitWriter::with_capacity(0);
            let mut fast = TopWriter::with_capacity(total * 2);
            for i in 0..total {
                let width = (i % 12 + 1) as u32;
                let value = (i as u32 * 0x9e37) & ((1 << width) - 1);
                slow.add_bits(u64::from(value), width);
                fast.add(TopWriter::entry(value, width));
                if i % 4 == 3 {
                    assert!(fast.flush());
                }
            }
            assert_eq!(slow.close(1 << 16), fast.close(1 << 16), "{total}");
        }
    }

    #[test]
    fn grouped_adds_equal_single_adds() {
        for total in [1usize, 5, 40, 333] {
            let mut single = TopWriter::with_capacity(total * 2);
            let mut grouped = TopWriter::with_capacity(total * 2);
            let entries: Vec<[u64; 2]> = (0..total)
                .map(|i| {
                    let width = (i % 11 + 1) as u32;
                    TopWriter::entry((i as u32 * 0x9e37) & ((1 << width) - 1), width)
                })
                .collect();
            for quint in entries.chunks(5) {
                for &entry in quint {
                    single.add(entry);
                }
                assert!(single.flush());
                match <[[u64; 2]; 5]>::try_from(quint) {
                    Ok(five) => grouped.add_group(five),
                    Err(_) => {
                        for &entry in quint {
                            grouped.add(entry);
                        }
                    }
                }
                assert!(grouped.flush());
            }
            assert_eq!(single.close(1 << 16), grouped.close(1 << 16), "{total}");
        }
    }

    #[test]
    fn reused_top_writers_append_whole_streams() {
        let mut writer = TopWriter::with_capacity(64);
        let mut out = alloc::vec![0xaa];
        for round in 0..3u32 {
            for i in 0..10 {
                writer.add(TopWriter::entry(i + round, 5));
            }
            assert!(writer.flush());
            writer.close_into(1 << 10, &mut out).unwrap();
        }
        // Three identical-length streams of 50 bits plus a mark: 7 bytes each.
        assert_eq!(out.len(), 1 + 3 * 7);
        // A failed close leaves the writer reusable.
        let mut tiny = TopWriter::with_capacity(64);
        assert!(tiny.close_into(8, &mut out).is_none());
        tiny.add(TopWriter::entry(1, 3));
        assert!(tiny.close_into(64, &mut out).is_some());
    }

    #[test]
    fn top_writers_fail_where_the_reference_does() {
        assert_eq!(TopWriter::entry(5, 0), [0, 0]);
        assert!(TopWriter::with_capacity(0).close(8).is_none());
        assert!(TopWriter::with_capacity(0).close(9).is_some());
        let mut full = TopWriter::with_capacity(8);
        for _ in 0..4 {
            full.add(TopWriter::entry(0xff, 8));
            assert!(full.flush());
        }
        assert!(full.close(12).is_none());
        // A buffer that cannot take another flush reports it.
        let mut small = TopWriter::with_capacity(0);
        let mut failed = false;
        for _ in 0..20 {
            small.add(TopWriter::entry(0xff, 8));
            failed |= !small.flush();
        }
        assert!(failed);
    }

    #[test]
    fn word_writers_apply_the_capacity_rule_and_report_a_full_buffer() {
        let mut writer = WordWriter::with_capacity(400);
        for _ in 0..200 {
            writer.add(0xffff, 16);
            assert!(writer.flush());
        }
        let out = writer.close(4096).unwrap();
        assert_eq!(out.len(), 401);
        assert!(WordWriter::with_capacity(0).close(8).is_none());
        assert!(WordWriter::with_capacity(0).close(9).is_some());
        let mut full = WordWriter::with_capacity(0);
        for _ in 0..4 {
            full.add(0xff, 8);
            assert!(full.flush());
        }
        assert!(full.close(12).is_none());
        // A buffer that cannot take another container says so, and a close
        // on it fails like the reference's overflow.
        let mut small = WordWriter::with_capacity(0);
        let mut failed = false;
        for _ in 0..20 {
            small.add(0xff, 8);
            if !small.flush() {
                failed = true;
                break;
            }
        }
        assert!(failed);
        assert!(small.close(4096).is_none());
        assert!(WordWriter::default().close(4096).is_none());
    }

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
