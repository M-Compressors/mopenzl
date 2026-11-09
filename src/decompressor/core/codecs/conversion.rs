//! Decoders of the `conversion` feature (`decode_conversion_binding.c`):
//! the type changes between serial, struct, numeric and string streams
//! that graphs insert explicitly or as implicit conversions.
//!
//! Every conversion keeps the bytes; most only retag the stream. Numeric
//! streams are little-endian here, so the `_le` conversions copy and the
//! `_be` ones swap each element.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;
use crate::shared::varint;

const fn conversion(
    id: u32,
    min_version: u32,
    regen: &'static [Type],
    inputs: &'static [Type],
    decode: super::DecodeFn,
) -> Desc {
    Desc {
        id,
        min_version,
        regens: regen,
        variable_regens: false,
        inputs,
        variable_inputs: 0,
        decode,
    }
}

/// `ZL_StandardTransformID_convert_serial_to_struct`: struct back to serial.
pub(crate) const SERIAL_TO_STRUCT: Desc =
    conversion(5, 3, &[Type::Serial], &[Type::Struct], to_bytes);

/// `ZL_StandardTransformID_convert_struct_to_serial`: serial back to
/// structs of the width the header records.
pub(crate) const STRUCT_TO_SERIAL: Desc =
    conversion(6, 3, &[Type::Struct], &[Type::Serial], serial_to_struct);

/// `ZL_StandardTransformID_convert_struct_to_num_le`: numbers back to
/// structs.
pub(crate) const STRUCT_TO_NUM_LE: Desc =
    conversion(7, 3, &[Type::Struct], &[Type::Numeric], same_width);

/// `ZL_StandardTransformID_convert_num_to_struct_le`: structs back to
/// numbers.
pub(crate) const NUM_TO_STRUCT_LE: Desc =
    conversion(8, 3, &[Type::Numeric], &[Type::Struct], same_width);

/// `ZL_StandardTransformID_convert_serial_to_num_le`: numbers back to
/// bytes.
pub(crate) const SERIAL_TO_NUM_LE: Desc =
    conversion(9, 3, &[Type::Serial], &[Type::Numeric], to_bytes);

/// `ZL_StandardTransformID_convert_num_to_serial_le`: bytes back to
/// numbers of the width the header records.
pub(crate) const NUM_TO_SERIAL_LE: Desc =
    conversion(10, 3, &[Type::Numeric], &[Type::Serial], serial_to_num);

/// `ZL_StandardTransformID_convert_serial_string`: strings back to their
/// concatenated bytes.
pub(crate) const SERIAL_STRING: Desc =
    conversion(11, 10, &[Type::Serial], &[Type::String], to_bytes);

/// `ZL_StandardTransformID_separate_string_components`: contents and
/// lengths back to strings.
pub(crate) const SEPARATE_STRING_COMPONENTS: Desc = conversion(
    12,
    10,
    &[Type::String],
    &[Type::Serial, Type::Numeric],
    join_strings,
);

/// `ZL_StandardTransformID_convert_struct_to_num_be`: big-endian numbers
/// back to structs.
pub(crate) const STRUCT_TO_NUM_BE: Desc =
    conversion(13, 21, &[Type::Struct], &[Type::Numeric], swapped);

/// `ZL_StandardTransformID_convert_serial_to_num_be`: big-endian numbers
/// back to bytes.
pub(crate) const SERIAL_TO_NUM_BE: Desc =
    conversion(14, 21, &[Type::Serial], &[Type::Numeric], swapped);

/// The input's bytes as a width-1 stream (`DI_revert_serial_to_struct`,
/// `DI_revert_serial_to_num_le`, `DI_extract_concatenatedFields`).
fn to_bytes(
    _: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let bytes = inputs[0].bytes;
    regens[0].fixed(1, bytes.len())?.extend_from_slice(bytes);
    Ok(())
}

/// The input retagged with its width (`DI_revert_struct_to_num_le`,
/// `DI_revert_num_to_struct_le`).
fn same_width(
    _: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    regens[0].copy_of(&inputs[0])
}

/// Each element byte-swapped; serial results keep width 1
/// (`convertFromNumWithOptionalSwap` with a swap).
fn swapped(
    _: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let input = &inputs[0];
    let width = if regens[0].ty() == Type::Serial {
        1
    } else {
        input.width
    };
    let out = regens[0].fixed(width, input.bytes.len() / width)?;
    for element in input.bytes.chunks_exact(input.width) {
        out.extend(element.iter().rev());
    }
    Ok(())
}

/// Bytes regrouped as structs of the header's varint width
/// (`DI_revert_struct_to_serial`).
fn serial_to_struct(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let (width, len) = varint::decode(ctx.header).or(Err(corrupt()))?;
    if width == 0 || len != ctx.header.len() {
        return Err(corrupt());
    }
    let width = usize::try_from(width).or(Err(corrupt()))?;
    let bytes = inputs[0].bytes;
    if !bytes.len().is_multiple_of(width) {
        return Err(corrupt());
    }
    regens[0]
        .fixed(width, bytes.len() / width)?
        .extend_from_slice(bytes);
    Ok(())
}

/// Bytes regrouped as numbers of `1 << header[0]` bytes
/// (`DI_revert_num_to_serial_le`).
fn serial_to_num(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let &[log] = ctx.header else {
        return Err(corrupt());
    };
    if log > 3 {
        return Err(corrupt());
    }
    let width = 1usize << log;
    let bytes = inputs[0].bytes;
    if !bytes.len().is_multiple_of(width) {
        return Err(corrupt());
    }
    regens[0]
        .fixed(width, bytes.len() / width)?
        .extend_from_slice(bytes);
    Ok(())
}

/// Strings from concatenated contents (input 0) and their lengths
/// (input 1, numbers of any width that fit 32 bits)
/// (`DI_revert_VSF_separation`).
fn join_strings(
    _: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let (contents, sizes) = (&inputs[0], &inputs[1]);
    let (bytes, lens) = regens[0].strings(sizes.len(), contents.bytes.len())?;
    let mut total = 0u64;
    for size in sizes.bytes.chunks_exact(sizes.width) {
        let mut value = [0u8; 8];
        value[..size.len()].copy_from_slice(size);
        let len = u32::try_from(u64::from_le_bytes(value)).or(Err(corrupt()))?;
        total += u64::from(len);
        lens.push(len);
    }
    if total != contents.bytes.len() as u64 {
        return Err(corrupt());
    }
    bytes.extend_from_slice(contents.bytes);
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
    fn descriptors_are_built_from_their_signature() {
        // The constructor also runs at compile time for every descriptor.
        let desc = conversion(5, 3, &[Type::Serial], &[Type::Struct], to_bytes);
        assert_eq!(
            (desc.id, desc.min_version),
            (SERIAL_TO_STRUCT.id, SERIAL_TO_STRUCT.min_version)
        );
        assert_eq!(
            (desc.regens, desc.inputs),
            (SERIAL_TO_STRUCT.regens, SERIAL_TO_STRUCT.inputs)
        );
        assert!(!desc.variable_regens && desc.variable_inputs == 0);
    }

    #[test]
    fn retagging_conversions_keep_bytes_and_width() {
        let bytes = [1u8, 2, 3, 4];
        let structs = run(&SERIAL_TO_STRUCT, &[], &[typed(Type::Struct, 2, &bytes)]).unwrap();
        assert_eq!((structs.ty(), structs.as_input().width), (Type::Serial, 1));
        let numbers = run(&NUM_TO_STRUCT_LE, &[], &[typed(Type::Struct, 4, &bytes)]).unwrap();
        assert_eq!((numbers.ty(), numbers.as_input().width), (Type::Numeric, 4));
        // Numbers are 1, 2, 4 or 8 bytes wide.
        let odd = [0u8; 3];
        assert!(run(&NUM_TO_STRUCT_LE, &[], &[typed(Type::Struct, 3, &odd)]).is_err());
        let back = run(&STRUCT_TO_NUM_LE, &[], &[typed(Type::Numeric, 2, &bytes)]).unwrap();
        assert_eq!((back.ty(), back.as_input().width), (Type::Struct, 2));
        let serial = run(&SERIAL_TO_NUM_LE, &[], &[typed(Type::Numeric, 2, &bytes)]).unwrap();
        assert_eq!(serial.bytes(), bytes);
        let lens = [1u32, 3];
        let strings = Input {
            ty: Type::String,
            width: 1,
            bytes: &bytes,
            lens: &lens,
        };
        assert_eq!(run(&SERIAL_STRING, &[], &[strings]).unwrap().bytes(), bytes);
    }

    #[test]
    fn big_endian_conversions_swap_each_element() {
        let bytes = [1u8, 2, 3, 4];
        let serial = run(&SERIAL_TO_NUM_BE, &[], &[typed(Type::Numeric, 2, &bytes)]).unwrap();
        assert_eq!(
            (serial.bytes(), serial.as_input().width),
            (&[2u8, 1, 4, 3][..], 1)
        );
        let structs = run(&STRUCT_TO_NUM_BE, &[], &[typed(Type::Numeric, 4, &bytes)]).unwrap();
        assert_eq!(
            (structs.bytes(), structs.as_input().width),
            (&[4u8, 3, 2, 1][..], 4)
        );
    }

    #[test]
    fn widths_come_from_the_header() {
        let bytes = [0u8; 6];
        let serial = typed(Type::Serial, 1, &bytes);
        let structs = run(&STRUCT_TO_SERIAL, &[3], &[serial]).unwrap();
        assert_eq!(structs.as_input().len(), 2);
        for header in [&[][..], &[0], &[4], &[3, 0], &[0x80]] {
            assert!(
                run(&STRUCT_TO_SERIAL, header, &[serial]).is_err(),
                "{header:?}"
            );
        }
        let numbers = run(&NUM_TO_SERIAL_LE, &[1], &[serial]).unwrap();
        assert_eq!(numbers.as_input().len(), 3);
        for header in [&[][..], &[2], &[4], &[1, 1]] {
            assert!(
                run(&NUM_TO_SERIAL_LE, header, &[serial]).is_err(),
                "{header:?}"
            );
        }
    }

    #[test]
    fn strings_join_contents_and_lengths() {
        let sizes: alloc::vec::Vec<u8> =
            [2u64, 0, 3].iter().flat_map(|v| v.to_le_bytes()).collect();
        let joined = run(
            &SEPARATE_STRING_COMPONENTS,
            &[],
            &[Input::serial(b"abcde"), typed(Type::Numeric, 8, &sizes)],
        )
        .unwrap();
        assert_eq!(joined.as_input().lens, [2, 0, 3]);
        assert_eq!(joined.bytes(), b"abcde");
        // Lengths that miss the content, or exceed 32 bits.
        assert!(
            run(
                &SEPARATE_STRING_COMPONENTS,
                &[],
                &[Input::serial(b"abcd"), typed(Type::Numeric, 8, &sizes)]
            )
            .is_err()
        );
        let huge = (1u64 << 32).to_le_bytes();
        assert!(
            run(
                &SEPARATE_STRING_COMPONENTS,
                &[],
                &[Input::serial(b""), typed(Type::Numeric, 8, &huge)]
            )
            .is_err()
        );
    }
}
