//! The integer delta node (`EI_delta_int`, `encode_delta_binding.c` and
//! `encode_delta_kernel.c`), codec 1.
//!
//! The first element travels in the codec header (format version 13 and
//! later, so always here) and the output holds the `n - 1` wrapping
//! differences that follow. An empty input has no header and an empty
//! output. Before version 13 the reference put the first element in the
//! stream; formats below 21 are not supported by this crate.

use alloc::vec::Vec;

use super::super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::numeric::{elements, push_le};

/// `ZL_NODE_DELTA_INT` (node 2).
pub(crate) static DELTA_INT: FnEncoder = FnEncoder(delta_int);

/// The deltas of `bytes` and its first element, `ZS_deltaEncode`: each
/// difference wraps at the element width.
fn encode(bytes: &[u8], width: usize) -> Option<(u64, Vec<u8>)> {
    let mut values = elements(bytes, width);
    let first = values.next()?;
    let mut deltas = Vec::with_capacity(bytes.len() - width);
    let mut previous = first;
    for value in values {
        push_le(&mut deltas, value.wrapping_sub(previous), width);
        previous = value;
    }
    Some((first, deltas))
}

fn delta_int<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let width = input.width();
    match encode(input.bytes(), width) {
        None => {
            cx.emit(0, width, Vec::new())?;
        }
        Some((first, deltas)) => {
            let mut header = Vec::with_capacity(width);
            push_le(&mut header, first, width);
            cx.send_header(&header)?;
            cx.emit(0, width, deltas)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn differences_wrap_at_the_element_width() {
        assert_eq!(encode(&[], 1), None);
        assert_eq!(encode(&[7], 1), Some((7, Vec::new())));
        assert_eq!(encode(&[250, 3, 4], 1), Some((250, alloc::vec![9, 1])));
        let wide = [1u64, 0, u64::MAX].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>();
        let (first, deltas) = encode(&wide, 8).unwrap();
        assert_eq!(first, 1);
        assert_eq!(deltas, [u64::MAX, u64::MAX].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
        let (first, deltas) = encode(&[0, 1, 0, 0, 0xff, 0xff], 2).unwrap();
        assert_eq!((first, deltas), (0x100, alloc::vec![0x00, 0xff, 0xff, 0xff]));
    }
}
