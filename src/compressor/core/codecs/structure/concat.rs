//! The concat nodes (`encode_concat_binding.c`): `concat_serial` 33,
//! `concat_num` 34, `concat_struct` 35 and `concat_string` 36.
//!
//! A concat node takes any number of inputs of one type (and one element
//! width) and writes the number of elements of every input as a `u32`
//! numeric stream, then all the elements back to back (for strings, the
//! contents and the concatenated string lengths).

use alloc::vec::Vec;

use crate::compressor::core::codecs::FnEncoder;
use crate::compressor::core::data::{Stream, Type};
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;

/// `EI_concat`, shared by the four node types.
pub(crate) static CONCAT: FnEncoder = FnEncoder(concat);

fn concat<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let ty = inputs[0].ty();
    let width = inputs[0].width();
    let mut nb_elts = 0usize;
    for input in inputs {
        if input.ty() != ty {
            return Err(NodeError::InvalidInput("concat types must be homogenous"));
        }
        if input.width() != width {
            return Err(NodeError::InvalidInput("concat widths must be homogenous"));
        }
        let count = input.n_elts();
        if count as u64 >= u64::from(u32::MAX) {
            return Err(NodeError::InvalidInput("too many elements to concatenate"));
        }
        nb_elts = nb_elts
            .checked_add(count)
            .ok_or(NodeError::InvalidInput("too many elements to concatenate"))?;
    }

    let mut sizes = Vec::with_capacity(inputs.len() * 4);
    let mut content = Vec::new();
    for input in inputs {
        sizes.extend_from_slice(&(input.n_elts() as u32).to_le_bytes());
        content.extend_from_slice(input.bytes());
    }
    cx.emit(0, 4, sizes)?;
    if ty == Type::String {
        let lens: Vec<u32> = inputs
            .iter()
            .flat_map(|input| input.lens().iter().copied())
            .collect();
        cx.emit_strings(1, content, lens)?;
    } else {
        cx.emit(1, width, content)?;
    }
    Ok(())
}
