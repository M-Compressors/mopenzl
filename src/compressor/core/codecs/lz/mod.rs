//! The `lz` node (`EI_lz`, `encode_lz_binding.c` with `encode_lz_kernel.c`),
//! node 52, codec 19.
//!
//! A byte-oriented LZ77 parse of a serial stream into four streams: the
//! literals (serial), the offsets (numeric, 16 or 32 bits depending on the
//! window), and the literal and match lengths (numeric, 16 bits). The codec
//! header is `varint(input size)`; the match lengths carry the integer
//! metadata `ZL_LZ_MIN_MATCH_LENGTH_METADATA_ID` = 4 for `mux_lengths`.
//!
//! The parser (fast, double-fast, greedy, lazy or lazy2) and its tables come
//! from the compression level, with each `ZL_LzParam_*` parameter able to
//! override one field; see [`kernel`].

mod kernel;

use alloc::vec::Vec;

use super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::varint;
use kernel::{MIN_MATCH, Parameters};

/// The node encoder.
pub(crate) static LZ: FnEncoder = FnEncoder(encode);

/// `ZL_LzParam_compressionLevel`.
const PARAM_COMPRESSION_LEVEL: i32 = crate::graph::params::lz::COMPRESSION_LEVEL;
/// `ZL_LzParam_acceleration`.
const PARAM_ACCELERATION: i32 = crate::graph::params::lz::ACCELERATION;
/// `ZL_LzParam_windowLog`.
const PARAM_WINDOW_LOG: i32 = crate::graph::params::lz::WINDOW_LOG;
/// `ZL_LzParam_strategy`.
const PARAM_STRATEGY: i32 = crate::graph::params::lz::STRATEGY;
/// `ZL_LzParam_hashLog1`.
const PARAM_HASH_LOG1: i32 = crate::graph::params::lz::HASH_LOG1;
/// `ZL_LzParam_hashLog2`.
const PARAM_HASH_LOG2: i32 = crate::graph::params::lz::HASH_LOG2;
/// `ZL_LzParam_hashLength`.
const PARAM_HASH_LENGTH: i32 = crate::graph::params::lz::HASH_LENGTH;
/// `ZL_LzParam_searchLog`.
const PARAM_SEARCH_LOG: i32 = crate::graph::params::lz::SEARCH_LOG;
/// `ZL_LZ_MIN_MATCH_LENGTH_METADATA_ID`.
pub(crate) const MIN_MATCH_LENGTH_METADATA_ID: i32 =
    crate::graph::params::lz::MIN_MATCH_LENGTH_METADATA_ID;
/// `kLzContentSizeBytes`.
const CONTENT_LIMIT: usize = 4_000_000_000;

/// `getLzParams`: the defaults of the level (the node's own or the global
/// one), with each non-zero override parameter replacing its field, then
/// adjusted to the input size.
fn parameters(ints: &[(i32, i32)], global_level: i32, src_size: usize) -> Parameters {
    let mut level = 0;
    // 0 means "keep the default".
    let mut overrides = [0u32; 7];
    for &(id, value) in ints {
        let slot = match id {
            PARAM_COMPRESSION_LEVEL => {
                level = value;
                continue;
            }
            PARAM_STRATEGY => 0,
            PARAM_WINDOW_LOG => 1,
            PARAM_HASH_LOG1 => 2,
            PARAM_HASH_LOG2 => 3,
            PARAM_HASH_LENGTH => 4,
            PARAM_ACCELERATION => 5,
            PARAM_SEARCH_LOG => 6,
            _ => continue,
        };
        overrides[slot] = value as u32;
    }
    if level == 0 {
        level = global_level;
    }
    let mut params = Parameters::for_level(level);
    let [
        strategy,
        window_log,
        hash_log1,
        hash_log2,
        hash_length,
        acceleration,
        search_log,
    ] = overrides;
    for (field, value) in [
        (&mut params.strategy, strategy),
        (&mut params.window_log, window_log),
        (&mut params.hash_log1, hash_log1),
        (&mut params.hash_log2, hash_log2),
        (&mut params.hash_length, hash_length),
        (&mut params.acceleration, acceleration),
        (&mut params.search_log, search_log),
    ] {
        if value != 0 {
            *field = value;
        }
    }
    params.adjust(src_size);
    params
}

/// `EI_lz`.
fn encode<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let src = input.bytes();
    if src.len() > CONTENT_LIMIT {
        return Err(NodeError::InvalidInput("lz input is too large"));
    }
    let params = parameters(cx.ints(), cx.level(), src.len());
    let width = params.offset_width();
    let sequences = kernel::encode(src, &params);
    debug_assert!(sequences.offsets.len() <= kernel::max_sequences(src.len()));

    let mut header = Vec::new();
    varint::push(&mut header, src.len() as u64);
    cx.send_header(&header)?;
    cx.emit(0, 1, sequences.literals)?;
    let offsets = if width == 4 {
        sequences
            .offsets
            .iter()
            .flat_map(|offset| offset.to_le_bytes())
            .collect()
    } else {
        sequences
            .offsets
            .iter()
            .flat_map(|&offset| (offset as u16).to_le_bytes())
            .collect()
    };
    cx.emit(1, width, offsets)?;
    let lengths = |values: &[u16]| values.iter().flat_map(|v| v.to_le_bytes()).collect();
    cx.emit(2, 2, lengths(&sequences.literal_lengths))?;
    cx.emit(3, 2, lengths(&sequences.match_lengths))?;
    cx.set_int_metadata(3, MIN_MATCH_LENGTH_METADATA_ID, MIN_MATCH as i32)?;
    Ok(())
}

#[cfg(test)]
mod tests;
