//! Indexed resource containers of independent OpenZL frames (`framed` feature).
//!
//! Each resource contains exactly one ordinary OpenZL frame. Its graph can be
//! selected independently, and its inputs can be serial, typed or multiple.
//! The container adds resource metadata and a checked directory for random
//! access. The outer format is specific to mopenzl; extract a resource's frame
//! before passing it to an ordinary OpenZL decoder.
//!
//! # Examples
//!
//! ```
//! # #[cfg(all(feature = "compression", feature = "decompression"))]
//! # {
//! use mopenzl::framing::{FramedCompressor, FramedDecompressor, FramedResource};
//! use mopenzl::graph::{CompressorGraph, StandardGraph};
//!
//! let store = CompressorGraph::standard(StandardGraph::Store);
//! let resources = [
//!     FramedResource::from(&b"first"[..]),
//!     FramedResource { graph: Some(&store), ..FramedResource::from(&b"second"[..]) },
//! ];
//! let bytes = FramedCompressor::new(Default::default())?.compress((&resources[..]).into())?;
//! let decoded = FramedDecompressor::new(Default::default())?.decompress(&bytes)?;
//! assert_eq!(decoded.resources[1].outputs[0].content(), b"second");
//! # }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use alloc::vec::Vec;
use thiserror::Error;

mod core;
#[cfg(feature = "decompression")]
mod decode;
#[cfg(feature = "compression")]
mod encode;
#[cfg(not(feature = "no_std"))]
mod io;

#[cfg(feature = "decompression")]
pub use decode::*;
#[cfg(feature = "compression")]
pub use encode::*;
#[cfg(not(feature = "no_std"))]
pub use io::*;

/// Bounds applied before accepting a resource or allocating from wire lengths.
///
/// Output counts content bytes of every typed output, excluding string lengths.
/// Codec workspace and caller-owned input/destinations are outside these caps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FramingConfig {
    /// Maximum resources per container (default 10,000).
    pub max_resources: usize,
    /// Maximum serialized metadata per resource (default 1 MiB).
    pub max_metadata_bytes: usize,
    /// Maximum encoded OpenZL frame per resource (default 64 MiB).
    pub max_frame_bytes: usize,
    /// Maximum aggregate decoded content per container (default 256 MiB).
    pub max_output_bytes: u64,
    /// Maximum complete wire container (default 1 GiB).
    pub max_container_bytes: u64,
}

impl Default for FramingConfig {
    fn default() -> Self {
        Self {
            max_resources: 10_000,
            max_metadata_bytes: 1 << 20,
            max_frame_bytes: 64 << 20,
            max_output_bytes: 256 << 20,
            max_container_bytes: 1 << 30,
        }
    }
}

/// A borrowed metadata field. Uppercase ASCII codes may repeat; `id` is a
/// unique UTF-8 resource name, and `mt` a unique signed little-endian i64
/// timestamp in microseconds since the Unix epoch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetadataField<'a> {
    /// Two uppercase ASCII letters, `id`, or `mt`.
    pub code: [u8; 2],
    /// Raw field value.
    pub value: &'a [u8],
}

/// An owned metadata field returned by the decoder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataEntry {
    /// The validated field code.
    pub code: [u8; 2],
    /// The field value.
    pub value: Vec<u8>,
}

/// One directory entry; offsets are relative to the container's first byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceInfo {
    /// Offset of the resource record, including its header and metadata.
    pub offset: u64,
    /// Total resource record length, including its OpenZL frame.
    pub length: u64,
    /// Total content bytes of the frame's outputs, excluding string lengths.
    pub decoded_size: u64,
}

/// Container, resource, codec or transport failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FramedError {
    /// Malformed structure, metadata, size, checksum or directory.
    #[error("invalid framed container: {0}")]
    Invalid(&'static str),
    /// A future container version.
    #[error("unsupported framed container version {0}")]
    UnsupportedVersion(u8),
    /// Input ended before the footer or inside a resource.
    #[error("unexpected end of framed container")]
    UnexpectedEndOfInput,
    /// A configured cap was exceeded.
    #[error("framing limit exceeded: {0}")]
    Limit(&'static str),
    /// A wire offset or host-sized length overflowed.
    #[error("framing size or offset overflow")]
    Overflow,
    /// A fallible framing allocation failed.
    #[error("framing allocation failed")]
    Allocation,
    /// An operation followed completion or failure.
    #[error("invalid framed session state")]
    InvalidState,
    /// Drain queued bytes before adding another resource.
    #[error("framed output must be drained before adding a resource")]
    OutputPending,
    /// Encoder configuration failed.
    #[cfg(feature = "compression")]
    #[error("framed encoder configuration: {0}")]
    Config(#[from] crate::ConfigError),
    /// A resource graph or encoder failed.
    #[cfg(feature = "compression")]
    #[error("framed resource compression: {0}")]
    Encode(#[from] crate::EncodeError),
    /// A resource codec, checksum, dictionary or decoder limit failed.
    #[cfg(feature = "decompression")]
    #[error("framed resource decompression: {0}")]
    Decode(#[from] crate::DecodeError),
    /// An underlying reader, writer or seek operation failed.
    #[cfg(not(feature = "no_std"))]
    #[error("framed transport: {0}")]
    Io(#[from] std::io::Error),
}
