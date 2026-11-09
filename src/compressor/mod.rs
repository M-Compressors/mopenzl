//! OpenZL compression.
//!
//! [`Compressor`] compresses serial input with the reference's default graph:
//! inputs below ten bytes are stored, larger ones go through the Zstandard
//! codec, and any chunk that would grow is stored instead. One-shot methods
//! compress a whole input at once; an [`EncoderSession`] compresses a frame
//! incrementally; the `io` adapters wrap a session in `std::io::Read` and
//! `std::io::Write`.

mod config;
pub(crate) mod core;
mod encoder;
mod error;
#[cfg(not(feature = "no_std"))]
pub mod io;
mod session;

pub use config::{ChunkSize, Chunking, ConfigError, EncoderConfig, Level, Parameter, SizeOverflow};
pub use encoder::{Compressor, CompressorBuilder};
pub use error::EncodeError;
pub use session::{
    EncoderSession, EncoderSessionOwned, EncoderStatus, InputSize, Operation, Progress,
    StreamConfig,
};
