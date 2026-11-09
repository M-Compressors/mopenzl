//! Decoders of the `split` feature: `splitn` (40), `splitn_struct` (47) and
//! `splitn_num` (48).
//!
//! Ports `decode_splitN_binding.c` (`DI_splitN`, shared by the three
//! codecs) and `decode_splitN_kernel.c` (`ZS_appendN`): the segments the
//! encoder cut an input into are joined back in order. All segments share
//! one type and element width. With no segment at all the width comes from
//! the codec header, a varint (an empty header means width 1).

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;
use crate::shared::varint;

/// A `GRAPH_VO_*` descriptor: any number of segments of type `$ty`.
macro_rules! splitn {
    ($id:expr, $min_version:expr, $ty:expr) => {
        Desc {
            id: $id,
            min_version: $min_version,
            regens: &[$ty],
            variable_regens: false,
            inputs: &[],
            variable_inputs: $ty.bit(),
            decode: join,
        }
    };
}

/// `ZL_StandardTransformID_splitn`: serial segments.
pub(crate) const SPLITN: Desc = splitn!(40, 9, Type::Serial);

/// `ZL_StandardTransformID_splitn_struct`: struct segments.
pub(crate) const SPLITN_STRUCT: Desc = splitn!(47, 14, Type::Struct);

/// `ZL_StandardTransformID_splitn_num`: numeric segments.
pub(crate) const SPLITN_NUM: Desc = splitn!(48, 14, Type::Numeric);

/// `DI_splitN`: concatenates the segments.
fn join(ctx: &mut Ctx<'_>, inputs: &[Input<'_>], regens: &mut [Stream]) -> Result<(), DecodeError> {
    let width = match inputs.first() {
        Some(first) => first.width,
        None if ctx.header.is_empty() => 1,
        None => {
            let (width, _) = varint::decode(ctx.header).map_err(|_| corrupt())?;
            usize::try_from(width).ok().ok_or_else(corrupt)?
        }
    };
    // Serial streams are one byte wide (`STREAM_validateTypeWidth`), which
    // `Stream::fixed` does not check.
    if regens[0].ty() == Type::Serial && width != 1 {
        return Err(corrupt());
    }
    let mut count = 0usize;
    for input in inputs {
        if input.width != width {
            return Err(corrupt());
        }
        count = count.checked_add(input.len()).ok_or_else(corrupt)?;
    }
    let out = regens[0].fixed(width, count)?;
    for input in inputs {
        out.extend_from_slice(input.bytes);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;

    fn run(desc: &Desc, header: &[u8], inputs: &[Input<'_>]) -> Result<Stream, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, header);
        let mut regens = [Stream::default()];
        regens[0].prepare(desc.regen_type(0), 1 << 16);
        (desc.decode)(&mut ctx, inputs, &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream)
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
    fn segments_are_joined_in_order() {
        let out = run(
            &SPLITN,
            &[],
            &[
                Input::serial(b"ab"),
                Input::serial(b""),
                Input::serial(b"cde"),
            ],
        )
        .unwrap();
        assert_eq!(out.bytes(), b"abcde");
        assert_eq!(out.ty(), Type::Serial);
        let (a, b) = ([1u8, 0, 2, 0], [3u8, 0]);
        let out = run(
            &SPLITN_NUM,
            &[],
            &[typed(Type::Numeric, 2, &a), typed(Type::Numeric, 2, &b)],
        )
        .unwrap();
        assert_eq!(
            (out.bytes(), out.as_input().width),
            (&[1u8, 0, 2, 0, 3, 0][..], 2)
        );
        let out = run(
            &SPLITN_STRUCT,
            &[],
            &[typed(Type::Struct, 3, &[1, 2, 3, 4, 5, 6])],
        )
        .unwrap();
        assert_eq!((out.as_input().len(), out.as_input().width), (2, 3));
    }

    #[test]
    fn widths_must_agree() {
        let bytes = [0u8; 4];
        assert_eq!(
            run(
                &SPLITN_NUM,
                &[],
                &[
                    typed(Type::Numeric, 2, &bytes),
                    typed(Type::Numeric, 4, &bytes)
                ]
            )
            .map(|_| ()),
            Err(corrupt())
        );
    }

    #[test]
    fn no_segment_takes_the_width_from_the_header() {
        let out = run(&SPLITN, &[], &[]).unwrap();
        assert_eq!((out.as_input().len(), out.as_input().width), (0, 1));
        // Serial streams are one byte wide, whatever the header says.
        assert_eq!(run(&SPLITN, &[4], &[]).map(|_| ()), Err(corrupt()));
        assert!(run(&SPLITN, &[1], &[]).is_ok());
        let out = run(&SPLITN_STRUCT, &[7], &[]).unwrap();
        assert_eq!((out.as_input().len(), out.as_input().width), (0, 7));
        let out = run(&SPLITN_NUM, &[4, 99], &[]).unwrap();
        assert_eq!(out.as_input().width, 4);
        // A bad varint, a zero width, a number width that is not 1/2/4/8.
        for header in [&[0x80u8][..], &[0], &[3]] {
            let desc = if header == [3] {
                &SPLITN_NUM
            } else {
                &SPLITN_STRUCT
            };
            assert_eq!(
                run(desc, header, &[]).map(|_| ()),
                Err(corrupt()),
                "{header:?}"
            );
        }
    }

    #[test]
    fn output_is_limited_by_the_budget() {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[]);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::Serial, 3);
        assert_eq!(
            (SPLITN.decode)(&mut ctx, &[Input::serial(b"abcd")], &mut regens),
            Err(corrupt())
        );
    }
}
