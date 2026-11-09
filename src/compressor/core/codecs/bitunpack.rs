//! The `bitunpack` node (`EI_bitunpack`, `encode_bitunpack_binding.c`),
//! node 27, codec 34.
//!
//! Serial bytes become numbers of `nbBits` bits (local parameter 1, 1 to
//! 64), of the smallest width that holds them. The input must be exactly
//! the bytes of whole fields plus padding. The codec header is `nbBits`
//! and, when the padding bits of the last byte are not zero, those bits as
//! a second byte (format version 7 and later; the node fails below).

use super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::bitpack_kernels;

/// The `bitunpack` node.
pub(crate) static BITUNPACK: FnEncoder = FnEncoder(encode_bitunpack);

/// `ZL_Bitunpack_numBits`.
const NUM_BITS: i32 = crate::graph::params::bitunpack::NUM_BITS;

/// `EI_bitunpack`.
fn encode_bitunpack<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let nb_bits = cx
        .int(NUM_BITS)
        .ok_or(NodeError::InvalidParam("the number of bits is not set"))?;
    if !(1..=64).contains(&nb_bits) {
        return Err(NodeError::InvalidParam(
            "the number of bits is out of range",
        ));
    }
    let nb_bits = nb_bits as usize;
    let src = inputs[0].bytes();
    let src_size = src.len();
    let nb_elts = src_size * 8 / nb_bits;
    if (nb_elts * nb_bits).div_ceil(8) != src_size {
        return Err(NodeError::InvalidInput(
            "the input does not fit whole fields and zero padding",
        ));
    }
    let width = match nb_bits {
        1..=8 => 1,
        9..=16 => 2,
        17..=32 => 4,
        _ => 8,
    };
    let (unpacked, used) = bitpack_kernels::decode(src, nb_elts, width, nb_bits as u32);
    if used != src_size {
        return Err(NodeError::InvalidInput("bitunpack consumed the wrong size"));
    }
    let mut header = [nb_bits as u8, 0];
    let mut header_size = 1;
    let rem_nb_bits = src_size * 8 - nb_elts * nb_bits;
    if rem_nb_bits != 0 {
        let rem_bits = src[src_size - 1] >> (8 - rem_nb_bits);
        if rem_bits != 0 {
            if cx.format_version() >= 7 {
                header[1] = rem_bits;
                header_size = 2;
            } else {
                return Err(NodeError::InvalidInput(
                    "Bitunpack supports non-zero trailing bits starting at format version 7",
                ));
            }
        }
    }
    cx.emit(0, width, unpacked)?;
    cx.send_header(&header[..header_size])?;
    Ok(())
}
