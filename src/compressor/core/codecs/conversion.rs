//! Conversion nodes (`encode_conversion_binding.c`): codecs 5 to 14.
//!
//! Conversions only reinterpret bytes, so they reference their input without
//! copying, except when the byte order has to change (big-endian numerics).
//! The decoder side reads the private header they send: the element width as
//! a varint (`struct_to_serial`) or as a one-byte `nextPow2` (`num_to_serial`).

use alloc::vec::Vec;

use super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::varint;

/// `ZL_trlip_tokenSize`: the local parameter of `serial_to_struct`.
const TOKEN_SIZE: i32 = crate::graph::params::conversion::SERIAL_TO_STRUCT_SIZE;
/// `ZL_SETSTRINGLENS_PARSINGF_PID`: a parser function, not supported.
const SET_STRING_LENS_PARSER: i32 = 520;
/// `ZL_SETSTRINGLENS_ARRAY_PID`: the lengths as a reference parameter.
pub(crate) const SET_STRING_LENS_ARRAY: i32 = 521;

const LITTLE_ENDIAN: bool = cfg!(target_endian = "little");

macro_rules! encoders {
    ($($name:ident => $function:expr;)*) => {$(
        #[doc = concat!("The `", stringify!($name), "` encoder.")]
        pub(crate) static $name: FnEncoder = FnEncoder($function);
    )*};
}

encoders! {
    CONVERT_SERIAL_TO_NUM8 => |cx, inputs| to_num(cx, &inputs[0], 1, false);
    CONVERT_SERIAL_TO_NUM_LE16 => |cx, inputs| to_num(cx, &inputs[0], 2, !LITTLE_ENDIAN);
    CONVERT_SERIAL_TO_NUM_LE32 => |cx, inputs| to_num(cx, &inputs[0], 4, !LITTLE_ENDIAN);
    CONVERT_SERIAL_TO_NUM_LE64 => |cx, inputs| to_num(cx, &inputs[0], 8, !LITTLE_ENDIAN);
    CONVERT_SERIAL_TO_NUM_BE16 => |cx, inputs| to_num(cx, &inputs[0], 2, LITTLE_ENDIAN);
    CONVERT_SERIAL_TO_NUM_BE32 => |cx, inputs| to_num(cx, &inputs[0], 4, LITTLE_ENDIAN);
    CONVERT_SERIAL_TO_NUM_BE64 => |cx, inputs| to_num(cx, &inputs[0], 8, LITTLE_ENDIAN);
    CONVERT_STRUCT_TO_NUM_LE => |cx, inputs| to_num(cx, &inputs[0], inputs[0].width(), !LITTLE_ENDIAN);
    CONVERT_STRUCT_TO_NUM_BE => |cx, inputs| to_num(cx, &inputs[0], inputs[0].width(), LITTLE_ENDIAN);
    CONVERT_SERIAL_TO_STRUCT => serial_to_struct;
    CONVERT_NUM_TO_STRUCT => num_to_struct;
    CONVERT_NUM_TO_SERIAL => num_to_serial;
    CONVERT_STRUCT_TO_SERIAL => struct_to_serial;
    SEPARATE_STRING_COMPONENTS => separate_string_components;
    SET_STRING_LENS => set_string_lens;
}

/// `convertToNumWithOptionalSwap`.
fn to_num<'a>(
    cx: &mut NodeCx<'_, 'a>,
    input: &Stream<'a>,
    width: usize,
    swap: bool,
) -> Result<(), NodeError> {
    if !matches!(width, 1 | 2 | 4 | 8) {
        return Err(NodeError::InvalidInput(
            "numeric width must be 1, 2, 4 or 8",
        ));
    }
    let size = input.content_size();
    if !size.is_multiple_of(width) {
        return Err(NodeError::InvalidInput(
            "input size is not a multiple of the width",
        ));
    }
    let n_elts = size / width;
    if swap {
        let mut bytes = input.bytes().to_vec();
        bytes.chunks_exact_mut(width).for_each(<[u8]>::reverse);
        cx.emit(0, width, bytes)?;
    } else {
        cx.reference(0, input, width, n_elts, 0)?;
    }
    Ok(())
}

/// `EI_convert_serial_to_struct`: the token size is a required parameter.
fn serial_to_struct<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let token = cx
        .int(TOKEN_SIZE)
        .ok_or(NodeError::InvalidParam("the token size is not set"))?;
    let token = usize::try_from(token)
        .ok()
        .filter(|&token| token > 0)
        .ok_or(NodeError::InvalidParam("the token size must be positive"))?;
    let size = inputs[0].content_size();
    if !size.is_multiple_of(token) {
        return Err(NodeError::InvalidInput(
            "input size is not a multiple of the token size",
        ));
    }
    cx.reference(0, &inputs[0], token, size / token, 0)?;
    Ok(())
}

/// `EI_convert_num_to_struct_le`: the width is preserved.
fn num_to_struct<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    cx.reference(0, input, input.width(), input.n_elts(), 0)?;
    Ok(())
}

/// `EI_convert_to_serial`.
fn to_serial<'a>(cx: &mut NodeCx<'_, 'a>, input: &Stream<'a>) -> Result<(), NodeError> {
    cx.reference(0, input, 1, input.content_size(), 0)?;
    Ok(())
}

/// `EI_convert_num_to_serial_le`: header is `nextPow2(width)` as one byte.
fn num_to_serial<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let width = inputs[0].width();
    let header = [width.next_power_of_two().trailing_zeros() as u8];
    cx.send_header(&header)?;
    to_serial(cx, &inputs[0])
}

/// `EI_convert_struct_to_serial`: header is `varint(width)`.
fn struct_to_serial<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let mut header = Vec::new();
    varint::push(&mut header, inputs[0].width() as u64);
    cx.send_header(&header)?;
    to_serial(cx, &inputs[0])
}

/// `NUMOP_numericWidthForValue`.
const fn width_for(max: u32) -> usize {
    if max < 256 {
        1
    } else if max < 65_536 {
        2
    } else {
        4
    }
}

/// `EI_separate_VSF_components`: the contents as serial and the lengths as a
/// numeric stream of the smallest width that holds the longest string.
fn separate_string_components<'a>(
    cx: &mut NodeCx<'_, 'a>,
    inputs: &[Stream<'a>],
) -> Result<(), NodeError> {
    let input = &inputs[0];
    to_serial(cx, input)?;
    let lens = input.lens();
    let width = width_for(lens.iter().copied().max().unwrap_or(0));
    let mut bytes = Vec::with_capacity(lens.len() * width);
    for &len in lens {
        bytes.extend_from_slice(&len.to_le_bytes()[..width]);
    }
    cx.emit(1, width, bytes)?;
    Ok(())
}

/// `EI_setStringLens`: serial bytes become strings of the given lengths,
/// passed as the reference parameter [`SET_STRING_LENS_ARRAY`] (a
/// `Vec<u32>`). The parser-function form (parameter 520) is not supported.
fn set_string_lens<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let lens = match cx.ref_param(SET_STRING_LENS_ARRAY) {
        Some(object) => object
            .downcast_ref::<Vec<u32>>()
            .ok_or(NodeError::InvalidParam(
                "the string lengths have the wrong type",
            ))?
            .clone(),
        None if cx.copy_param(SET_STRING_LENS_PARSER).is_some() => {
            return Err(NodeError::UnsupportedParam("string length parsers"));
        }
        None => return Err(NodeError::InvalidParam("setStringLens parser not provided")),
    };
    let input = &inputs[0];
    let total: u64 = lens.iter().map(|&len| u64::from(len)).sum();
    if total != input.content_size() as u64 {
        return Err(NodeError::InvalidParam(
            "the string lengths do not sum to the input size",
        ));
    }
    cx.reference_strings(0, input, lens)?;
    Ok(())
}
