//! Application-defined OpenZL codecs.
//!
//! Register the same wire ID and signature with the graph builder and decoder.
//! IDs occupy a separate namespace from standard codecs. Callbacks are shared
//! between operations and must keep per-invocation state in their context.
//!
//! # Examples
//!
//! ```
//! use mopenzl::custom_codec::{CustomCodec, Signature, CodecInput, CodecError};
//! use mopenzl::DataType;
//! struct Identity;
//! impl CustomCodec for Identity {
//!     fn signature(&self) -> Signature {
//!         Signature { inputs: &[DataType::Serial], outputs: &[DataType::Serial],
//!             variable_input: false, variable_outputs: &[] }
//!     }
//! # #[cfg(feature = "compression")]
//!     fn encode(&self, cx: &mut mopenzl::custom_codec::EncoderContext<'_>, inputs: &[CodecInput<'_>]) -> Result<(), CodecError> {
//!         cx.emit(0, 1, inputs[0].bytes.to_vec()).map(|_| ())
//!     }
//! # #[cfg(feature = "decompression")]
//!     fn decode(&self, cx: &mut mopenzl::custom_codec::DecoderContext<'_>, inputs: &[CodecInput<'_>]) -> Result<(), CodecError> {
//!         cx.emit(0, 1, inputs[0].bytes)
//!     }
//! }
//! # #[cfg(feature = "compression")]
//! # {
//! use mopenzl::graph::{GraphBuilder, StandardGraph};
//! let mut builder = GraphBuilder::new();
//! let node = builder.custom_codec(1000, Identity)?;
//! let start = builder.static_graph(node, &[StandardGraph::Store.into()])?;
//! let graph = builder.build(start)?;
//! let mut encoder = mopenzl::Compressor::builder(Default::default()).with_graph(graph).build()?;
//! let frame = encoder.compress(b"application-defined codec")?;
//! # #[cfg(feature = "decompression")]
//! # {
//! let mut decoder = mopenzl::Decompressor::builder(Default::default())
//!     .with_custom_codec(1000, Identity)?.build();
//! assert_eq!(decoder.decompress(&frame)?, b"application-defined codec");
//! # }
//! # }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod core;

use crate::DataType;
#[cfg(feature = "compression")]
use alloc::vec::Vec;
use thiserror::Error;

/// A failure of codec registration, a callback, or its output validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[non_exhaustive]
pub enum CodecError {
    /// Invalid signature, stream, outcome, or callback data.
    #[error("{0}")]
    Invalid(&'static str),
    /// A decoder already uses this custom wire ID.
    #[error("custom codec {0} is already registered")]
    DuplicateId(u32),
}

/// Encoder port types, shared by the matching decoder.
///
/// The decoder reads `outputs` followed by all variable outputs in creation
/// order, and regenerates `inputs` in their original order. Static slices
/// ensure a registered signature cannot change during an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Signature {
    /// Input ports; must contain at least one type.
    pub inputs: &'static [DataType],
    /// The final input port repeats zero or more times.
    pub variable_input: bool,
    /// Mandatory singleton outputs, emitted exactly once in any order.
    pub outputs: &'static [DataType],
    /// Variable outcomes, each with a distinct type; emitted zero or more times.
    pub variable_outputs: &'static [DataType],
}

/// A borrowed stream supplied to a custom callback. Numeric bytes are little-endian.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodecInput<'a> {
    /// The stream's type.
    pub data_type: DataType,
    /// Bytes per element; one for serial and string streams.
    pub width: usize,
    /// Content bytes, concatenated for strings.
    pub bytes: &'a [u8],
    /// One length per string; empty for other stream types.
    pub string_lengths: &'a [u32],
}

/// A thread-safe application codec. Methods exist only for enabled directions.
pub trait CustomCodec: Send + Sync + 'static {
    /// Describes the ports. Registration validates and freezes this signature.
    fn signature(&self) -> Signature;

    /// Encodes one node invocation, committing every mandatory output.
    ///
    /// # Errors
    /// Return a codec error for invalid inputs, parameters, or failed encoding.
    #[cfg(feature = "compression")]
    fn encode(
        &self,
        cx: &mut EncoderContext<'_>,
        inputs: &[CodecInput<'_>],
    ) -> Result<(), CodecError>;

    /// Decodes one invocation, committing every regenerated stream.
    ///
    /// # Errors
    /// Return a codec error for malformed headers or input streams.
    #[cfg(feature = "decompression")]
    fn decode(
        &self,
        cx: &mut DecoderContext<'_>,
        inputs: &[CodecInput<'_>],
    ) -> Result<(), CodecError>;
}

/// Parameters and output creation for one custom encoder invocation.
#[cfg(feature = "compression")]
pub struct EncoderContext<'a> {
    api: &'a mut dyn core::EncoderApi,
}

#[cfg(feature = "compression")]
impl ::core::fmt::Debug for EncoderContext<'_> {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        f.write_str("EncoderContext")
    }
}

#[cfg(feature = "compression")]
impl EncoderContext<'_> {
    /// The global compression level.
    pub fn level(&self) -> i32 {
        self.api.level()
    }
    /// The frame's format version.
    pub fn format_version(&self) -> u32 {
        self.api.format_version()
    }
    /// A local integer parameter, including runtime overrides.
    pub fn local_int(&self, id: i32) -> Option<i32> {
        self.api.local_int(id)
    }
    /// A local byte parameter.
    pub fn local_bytes(&self, id: i32) -> Option<&[u8]> {
        self.api.local_bytes(id)
    }
    /// Commits a fixed-width output and returns its index among the outputs.
    /// Singleton indices follow port order; variable outputs are appended in
    /// creation order.
    ///
    /// # Errors
    /// Invalid outcome, duplicate singleton, width, or partial element.
    pub fn emit(
        &mut self,
        outcome: usize,
        width: usize,
        bytes: Vec<u8>,
    ) -> Result<usize, CodecError> {
        self.api.emit(outcome, width, bytes)
    }
    /// Commits concatenated strings with one length per string.
    ///
    /// # Errors
    /// Invalid outcome, duplicate singleton, or inconsistent lengths.
    pub fn emit_strings(
        &mut self,
        outcome: usize,
        bytes: Vec<u8>,
        lengths: Vec<u32>,
    ) -> Result<usize, CodecError> {
        self.api.emit_strings(outcome, bytes, lengths)
    }
    /// Sends the private header, at most once per invocation (up to 1 MB).
    ///
    /// # Errors
    /// A second header or a header beyond the format limit.
    pub fn send_header(&mut self, header: &[u8]) -> Result<(), CodecError> {
        self.api.send_header(header)
    }
}

/// Header, allocation budget and output creation for one custom decoder invocation.
#[cfg(feature = "decompression")]
#[derive(Debug)]
pub struct DecoderContext<'a> {
    header: &'a [u8],
    streams: &'a mut [crate::decompressor::core::data::Stream],
    committed: alloc::boxed::Box<[bool]>,
    budget: usize,
}

#[cfg(feature = "decompression")]
impl DecoderContext<'_> {
    /// The private header emitted by the encoder.
    pub const fn header(&self) -> &[u8] {
        self.header
    }
    /// Number of streams to regenerate, including repeated input ports.
    pub const fn output_count(&self) -> usize {
        self.streams.len()
    }
    /// Maximum bytes per regenerated stream, including four bytes per string length.
    /// Check this before allocating callback workspace for untrusted input.
    pub const fn output_budget(&self) -> usize {
        self.budget
    }
    /// Commits a fixed-width regenerated stream, copying its content.
    ///
    /// # Errors
    /// Invalid index, duplicate output, width, partial element, or exceeded budget.
    pub fn emit(&mut self, index: usize, width: usize, bytes: &[u8]) -> Result<(), CodecError> {
        core::emit(self, index, width, bytes)
    }
    /// Commits a string stream, copying its concatenated content and lengths.
    ///
    /// # Errors
    /// Invalid index, duplicate output, inconsistent lengths, or exceeded budget.
    pub fn emit_strings(
        &mut self,
        index: usize,
        bytes: &[u8],
        lengths: &[u32],
    ) -> Result<(), CodecError> {
        core::emit_strings(self, index, bytes, lengths)
    }
}

pub(crate) use core::*;
