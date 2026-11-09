//! What can go wrong while a frame is being encoded.
//!
//! Configuration mistakes are reported by [`ConfigError`](super::ConfigError)
//! when a value is validated; what is left needs an operation in flight.

use thiserror::Error;

use super::config::SizeOverflow;
use super::core::CoreError;
use super::core::stream::StreamError;
use crate::graph::GraphError;

/// Error returned by an encoding operation.
///
/// # Examples
///
/// ```
/// use mopenzl::{Compressor, EncodeError};
///
/// let mut encoder = Compressor::new(Default::default())?;
/// let mut cramped = [0u8; 4];
/// assert!(matches!(
///     encoder.compress_to_slice(b"a payload that will not fit in four bytes", &mut cramped),
///     Err(EncodeError::OutputTooSmall { provided: 4 })
/// ));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Error, Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum EncodeError {
    /// The caller's destination cannot hold the whole frame.
    ///
    /// Size it with [`Compressor::max_compressed_size`] to make this
    /// impossible.
    ///
    /// [`Compressor::max_compressed_size`]: crate::Compressor::max_compressed_size
    #[error("a destination of {provided} bytes cannot hold the compressed frame")]
    OutputTooSmall {
        /// How many bytes the caller offered.
        provided: usize,
    },
    /// The input does not match the size declared for the stream.
    #[error("the stream was declared as {declared} bytes but received a different amount")]
    InputSizeMismatch {
        /// The declared size.
        declared: u64,
    },
    /// The session has already failed.
    #[error("the session cannot accept this operation in its current state")]
    InvalidState,
    /// The compressed-size bound does not fit in a `usize`.
    #[error(transparent)]
    Bound(#[from] SizeOverflow),
    /// A graph failed to compress the input.
    #[error(transparent)]
    Graph(#[from] GraphError),
    /// An internal invariant of the encoder failed.
    #[error("internal encoder failure")]
    Internal,
}

impl EncodeError {
    /// Translates a core failure at the public boundary.
    pub(crate) fn from_core(error: CoreError, provided: usize) -> Self {
        match error {
            CoreError::Graph(error) => Self::Graph(GraphError::from(error)),
            CoreError::OutputFull => Self::OutputTooSmall { provided },
            #[cfg(feature = "zstd")]
            CoreError::Internal => Self::Internal,
        }
    }

    /// Translates a stream failure at the public boundary.
    pub(crate) const fn from_stream(error: StreamError, declared: Option<u64>) -> Self {
        match error {
            StreamError::SizeMismatch => Self::InputSizeMismatch {
                declared: match declared {
                    Some(size) => size,
                    None => 0,
                },
            },
            // A session's output is its own growable buffer.
            StreamError::Core(_) => Self::Internal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn core_errors_map_to_public_variants() {
        assert_eq!(
            EncodeError::from_core(CoreError::OutputFull, 3),
            EncodeError::OutputTooSmall { provided: 3 }
        );
        #[cfg(feature = "zstd")]
        assert_eq!(
            EncodeError::from_core(CoreError::Internal, 0),
            EncodeError::Internal
        );
        assert_eq!(
            EncodeError::from_stream(StreamError::SizeMismatch, Some(9)),
            EncodeError::InputSizeMismatch { declared: 9 }
        );
        assert_eq!(
            EncodeError::from_stream(StreamError::SizeMismatch, None),
            EncodeError::InputSizeMismatch { declared: 0 }
        );
        assert_eq!(
            EncodeError::from_stream(StreamError::Core(CoreError::OutputFull), None),
            EncodeError::Internal
        );
        assert_eq!(
            EncodeError::from(SizeOverflow),
            EncodeError::Bound(SizeOverflow)
        );
    }

    #[test]
    fn messages_describe_the_failure() {
        assert!(
            EncodeError::OutputTooSmall { provided: 4 }
                .to_string()
                .contains('4')
        );
        assert!(EncodeError::InvalidState.to_string().contains("state"));
        assert!(EncodeError::Internal.to_string().contains("internal"));
    }
}
