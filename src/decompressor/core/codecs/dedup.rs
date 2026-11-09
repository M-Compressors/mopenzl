//! Decoder of the `dedup` feature: `dedup_num` (56).
//!
//! Ports `decode_dedup_binding.c` (`DI_dedup_num`): the encoder stored one
//! copy of numeric streams that were all identical, and the decoder
//! regenerates as many copies as the chunk asks for (the reference shares
//! the one stream by reference; here each regenerated stream owns a copy).

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_dedup_num`.
pub(crate) const DEDUP_NUM: Desc = Desc {
    id: 56,
    min_version: 16,
    regens: &[Type::Numeric],
    variable_regens: true,
    inputs: &[Type::Numeric],
    variable_inputs: 0,
    decode: copies,
};

/// `DI_dedup_num`.
fn copies(_: &mut Ctx<'_>, inputs: &[Input<'_>], regens: &mut [Stream]) -> Result<(), DecodeError> {
    regens
        .iter_mut()
        .try_for_each(|regen| regen.copy_of(&inputs[0]))
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;
    use crate::decompressor::core::data::corrupt;

    fn run(
        input: Input<'_>,
        outputs: usize,
        budget: usize,
    ) -> Result<alloc::vec::Vec<Stream>, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[]);
        let mut regens: alloc::vec::Vec<Stream> = (0..outputs).map(|_| Stream::default()).collect();
        for stream in &mut regens {
            stream.prepare(Type::Numeric, budget);
        }
        (DEDUP_NUM.decode)(&mut ctx, &[input], &mut regens)?;
        regens.iter().try_for_each(Stream::validate)?;
        Ok(regens)
    }

    fn numbers(width: usize, bytes: &[u8]) -> Input<'_> {
        Input {
            ty: Type::Numeric,
            width,
            bytes,
            lens: &[],
        }
    }

    #[test]
    fn every_regenerated_stream_is_a_copy() {
        let bytes = [1u8, 0, 2, 0, 3, 0];
        let out = run(numbers(2, &bytes), 4, 64).unwrap();
        assert_eq!(out.len(), 4);
        for stream in &out {
            assert_eq!(stream.bytes(), bytes);
            assert_eq!(stream.as_input().width, 2);
        }
    }

    #[test]
    fn empty_streams_keep_their_width() {
        let out = run(numbers(8, &[]), 2, 64).unwrap();
        assert!(
            out.iter()
                .all(|s| s.as_input().len() == 0 && s.as_input().width == 8)
        );
    }

    #[test]
    fn copies_beyond_the_budget_are_corrupt() {
        assert_eq!(run(numbers(1, &[0; 16]), 1, 8).map(|_| ()), Err(corrupt()));
    }
}
