//! The `parse_int` node 38 (`encode_parse_int_binding.c`,
//! `encode_parse_int_kernel.c`).
//!
//! Strings holding canonical decimal `i64` values become a numeric stream of
//! width 8. A string must be an optional `-`, then digits without leading
//! zeros (`0` alone is fine, `-0` is not), and fit an `i64`; anything else
//! fails the node, which is how `try_parse_int` detects exceptions.
//!
//! The reference has an AVX2 and a scalar parser that accept exactly the
//! same strings; this port is the scalar one (`ZL_parseInt64_fallback`).
//! The `try_parse_int` graph hands the node values it already parsed through
//! the `ZL_PARSE_INT_PREPARSED_PARAMS` reference parameter (a `Vec<i64>`, or
//! the native-endian bytes of a copy parameter).

use alloc::vec::Vec;

use crate::compressor::core::codecs::FnEncoder;
use super::{PREPARSED_PARAMS_PID, parse_int64};
use super::util::{Param, local_param};
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;


/// `EI_parseInt`.
pub(crate) static PARSE_INT: FnEncoder = FnEncoder(parse_int);

fn parse_int<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let nb_elts = input.n_elts();
    let mut bytes = Vec::with_capacity(nb_elts * 8);
    let preparsed = match local_param(cx, PREPARSED_PARAMS_PID) {
        Some(Param::Object(object)) => Some(
            object
                .downcast_ref::<Vec<i64>>()
                .ok_or(NodeError::InvalidParam(
                    "the pre-parsed integers have the wrong type",
                ))?
                .clone(),
        ),
        Some(Param::Bytes(raw)) => {
            if raw.len() % 8 != 0 {
                return Err(NodeError::InvalidParam(
                    "the pre-parsed integers are not whole integers",
                ));
            }
            Some(
                raw.as_chunks::<8>()
                    .0
                    .iter()
                    .map(|chunk| i64::from_ne_bytes(*chunk))
                    .collect(),
            )
        }
        None => None,
    };
    match preparsed {
        Some(values) if nb_elts > 0 => {
            if values.len() != nb_elts {
                return Err(NodeError::InvalidParam(
                    "the pre-parsed integers do not match the strings",
                ));
            }
            for value in values {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        _ => {
            let mut rest = input.bytes();
            for &len in input.lens() {
                let (text, tail) = rest.split_at(len as usize);
                let value = parse_int64(text).ok_or(NodeError::InvalidInput(
                    "a string is not a canonical integer",
                ))?;
                bytes.extend_from_slice(&value.to_le_bytes());
                rest = tail;
            }
        }
    }
    cx.emit(0, 8, bytes)?;
    Ok(())
}
