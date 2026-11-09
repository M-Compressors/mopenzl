//! Helpers for the unit tests of the LZ nodes: running a node encoder
//! directly, without the graph engine.

use alloc::borrow::Cow;
#[cfg(any(feature = "field-lz", feature = "lz"))]
use alloc::vec::Vec;

use crate::compressor::config::EncoderConfig;
use crate::compressor::core::Params;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::compressor::core::exec::node_cx::NodeOutputs;
use crate::compressor::core::params::LocalParams;
use crate::compressor::core::registry::nodes;
use crate::{Backend, Level};

/// Runs `body` with a fresh context of standard node `node`.
pub(crate) fn with_cx<'a, T>(
    node: u32,
    ints: &[(i32, i32)],
    level: i32,
    body: impl FnOnce(NodeCx<'_, 'a>) -> T,
) -> T {
    let spec = nodes::standard(node).unwrap();
    let local = (!ints.is_empty()).then(|| LocalParams {
        ints: Cow::Owned(ints.to_vec()),
        ..LocalParams::default()
    });
    let config = EncoderConfig::default().with_level(Level::try_from(level).unwrap());
    let params = Params::from(&config);
    #[cfg(feature = "zstd")]
    let mut zstd = crate::compressor::core::zstd::ZstdNode::new(params.level, Backend::default());
    #[cfg(not(feature = "zstd"))]
    let _ = Backend::default();
    let cx = NodeCx::new(
        spec,
        local.as_ref(),
        &params,
        #[cfg(feature = "zstd")]
        &mut zstd,
    );
    body(cx)
}

/// Runs standard node `node` on `inputs` with global level `level` and the
/// local integer parameters `ints` (the node's defaults when empty).
pub(crate) fn run_node<'a>(
    node: u32,
    ints: &[(i32, i32)],
    level: i32,
    inputs: &[Stream<'a>],
) -> Result<NodeOutputs<'a>, NodeError> {
    let spec = nodes::standard(node).unwrap();
    with_cx(node, ints, level, |mut cx| {
        spec.encoder.encode(&mut cx, inputs)?;
        cx.finish()
    })
}

#[cfg(any(feature = "field-lz", feature = "lz"))]
/// The 16-bit little-endian values of `bytes`.
pub(crate) fn u16s(bytes: &[u8]) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .collect()
}

#[cfg(any(feature = "field-lz", feature = "lz"))]
/// The 32-bit little-endian values of `bytes`.
pub(crate) fn u32s(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&c| u32::from_le_bytes(c))
        .collect()
}

#[cfg(any(feature = "field-lz", feature = "lz"))]
/// Deterministic bytes of low entropy: a few words, shuffled.
pub(crate) fn text(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2_654_435_761) | 1;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        out.extend_from_slice(
            [
                &b"alpha "[..],
                b"beta ",
                b"gamma ",
                b"delta ",
                b"the ",
                b"of ",
            ][(state >> 24) as usize % 6],
        );
    }
    out.truncate(len);
    out
}

#[cfg(any(feature = "field-lz", feature = "lz"))]
/// Deterministic incompressible bytes.
pub(crate) fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2_654_435_761) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 24) as u8
        })
        .collect()
}
