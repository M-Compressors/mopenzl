//! Decoder of the `split-by-struct` feature: `splitByStruct` (41).
//!
//! Ports `decode_splitByStruct_binding.c` (`DI_splitByStruct`) and
//! `decode_splitByStruct_kernel.c` (`ZS_dispatchArrayFixedSizeStruct_decode`
//! with its 8-byte-field fast path): each input is one field of an array of
//! structs, stored as a struct stream, and the output is the serial array
//! with every struct's fields back to back.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_splitByStruct`.
pub(crate) const SPLIT_BY_STRUCT: Desc = Desc {
    id: 41,
    min_version: 9,
    regens: &[Type::Serial],
    variable_regens: false,
    inputs: &[],
    variable_inputs: Type::Struct.bit(),
    decode: join_fields,
};

/// `DI_splitByStruct`.
fn join_fields(
    _: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let Some(first) = inputs.first() else {
        return Err(corrupt());
    };
    let count = first.len();
    let mut struct_size = 0usize;
    for input in inputs {
        if input.len() != count {
            return Err(corrupt());
        }
        struct_size = struct_size.checked_add(input.width).ok_or_else(corrupt)?;
    }
    let total = count.checked_mul(struct_size).ok_or_else(corrupt)?;
    let out = regens[0].fixed(1, total)?;
    let mut fields: alloc::vec::Vec<_> = inputs
        .iter()
        .map(|input| input.bytes.chunks_exact(input.width))
        .collect();
    for _ in 0..count {
        for field in &mut fields {
            out.extend_from_slice(field.next().ok_or_else(corrupt)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;

    fn run(inputs: &[Input<'_>]) -> Result<Stream, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[]);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::Serial, 1 << 16);
        (SPLIT_BY_STRUCT.decode)(&mut ctx, inputs, &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream)
    }

    fn field(width: usize, bytes: &[u8]) -> Input<'_> {
        Input {
            ty: Type::Struct,
            width,
            bytes,
            lens: &[],
        }
    }

    #[test]
    fn fields_are_joined_element_by_element() {
        let out = run(&[field(1, b"ace"), field(2, b"1b2d3f")]).unwrap();
        assert_eq!(out.bytes(), b"a1bc2de3f");
        let out = run(&[field(1, b"abc")]).unwrap();
        assert_eq!(out.bytes(), b"abc");
        // Fields wider than eight bytes take the generic path in C.
        let wide = [7u8; 20];
        let out = run(&[field(10, &wide), field(1, &[1, 2])]).unwrap();
        assert_eq!(out.bytes().len(), 22);
        assert_eq!(&out.bytes()[10..11], &[1]);
    }

    #[test]
    fn empty_fields_make_an_empty_array() {
        assert_eq!(run(&[field(4, &[]), field(2, &[])]).unwrap().bytes(), b"");
    }

    #[test]
    fn no_fields_or_unequal_counts_are_corrupt() {
        assert_eq!(run(&[]).map(|_| ()), Err(corrupt()));
        assert_eq!(
            run(&[field(1, b"ab"), field(1, b"abc")]).map(|_| ()),
            Err(corrupt())
        );
    }

    #[test]
    fn overflowing_struct_sizes_are_corrupt() {
        // Two fields whose widths overflow `usize` when added.
        let (a, b) = (field(usize::MAX, &[]), field(2, &[]));
        assert_eq!(run(&[a, b]).map(|_| ()), Err(corrupt()));
    }
}
