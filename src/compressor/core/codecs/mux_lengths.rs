//! The `mux_lengths` node (`EI_mux_lengths`, `encode_mux_lengths_binding.c`
//! and `encode_mux_lengths_kernel.c`), node 53, codec 65.
//!
//! Packs the literal length and the match length of each sequence into one
//! byte: `split_point` bits for the literal length (saturated) and the other
//! `8 - split_point` for the match length minus the bias (saturated). A
//! length that saturates its field also writes `length - mask` to the
//! overflow stream, literal length first. The codec header is the single
//! byte `split_point | (bias << 4)`.
//!
//! The reference has AVX2 and SVE2 kernels for 16-bit lengths; they produce
//! the same bytes as the scalar loop ported here.
//!
//! Parameters: `ZL_MUX_LENGTHS_SPLIT_POINT_PID` (0, chosen to minimise the
//! overflow count when unset) and `ZL_MUX_LENGTHS_MATCH_LENGTH_BIAS_PID`
//! (1, else the `lz` match-length metadata, else 0).

use alloc::vec::Vec;

use super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;

/// The node encoder.
pub(crate) static MUX_LENGTHS: FnEncoder = FnEncoder(encode);

/// `ZL_MUX_LENGTHS_SPLIT_POINT_PID`.
const SPLIT_POINT: i32 = crate::graph::params::mux_lengths::SPLIT_POINT;
/// `ZL_MUX_LENGTHS_MATCH_LENGTH_BIAS_PID`.
const MATCH_LENGTH_BIAS: i32 = crate::graph::params::mux_lengths::MATCH_LENGTH_BIAS;
/// `ZL_LZ_MIN_MATCH_LENGTH_METADATA_ID`: where `lz` records its minimum
/// match length on the match lengths.
const MIN_MATCH_LENGTH_METADATA_ID: i32 = crate::graph::params::lz::MIN_MATCH_LENGTH_METADATA_ID;

/// `ZL_readN`: a little-endian integer of `W` bytes.
#[inline(always)]
fn read<const W: usize>(bytes: &[u8], index: usize) -> u64 {
    let mut word = [0u8; 8];
    word[..W].copy_from_slice(&bytes[index * W..(index + 1) * W]);
    u64::from_le_bytes(word)
}

/// The literal and match length of element `index`, the latter minus the
/// bias with 64-bit wraparound.
#[inline(always)]
fn lengths<const W: usize>(ll: &[u8], ml: &[u8], index: usize, bias: u32) -> (u64, u64) {
    (
        read::<W>(ll, index),
        read::<W>(ml, index).wrapping_sub(u64::from(bias)),
    )
}

/// The masks of the literal and match fields.
const fn masks(split: u32) -> (u64, u64) {
    (
        ((1u32 << split) - 1) as u64,
        ((1u32 << (8 - split)) - 1) as u64,
    )
}

/// `ZL_muxLengthsComputeSplitPoint`: the split point (1 to 7) with the
/// fewest overflowing lengths, the lowest on ties, and that count.
fn compute_split(ll: &[u8], ml: &[u8], count: usize, width: usize, bias: u32) -> (u32, usize) {
    match width {
        1 => compute_split_w::<1>(ll, ml, count, bias),
        2 => compute_split_w::<2>(ll, ml, count, bias),
        4 => compute_split_w::<4>(ll, ml, count, bias),
        _ => compute_split_w::<8>(ll, ml, count, bias),
    }
}

/// [`compute_split`] for `W`-byte lengths.
fn compute_split_w<const W: usize>(ll: &[u8], ml: &[u8], count: usize, bias: u32) -> (u32, usize) {
    let mut ll_stats = [0usize; 256];
    let mut ml_stats = [0usize; 256];
    for i in 0..count {
        let (l, m) = lengths::<W>(ll, ml, i, bias);
        ll_stats[l.min(255) as usize] += 1;
        ml_stats[m.min(255) as usize] += 1;
    }
    let mut best_bits = 0;
    let mut best = 2 * count + 1;
    for bits in 1..8u32 {
        let (ll_mask, ml_mask) = masks(bits);
        let extra: usize = ll_stats[ll_mask as usize..].iter().sum::<usize>()
            + ml_stats[ml_mask as usize..].iter().sum::<usize>();
        if extra < best {
            best = extra;
            best_bits = bits;
        }
    }
    (best_bits, best)
}

/// `ZL_muxLengthsEncode`: the muxed bytes and the overflow stream.
fn mux(
    ll: &[u8],
    ml: &[u8],
    count: usize,
    width: usize,
    split: u32,
    bias: u32,
) -> (Vec<u8>, Vec<u8>) {
    match width {
        1 => mux_w::<1>(ll, ml, count, split, bias),
        2 => mux_w::<2>(ll, ml, count, split, bias),
        4 => mux_w::<4>(ll, ml, count, split, bias),
        _ => mux_w::<8>(ll, ml, count, split, bias),
    }
}

/// [`mux`] for `W`-byte lengths.
fn mux_w<const W: usize>(
    ll: &[u8],
    ml: &[u8],
    count: usize,
    split: u32,
    bias: u32,
) -> (Vec<u8>, Vec<u8>) {
    let (ll_mask, ml_mask) = masks(split);
    let mut muxed = Vec::with_capacity(count);
    let mut long = Vec::new();
    for i in 0..count {
        let (l, m) = lengths::<W>(ll, ml, i, bias);
        let mut byte = 0u8;
        // The literal length overflow goes first when both overflow.
        if l >= ll_mask {
            byte |= ll_mask as u8;
            long.extend_from_slice(&(l - ll_mask).to_le_bytes()[..W]);
        } else {
            byte |= l as u8;
        }
        if m >= ml_mask {
            byte |= (ml_mask << split) as u8;
            long.extend_from_slice(&(m - ml_mask).to_le_bytes()[..W]);
        } else {
            byte |= (m << split) as u8;
        }
        muxed.push(byte);
    }
    (muxed, long)
}

/// `EI_mux_lengths`.
fn encode<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let (ll_in, ml_in) = (&inputs[0], &inputs[1]);
    let width = ll_in.width();
    let count = ll_in.n_elts();
    if width != ml_in.width() {
        return Err(NodeError::InvalidInput(
            "literal and match lengths must have same element width",
        ));
    }
    if count != ml_in.n_elts() {
        return Err(NodeError::InvalidInput(
            "literal and match lengths must have same element count",
        ));
    }
    let bias = match cx.int(MATCH_LENGTH_BIAS) {
        Some(value) => value as u32,
        None => ml_in
            .metadata()
            .iter()
            .find(|&&(id, _)| id == MIN_MATCH_LENGTH_METADATA_ID)
            .map_or(0, |&(_, value)| value as u32),
    };
    if bias > 15 {
        return Err(NodeError::InvalidParam(
            "match_length_bias must be in [0, 15]",
        ));
    }
    let (ll, ml) = (ll_in.bytes(), ml_in.bytes());
    let split = match cx.int(SPLIT_POINT) {
        Some(value) => {
            let split = value as u32;
            if split > 8 {
                return Err(NodeError::InvalidParam("split_point must be in [0, 8]"));
            }
            split
        }
        None => compute_split(ll, ml, count, width, bias).0,
    };
    let (muxed, long) = mux(ll, ml, count, width, split, bias);
    cx.send_header(&[(split | (bias << 4)) as u8])?;
    cx.emit(0, 1, muxed)?;
    cx.emit(1, width, long)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::core::data::{Bytes, Type};
    use alloc::vec::Vec;

    fn le(values: &[u64], width: usize) -> Vec<u8> {
        values
            .iter()
            .flat_map(|v| v.to_le_bytes()[..width].to_vec())
            .collect()
    }

    #[test]
    fn small_lengths_pack_into_one_byte_and_large_ones_overflow() {
        let ll = le(&[1, 7, 3, 300], 2);
        let ml = le(&[4, 4, 100, 5], 2);
        // Split 3: the literal field holds 0..=7, the match field 0..=31.
        let (muxed, long) = mux(&ll, &ml, 4, 2, 3, 0);
        assert_eq!(
            muxed,
            [1 | (4 << 3), 7 | (4 << 3), 3 | (31 << 3), 7 | (5 << 3)]
        );
        // ll 7 - 7, ml 100 - 31, ll 300 - 7.
        assert_eq!(long, le(&[0, 69, 293], 2));
    }

    #[test]
    fn the_bias_is_subtracted_from_match_lengths_with_wraparound() {
        let ll = le(&[0, 0], 1);
        let ml = le(&[2, 9], 1);
        // 2 - 4 wraps around and saturates the field; 9 - 4 fits it.
        let (muxed, long) = mux(&ll, &ml, 2, 1, 4, 4);
        assert_eq!(muxed, [0xF0, 5 << 4]);
        // The wrapped value minus the mask 15, truncated to the width.
        assert_eq!(long, [(2u64.wrapping_sub(4).wrapping_sub(15)) as u8]);
    }

    #[test]
    fn the_degenerate_splits_send_one_length_entirely_to_the_overflow() {
        let ll = le(&[5], 1);
        let ml = le(&[9], 1);
        let (muxed, long) = mux(&ll, &ml, 1, 1, 0, 0);
        assert_eq!(muxed, [9]);
        assert_eq!(long, [5]);
        let (muxed, long) = mux(&ll, &ml, 1, 1, 8, 0);
        assert_eq!(muxed, [5]);
        assert_eq!(long, [9]);
    }

    #[test]
    fn the_split_point_minimises_overflow_and_takes_the_lowest_on_ties() {
        let ll = le(&[2; 10], 1);
        let ml = le(&[2; 10], 1);
        // Split 2: literal mask 3, match mask 63: nothing overflows.
        let (split, long) = compute_split(&ll, &ml, 10, 1, 0);
        assert_eq!((split, long), (2, 0));
        // Larger lengths push the choice to the field that needs them.
        let ll = le(&[40; 10], 1);
        let ml = le(&[1; 10], 1);
        assert_eq!(compute_split(&ll, &ml, 10, 1, 0), (6, 0));
        assert_eq!(compute_split(&[], &[], 0, 1, 0), (1, 0));
    }

    #[cfg(feature = "lz")]
    #[test]
    fn the_node_reads_the_bias_from_the_lz_metadata_and_a_parameter_overrides_it() {
        use crate::compressor::core::codecs::test_util::{run_node, text};
        let src = text(30_000, 3);
        let lz = run_node(52, &[], 3, &[Stream::serial(&src)]).unwrap();
        let [_, _, ll, ml] = &lz.singles[..] else {
            panic!("four outputs");
        };
        let inputs = [ll.clone(), ml.clone()];
        let out = run_node(53, &[], 3, &inputs).unwrap();
        let header = out.header.unwrap();
        assert_eq!(header[0] >> 4, 4, "ZL_LZ_MIN_MATCH from the metadata");
        let out = run_node(53, &[(1, 2), (0, 5)], 3, &inputs).unwrap();
        assert_eq!(out.header.unwrap(), [5 | (2 << 4)]);
        // Without metadata and parameters the bias is 0.
        let plain = [
            Stream::new(Type::Numeric, 2, Bytes::from(le(&[3, 4], 2))),
            Stream::new(Type::Numeric, 2, Bytes::from(le(&[5, 6], 2))),
        ];
        let out = run_node(53, &[], 3, &plain).unwrap();
        assert_eq!(out.header.unwrap()[0] >> 4, 0);
    }

    #[test]
    fn invalid_inputs_and_parameters_are_rejected() {
        use crate::compressor::core::codecs::test_util::run_node;
        let make = |width, values: &[u64]| {
            Stream::new(Type::Numeric, width, Bytes::from(le(values, width)))
        };
        let a = [make(2, &[1, 2]), make(4, &[1, 2])];
        assert!(matches!(
            run_node(53, &[], 3, &a),
            Err(NodeError::InvalidInput(_))
        ));
        let b = [make(2, &[1, 2]), make(2, &[1])];
        assert!(matches!(
            run_node(53, &[], 3, &b),
            Err(NodeError::InvalidInput(_))
        ));
        let ok = [make(2, &[1, 2]), make(2, &[3, 4])];
        for ints in [[(1, 16)], [(1, -1)], [(0, 9)], [(0, -1)]] {
            assert!(matches!(
                run_node(53, &ints, 3, &ok),
                Err(NodeError::InvalidParam(_))
            ));
        }
    }

    #[test]
    fn every_width_reads_and_writes_whole_elements() {
        for width in [1usize, 2, 4, 8] {
            let ll = le(&[1, 200], width);
            let ml = le(&[3, 250], width);
            let (muxed, long) = mux(&ll, &ml, 2, width, 4, 1);
            assert_eq!(muxed.len(), 2);
            assert_eq!(long.len() % width, 0);
            let value = match width {
                1 => read::<1>(&ll, 1),
                2 => read::<2>(&ll, 1),
                4 => read::<4>(&ll, 1),
                _ => read::<8>(&ll, 1),
            };
            assert_eq!(value, 200);
        }
    }
}
