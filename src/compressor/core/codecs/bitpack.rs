//! The `bitpack_serial` (63, codec 27) and `bitpack_int` (64, codec 28) nodes.
//!
//! Ports `codecs/bitpack/encode_bitpack_binding.c` (`EI_bitpack_typed`);
//! the kernels are in [`crate::shared::bitpack_kernels`].
//!
//! `bitpack` packs every element into `1 + highbit(max)` bits (one bit when
//! the maximum is zero). Its codec header is `log2(width) << 6 | (nbBits -
//! 1)`, plus a second byte counting the padding fields the last byte could
//! still hold when there are any.

use super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::bitpack_kernels;

/// `bitpack_serial` and `bitpack_int`.
pub(crate) static BITPACK: FnEncoder = FnEncoder(encode_bitpack);

/// `computeNbBits`: the bits of the largest element.
fn compute_nb_bits(bytes: &[u8], width: usize) -> u32 {
    let mut max = 0u64;
    for element in bytes.chunks_exact(width) {
        let mut buffer = [0u8; 8];
        buffer[..width].copy_from_slice(element);
        max = max.max(u64::from_le_bytes(buffer));
    }
    1 + max.checked_ilog2().unwrap_or(0)
}

/// `EI_bitpack_typed`.
fn encode_bitpack<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let width = input.width();
    if !matches!(width, 1 | 2 | 4 | 8) {
        return Err(NodeError::InvalidInput(
            "Bitpack expects element width of 1, 2, 4 or 8",
        ));
    }
    let nb_elts = input.n_elts();
    let nb_bits = compute_nb_bits(input.bytes(), width);
    let packed = bitpack_kernels::encode(input.bytes(), width, nb_bits);
    let max_nb_elts = packed.len() * 8 / nb_bits as usize;
    // Bits 0 to 5 hold nbBits - 1, bits 6 and 7 log2(width).
    let first = ((width.trailing_zeros() as u8) << 6) | (nb_bits - 1) as u8;
    if max_nb_elts > nb_elts {
        cx.send_header(&[first, (max_nb_elts - nb_elts) as u8])?;
    } else {
        cx.send_header(&[first])?;
    }
    cx.emit(0, 1, packed)?;
    Ok(())
}
