//! Decoder of the `partition` feature: `partition` (64).
//!
//! Ports `codecs/partition/decode_partition_binding.c`,
//! `decode_partition_kernel.c` and the header parser of
//! `common_partition.c`; the format is in `codecs/partition/spec.md`.
//!
//! The values were split into contiguous ranges (partitions). Input 0 holds
//! one bucket ID byte per value, input 1 a bit stream of the offsets inside
//! the buckets, `ceil(log2(size))` bits each. A value is `base[bucket] +
//! offset`, written as a little-endian number of the header's width.
//!
//! Not ported: the decoder fusion `partition + bitpack_int`
//! (`decode_partition_bitpack_fusion.c`), which only speeds up the same
//! output.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;
use crate::shared::varint;

/// `ZL_StandardTransformID_partition`: bucket IDs and offset bits back to
/// numbers.
pub(crate) const PARTITION: Desc = Desc {
    id: 64,
    min_version: 24,
    regens: &[Type::Numeric],
    variable_regens: false,
    inputs: &[Type::Numeric, Type::Serial],
    variable_inputs: 0,
    decode,
};

/// `ZL_PARTITION_MAX_PARTITIONS`.
const MAX_PARTITIONS: usize = 256;

/// The partitions: where they start and how many offset bits each has.
struct Partitions {
    bases: [u64; MAX_PARTITIONS],
    bits: [u8; MAX_PARTITIONS],
    count: usize,
}

impl Partitions {
    /// Partitions from `start` and their `sizes`
    /// (`ZL_PartitionParams_validate`, `computeBasesU64`, `computeBits`).
    fn new(start: u64, sizes: &[u64]) -> Result<Self, DecodeError> {
        if sizes.is_empty() || sizes.len() > MAX_PARTITIONS || (sizes.len() == 1 && start == 0) {
            return Err(corrupt());
        }
        let mut this = Self {
            bases: [0; MAX_PARTITIONS],
            bits: [0; MAX_PARTITIONS],
            count: sizes.len(),
        };
        let mut next = start;
        for (i, &size) in sizes.iter().enumerate() {
            if size == 0 {
                return Err(corrupt());
            }
            this.bases[i] = next;
            this.bits[i] = if size <= 1 {
                0
            } else {
                (64 - (size - 1).leading_zeros()) as u8
            };
            let (sum, wrapped) = next.overflowing_add(size);
            // Only the last partition may end exactly at 2^64.
            if wrapped && !(i + 1 == sizes.len() && sum == 0) {
                return Err(corrupt());
            }
            next = sum;
        }
        Ok(this)
    }

    /// One of the built-in presets (`ZL_PartitionParams_getPreset`).
    fn preset(id: u8) -> Result<Self, DecodeError> {
        let mut sizes = [0u64; 44];
        let (start, len) = match id {
            0 => {
                for (i, size) in sizes.iter_mut().take(32).enumerate() {
                    *size = 1 << i;
                }
                (1, 32)
            }
            1 => {
                sizes[..16].fill(1);
                for (i, size) in sizes.iter_mut().enumerate().skip(16) {
                    *size = 1 << (i - 12);
                }
                (0, 44)
            }
            2 => {
                sizes[0] = 2;
                for (i, size) in sizes.iter_mut().enumerate().take(16).skip(1) {
                    *size = 1 << i;
                }
                (0, 16)
            }
            _ => return Err(corrupt()),
        };
        Self::new(start, &sizes[..len])
    }
}

/// Reads fields of up to 64 bits, least significant bit first, from a
/// byte slice (`ZS_BitDStreamFF`). Reading past the end is an error.
struct Bits<'a> {
    src: &'a [u8],
    position: usize,
}

impl<'a> Bits<'a> {
    const fn new(src: &'a [u8]) -> Self {
        Self { src, position: 0 }
    }

    /// The next `count` bits (at most 64).
    fn read(&mut self, count: u32) -> Result<u64, DecodeError> {
        let end = self
            .position
            .checked_add(count as usize)
            .filter(|&end| end <= self.src.len().saturating_mul(8))
            .ok_or_else(corrupt)?;
        if count == 0 {
            return Ok(0);
        }
        let (byte, shift) = (self.position >> 3, self.position & 7);
        let rest = self.src.get(byte..).unwrap_or_default();
        let mut window = [0u8; 16];
        let take = rest.len().min(16);
        window[..take].copy_from_slice(&rest[..take]);
        let mask = if count == 64 {
            u64::MAX
        } else {
            (1u64 << count) - 1
        };
        self.position = end;
        Ok(((u128::from_le_bytes(window) >> shift) as u64) & mask)
    }
}

/// Parses the codec header: the output width and the partitions
/// (`ZL_PartitionParams_parseHeader`).
fn parse_header(header: &[u8]) -> Result<(usize, Partitions), DecodeError> {
    let (&flags, mut rest) = header.split_first().ok_or_else(corrupt)?;
    let width = 1usize << (flags & 3);
    if flags & 0x04 != 0 {
        return Ok((width, Partitions::preset(flags >> 3)?));
    }
    let read_varint = |rest: &mut &[u8]| -> Result<u64, DecodeError> {
        let (value, used) = varint::decode(rest).map_err(|_| corrupt())?;
        *rest = &rest[used..];
        Ok(value)
    };
    let start = if flags & 0x08 != 0 {
        0
    } else {
        read_varint(&mut rest)?
    };
    let mut sizes = [0u64; MAX_PARTITIONS];
    let count;
    if flags & 0x20 != 0 {
        // Sizes are powers of two, stored as `log2` fields of `numBits`
        // bits, then a sentinel one bit.
        let num_bits = u32::from(flags >> 6) + 3;
        let last = *rest.last().ok_or_else(corrupt)?;
        if last == 0 {
            return Err(corrupt());
        }
        let unused = 8 - last.ilog2() as usize;
        let total = 8 * rest.len() - unused;
        if !total.is_multiple_of(num_bits as usize) {
            return Err(corrupt());
        }
        count = total / num_bits as usize;
        if count > MAX_PARTITIONS {
            return Err(corrupt());
        }
        let mut bits = Bits::new(rest);
        for size in &mut sizes[..count] {
            *size = 1u64 << bits.read(num_bits)?;
        }
    } else {
        let mut n = 0;
        while !rest.is_empty() {
            if n >= MAX_PARTITIONS {
                return Err(corrupt());
            }
            sizes[n] = read_varint(&mut rest)?;
            n += 1;
        }
        count = n;
    }
    Ok((width, Partitions::new(start, &sizes[..count])?))
}

/// `DI_partition`.
fn decode(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let (buckets, offsets) = (&inputs[0], &inputs[1]);
    if buckets.width != 1 {
        return Err(corrupt());
    }
    let (width, partitions) = parse_header(ctx.header)?;
    let out = regens[0].fixed(width, buckets.bytes.len())?;
    match width {
        1 => fill::<1>(out, buckets.bytes, offsets.bytes, &partitions),
        2 => fill::<2>(out, buckets.bytes, offsets.bytes, &partitions),
        4 => fill::<4>(out, buckets.bytes, offsets.bytes, &partitions),
        _ => fill::<8>(out, buckets.bytes, offsets.bytes, &partitions),
    }
}

/// `ZL_partitionDecode`: one value per bucket ID, `W` bytes each.
fn fill<const W: usize>(
    out: &mut alloc::vec::Vec<u8>,
    buckets: &[u8],
    offsets: &[u8],
    partitions: &Partitions,
) -> Result<(), DecodeError> {
    let mut bits = Bits::new(offsets);
    for &bucket in buckets {
        let bucket = usize::from(bucket);
        if bucket >= partitions.count {
            return Err(corrupt());
        }
        let offset = bits.read(u32::from(partitions.bits[bucket]))?;
        let value = partitions.bases[bucket].wrapping_add(offset);
        out.extend_from_slice(&value.to_le_bytes()[..W]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn run(
        header: &[u8],
        buckets: &[u8],
        offsets: &[u8],
        budget: usize,
    ) -> Result<Vec<u8>, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, header);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::Numeric, budget);
        let inputs = [
            Input {
                ty: Type::Numeric,
                width: 1,
                bytes: buckets,
                lens: &[],
            },
            Input::serial(offsets),
        ];
        (PARTITION.decode)(&mut ctx, &inputs, &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream.bytes().to_vec())
    }

    /// The example of the specification: start 10, sizes 4, 8, 4.
    #[test]
    fn general_headers_follow_the_specification_example() {
        // flags: width 1, start present, not pow2; varints 10, 4, 8, 4.
        let header = [0u8, 10, 4, 8, 4];
        // Offsets 2, 3, 1, 0 in 2, 3, 2, 3 bits: 10 | 011<<2 | 01<<5 | 000<<7.
        let offsets = [0b0010_1110, 0b0000_0000];
        let out = run(&header, &[0, 1, 2, 1], &offsets, 16).unwrap();
        assert_eq!(out, [12, 17, 23, 14]);
    }

    #[test]
    fn power_of_two_headers_store_log2_sizes() {
        // Width 2, first value zero, pow2, 3-bit fields 1, 2, 3 (sizes 2, 4,
        // 8), then a sentinel: 001 010 011 1 = 0b1_011_010_001.
        let flags = 1 | 0x08 | 0x20;
        let header = [flags, 0xd1, 0x02];
        // Bucket 2 offset 5 (3 bits), bucket 1 offset 3 (2 bits).
        let offsets = [0b0001_1101];
        let out = run(&header, &[2, 1], &offsets, 16).unwrap();
        assert_eq!(out, [(6 + 5), 0, (2 + 3), 0]);
    }

    #[test]
    fn presets_cover_their_ranges() {
        // Quantize offsets: bucket b holds [2^b, 2^(b+1)); width 4.
        let out = run(&[2 | 4], &[0, 3], &[0b0000_0101, 0], 16).unwrap();
        assert_eq!(out, [1, 0, 0, 0, 8 + 5, 0, 0, 0]);
        // Quantize lengths: buckets 0..16 are single values; bucket 16
        // holds [16, 32).
        let out = run(&[8 | 4], &[3, 16], &[0b0000_1010], 16).unwrap();
        assert_eq!(out, [3, 16 + 10]);
        // Varbyte16: [0, 2), [2, 4), [4, 8).
        let out = run(&[2 << 3 | 4 | 1], &[0, 1, 2], &[0b0000_1101], 32).unwrap();
        assert_eq!(out, [1, 0, 2, 0, 7, 0]);
        // Unknown presets.
        assert!(run(&[3 << 3 | 4], &[], &[], 16).is_err());
    }

    #[test]
    fn eight_byte_values_use_all_partitions_to_the_end_of_the_range() {
        // One partition [1, 2^64) cannot be built, but [0, 2^63) + [2^63, 2^64) can.
        let mut header = alloc::vec![3 | 0x08];
        for _ in 0..2 {
            header.extend([0x80u8; 9]);
            header.push(1);
        }
        // Bucket 1 offset 2^63 - 1: 63 bits of ones, bucket 0 offset 0 uses none... 63 bits each.
        let mut offsets = [0xffu8; 8];
        offsets[7] = 0x7f;
        let out = run(&header, &[1], &offsets, 16).unwrap();
        assert_eq!(out, u64::MAX.to_le_bytes());
    }

    #[test]
    fn invalid_headers_are_rejected() {
        let empty: [u8; 0] = [];
        // No header, no sizes, a zero size, a single no-op partition.
        assert!(run(&[], &[], &empty, 16).is_err());
        assert!(run(&[0x08], &[], &empty, 16).is_err());
        assert!(run(&[0x08, 0], &[], &empty, 16).is_err());
        assert!(run(&[0x08, 4], &[], &empty, 16).is_err());
        // Truncated varint; overflow of the range; too many sizes.
        assert!(run(&[0, 0x80], &[], &empty, 16).is_err());
        let half = [0x80u8, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01];
        let mut wrap = alloc::vec![0x08u8];
        wrap.extend(half);
        wrap.extend(half);
        wrap.push(1);
        assert!(run(&wrap, &[], &empty, 16).is_err());
        wrap.pop();
        assert!(run(&wrap, &[], &empty, 16).is_ok());
        let mut many = alloc::vec![0x08u8];
        many.extend([1u8; 257]);
        assert!(run(&many, &[], &empty, 16).is_err());
        // Power-of-two streams: missing, no sentinel, not a multiple.
        assert!(run(&[0x28], &[], &empty, 16).is_err());
        assert!(run(&[0x28, 0], &[], &empty, 16).is_err());
        assert!(run(&[0x28, 0b0000_0110], &[], &empty, 16).is_err());
        // 300 fields of 3 bits.
        let mut big = alloc::vec![0x28u8];
        big.extend([0u8; 112]);
        big.push(1);
        assert!(run(&big, &[], &empty, 16).is_err());
    }

    #[test]
    fn invalid_streams_are_rejected() {
        let header = [0u8, 10, 4, 8, 4];
        assert!(run(&header, &[3], &[0], 16).is_err());
        // Offsets run out, and the output budget.
        assert!(run(&header, &[1, 1, 1], &[0], 16).is_err());
        assert!(run(&header, &[0; 5], &[0; 4], 4).is_err());
        // An empty value stream needs nothing.
        assert!(run(&header, &[], &[], 16).unwrap().is_empty());
        // Bucket IDs are bytes.
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &header);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::Numeric, 16);
        let wide = Input {
            ty: Type::Numeric,
            width: 2,
            bytes: &[0, 0],
            lens: &[],
        };
        assert!((PARTITION.decode)(&mut ctx, &[wide, Input::serial(&[])], &mut regens).is_err());
    }

    #[test]
    fn bit_reader_handles_wide_fields_and_the_end() {
        let data = [0xffu8; 9];
        let mut bits = Bits::new(&data);
        assert_eq!(bits.read(3).unwrap(), 7);
        assert_eq!(bits.read(64).unwrap(), u64::MAX);
        assert!(bits.read(6).is_err());
        assert_eq!(bits.read(5).unwrap(), 31);
        assert_eq!(bits.read(0).unwrap(), 0);
    }
}
