//! The Zstandard codec (`DI_zstd`), feature `zstd`.
//!
//! One serial stream in: the element width as a varint, then a magicless
//! frame; one serial stream out. Serial streams have width 1, so any other
//! recorded width fails, as in the reference. The engine decodes a chunk's
//! final output straight into the destination instead (see
//! `Engine::decode_chunk`); this entry point serves intermediate streams.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::core::zstd::{content_size, split, zstd_dictionary};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_zstd`.
pub(crate) const ZSTD: Desc = Desc {
    id: 22,
    min_version: 3,
    regens: &[Type::Serial],
    variable_regens: false,
    inputs: &[Type::Serial],
    variable_inputs: 0,
    decode,
};

fn decode(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let (width, frame) = split(inputs[0].bytes)?;
    let size = content_size(frame)?;
    if !size.is_multiple_of(width) {
        return Err(corrupt());
    }
    let width = usize::try_from(width).or(Err(corrupt()))?;
    let size = usize::try_from(size).or(Err(corrupt()))?;
    let out = regens[0].fixed(width, size / width)?;
    out.resize(size, 0);
    ctx.state.zstd.decode(frame, out, zstd_dictionary(ctx.dict))
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;

    fn stream(width: u8, input: &[u8]) -> alloc::vec::Vec<u8> {
        let mut encoder = mzstd::Compressor::default();
        let mut stream = alloc::vec![width];
        stream.extend_from_slice(&encoder.compress(input).unwrap()[4..]);
        stream
    }

    #[test]
    fn intermediate_streams_are_serial_bytes() {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[]);
        let bytes = stream(1, &[1, 2, 3, 4]);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::Serial, 64);
        decode(&mut ctx, &[Input::serial(&bytes)], &mut regens).unwrap();
        assert_eq!(regens[0].as_input().width, 1);
        assert_eq!(regens[0].bytes(), [1, 2, 3, 4]);
        // A serial stream has width 1 (`STREAM_validateTypeWidth`), so a
        // recorded width of 2 fails as it does in the reference.
        let bytes = stream(2, &[1, 2, 3, 4]);
        regens[0].prepare(Type::Serial, 64);
        assert_eq!(
            decode(&mut ctx, &[Input::serial(&bytes)], &mut regens),
            Err(corrupt())
        );
        // Three bytes do not split into two-byte elements.
        let bytes = stream(2, &[1, 2, 3]);
        regens[0].prepare(Type::Serial, 64);
        assert_eq!(
            decode(&mut ctx, &[Input::serial(&bytes)], &mut regens),
            Err(corrupt())
        );
        // A frame larger than the budget.
        let bytes = stream(1, &[0; 100]);
        regens[0].prepare(Type::Serial, 64);
        assert_eq!(
            decode(&mut ctx, &[Input::serial(&bytes)], &mut regens),
            Err(corrupt())
        );
    }
}
