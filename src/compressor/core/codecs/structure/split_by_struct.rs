//! The private `split_by_struct` node 69 (`encode_splitByStruct_binding.c`,
//! `encode_splitByStruct_kernel.c`).
//!
//! A serial stream holding an array of fixed-size structures is cut into one
//! struct stream per field. The field sizes are the copy parameter
//! `ZL_SPLITBYSTRUCT_FIELDSIZES_PID`, a native-endian `size_t` array.

use alloc::vec::Vec;

use crate::compressor::core::codecs::FnEncoder;
use super::util::usize_words;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;

/// `ZL_SPLITBYSTRUCT_FIELDSIZES_PID`.
pub(crate) const FIELD_SIZES_PID: i32 = crate::graph::params::split_by_struct::FIELD_SIZES;

/// `EI_splitByStruct`.
pub(crate) static SPLIT_BY_STRUCT: FnEncoder = FnEncoder(split_by_struct);

fn split_by_struct<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let structure = cx.copy_param(FIELD_SIZES_PID).ok_or(NodeError::InvalidParam(
        "splitByStruct requires structure description",
    ))?;
    let field_sizes = usize_words(structure);
    let in_size = input.n_elts();
    // The reference sums with `size_t` wraparound; a sum that wraps cannot
    // describe a structure that fits in memory.
    let struct_size = field_sizes
        .iter()
        .try_fold(0usize, |total, &size| total.checked_add(size))
        .ok_or(NodeError::InvalidParam("structure size overflows"))?;
    if struct_size == 0 {
        return Err(NodeError::InvalidParam("structure must have a size > 0"));
    }
    if !in_size.is_multiple_of(struct_size) {
        return Err(NodeError::InvalidInput(
            "splitByStruct requires an input size which is a multiple of the structure size",
        ));
    }
    let nb_structs = in_size / struct_size;
    if field_sizes.contains(&0) {
        return Err(NodeError::InvalidParam("must not have a field size of zero"));
    }
    let mut fields: Vec<Vec<u8>> = field_sizes
        .iter()
        .map(|&size| Vec::with_capacity(nb_structs.saturating_mul(size)))
        .collect();
    for record in input.bytes().chunks_exact(struct_size) {
        let mut rest = record;
        for (field, &size) in fields.iter_mut().zip(&field_sizes) {
            let (head, tail) = rest.split_at(size);
            field.extend_from_slice(head);
            rest = tail;
        }
    }
    for (field, &size) in fields.into_iter().zip(&field_sizes) {
        cx.emit(0, size, field)?;
    }
    Ok(())
}
