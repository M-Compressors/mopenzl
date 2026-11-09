//! The partition node (`EI_partition`, `encode_partition_binding.c`,
//! `common_partition.c` and `encode_partition_kernel.c`), codec 64.
//!
//! The value range is cut into up to 256 contiguous partitions. Each element
//! becomes the index of its partition (output 0, one byte per element) and
//! its offset inside the partition, written with `ceil(log2(size))` bits
//! (output 1, a little-endian bit stream, least significant bit first).
//!
//! Parameters: the int parameter `ZL_PARTITION_PRESET_PID` (120) selects a
//! built-in preset; otherwise the copy parameter `ZL_PARTITION_CUSTOM_PID`
//! (121) holds `startValue` then the partition sizes as little-endian 64-bit
//! numbers. The header is described in the reference's `spec.md`.
//!
//! The reference has specialized 16-bit and 32-bit kernels (lookup tables and
//! two-element or four-element packing) next to a generic one. They produce
//! the generic kernel's streams and fail on the same inputs (a value outside
//! the partitioned range), so this port has the generic kernel only.

use alloc::vec::Vec;

use super::super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::numeric::{elements, find_max_u8};
use crate::shared::varint;

/// `ZL_PARTITION_PRESET_PID`.
const PRESET_PID: i32 = crate::graph::params::partition::PRESET;
/// `ZL_PARTITION_CUSTOM_PID`.
const CUSTOM_PID: i32 = crate::graph::params::partition::CUSTOM;
/// `ZL_PARTITION_MAX_PARTITIONS`.
const MAX_PARTITIONS: usize = 256;
/// `ZL_PartitionParamsPreset_custom`: the number of presets.
const PRESET_CUSTOM: u8 = 3;

const HEADER_IS_PRESET_BIT: u8 = 0x4;
const HEADER_IS_FIRST_VALUE_ZERO_BIT: u8 = 0x8;
const HEADER_IS_POW2_BIT: u8 = 0x20;

/// `ZL_NODE_PARTITION` (node 48).
pub(crate) static PARTITION: FnEncoder = FnEncoder(partition);

/// `ZL_PartitionParams`: the first partition's start and every size.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PartitionParams {
    /// The first value of the first partition.
    pub(crate) start: u64,
    /// The size of each partition, in order.
    pub(crate) sizes: Vec<u64>,
}

impl PartitionParams {
    /// `ZL_PartitionParams_validate`: one to 256 non-empty partitions, a
    /// non-zero start when there is only one, and no overflow past 2^64 (the
    /// last partition may end exactly there).
    pub(crate) fn is_valid(&self) -> bool {
        if self.sizes.is_empty() || self.sizes.len() > MAX_PARTITIONS {
            return false;
        }
        if self.sizes.len() == 1 && self.start == 0 {
            return false;
        }
        let mut sum = self.start;
        for (index, &size) in self.sizes.iter().enumerate() {
            if size == 0 {
                return false;
            }
            let (next, overflow) = sum.overflowing_add(size);
            if overflow && !(index + 1 == self.sizes.len() && next == 0) {
                return false;
            }
            sum = next;
        }
        true
    }

    /// `ZL_PartitionParams_getPreset`.
    fn preset(preset: i32) -> Option<Self> {
        match preset {
            // Quantize offsets: sizes 1, 2, 4, ... 2^31 from 1.
            0 => Some(Self {
                start: 1,
                sizes: (0..32).map(|log| 1u64 << log).collect(),
            }),
            // Quantize lengths: sixteen ones, then 16, 32, ... 2^31.
            1 => Some(Self {
                start: 0,
                sizes: (0..16)
                    .map(|_| 1u64)
                    .chain((4..32).map(|log| 1u64 << log))
                    .collect(),
            }),
            // Varbyte16: 2, 2, 4, 8, ... 2^15.
            2 => Some(Self {
                start: 0,
                sizes: core::iter::once(2u64)
                    .chain((1..16).map(|log| 1u64 << log))
                    .collect(),
            }),
            _ => None,
        }
    }

    /// `ZL_PartitionParams_fromLocalParams`: the parameters and the preset
    /// number (3 for custom).
    fn from_node(cx: &NodeCx<'_, '_>) -> Result<(Self, u8), NodeError> {
        if let Some(preset) = cx.int(PRESET_PID)
            && let Some(params) = Self::preset(preset)
        {
            return Ok((params, preset as u8));
        }
        let Some(bytes) = cx.copy_param(CUSTOM_PID) else {
            return Err(NodeError::InvalidParam(
                "Neither preset nor custom partition found",
            ));
        };
        if bytes.len() % 8 != 0 || bytes.len() < 16 {
            return Err(NodeError::InvalidParam("invalid partition parameter size"));
        }
        let mut words = elements(bytes, 8);
        let start = words.next().unwrap_or(0);
        let params = Self {
            start,
            sizes: words.collect(),
        };
        if !params.is_valid() {
            return Err(NodeError::InvalidParam("invalid partition parameters"));
        }
        Ok((params, PRESET_CUSTOM))
    }

    /// `ZL_PartitionParams_computeBits`: `ceil(log2(size))` per partition.
    fn bits(&self) -> Vec<u8> {
        self.sizes.iter().map(|&size| next_pow2(size)).collect()
    }

    /// `ZL_PartitionParams_computeBasesU64`: the first value of each
    /// partition.
    fn bases(&self) -> Vec<u64> {
        let mut next = self.start;
        self.sizes
            .iter()
            .map(|&size| {
                let base = next;
                next = next.wrapping_add(size);
                base
            })
            .collect()
    }
}

/// The copy parameter bytes of a custom partition list (what
/// `ZL_Compressor_buildPartitionNode` stores): the start, then the sizes, as
/// little-endian 64-bit numbers.
pub(crate) fn custom_params_bytes(start: u64, sizes: &[u64]) -> Vec<u8> {
    core::iter::once(start)
        .chain(sizes.iter().copied())
        .flat_map(u64::to_le_bytes)
        .collect()
}

/// A least-significant-bit-first bit writer (`ZS_BitCStreamFF`).
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    container: u128,
    filled: u32,
}

impl BitWriter {
    /// Appends the low `bits` bits of `value`.
    fn write(&mut self, value: u64, bits: u32) {
        debug_assert!(bits <= 64);
        let mask = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
        self.container |= u128::from(value & mask) << self.filled;
        self.filled += bits;
        while self.filled >= 8 {
            self.bytes.push(self.container as u8);
            self.container >>= 8;
            self.filled -= 8;
        }
    }

    /// `ZS_BitCStreamFF_finish`: pads the last byte with zero bits.
    fn finish(mut self) -> Vec<u8> {
        if self.filled > 0 {
            self.bytes.push(self.container as u8);
        }
        self.bytes
    }
}

/// `ZL_partitionEncode`: the bucket of every element and the offset bits.
///
/// # Errors
///
/// A value outside the partitioned range.
fn encode(
    bytes: &[u8],
    width: usize,
    params: &PartitionParams,
) -> Result<(Vec<u8>, Vec<u8>), NodeError> {
    let bases = params.bases();
    let bits = params.bits();
    let last = params.sizes.len() - 1;
    let max_value = bases[last].wrapping_add(params.sizes[last] - 1);
    let mut buckets = Vec::with_capacity(bytes.len() / width);
    let mut stream = BitWriter::default();
    for value in elements(bytes, width) {
        if value < bases[0] || value > max_value {
            return Err(NodeError::InvalidInput("Value out of range of partitions"));
        }
        let partition = bases.partition_point(|&base| base <= value) - 1;
        buckets.push(partition as u8);
        stream.write(value - bases[partition], u32::from(bits[partition]));
    }
    Ok((buckets, stream.finish()))
}

/// `ZL_PartitionParams_sendHeader`: the codec header.
fn header(params: &PartitionParams, preset: u8, width: usize) -> Vec<u8> {
    let mut flags = width.trailing_zeros() as u8;
    if preset != PRESET_CUSTOM {
        return alloc::vec![flags | HEADER_IS_PRESET_BIT | (preset << 3)];
    }
    if params.start == 0 {
        flags |= HEADER_IS_FIRST_VALUE_ZERO_BIT;
    }
    let mut out = Vec::new();
    if params.sizes.iter().all(|size| size.is_power_of_two()) {
        flags |= HEADER_IS_POW2_BIT;
        let bits = params.bits();
        let max_bits = u32::from(find_max_u8(&bits)) + 1;
        let num_bits = max_bits.next_power_of_two().trailing_zeros().max(3);
        flags |= ((num_bits - 3) as u8) << 6;
        out.push(flags);
        if params.start != 0 {
            varint::push(&mut out, params.start);
        }
        let mut stream = BitWriter::default();
        for &log in &bits {
            stream.write(u64::from(log), num_bits);
        }
        stream.write(1, 1);
        out.extend_from_slice(&stream.finish());
    } else {
        out.push(flags);
        if params.start != 0 {
            varint::push(&mut out, params.start);
        }
        for &size in &params.sizes {
            varint::push(&mut out, size);
        }
    }
    out
}

fn partition<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let width = input.width();
    let (params, preset) = PartitionParams::from_node(cx)?;
    let (buckets, bits) = encode(input.bytes(), width, &params)?;
    cx.send_header(&header(&params, preset, width))?;
    cx.emit(0, 1, buckets)?;
    cx.emit(1, 1, bits)?;
    Ok(())
}

/// `ZL_nextPow2`: the `n` with `1 << n >= size`, which is 64 for sizes
/// above 2^63, where no `u64` power of two is large enough.
const fn next_pow2(size: u64) -> u8 {
    match size.checked_next_power_of_two() {
        Some(power) => power.trailing_zeros() as u8,
        None => 64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fuzz find (`node_roundtrip`): a custom partition above 2^63 made
    /// `u64::next_power_of_two` overflow; `ZL_nextPow2` gives 64 there.
    #[test]
    fn bit_widths_follow_the_reference_up_to_the_top_bit() {
        let cases = [
            (0, 0),
            (1, 0),
            (2, 1),
            (3, 2),
            (4, 2),
            (5, 3),
            (1 << 63, 63),
            ((1 << 63) + 1, 64),
            (u64::MAX, 64),
        ];
        for (size, bits) in cases {
            assert_eq!(next_pow2(size), bits, "{size}");
        }
    }

    fn params(start: u64, sizes: &[u64]) -> PartitionParams {
        PartitionParams {
            start,
            sizes: sizes.to_vec(),
        }
    }

    #[test]
    fn validation_follows_the_reference_rules() {
        assert!(!params(0, &[]).is_valid());
        assert!(!params(0, &[4]).is_valid());
        assert!(params(1, &[4]).is_valid());
        assert!(!params(0, &[4, 0]).is_valid());
        assert!(!params(0, &[1; 257]).is_valid());
        assert!(params(0, &[1; 256]).is_valid());
        assert!(!params(u64::MAX, &[2, 1]).is_valid());
        assert!(!params(u64::MAX, &[1, 1]).is_valid());
        // The last partition may end exactly at 2^64.
        assert!(params(u64::MAX, &[1]).is_valid());
        assert!(params(1 << 63, &[1 << 63]).is_valid());
        assert!(params(0, &[1 << 63, 1 << 63]).is_valid());
        assert!(!params(1, &[1 << 63, 1 << 63]).is_valid());
    }

    #[test]
    fn presets_have_the_documented_shapes() {
        let offsets = PartitionParams::preset(0).unwrap();
        assert_eq!((offsets.start, offsets.sizes.len()), (1, 32));
        let lengths = PartitionParams::preset(1).unwrap();
        assert_eq!((lengths.start, lengths.sizes.len()), (0, 44));
        assert_eq!(lengths.sizes[15..18], [1, 16, 32]);
        let varbyte = PartitionParams::preset(2).unwrap();
        assert_eq!(varbyte.sizes[..4], [2, 2, 4, 8]);
        assert_eq!(varbyte.sizes.len(), 16);
        assert!(PartitionParams::preset(3).is_none() && PartitionParams::preset(-1).is_none());
        for preset in 0..3 {
            assert!(PartitionParams::preset(preset).unwrap().is_valid());
        }
    }

    #[test]
    fn elements_are_split_into_buckets_and_offsets() {
        // The specification's example: start 10, sizes 4, 8, 4.
        let params = params(10, &[4, 8, 4]);
        let values: Vec<u8> = [12u16, 17, 23, 14].iter().flat_map(|v| v.to_le_bytes()).collect();
        let (buckets, bits) = encode(&values, 2, &params).unwrap();
        assert_eq!(buckets, [0, 1, 2, 1]);
        // Offsets 2, 3, 1, 0 in 2, 3, 2, 3 bits, least significant first.
        assert_eq!(bits, [0b0010_1110, 0]);
        let out_of_range: Vec<u8> = 9u16.to_le_bytes().to_vec();
        assert!(encode(&out_of_range, 2, &params).is_err());
        assert!(encode(&26u16.to_le_bytes(), 2, &params).is_err());
    }

    #[test]
    fn wide_offsets_use_up_to_sixty_four_bits() {
        let params = params(0, &[1 << 63, 1 << 63]);
        let values: Vec<u8> = [u64::MAX, 5].iter().flat_map(|v| v.to_le_bytes()).collect();
        let (buckets, bits) = encode(&values, 8, &params).unwrap();
        assert_eq!(buckets, [1, 0]);
        assert_eq!(bits.len(), 16);
        // 63 offset bits of 2^64 - 1, then the low bit of 5.
        assert_eq!(u64::from_le_bytes(bits[..8].try_into().unwrap()), u64::MAX);
        assert_eq!(u64::from_le_bytes(bits[8..].try_into().unwrap()), 2);
    }

    #[test]
    fn headers_pick_the_compact_form_when_they_can() {
        assert_eq!(header(&PartitionParams::preset(1).unwrap(), 1, 2), [0b1101]);
        // Power-of-two sizes: log2 values in 3-bit lanes plus a sentinel bit.
        let compact = header(&params(0, &[4, 8]), 3, 4);
        assert_eq!(compact, [0x20 | 0x8 | 2, 0b0101_1010]);
        // A non-power-of-two size falls back to varints.
        assert_eq!(header(&params(5, &[3, 4]), 3, 1), [0, 5, 3, 4]);
    }
}
