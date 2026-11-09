//! Decoders of the `concat` feature: `concat_serial` (55), `concat_num`
//! (57), `concat_struct` (58) and `concat_string` (59).
//!
//! Ports `decode_concat_binding.c` (`DI_concat`, one function for all four
//! types). The inputs are a numeric stream of 32-bit segment sizes, in
//! elements (strings, for string streams), and the concatenated stream; the
//! codec regenerates one stream per size, each a slice of the concatenation.
//! These are multi-output codecs: the chunk names how many streams to
//! regenerate, and it must equal the number of sizes (the reference reports
//! fewer as corruption and cannot create more).

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// A `CONCAT_*_GRAPH` descriptor: any number of `$ty` streams from sizes
/// and the concatenation.
macro_rules! concat {
    ($id:expr, $min_version:expr, $ty:expr) => {
        Desc {
            id: $id,
            min_version: $min_version,
            regens: &[$ty],
            variable_regens: true,
            inputs: &[Type::Numeric, $ty],
            variable_inputs: 0,
            decode: split,
        }
    };
}

/// `ZL_StandardTransformID_concat_serial`.
pub(crate) const CONCAT_SERIAL: Desc = concat!(55, 16, Type::Serial);

/// `ZL_StandardTransformID_concat_num`.
pub(crate) const CONCAT_NUM: Desc = concat!(57, 17, Type::Numeric);

/// `ZL_StandardTransformID_concat_struct`.
pub(crate) const CONCAT_STRUCT: Desc = concat!(58, 17, Type::Struct);

/// `ZL_StandardTransformID_concat_string`.
pub(crate) const CONCAT_STRING: Desc = concat!(59, 18, Type::String);

/// `DI_concat`.
fn split(_: &mut Ctx<'_>, inputs: &[Input<'_>], regens: &mut [Stream]) -> Result<(), DecodeError> {
    let (sizes, whole) = (&inputs[0], &inputs[1]);
    if sizes.width != 4 || sizes.len() == 0 || sizes.len() != regens.len() {
        return Err(corrupt());
    }
    let sizes = sizes
        .bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&size| u32::from_le_bytes(size) as usize);
    let total = whole.len();
    let mut first = 0usize;
    if whole.ty == Type::String {
        let mut byte = 0usize;
        for (size, regen) in sizes.zip(regens) {
            let end = first.checked_add(size).filter(|&end| end <= total);
            let lens = &whole.lens[first..end.ok_or_else(corrupt)?];
            let len = lens.iter().map(|&len| len as usize).sum::<usize>();
            let (bytes, out_lens) = regen.strings(size, len)?;
            bytes.extend_from_slice(&whole.bytes[byte..byte + len]);
            out_lens.extend_from_slice(lens);
            first += size;
            byte += len;
        }
    } else {
        for (size, regen) in sizes.zip(regens) {
            let end = first.checked_add(size).filter(|&end| end <= total);
            let end = end.ok_or_else(corrupt)?;
            regen
                .fixed(whole.width, size)?
                .extend_from_slice(&whole.bytes[first * whole.width..end * whole.width]);
            first = end;
        }
    }
    if first != total {
        return Err(corrupt());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn run(desc: &Desc, inputs: &[Input<'_>], outputs: usize) -> Result<Vec<Stream>, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[]);
        let mut regens: Vec<Stream> = (0..outputs).map(|_| Stream::default()).collect();
        for (n, stream) in regens.iter_mut().enumerate() {
            stream.prepare(desc.regen_type(n), 1 << 16);
        }
        (desc.decode)(&mut ctx, inputs, &mut regens)?;
        regens.iter().try_for_each(Stream::validate)?;
        Ok(regens)
    }

    fn sizes(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn typed(ty: Type, width: usize, bytes: &[u8]) -> Input<'_> {
        Input {
            ty,
            width,
            bytes,
            lens: &[],
        }
    }

    #[test]
    fn serial_slices_follow_the_sizes() {
        let sizes = sizes(&[2, 0, 3]);
        let out = run(
            &CONCAT_SERIAL,
            &[typed(Type::Numeric, 4, &sizes), Input::serial(b"abcde")],
            3,
        )
        .unwrap();
        let contents: Vec<&[u8]> = out.iter().map(Stream::bytes).collect();
        assert_eq!(contents, [&b"ab"[..], b"", b"cde"]);
    }

    #[test]
    fn numeric_and_struct_slices_keep_their_width() {
        let sizes = sizes(&[1, 2]);
        let bytes = [1u8, 0, 2, 0, 3, 0];
        let out = run(
            &CONCAT_NUM,
            &[
                typed(Type::Numeric, 4, &sizes),
                typed(Type::Numeric, 2, &bytes),
            ],
            2,
        )
        .unwrap();
        assert_eq!(out[0].bytes(), [1, 0]);
        assert_eq!(
            (out[1].bytes(), out[1].as_input().width),
            (&[2u8, 0, 3, 0][..], 2)
        );
        let out = run(
            &CONCAT_STRUCT,
            &[
                typed(Type::Numeric, 4, &sizes),
                typed(Type::Struct, 2, &bytes),
            ],
            2,
        )
        .unwrap();
        assert_eq!(out[1].as_input().len(), 2);
    }

    #[test]
    fn string_slices_split_the_lengths_too() {
        let sizes = sizes(&[2, 0, 1]);
        let lens = [1u32, 2, 3];
        let whole = Input {
            ty: Type::String,
            width: 1,
            bytes: b"abbccc",
            lens: &lens,
        };
        let out = run(&CONCAT_STRING, &[typed(Type::Numeric, 4, &sizes), whole], 3).unwrap();
        assert_eq!(out[0].bytes(), b"abb");
        assert_eq!(out[0].as_input().lens, [1, 2]);
        assert_eq!(out[1].as_input().len(), 0);
        assert_eq!(out[2].as_input().lens, [3]);
    }

    #[test]
    fn inconsistent_sizes_are_corrupt() {
        let one = sizes(&[3]);
        let data = Input::serial(b"abcd");
        // Sizes not covering the stream, or overrunning it.
        assert_eq!(
            run(&CONCAT_SERIAL, &[typed(Type::Numeric, 4, &one), data], 1).map(|_| ()),
            Err(corrupt())
        );
        let big = sizes(&[5]);
        assert_eq!(
            run(&CONCAT_SERIAL, &[typed(Type::Numeric, 4, &big), data], 1).map(|_| ()),
            Err(corrupt())
        );
        let huge = sizes(&[u32::MAX, u32::MAX]);
        assert_eq!(
            run(&CONCAT_SERIAL, &[typed(Type::Numeric, 4, &huge), data], 2).map(|_| ()),
            Err(corrupt())
        );
        // No sizes, wide sizes, a stream count that differs from the sizes.
        let none = [0u8; 0];
        assert_eq!(
            run(
                &CONCAT_SERIAL,
                &[typed(Type::Numeric, 4, &none), Input::serial(b"")],
                1
            )
            .map(|_| ()),
            Err(corrupt())
        );
        let wide = [4u8, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            run(&CONCAT_SERIAL, &[typed(Type::Numeric, 8, &wide), data], 1).map(|_| ()),
            Err(corrupt())
        );
        let two = sizes(&[2, 2]);
        for outputs in [1, 3] {
            assert_eq!(
                run(
                    &CONCAT_SERIAL,
                    &[typed(Type::Numeric, 4, &two), data],
                    outputs
                )
                .map(|_| ()),
                Err(corrupt())
            );
        }
        // Strings: sizes overrun the strings; leftover strings.
        let lens = [1u32, 1];
        let whole = Input {
            ty: Type::String,
            width: 1,
            bytes: b"ab",
            lens: &lens,
        };
        for bad in [sizes(&[3]), sizes(&[1])] {
            assert_eq!(
                run(&CONCAT_STRING, &[typed(Type::Numeric, 4, &bad), whole], 1).map(|_| ()),
                Err(corrupt())
            );
        }
    }
}
