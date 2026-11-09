//! The constant codecs (`DI_constant_typed`, `decode_constant_kernel.c`),
//! feature `constant`.
//!
//! The input holds exactly one element (a byte for serial streams, a record
//! for struct streams); the header is the output element count as a varint,
//! at least 1 and filling the header exactly. The output repeats the element
//! and keeps the input's width.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;
use crate::shared::varint;

/// `ZL_StandardTransformID_constant_serial`.
pub(crate) const CONSTANT_SERIAL: Desc = Desc {
    id: 44,
    min_version: 11,
    regens: &[Type::Serial],
    variable_regens: false,
    inputs: &[Type::Serial],
    variable_inputs: 0,
    decode,
};

/// `ZL_StandardTransformID_constant_fixed`.
pub(crate) const CONSTANT_FIXED: Desc = Desc {
    id: 45,
    min_version: 11,
    regens: &[Type::Struct],
    variable_regens: false,
    inputs: &[Type::Struct],
    variable_inputs: 0,
    decode,
};

fn decode(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let input = &inputs[0];
    if input.len() != 1 {
        return Err(corrupt());
    }
    let (count, used) = varint::decode(ctx.header).map_err(|_| corrupt())?;
    if used != ctx.header.len() || count == 0 {
        return Err(corrupt());
    }
    // A count beyond `usize` fails the budget.
    let count = usize::try_from(count).unwrap_or(usize::MAX);
    let out = regens[0].fixed(input.width, count)?;
    if input.width == 1 {
        out.resize(count, input.bytes[0]);
    } else {
        for _ in 0..count {
            out.extend_from_slice(input.bytes);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    const DESC: Desc = CONSTANT_SERIAL;
    use super::super::State;
    use super::*;
    use crate::Backend;

    fn run(desc: &Desc, header: &[u8], input: Input<'_>) -> Result<Stream, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, header);
        let mut regens = [Stream::default()];
        regens[0].prepare(desc.regen_type(0), 1 << 16);
        (desc.decode)(&mut ctx, &[input], &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream)
    }

    fn records(width: usize, bytes: &[u8]) -> Input<'_> {
        Input {
            ty: Type::Struct,
            width,
            bytes,
            lens: &[],
        }
    }

    #[test]
    fn a_byte_repeats_for_serial_streams() {
        let out = run(&DESC, &[5], Input::serial(&[0xab])).unwrap();
        assert_eq!(out.bytes(), [0xab; 5]);
        assert_eq!((out.ty(), out.as_input().width), (Type::Serial, 1));
        // A two-byte varint: 200 copies.
        let out = run(&DESC, &[0xc8, 0x01], Input::serial(&[7])).unwrap();
        assert_eq!(out.bytes(), [7; 200]);
    }

    #[test]
    fn a_record_repeats_for_struct_streams() {
        let out = run(&CONSTANT_FIXED, &[3], records(2, b"ab")).unwrap();
        assert_eq!(out.bytes(), b"ababab");
        assert_eq!((out.ty(), out.as_input().width), (Type::Struct, 2));
        let out = run(&CONSTANT_FIXED, &[1], records(40, &[9; 40])).unwrap();
        assert_eq!(out.bytes(), [9; 40]);
        let out = run(&CONSTANT_FIXED, &[2], records(1, b"x")).unwrap();
        assert_eq!(out.bytes(), b"xx");
    }

    #[test]
    fn the_input_must_hold_exactly_one_element() {
        assert_eq!(run(&DESC, &[2], Input::serial(&[])).err(), Some(corrupt()));
        assert_eq!(
            run(&DESC, &[2], Input::serial(&[1, 2])).err(),
            Some(corrupt())
        );
        assert_eq!(
            run(&CONSTANT_FIXED, &[2], records(2, b"abcd")).err(),
            Some(corrupt())
        );
    }

    #[test]
    fn the_header_must_be_one_positive_varint() {
        let input = Input::serial(&[1]);
        for header in [&[][..], &[0], &[0x80], &[2, 0], &[0x80, 0x00]] {
            assert_eq!(
                run(&DESC, header, input).err(),
                Some(corrupt()),
                "{header:?}"
            );
        }
    }

    #[test]
    fn counts_beyond_the_budget_are_corrupt() {
        let mut huge = alloc::vec![0xff; 9];
        huge.push(1);
        assert_eq!(
            run(&DESC, &huge, Input::serial(&[1])).err(),
            Some(corrupt())
        );
        assert_eq!(
            run(&CONSTANT_FIXED, &[0xff, 0xff, 0x03], records(64, &[0; 64])).err(),
            Some(corrupt())
        );
    }
}
