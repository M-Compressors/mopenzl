//! OpenZL decompression.
//!
//! [`Decompressor`] decodes frames of format versions 21 to 27 whose chunks
//! use the codecs of the reference's default graph: stored streams and
//! Zstandard. Frames using other codecs are reported as
//! [`DecodeError::Unsupported`]. One-shot methods decode a whole input; a
//! [`DecoderSession`] decodes incrementally; the `io` adapters wrap a
//! session in `std::io::Read` and `std::io::Write`.

mod config;
pub(crate) mod core;
mod decoder;
mod error;
mod info;
#[cfg(not(feature = "no_std"))]
pub mod io;
mod session;
mod typed;

pub use config::{DecodeLimits, DecoderConfig, FrameMode};
pub use decoder::{Decompressor, DecompressorBuilder};
pub use error::{DecodeError, InvalidDataKind, Unsupported};
pub use info::{FrameInfo, OutputInfo};
pub use session::{
    DecodeFailure, DecodeOperation, DecodeProgress, DecoderSession, DecoderSessionOwned,
    DecoderStatus,
};
pub use typed::TypedOutput;
