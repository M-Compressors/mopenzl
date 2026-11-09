//! The bit-split nodes (`encode_bitSplit_binding.c`,
//! `encode_bitSplit_kernel.c`, `encode_bitsplit_{top8,fp,bf16}_binding.c`),
//! codec 63.
//!
//! A numeric stream is cut into bit fields, least significant first; each
//! field becomes its own numeric output of the smallest width that holds it
//! (1 to 8 bits: one byte, up to 16: two, up to 32: four, else eight). The
//! header is the input element width followed by the field widths, without
//! the last one when the fields cover the whole element. Fields may cover
//! less than the element only when the remaining top bits are all zero.
//!
//! * `bitSplit` (private node 75) takes the field widths from the copy
//!   parameter 701 (one byte each, 1 to 64 of them);
//! * `bitsplit_top8` splits the largest element's significant bits into the
//!   low remainder and the top 8 bits;
//! * `bitsplit_fp` and `bitsplit_bf16` use the IEEE binary16/32/64 and
//!   bfloat16 mantissa, exponent and sign widths.
//!
//! The reference's special kernels for the floating-point layouts write what
//! the generic kernel writes, so this port has the generic one only.

use alloc::vec::Vec;

use super::super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::numeric::{elements, push_le};

/// `ZL_BITSPLIT_WIDTHS_PID`.
const WIDTHS_PID: i32 = 701;
/// The most fields one split takes.
const MAX_FIELDS: usize = 64;

/// `bit_split` (private node 75).
pub(crate) static BITSPLIT: FnEncoder = FnEncoder(bit_split);
/// `ZL_NODE_BITSPLIT_TOP8` (node 45).
pub(crate) static BITSPLIT_TOP8: FnEncoder = FnEncoder(top8);
/// `ZL_NODE_BITSPLIT_FP` (node 46).
pub(crate) static BITSPLIT_FP: FnEncoder = FnEncoder(fp);
/// `ZL_NODE_BITSPLIT_BF16` (node 47).
pub(crate) static BITSPLIT_BF16: FnEncoder = FnEncoder(bf16);

/// `ZL_bitSplit_outputEltWidth`.
const fn output_width(bits: u8) -> usize {
    match bits {
        0..=8 => 1,
        9..=16 => 2,
        17..=32 => 4,
        _ => 8,
    }
}

/// `ZL_bitSplit_paramsAreValid`: the sum of the widths, or `None` when one
/// is zero or the sum exceeds the element's bits.
fn checked_sum(widths: &[u8], element_bits: usize) -> Option<usize> {
    if widths.contains(&0) {
        return None;
    }
    let sum: usize = widths.iter().map(|&width| usize::from(width)).sum();
    (sum <= element_bits).then_some(sum)
}

/// `ZL_bitSplitEncode`: one stream of fields per width.
fn split(bytes: &[u8], width: usize, widths: &[u8]) -> Vec<Vec<u8>> {
    let mut outputs: Vec<Vec<u8>> = widths.iter().map(|_| Vec::new()).collect();
    for value in elements(bytes, width) {
        let mut position = 0u32;
        for (output, &bits) in outputs.iter_mut().zip(widths) {
            let mask = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
            push_le(output, (value >> position) & mask, output_width(bits));
            position += u32::from(bits);
        }
    }
    outputs
}

/// `EI_bitSplit_withWidths`.
fn with_widths<'a>(
    cx: &mut NodeCx<'_, 'a>,
    input: &Stream<'a>,
    widths: &[u8],
) -> Result<(), NodeError> {
    if widths.is_empty() {
        return Err(NodeError::InvalidParam(
            "bitSplit requires at least one bit width",
        ));
    }
    if widths.len() > MAX_FIELDS {
        return Err(NodeError::InvalidParam(
            "bitSplit supports at most 64 bit widths",
        ));
    }
    let width = input.width();
    let element_bits = 8 * width;
    let sum = checked_sum(widths, element_bits).ok_or(NodeError::InvalidParam(
        "bitSplit parameter validation failed",
    ))?;
    if sum < element_bits {
        let mask = !((1u64 << sum) - 1);
        if elements(input.bytes(), width).any(|value| value & mask != 0) {
            return Err(NodeError::InvalidInput(
                "bitSplit: top bits must be zero for partial coverage",
            ));
        }
    }
    let mut header = Vec::with_capacity(1 + widths.len());
    header.push(width as u8);
    let kept = if sum == element_bits {
        widths.len() - 1
    } else {
        widths.len()
    };
    header.extend_from_slice(&widths[..kept]);
    cx.send_header(&header)?;
    for (output, &bits) in split(input.bytes(), width, widths).into_iter().zip(widths) {
        cx.emit(0, output_width(bits), output)?;
    }
    Ok(())
}

/// `EI_bitSplit`.
fn bit_split<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let widths = cx.copy_param(WIDTHS_PID).ok_or(NodeError::InvalidParam(
        "bitSplit requires bit widths parameter",
    ))?;
    let widths = widths.to_vec();
    with_widths(cx, &inputs[0], &widths)
}

/// `EI_bitsplit_top8`.
fn top8<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let max = elements(input.bytes(), input.width()).max().unwrap_or(0);
    let effective = 64 - max.leading_zeros() as u8;
    if effective <= 8 {
        with_widths(cx, input, &[effective.max(1)])
    } else {
        with_widths(cx, input, &[effective - 8, 8])
    }
}

/// `EI_bitsplit_fp`.
fn fp<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let widths: [u8; 3] = match input.width() {
        2 => [10, 5, 1],
        4 => [23, 8, 1],
        8 => [52, 11, 1],
        _ => {
            return Err(NodeError::InvalidInput(
                "bitsplit_fp requires IEEE 754 floats (2, 4, or 8-byte elements)",
            ));
        }
    };
    with_widths(cx, input, &widths)
}

/// `EI_bitsplit_bf16`.
fn bf16<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    if input.width() != 2 {
        return Err(NodeError::InvalidInput(
            "bitsplit_bf16 requires 2-byte elements (bfloat16)",
        ));
    }
    with_widths(cx, input, &[7, 8, 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_widths_follow_the_field_size() {
        assert_eq!(
            [1u8, 8, 9, 16, 17, 32, 33, 64].map(output_width),
            [1, 1, 2, 2, 4, 4, 8, 8]
        );
    }

    #[test]
    fn widths_are_validated_against_the_element() {
        assert_eq!(checked_sum(&[4, 4], 8), Some(8));
        assert_eq!(checked_sum(&[4, 3], 8), Some(7));
        assert_eq!(checked_sum(&[4, 5], 8), None);
        assert_eq!(checked_sum(&[4, 0], 8), None);
    }

    #[test]
    fn fields_are_cut_least_significant_first() {
        let bytes: Vec<u8> = [0xabcdu16, 0x0123].iter().flat_map(|v| v.to_le_bytes()).collect();
        let outputs = split(&bytes, 2, &[4, 4, 8]);
        assert_eq!(outputs, [alloc::vec![0xd, 0x3], alloc::vec![0xc, 0x2], alloc::vec![0xab, 0x01]]);
        // A 52/11/1 split of binary64.
        let value = (1u64 << 63) | (0x3ffu64 << 52) | 0x5_0000_0000_0001;
        let outputs = split(&value.to_le_bytes(), 8, &[52, 11, 1]);
        assert_eq!(outputs[0], (value & ((1 << 52) - 1)).to_le_bytes());
        assert_eq!(outputs[1], 0x3ffu16.to_le_bytes());
        assert_eq!(outputs[2], [1]);
        // A 64-bit field takes the whole element.
        assert_eq!(split(&u64::MAX.to_le_bytes(), 8, &[64])[0], u64::MAX.to_le_bytes());
    }
}
