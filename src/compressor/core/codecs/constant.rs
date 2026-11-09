//! The constant nodes (`codecs/constant/encode_constant_binding.c`):
//! `constant_serial` (70) and `constant_fixed` (71).
//!
//! A stream whose elements are all equal becomes that one element plus a
//! header holding the element count as a varint. Anything else, and an empty
//! stream, is an error: the graphs only route verified constants here.

use super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::varint;

/// The `constant_serial` and `constant_fixed` encoder (`EI_constant_typed`).
pub(crate) static CONSTANT: FnEncoder = FnEncoder(encode);

/// `ZS_isConstantStream`: at least one element, all equal.
pub(crate) fn is_constant(src: &[u8], n_elts: usize, elt_width: usize) -> bool {
    n_elts != 0
        && elt_width != 0
        && src[..n_elts * elt_width]
            .chunks_exact(elt_width)
            .all(|elt| elt == &src[..elt_width])
}

/// `EI_constant_typed`.
fn encode<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let src = input.bytes();
    let n_elts = input.n_elts();
    let elt_width = input.width();
    if n_elts < 1 {
        return Err(NodeError::InvalidInput(
            "a constant stream needs an element",
        ));
    }
    if !is_constant(src, n_elts, elt_width) {
        return Err(NodeError::InvalidInput("the stream is not constant"));
    }
    let (header, len) = varint::encode(n_elts as u64);
    cx.send_header(&header[..len])?;
    cx.emit(0, elt_width, src[..elt_width].to_vec())?;
    Ok(())
}
