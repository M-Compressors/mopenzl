//! The Zstandard node (`EI_zstd`, encode_zstd_binding.c), node 62.
//!
//! The reference resets a `ZSTD_CCtx`, selects the magicless format and the
//! global compression level, sets `literalCompressionMode = uncompressed`
//! when the decompression level is 1, then applies the node's local integer
//! parameters in list order (they are raw `ZSTD_c_*` IDs) before running
//! zstd. The output is `varint(element width)` followed by the magicless
//! frame.
//!
//! The parameters map onto `mzstd`'s `CParameter` (`ZSTD_CCtx_setParameter`
//! of a single-threaded build, with its clamping and range errors). The
//! binding's own refusals come first: `ZSTD_c_format` and
//! `ZSTD_c_contentSizeFlag` cannot be modified, and the window, hash, chain
//! and long-distance hash logs stop three below their maximum
//! (`EI_zstd_parameter_valid`). Every refusal fails the node, as in C.
//!
//! Input of at most eight elements of at least 1024 bytes is compressed one
//! flushed block per element (`ZstdNode::encode_elements`). The registered
//! node takes serial streams only (`PIPE_GRAPH`), whose width is 1, so that
//! path serves the deprecated struct variant of the codec and tests; the node
//! reads the width and element count from its input anyway.
//!
//! A node that references a dictionary (`ZL_Encoder_getMaterializedDict`)
//! compresses with it through the one-shot `ZSTD_CCtx_refCDict` +
//! `ZSTD_compress2` route; the block-splitting route ignores it, as in C.
//! The dictionary is a `ZSTD_createCDict_advanced` made at the level stored
//! in its content, which `mzstd` models as a standalone prepared dictionary
//! (see `registry::dict`).

use mzstd::CParameter;

use super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::compressor::core::registry::dict::Prepared;
use crate::compressor::core::zstd::zstd_config;

/// The node encoder.
pub(crate) static ZSTD: FnEncoder = FnEncoder(encode);

/// `ZSTD_c_format`.
const C_FORMAT: i32 = crate::graph::params::zstd::FORMAT;
/// `ZSTD_c_contentSizeFlag`.
const C_CONTENT_SIZE_FLAG: i32 = crate::graph::params::zstd::CONTENT_SIZE_FLAG;
/// `ZSTD_MEMORY_USAGE_CAP_LOG_FACTOR`.
const MEMORY_USAGE_CAP_LOG_FACTOR: u32 = 3;

/// The configuration of a run: the global parameters, then the local integer
/// parameters in order (`EI_zstdWithCCtx`).
fn node_config(cx: &NodeCx<'_, '_>) -> Result<mzstd::EncoderConfig, NodeError> {
    let mut config = zstd_config(0);
    let level = CParameter::try_from((crate::graph::params::zstd::COMPRESSION_LEVEL, cx.level()))
        .map_err(|_| NodeError::InvalidParam("invalid zstd compression level"))?;
    config = config.with_parameter(level);
    if cx.decompression_level() == 1 {
        config = config.with_parameter(CParameter::LiteralCompressionMode(mzstd::Switch::Disabled));
    }
    for &(id, value) in cx.ints() {
        if id == C_FORMAT || id == C_CONTENT_SIZE_FLAG {
            return Err(NodeError::InvalidParam(
                "this zstd parameter cannot be modified",
            ));
        }
        let parameter = CParameter::try_from((id, value))
            .map_err(|_| NodeError::InvalidParam("zstd rejected the parameter"))?
            .check_memory_cap(MEMORY_USAGE_CAP_LOG_FACTOR)
            .map_err(|_| NodeError::InvalidParam("zstd memory parameter above its cap"))?;
        config = config.with_parameter(parameter);
    }
    Ok(config)
}

fn encode<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let config = node_config(cx)?;
    cx.zstd
        .set_config(config)
        .map_err(|()| NodeError::InvalidParam("invalid zstd parameter combination"))?;
    let dictionary = cx.dictionary.map(|Prepared::Zstd(dictionary)| dictionary);
    let stream = cx
        .zstd
        .encode_elements(input.bytes(), input.width(), input.n_elts(), dictionary)
        .ok_or(NodeError::Backend("zstd"))?
        .to_vec();
    cx.emit(0, 1, stream)?;
    Ok(())
}
