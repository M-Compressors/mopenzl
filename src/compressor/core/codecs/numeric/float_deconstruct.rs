//! The float-deconstruct nodes (`EI_float32_deconstruct`,
//! `EI_bfloat16_deconstruct`, `EI_float16_deconstruct`,
//! `encode_float_deconstruct_binding.c` and
//! `encode_float_deconstruct_kernel.c`), codec 33.
//!
//! Floating-point numbers are split into a struct stream of the sign and
//! fraction bits and a serial stream of exponent bytes. The sign moves to
//! bit 0 of the sign-fraction element, the fraction above it. The element type
//! is the one-byte header (0 float32, 1 bfloat16, 2 float16); format versions
//! below 5, which had no header, are not supported.
//!
//! The reference has AVX2 kernels; they write what the scalar kernel writes,
//! which is what this port does.

use alloc::vec::Vec;

use super::super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::numeric::{elements, push_le};

/// `ZL_NODE_FLOAT32_DECONSTRUCT` (node 6).
pub(crate) static FLOAT32_DECONSTRUCT: FnEncoder =
    FnEncoder(|cx, inputs| deconstruct(cx, &inputs[0], Kind::Float32));
/// `ZL_NODE_BFLOAT16_DECONSTRUCT` (node 7).
pub(crate) static BFLOAT16_DECONSTRUCT: FnEncoder =
    FnEncoder(|cx, inputs| deconstruct(cx, &inputs[0], Kind::Bfloat16));
/// `ZL_NODE_FLOAT16_DECONSTRUCT` (node 8).
pub(crate) static FLOAT16_DECONSTRUCT: FnEncoder =
    FnEncoder(|cx, inputs| deconstruct(cx, &inputs[0], Kind::Float16));

/// `FLTDECON_ElementType_e`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Kind {
    Float32 = 0,
    Bfloat16 = 1,
    Float16 = 2,
}

impl Kind {
    /// `FLTDECON_ElementWidth`.
    const fn element_width(self) -> usize {
        match self {
            Self::Float32 => 4,
            Self::Bfloat16 | Self::Float16 => 2,
        }
    }

    /// `FLTDECON_SignFracWidth`.
    const fn sign_fraction_width(self) -> usize {
        match self {
            Self::Float32 => 3,
            Self::Bfloat16 => 1,
            Self::Float16 => 2,
        }
    }

    /// The exponent byte and the sign-fraction value of one element.
    const fn split(self, value: u64) -> (u8, u64) {
        match self {
            Self::Float32 => {
                let f = value as u32;
                ((f >> 23) as u8, f.rotate_left(1) as u64)
            }
            Self::Bfloat16 => {
                let bf = value as u16;
                ((bf >> 7) as u8, ((bf >> 15) as u8 | (bf << 1) as u8) as u64)
            }
            Self::Float16 => {
                let f = value as u16;
                let exp_frac = f << 1;
                ((exp_frac >> 11) as u8, ((exp_frac & 0x7ff) | (f >> 15)) as u64)
            }
        }
    }
}

/// `float_deconstruct`.
fn deconstruct<'a>(
    cx: &mut NodeCx<'_, 'a>,
    input: &Stream<'a>,
    kind: Kind,
) -> Result<(), NodeError> {
    if input.width() != kind.element_width() {
        return Err(NodeError::InvalidInput("unexpected element width"));
    }
    cx.send_header(&[kind as u8])?;
    let sign_fraction_width = kind.sign_fraction_width();
    let mut sign_fraction = Vec::with_capacity(input.n_elts() * sign_fraction_width);
    let mut exponent = Vec::with_capacity(input.n_elts());
    for value in elements(input.bytes(), input.width()) {
        let (exp, packed) = kind.split(value);
        exponent.push(exp);
        push_le(&mut sign_fraction, packed, sign_fraction_width);
    }
    cx.emit(0, sign_fraction_width, sign_fraction)?;
    cx.emit(1, 1, exponent)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float32_keeps_the_sign_in_bit_zero() {
        // -1.5f32 = 1 | 01111111 | 1000...0
        let bits = (-1.5f32).to_bits();
        let (exponent, packed) = Kind::Float32.split(u64::from(bits));
        assert_eq!(exponent, 0x7f);
        // Only the low three bytes are stored.
        let stored = packed & 0xff_ffff;
        assert_eq!(stored & 1, 1);
        assert_eq!(stored >> 1, 1 << 22);
    }

    #[test]
    fn half_precision_layouts_split_exponent_and_fraction() {
        // bfloat16 of -2.0: sign 1, exponent 0x80, fraction 0.
        let (exponent, packed) = Kind::Bfloat16.split(0xc000);
        assert_eq!((exponent, packed), (0x80, 1));
        // binary16 of -2.0 = 0xc000: sign 1, exponent 0b10000, fraction 0.
        let (exponent, packed) = Kind::Float16.split(0xc000);
        assert_eq!((exponent, packed), (0b10000, 1));
        // A fraction bit survives: 0x3c01 is 1 + 2^-10.
        assert_eq!(Kind::Float16.split(0x3c01), (0b01111, 0b10));
        assert_eq!(
            [Kind::Float32, Kind::Bfloat16, Kind::Float16].map(Kind::element_width),
            [4, 2, 2]
        );
    }
}
