//! The LZ4 node (`EI_lz4`, `encode_lz4_binding.c`), node 86, codec 62.
//!
//! The reference calls `LZ4_compress_fast` with acceleration
//! `-level + 1` (1 for a level of 0 or 1) for levels up to 1 and
//! `LZ4_compress_HC` above, into a buffer of `LZ4_compressBound` bytes; the
//! level is the global compression level unless the node's integer
//! parameter 0 overrides it. The output is the raw LZ4 block, and the codec
//! header is the input size as a varint.
//!
//! Both compressors are safe-Rust ports of lz4 1.10.0 (`lz4.c` and
//! `lz4hc.c`, including the `lz4mid` strategy and the optimal parser) and
//! are byte-identical to the C library for the case OpenZL uses: a fresh
//! stream with an output buffer large enough that it never limits the
//! output. The limited-output, fill-output, dictionary and streaming entry
//! points of LZ4 are not ported.

mod block;
mod fast;
mod hc;
mod optimal;
#[cfg(test)]
mod tests;

use alloc::vec::Vec;

use super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::varint;

pub(crate) use block::MAX_INPUT_SIZE;

/// The node encoder.
pub(crate) static LZ4: FnEncoder = FnEncoder(encode);

/// `ZL_LZ4_COMPRESSION_LEVEL_OVERRIDE_PID`.
const LEVEL_OVERRIDE: i32 = crate::graph::params::lz4::COMPRESSION_LEVEL_OVERRIDE;

/// The raw LZ4 block the reference writes for `src` at `level`: `None`
/// when `src` exceeds `LZ4_MAX_INPUT_SIZE`.
pub(crate) fn compress_block(src: &[u8], level: i32) -> Option<Vec<u8>> {
    if src.len() > MAX_INPUT_SIZE {
        return None;
    }
    Some(if level <= 1 {
        let acceleration = if level < 0 {
            level.wrapping_neg().wrapping_add(1)
        } else {
            1
        };
        fast::compress(src, acceleration)
    } else {
        hc::compress(src, level)
    })
}

fn encode<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let size = input.content_size();
    let level = cx.int(LEVEL_OVERRIDE).unwrap_or_else(|| cx.level());
    let block = compress_block(input.bytes(), level)
        .ok_or(NodeError::InvalidInput("the input is too large for LZ4"))?;
    let mut header = Vec::new();
    varint::push(&mut header, size as u64);
    cx.emit(0, 1, block)?;
    cx.send_header(&header)?;
    Ok(())
}
