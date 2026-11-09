//! The `interleave_string` node 39 (`encode_interleave_binding.c`).
//!
//! Round-robin interleaving of the strings of up to 512 string inputs that
//! all hold the same number of strings: string 0 of input 0, string 0 of
//! input 1, ..., string 1 of input 0, and so on. The codec header is the
//! number of inputs as a native-endian `u32`.

use alloc::vec::Vec;

use crate::compressor::core::codecs::FnEncoder;
use crate::compressor::core::data::{Stream, Type};
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;

/// `ZL_INTERLEAVE_MAX_INPUTS`.
const MAX_INPUTS: usize = 512;

/// `EI_interleave`.
pub(crate) static INTERLEAVE: FnEncoder = FnEncoder(interleave);

fn interleave<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    if inputs.is_empty() {
        return Err(NodeError::InvalidInput("need at least one input"));
    }
    if inputs.len() > MAX_INPUTS {
        return Err(NodeError::InvalidInput(
            "too many inputs, only up to 512 are supported",
        ));
    }
    if inputs.iter().any(|input| input.ty() != Type::String) {
        return Err(NodeError::InvalidInput("only string inputs are supported"));
    }
    let strings_per_input = inputs[0].n_elts();
    if inputs.iter().any(|input| input.n_elts() != strings_per_input) {
        return Err(NodeError::InvalidInput(
            "all inputs must have the same number of strings",
        ));
    }
    cx.send_header(&(inputs.len() as u32).to_ne_bytes())?;

    let total: usize = inputs.iter().map(Stream::content_size).sum();
    let mut content = Vec::with_capacity(total);
    let mut lens = Vec::with_capacity(strings_per_input * inputs.len());
    let mut rests: Vec<&[u8]> = inputs.iter().map(Stream::bytes).collect();
    for i in 0..strings_per_input {
        for (input, rest) in inputs.iter().zip(rests.iter_mut()) {
            let len = input.lens()[i];
            let (string, tail) = rest.split_at(len as usize);
            content.extend_from_slice(string);
            lens.push(len);
            *rest = tail;
        }
    }
    cx.emit_strings(0, content, lens)?;
    Ok(())
}
