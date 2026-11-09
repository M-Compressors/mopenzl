//! Decoder of the `parse_int` feature: `parse_int` (60).
//!
//! Ports `codecs/parse_int/decode_parse_int_binding.c` and
//! `decode_parse_int_kernel.c`. The input is a numeric stream of 64-bit
//! signed integers; each becomes its canonical decimal string (no leading
//! zeros, no `+`, a single `-` when negative). The C kernel formats four
//! digits at a time from a lookup table, backwards; the result is the same
//! as plain division, which is what is used here.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_parse_int`: integers back to decimal strings.
pub(crate) const PARSE_INT: Desc = Desc {
    id: 60,
    min_version: 19,
    regens: &[Type::String],
    variable_regens: false,
    inputs: &[Type::Numeric],
    variable_inputs: 0,
    decode,
};

/// Characters of the decimal form of `value`: digits plus the sign.
const fn text_len(value: i64) -> usize {
    let digits = match value.unsigned_abs().checked_ilog10() {
        Some(log) => log as usize + 1,
        None => 1,
    };
    digits + (value < 0) as usize
}

/// `DI_parseInt`.
fn decode(_: &mut Ctx<'_>, inputs: &[Input<'_>], regens: &mut [Stream]) -> Result<(), DecodeError> {
    let numbers = &inputs[0];
    if numbers.width != 8 {
        return Err(corrupt());
    }
    let values = || {
        numbers
            .bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|n| i64::from_le_bytes(*n))
    };
    let total = values().map(text_len).try_fold(0usize, usize::checked_add);
    let (bytes, lens) = regens[0].strings(numbers.len(), total.ok_or_else(corrupt)?)?;
    for value in values() {
        let start = bytes.len();
        if value < 0 {
            bytes.push(b'-');
        }
        let mut magnitude = value.unsigned_abs();
        let mut digits = [0u8; 20];
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = b'0' + (magnitude % 10) as u8;
            magnitude /= 10;
            if magnitude == 0 {
                break;
            }
        }
        bytes.extend_from_slice(&digits[at..]);
        lens.push((bytes.len() - start) as u32);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    fn run(width: usize, bytes: &[u8], budget: usize) -> Result<Stream, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[]);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::String, budget);
        let input = Input {
            ty: Type::Numeric,
            width,
            bytes,
            lens: &[],
        };
        (PARSE_INT.decode)(&mut ctx, &[input], &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream)
    }

    #[test]
    fn integers_become_canonical_decimals() {
        let values = [
            0i64,
            1,
            -1,
            9,
            10,
            99,
            100,
            12345,
            -12345,
            9_999_999_999,
            i64::MAX,
            i64::MIN,
            i64::MIN + 1,
            1_000_000_000_000_000_000,
        ];
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let out = run(8, &bytes, 1 << 10).unwrap();
        let text: String = values.iter().map(i64::to_string).collect();
        assert_eq!(out.bytes(), text.as_bytes());
        let lens: Vec<u32> = values.iter().map(|v| v.to_string().len() as u32).collect();
        assert_eq!(out.as_input().lens, lens);
    }

    #[test]
    fn empty_streams_and_bad_inputs() {
        assert!(run(8, &[], 0).unwrap().as_input().lens.is_empty());
        // Only 64-bit numbers parse.
        assert!(run(4, &[0; 8], 1 << 10).is_err());
        // The output is bounded by the budget.
        let bytes = 123456i64.to_le_bytes();
        assert!(run(8, &bytes, 9).is_err());
        assert!(run(8, &bytes, 10).is_ok());
    }
}
