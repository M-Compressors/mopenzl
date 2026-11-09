//! The range-pack node (`EI_rangePack`, `encode_range_pack_binding.c` and
//! `encode_range_pack_kernel.c`), codec 35.
//!
//! Subtracts the minimum and narrows the elements to the smallest width that
//! holds the range. The header is the source width followed, when the minimum
//! is not zero, by the minimum as a source-width little-endian number.

use alloc::vec::Vec;

use super::super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::numeric::{elements, push_le, width_for_value};

/// `ZL_NODE_RANGE_PACK` (node 28).
pub(crate) static RANGE_PACK: FnEncoder = FnEncoder(range_pack);

/// `ZL_computeUnsignedRange`: the smallest and largest element, `(0, 0)` for
/// an empty stream.
fn range(bytes: &[u8], width: usize) -> (u64, u64) {
    let mut values = elements(bytes, width);
    let Some(first) = values.next() else {
        return (0, 0);
    };
    values.fold((first, first), |(min, max), value| {
        (min.min(value), max.max(value))
    })
}

fn range_pack<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let width = input.width();
    let (min, max) = range(input.bytes(), width);
    let dst_width = width_for_value(max - min);
    let mut out = Vec::with_capacity(input.n_elts() * dst_width);
    for value in elements(input.bytes(), width) {
        push_le(&mut out, value - min, dst_width);
    }
    cx.emit(0, dst_width, out)?;
    let mut header = Vec::with_capacity(1 + width);
    header.push(width as u8);
    if min != 0 {
        push_le(&mut header, min, width);
    }
    cx.send_header(&header)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_range_of_an_empty_stream_is_zero() {
        assert_eq!(range(&[], 4), (0, 0));
        assert_eq!(range(&[9, 0, 3, 0], 2), (3, 9));
        assert_eq!(range(&[0xff; 8], 8), (u64::MAX, u64::MAX));
    }
}
