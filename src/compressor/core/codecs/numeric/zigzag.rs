//! The zigzag node (`EI_zigzag_num`, `encode_zigzag_kernel.c`), codec 3.
//!
//! Each element, read as a signed integer of its width, becomes
//! `(n << 1) ^ (n >> (bits - 1))`: small magnitudes of either sign map to
//! small unsigned numbers. No header, same width and count.

use alloc::vec::Vec;

use super::super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::numeric::{elements, push_le};

/// `ZL_NODE_ZIGZAG` (node 4).
pub(crate) static ZIGZAG: FnEncoder = FnEncoder(zigzag);

/// `ZL_zigzagEncodeN` for one element of `bits` bits, held zero-extended.
const fn map(value: u64, bits: u32) -> u64 {
    let negative = (value >> (bits - 1)) & 1;
    (value << 1) ^ negative.wrapping_neg()
}

fn zigzag<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let width = input.width();
    let bits = 8 * width as u32;
    let mut out = Vec::with_capacity(input.content_size());
    for value in elements(input.bytes(), width) {
        push_le(&mut out, map(value, bits), width);
    }
    cx.emit(0, width, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_values_interleave_by_magnitude() {
        let out = |value: u64, bits: u32| map(value, bits) & (u64::MAX >> (64 - bits));
        assert_eq!(out(0, 8), 0);
        assert_eq!(out(0xff, 8), 1); // -1
        assert_eq!(out(1, 8), 2);
        assert_eq!(out(0xfe, 8), 3); // -2
        assert_eq!(out(0x80, 8), 0xff); // i8::MIN
        assert_eq!(out(0x7f, 8), 0xfe);
        assert_eq!(out(0x8000, 16), 0xffff);
        assert_eq!(out(u64::from(u32::MAX), 32), 1);
        assert_eq!(out(1 << 63, 64), u64::MAX);
        assert_eq!(out(u64::MAX, 64), 1);
    }
}
