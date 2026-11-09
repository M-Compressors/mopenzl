//! Node encoders: the codecs the engine can run.
//!
//! Every standard node of the registry points at one of the statics here.
//! Every standard node has its own encoder; a row points at [`DISABLED`]
//! instead when its codec's Cargo feature is off, or at [`LEGACY`] for a
//! deprecated node never written at the supported format versions. Both fail
//! the node with a dedicated
//! [`NodeError`], so the engine reacts exactly as it does to a codec that
//! fails in the reference: a permissive compression falls back to
//! `compress_generic`, a selector's trial compression skips the candidate,
//! and a chunk that cannot be built is stored.
//!
//! # Adding an encoder
//!
//! Write a function `fn(&mut NodeCx<'_, 'a>, &[Stream<'a>]) -> Result<(),
//! NodeError>`, wrap it in a [`FnEncoder`] static, and point the node's row
//! in [`registry::nodes`](super::registry::nodes) at it.

#[cfg(feature = "bitpack")]
pub(crate) mod bitpack;
#[cfg(feature = "bitunpack")]
pub(crate) mod bitunpack;
#[cfg(feature = "constant")]
pub(crate) mod constant;
pub(crate) mod conversion;
#[cfg(all(feature = "entropy", feature = "compression"))]
pub(crate) mod entropy;
#[cfg(feature = "field-lz")]
pub(crate) mod field_lz;
#[cfg(feature = "flatpack")]
pub(crate) mod flatpack;
#[cfg(feature = "lz")]
pub(crate) mod lz;
#[cfg(feature = "lz4")]
pub(crate) mod lz4;
#[cfg(any(feature = "field-lz", feature = "lz"))]
pub(crate) mod lz_common;
#[cfg(feature = "mux-lengths")]
pub(crate) mod mux_lengths;
pub(crate) mod numeric;
#[cfg(all(feature = "pivco-huffman", feature = "compression"))]
pub(crate) mod pivco_huffman;
#[cfg(feature = "quantize")]
pub(crate) mod quantize;
pub(crate) mod structure;
#[cfg(all(
    test,
    any(
        feature = "field-lz",
        feature = "lz",
        feature = "mux-lengths",
        feature = "quantize"
    )
))]
pub(crate) mod test_util;
#[cfg(feature = "zstd")]
pub(crate) mod zstd_node;

use super::data::Stream;
use super::error::NodeError;
use super::exec::{Encode, NodeCx};

#[cfg(feature = "bitpack")]
pub(crate) use bitpack::BITPACK;
/// Without the `bitpack` feature the node's row selects [`DISABLED`] instead.
#[cfg(not(feature = "bitpack"))]
pub(crate) static BITPACK: Disabled = Disabled;
#[cfg(feature = "bitunpack")]
pub(crate) use bitunpack::BITUNPACK;
/// Without the `bitunpack` feature the node's row selects [`DISABLED`] instead.
#[cfg(not(feature = "bitunpack"))]
pub(crate) static BITUNPACK: Disabled = Disabled;
#[cfg(feature = "flatpack")]
pub(crate) use flatpack::FLATPACK;
/// Without the `flatpack` feature the node's row selects [`DISABLED`] instead.
#[cfg(not(feature = "flatpack"))]
pub(crate) static FLATPACK: Disabled = Disabled;
#[cfg(feature = "lz4")]
pub(crate) use lz4::LZ4;
/// Without the `lz4` feature the node's row selects [`DISABLED`] instead.
#[cfg(not(feature = "lz4"))]
pub(crate) static LZ4: Disabled = Disabled;
#[cfg(feature = "field-lz")]
pub(crate) use field_lz::FIELD_LZ;
/// Without the `field-lz` feature the node's row selects [`DISABLED`] instead.
#[cfg(not(feature = "field-lz"))]
pub(crate) static FIELD_LZ: Disabled = Disabled;
#[cfg(feature = "lz")]
pub(crate) use lz::LZ;
/// Without the `lz` feature the node's row selects [`DISABLED`] instead.
#[cfg(not(feature = "lz"))]
pub(crate) static LZ: Disabled = Disabled;
#[cfg(feature = "mux-lengths")]
pub(crate) use mux_lengths::MUX_LENGTHS;
/// Without the `mux-lengths` feature the node's row selects [`DISABLED`] instead.
#[cfg(not(feature = "mux-lengths"))]
pub(crate) static MUX_LENGTHS: Disabled = Disabled;
#[cfg(feature = "quantize")]
pub(crate) use quantize::{QUANTIZE_LENGTHS, QUANTIZE_OFFSETS};
/// Without the `quantize` feature the node's rows select [`DISABLED`] instead.
#[cfg(not(feature = "quantize"))]
pub(crate) static QUANTIZE_OFFSETS: Disabled = Disabled;
/// See [`QUANTIZE_OFFSETS`].
#[cfg(not(feature = "quantize"))]
pub(crate) static QUANTIZE_LENGTHS: Disabled = Disabled;
#[cfg(feature = "zstd")]
pub(crate) use zstd_node::ZSTD;
/// Without the `zstd` feature the node's row points here, which is never
/// run: the row selects [`DISABLED`].
#[cfg(not(feature = "zstd"))]
pub(crate) static ZSTD: Disabled = Disabled;

/// Statics for the entropy-family nodes: the real encoder with its feature,
/// a never-run [`Disabled`] without (the registry rows select
/// [`DISABLED`] in that case).
macro_rules! feature_encoders {
    ($($feature:literal, $module:ident => [$($name:ident),*];)*) => {$($(
        #[cfg(feature = $feature)]
        pub(crate) use $module::$name;
        #[cfg(not(feature = $feature))]
        #[doc = concat!("Without the `", $feature, "` feature the node's row selects `DISABLED`.")]
        pub(crate) static $name: Disabled = Disabled;
    )*)*};
}

feature_encoders! {
    "constant", constant => [CONSTANT];
    "entropy", entropy => [FSE_V2, FSE_NCOUNT, HUFFMAN_V2, HUFFMAN_STRUCT_V2];
    "pivco-huffman", pivco_huffman => [PIVCO_HUFFMAN];
}

/// The signature of an encoder function.
pub(crate) type EncodeFn =
    for<'a, 'x> fn(&mut NodeCx<'x, 'a>, &[Stream<'a>]) -> Result<(), NodeError>;

/// An encoder made of a plain function.
#[derive(Copy, Clone)]
pub(crate) struct FnEncoder(pub(crate) EncodeFn);

impl core::fmt::Debug for FnEncoder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("FnEncoder")
    }
}

impl Encode for FnEncoder {
    fn encode<'a>(&self, cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
        (self.0)(cx, inputs)
    }
}

/// Fails every run: the codec's Cargo feature is off.
#[derive(Debug)]
pub(crate) struct Disabled;

/// Fails every run: the node is deprecated.
#[derive(Debug)]
pub(crate) struct Legacy;

/// See [`Disabled`].
pub(crate) static DISABLED: Disabled = Disabled;
/// See [`Legacy`].
pub(crate) static LEGACY: Legacy = Legacy;

impl Encode for Disabled {
    fn encode<'a>(&self, cx: &mut NodeCx<'_, 'a>, _: &[Stream<'a>]) -> Result<(), NodeError> {
        Err(NodeError::Disabled(cx.spec().name))
    }
}

impl Encode for Legacy {
    fn encode<'a>(&self, cx: &mut NodeCx<'_, 'a>, _: &[Stream<'a>]) -> Result<(), NodeError> {
        Err(NodeError::Legacy(cx.spec().name))
    }
}

#[cfg(all(test, feature = "zstd", feature = "conversion"))]
mod tests;
