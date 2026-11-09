//! The sentinel nodes (`EI_sentinel_byte` and `EI_sentinel`,
//! `encode_sentinel_binding.c` and `encode_sentinel_kernel.c`), codec 18.
//!
//! Both replace exceptional values by a sentinel marker and move the original
//! values to an exceptions stream (see the reference's `spec.md`):
//!
//! * `sentinel_byte` narrows 2, 4 or 8-byte elements to one byte; every
//!   element of 255 or more becomes 255 and goes to the exceptions. It sends
//!   no header (the sentinel is the default, 255).
//! * `sentinel_num` keeps the element width and takes the exception positions
//!   from the copy parameter `ZL_SENTINEL_INDICES_PID` (130, an array of
//!   64-bit little-endian indices, increasing) and optionally the sentinel
//!   from `ZL_SENTINEL_VALUE_PID` (131, 8 bytes; default the width's maximum).
//!   The header is the sentinel as a varint unless it is the maximum.

use alloc::vec::Vec;

use super::super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::numeric::{elements, max_value_for_width, push_le};
use crate::shared::varint;

/// `ZL_SENTINEL_INDICES_PID`.
const INDICES_PID: i32 = crate::graph::params::sentinel::INDICES;
/// `ZL_SENTINEL_VALUE_PID`.
const VALUE_PID: i32 = crate::graph::params::sentinel::VALUE;

/// `ZL_NODE_SENTINEL_BYTE` (node 50).
pub(crate) static SENTINEL_BYTE: FnEncoder = FnEncoder(sentinel_byte);
/// `ZL_NODE_SENTINEL_NUM` (node 51).
pub(crate) static SENTINEL_NUM: FnEncoder = FnEncoder(sentinel_num);

/// `ZL_sentinelByteEncode`: the one-byte values and the exceptions.
fn byte_encode(bytes: &[u8], width: usize) -> (Vec<u8>, Vec<u8>) {
    let mut values = Vec::with_capacity(bytes.len() / width);
    let mut exceptions = Vec::new();
    for value in elements(bytes, width) {
        if value < 255 {
            values.push(value as u8);
        } else {
            values.push(255);
            push_le(&mut exceptions, value, width);
        }
    }
    (values, exceptions)
}

fn sentinel_byte<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let width = input.width();
    if width == 1 {
        return Err(NodeError::InvalidInput(
            "sentinel_byte does not accept 1-byte inputs",
        ));
    }
    let (values, exceptions) = byte_encode(input.bytes(), width);
    cx.emit(0, 1, values)?;
    cx.emit(1, width, exceptions)?;
    Ok(())
}

/// `ZL_sentinelEncode`: values with the sentinel at the exception positions,
/// and the original values there.
///
/// # Errors
///
/// A sentinel at a position that is not an exception, or indices that are
/// unordered or out of range (the reference's return codes 1 and 2).
fn encode(
    bytes: &[u8],
    width: usize,
    indices: &[u64],
    sentinel: u64,
) -> Result<(Vec<u8>, Vec<u8>), NodeError> {
    let mut values = Vec::with_capacity(bytes.len());
    let mut exceptions = Vec::with_capacity(indices.len() * width);
    let mut next = 0;
    for (position, value) in elements(bytes, width).enumerate() {
        if indices.get(next) == Some(&(position as u64)) {
            push_le(&mut values, sentinel, width);
            push_le(&mut exceptions, value, width);
            next += 1;
        } else if value == sentinel {
            return Err(NodeError::InvalidInput(
                "Sentinel encode validation failed",
            ));
        } else {
            push_le(&mut values, value, width);
        }
    }
    if next != indices.len() {
        return Err(NodeError::InvalidInput(
            "Sentinel encode validation failed",
        ));
    }
    Ok((values, exceptions))
}

fn sentinel_num<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let width = input.width();
    let indices: Vec<u64> = elements(
        cx.copy_param(INDICES_PID)
            .ok_or(NodeError::InvalidParam("Missing indices"))?,
        8,
    )
    .collect();
    let max_value = max_value_for_width(width);
    let mut sentinel = max_value;
    if let Some(bytes) = super::param_ref(cx, VALUE_PID) {
        let bytes: [u8; 8] = bytes
            .try_into()
            .map_err(|_| NodeError::InvalidParam("Sentinel value must be 64 bits"))?;
        sentinel = u64::from_le_bytes(bytes);
    }
    sentinel &= max_value;
    let (values, exceptions) = encode(input.bytes(), width, &indices, sentinel)?;
    if sentinel != max_value {
        let mut header = Vec::new();
        varint::push(&mut header, sentinel);
        cx.send_header(&header)?;
    }
    cx.emit(0, width, values)?;
    cx.emit(1, width, exceptions)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_values_become_exceptions() {
        let bytes: Vec<u8> = [3u32, 254, 255, 70_000]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let (values, exceptions) = byte_encode(&bytes, 4);
        assert_eq!(values, [3, 254, 255, 255]);
        assert_eq!(
            exceptions,
            [255u32, 70_000].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>()
        );
        assert_eq!(byte_encode(&[], 2), (Vec::new(), Vec::new()));
    }

    #[test]
    fn indices_mark_exceptions_and_are_validated() {
        let bytes = [1u8, 9, 3, 9, 5];
        assert_eq!(encode(&bytes, 1, &[1, 3], 200), Ok((alloc::vec![1, 200, 3, 200, 5], alloc::vec![9, 9])));
        assert_eq!(encode(&bytes, 1, &[], 200).map(|(v, _)| v), Ok(bytes.to_vec()));
        // The sentinel appears at a position that is not an exception.
        assert!(encode(&bytes, 1, &[1], 9).is_err());
        // Out of range, then unordered.
        assert!(encode(&bytes, 1, &[1, 9], 200).is_err());
        assert!(encode(&bytes, 1, &[3, 1], 200).is_err());
    }
}
