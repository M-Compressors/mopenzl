//! Safe Rust [OpenZL] codecs, independently selected with Cargo features.
//!
//! OpenZL is Meta's format-aware compression framework: a frame records the
//! graph of codecs that produced it, so one universal decoder reads every
//! frame. This crate ports the framework's core to Rust, starting with what
//! `ZL_compress` and `ZL_decompress` do for serial data:
//!
//! - **Byte-identical encoder.** For the same input and global parameters
//!   (level, format version, checksums), [`Compressor`] writes exactly the
//!   frame of OpenZL v0.3.0's `ZL_CCtx_compress` with its default graph, or,
//!   with [`Chunking::Fixed`], of its serial segmenter. Inputs below ten bytes
//!   are stored, larger ones go through Zstandard (the safe-Rust `mzstd`,
//!   byte-identical to the zstd 1.5.7 OpenZL vendors), and any chunk that
//!   would grow is stored instead.
//! - **Decoder** for frames of format versions 21 to 27 built from those
//!   codecs, with both checksums verified, bounded input and output, and
//!   precise errors for frames that need codecs not ported yet.
//! - **No `unsafe`.** Kernels use [`fearless_simd`] with the instruction set
//!   detected once per codec ([`Backend`]); every backend writes the same
//!   bytes.
//! - **Sans-I/O sessions** that work only with the buffers you pass in, plus
//!   `std::io` adapters.
//!
//! `compression` and `decompression` are enabled by default. Disable default
//! features and select either codec alongside `std` or `no_std`:
//!
//! ```toml
//! mopenzl = { version = "0.2", default-features = false, features = ["std", "decompression"] }
//! ```
//!
//! # Choose your API
//!
//! | I/O shape | Compression | Decompression |
//! | --- | --- | --- |
//! | Return a new `Vec<u8>` | [`Compressor::compress`] | [`Decompressor::decompress`] |
//! | Append to an existing Vec | [`Compressor::compress_into`] | [`Decompressor::decompress_into`] |
//! | Write into a caller-owned slice | [`Compressor::compress_to_slice`] | [`Decompressor::decompress_to_slice`] |
//! | Write into uninitialized memory | [`Compressor::compress_to_uninit`] | [`Decompressor::decompress_to_uninit`] |
//! | Pull output through `std::io::Read` | `Compressor::reader` | `Decompressor::reader` |
//! | Push input through `std::io::Write` | `Compressor::writer` | `Decompressor::writer` |
//! | Drive input/output incrementally | [`Compressor::start`] → [`EncoderSession`] | [`Decompressor::start`] → [`DecoderSession`] |
//! | Same, into uninitialized memory | [`EncoderSession::process_uninit`] | [`DecoderSession::process_uninit`] |
//! | Same, with the session owning the codec | [`Compressor::into_session`] → [`EncoderSessionOwned`] | [`Decompressor::into_session`] → [`DecoderSessionOwned`] |
//!
//! ```rust
//! use mopenzl::{Compressor, Decompressor, EncoderConfig, Level};
//!
//! let mut encoder = Compressor::new(EncoderConfig::from(Level::try_from(3)?))?;
//! let mut decoder = Decompressor::default();
//! let mut frame = Vec::new();
//! for input in [b"first payload".as_slice(), b"second payload".as_slice()] {
//!     frame.clear(); // Keep the allocation; compress_into appends.
//!     encoder.compress_into(input, &mut frame)?;
//!     assert_eq!(decoder.decompress(&frame)?, input);
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Streaming and chunks
//!
//! A frame header records the frame's total size, and the reference decoder
//! rejects frames that leave it unknown. Declare the size with
//! [`InputSize::Exact`] when you know it: a session then writes the header at
//! once and holds at most one chunk of input. With [`InputSize::Unknown`] it
//! holds the whole input until the frame ends. [`Chunking::Fixed`] bounds a
//! chunk, so with a declared size both the encoder and the decoder work in
//! bounded memory; the default, [`Chunking::Whole`], matches `ZL_compress`.
//!
//! ```rust
//! # #[cfg(not(feature = "no_std"))]
//! # {
//! use mopenzl::{ChunkSize, Chunking, Compressor, Decompressor, EncoderConfig, InputSize};
//! use std::io::{Read, Write};
//!
//! let payload = b"streamed payload ".repeat(10_000);
//! let config = EncoderConfig::default().with_chunking(Chunking::Fixed(ChunkSize::MIN));
//! let mut encoder = Compressor::new(config)?;
//! let mut writer = encoder.writer(Vec::new(), InputSize::Exact(payload.len() as u64).into());
//! writer.write_all(&payload)?;
//! let frame = writer.finish().map_err(mopenzl::io::FinishError::into_error)?;
//!
//! let mut decoder = Decompressor::default();
//! let mut content = Vec::new();
//! decoder.reader(&frame[..]).read_to_end(&mut content)?;
//! assert_eq!(content, payload);
//! # }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Untrusted input
//!
//! Limits are unlimited by default; set [`DecodeLimits`] for untrusted
//! frames. A frame declaring more output than allowed fails before any chunk
//! is decoded.
//!
//! ```rust
//! use mopenzl::{DecodeLimits, DecoderConfig, Decompressor};
//!
//! let limits = DecodeLimits::default()
//!     .with_max_input_bytes(Some(1 << 20))
//!     .with_max_output_bytes(Some(8 << 20));
//! let mut decoder = Decompressor::new(limits.into());
//! assert!(decoder.decompress(b"not a frame").is_err());
//! ```
//!
//! # Select codecs
//!
//! Features are additive; the defaults enable everything.
//!
//! | Feature | Enables |
//! | --- | --- |
//! | `compression` | [`Compressor`] and its sessions |
//! | `decompression` | [`Decompressor`] and its sessions |
//! | `std` | the `std::io` adapters, `std::error::Error` and runtime SIMD detection |
//! | `no_std` | alloc-only builds with the compile-time SIMD baseline |
//! | `zstd` | the Zstandard codec (`mzstd`), the default graph's backend |
//!
//! Every OpenZL codec has its own feature, all enabled by default. Without a
//! codec's feature the encoder never writes it (chunks that would use it are
//! stored, so frames stop matching the reference's default graph) and the
//! decoder reports frames using it as [`Unsupported`].
//!
//! ```toml
//! mopenzl = { version = "0.2", default-features = false, features = ["std", "decompression", "zstd"] }
//! ```
//!
//! [OpenZL]: https://github.com/facebook/openzl
#![cfg_attr(
    feature = "compression",
    doc = r#"
[`Chunking::Fixed`]: crate::Chunking::Fixed
[`Chunking::Whole`]: crate::Chunking::Whole
[`Compressor::compress_into`]: crate::Compressor::compress_into
[`Compressor::compress_to_slice`]: crate::Compressor::compress_to_slice
[`Compressor::compress_to_uninit`]: crate::Compressor::compress_to_uninit
[`Compressor::compress`]: crate::Compressor::compress
[`Compressor::into_session`]: crate::Compressor::into_session
[`Compressor::start`]: crate::Compressor::start
[`Compressor`]: crate::Compressor
[`EncoderSession::process_uninit`]: crate::EncoderSession::process_uninit
[`EncoderSessionOwned`]: crate::EncoderSessionOwned
[`EncoderSession`]: crate::EncoderSession
[`InputSize::Exact`]: crate::InputSize::Exact
[`InputSize::Unknown`]: crate::InputSize::Unknown
"#
)]
#![cfg_attr(
    not(feature = "compression"),
    doc = r#"
[`Chunking::Fixed`]: #select-codecs
[`Chunking::Whole`]: #select-codecs
[`Compressor::compress_into`]: #select-codecs
[`Compressor::compress_to_slice`]: #select-codecs
[`Compressor::compress_to_uninit`]: #select-codecs
[`Compressor::compress`]: #select-codecs
[`Compressor::into_session`]: #select-codecs
[`Compressor::start`]: #select-codecs
[`Compressor`]: #select-codecs
[`EncoderSession::process_uninit`]: #select-codecs
[`EncoderSessionOwned`]: #select-codecs
[`EncoderSession`]: #select-codecs
[`InputSize::Exact`]: #select-codecs
[`InputSize::Unknown`]: #select-codecs
"#
)]
#![cfg_attr(
    feature = "decompression",
    doc = r#"
[`DecodeLimits`]: crate::DecodeLimits
[`DecoderSession::process_uninit`]: crate::DecoderSession::process_uninit
[`DecoderSessionOwned`]: crate::DecoderSessionOwned
[`DecoderSession`]: crate::DecoderSession
[`Decompressor::decompress_into`]: crate::Decompressor::decompress_into
[`Decompressor::decompress_to_slice`]: crate::Decompressor::decompress_to_slice
[`Decompressor::decompress_to_uninit`]: crate::Decompressor::decompress_to_uninit
[`Decompressor::decompress`]: crate::Decompressor::decompress
[`Decompressor::into_session`]: crate::Decompressor::into_session
[`Decompressor::start`]: crate::Decompressor::start
[`Decompressor`]: crate::Decompressor
"#
)]
#![cfg_attr(
    not(feature = "decompression"),
    doc = r#"
[`DecodeLimits`]: #select-codecs
[`DecoderSession::process_uninit`]: #select-codecs
[`DecoderSessionOwned`]: #select-codecs
[`DecoderSession`]: #select-codecs
[`Decompressor::decompress_into`]: #select-codecs
[`Decompressor::decompress_to_slice`]: #select-codecs
[`Decompressor::decompress_to_uninit`]: #select-codecs
[`Decompressor::decompress`]: #select-codecs
[`Decompressor::into_session`]: #select-codecs
[`Decompressor::start`]: #select-codecs
[`Decompressor`]: #select-codecs
"#
)]
#![cfg_attr(
    not(feature = "decompression"),
    doc = "[`Unsupported`]: #select-codecs"
)]
#![cfg_attr(
    not(any(feature = "compression", feature = "decompression")),
    doc = "[`Backend`]: #select-codecs\n\n[`fearless_simd`]: https://docs.rs/fearless_simd"
)]
#![cfg_attr(not(test), forbid(unsafe_code))]
#![cfg_attr(feature = "no_std", no_std)]
#![deny(missing_docs)]
#![deny(missing_debug_implementations)]
#![deny(rustdoc::broken_intra_doc_links)]

#[cfg(any(feature = "compression", feature = "decompression"))]
extern crate alloc;

#[cfg(test)]
extern crate std;

#[cfg(any(feature = "compression", feature = "decompression"))]
mod backend;
#[cfg(any(feature = "compression", feature = "decompression"))]
mod data_type;
#[cfg(any(feature = "compression", feature = "decompression"))]
mod format;
#[cfg(any(feature = "compression", feature = "decompression"))]
mod retention;
#[cfg(any(feature = "compression", feature = "decompression"))]
mod shared;
#[cfg(any(feature = "compression", feature = "decompression"))]
pub use backend::Backend;
#[cfg(any(feature = "compression", feature = "decompression"))]
pub use data_type::DataType;
#[cfg(any(feature = "compression", feature = "decompression"))]
pub use format::{FormatVersion, UnsupportedFormatVersion};
#[cfg(any(feature = "compression", feature = "decompression"))]
pub use retention::RetentionPolicy;

#[cfg(all(
    any(feature = "compression", feature = "decompression"),
    not(feature = "no_std")
))]
mod finish_error;
#[cfg(all(
    any(feature = "compression", feature = "decompression"),
    not(feature = "no_std")
))]
pub mod io;

#[cfg(any(feature = "compression", feature = "decompression"))]
pub mod dict;
#[cfg(feature = "compression")]
pub mod graph;
#[cfg(feature = "compression")]
pub mod input;

#[cfg(feature = "compression")]
pub mod compressor;
#[cfg(feature = "compression")]
pub use compressor::{
    ChunkSize, Chunking, Compressor, CompressorBuilder, ConfigError, EncodeError, EncoderConfig,
    EncoderSession, EncoderSessionOwned, EncoderStatus, InputSize, Level, Operation, Parameter,
    Progress, SizeOverflow, StreamConfig,
};

#[cfg(feature = "decompression")]
pub mod decompressor;
#[cfg(feature = "decompression")]
pub use decompressor::{
    DecodeError, DecodeFailure, DecodeLimits, DecodeOperation, DecodeProgress, DecoderConfig,
    DecoderSession, DecoderSessionOwned, DecoderStatus, Decompressor, DecompressorBuilder,
    FrameInfo, FrameMode, InvalidDataKind, TypedOutput, Unsupported,
};
