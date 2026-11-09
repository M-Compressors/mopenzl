//! Decoder of the `sentinel` feature: `sentinel` (18).
//!
//! Ports `decode_sentinel_binding.c` and `decode_sentinel_kernel.c`
//! (`ZL_sentinelDecode`). The values stream holds one element per output
//! element, possibly narrower than the output; where it equals the
//! sentinel the real value is the next element of the exceptions stream.
//! The output has the exceptions' width. The codec header is empty for the
//! default sentinel, the all-ones value of the values' width, and a varint
//! otherwise.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;
use crate::shared::varint;

/// `ZL_StandardTransformID_sentinel`.
pub(crate) const SENTINEL: Desc = Desc {
    id: 18,
    min_version: 24,
    regens: &[Type::Numeric],
    variable_regens: false,
    inputs: &[Type::Numeric, Type::Numeric],
    variable_inputs: 0,
    decode: restore,
};

/// The little-endian number in `bytes` (at most 8 bytes).
fn read_le(bytes: &[u8]) -> u64 {
    let mut word = [0u8; 8];
    word[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(word)
}

/// `DI_sentinel`.
fn restore(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let (values, exceptions) = (&inputs[0], &inputs[1]);
    let (value_width, width) = (values.width, exceptions.width);
    if value_width > width {
        return Err(corrupt());
    }
    let max = if value_width >= 8 {
        u64::MAX
    } else {
        (1u64 << (8 * value_width)) - 1
    };
    let sentinel = if ctx.header.is_empty() {
        max
    } else {
        let (sentinel, _) = varint::decode(ctx.header).map_err(|_| corrupt())?;
        if sentinel > max {
            return Err(corrupt());
        }
        sentinel
    };
    let out = regens[0].fixed(width, values.len())?;
    let mut exceptions = exceptions.bytes.chunks_exact(width);
    for value in values.bytes.chunks_exact(value_width) {
        let value = read_le(value);
        let restored = if value == sentinel {
            read_le(exceptions.next().ok_or_else(corrupt)?)
        } else {
            value
        };
        out.extend_from_slice(&restored.to_le_bytes()[..width]);
    }
    if exceptions.next().is_some() {
        return Err(corrupt());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;

    fn numbers(width: usize, bytes: &[u8]) -> Input<'_> {
        Input {
            ty: Type::Numeric,
            width,
            bytes,
            lens: &[],
        }
    }

    fn run(header: &[u8], values: Input<'_>, exceptions: Input<'_>) -> Result<Stream, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, header);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::Numeric, 1 << 16);
        restore(&mut ctx, &[values, exceptions], &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream)
    }

    #[test]
    fn the_default_sentinel_is_all_ones() {
        // Bytes widened to 32 bits; 255 marks an exception.
        let exceptions: alloc::vec::Vec<u8> = [1000u32, 70000]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let out = run(&[], numbers(1, &[3, 255, 4, 255]), numbers(4, &exceptions)).unwrap();
        let expected: alloc::vec::Vec<u8> = [3u32, 1000, 4, 70000]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(out.bytes(), expected);
        assert_eq!(out.as_input().width, 4);
    }

    #[test]
    fn a_header_names_another_sentinel() {
        let out = run(&[0], numbers(2, &[0, 0, 1, 0]), numbers(2, &[9, 9])).unwrap();
        assert_eq!(out.bytes(), [9, 9, 1, 0]);
        // 64-bit values at the maximum sentinel and an empty exceptions list.
        let values = u64::MAX.to_le_bytes();
        let out = run(&[], numbers(8, &values[..0]), numbers(8, &[])).unwrap();
        assert_eq!(out.as_input().len(), 0);
        let out = run(&[], numbers(8, &values), numbers(8, &7u64.to_le_bytes())).unwrap();
        assert_eq!(out.bytes(), 7u64.to_le_bytes());
        // A sentinel as large as the width allows.
        let out = run(
            &[0xff, 0xff, 0x03],
            numbers(4, &[0xff, 0xff, 0, 0]),
            numbers(4, &[1, 0, 0, 0]),
        )
        .unwrap();
        assert_eq!(out.bytes(), [1, 0, 0, 0]);
    }

    #[test]
    fn missing_or_leftover_exceptions_are_corrupt() {
        assert_eq!(
            run(&[], numbers(1, &[255, 255]), numbers(2, &[1, 0])).map(|_| ()),
            Err(corrupt())
        );
        assert_eq!(
            run(&[], numbers(1, &[1]), numbers(2, &[1, 0])).map(|_| ()),
            Err(corrupt())
        );
    }

    #[test]
    fn widths_and_headers_are_checked() {
        // Values wider than the output.
        assert_eq!(
            run(&[], numbers(4, &[0; 4]), numbers(2, &[])).map(|_| ()),
            Err(corrupt())
        );
        // A sentinel past the values' width, and a truncated varint.
        assert_eq!(
            run(&[0x80, 0x02], numbers(1, &[0]), numbers(1, &[])).map(|_| ()),
            Err(corrupt())
        );
        assert_eq!(
            run(&[0x80], numbers(1, &[0]), numbers(1, &[])).map(|_| ()),
            Err(corrupt())
        );
    }
}
