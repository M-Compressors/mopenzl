//! The `transpose_split` node 3 (`encode_transpose_binding.c`).
//!
//! A struct stream of `N` elements of `W` bytes becomes `W` serial streams of
//! `N` bytes: stream `j` holds byte `j` of every element. This is the
//! non-deprecated node; the pre-format-11 `transpose` and `transpose_split2/4/8`
//! nodes are never written at the supported format versions.

use alloc::vec::Vec;

use crate::compressor::core::codecs::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;

/// `EI_transpose_split`.
pub(crate) static TRANSPOSE_SPLIT: FnEncoder = FnEncoder(transpose_split);

fn transpose_split<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let width = input.width();
    let n_elts = input.n_elts();
    let mut lanes: Vec<Vec<u8>> = (0..width).map(|_| Vec::with_capacity(n_elts)).collect();
    for element in input.bytes().chunks_exact(width) {
        for (lane, &byte) in lanes.iter_mut().zip(element) {
            lane.push(byte);
        }
    }
    for lane in lanes {
        cx.emit(0, 1, lane)?;
    }
    Ok(())
}
