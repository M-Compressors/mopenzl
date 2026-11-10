//! What can go wrong while frames are decoded.

use thiserror::Error;

use crate::dict::BundleId;

/// The structure in which invalid compressed data was detected.
///
/// Carried by [`DecodeError::InvalidData`]. It names the malformed part of
/// the format, not a byte offset; sessions report accepted-byte counts
/// separately.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[non_exhaustive]
pub enum InvalidDataKind {
    /// Bytes where a frame should start are not an OpenZL magic number.
    #[error("frame magic number")]
    Magic,
    /// A malformed frame header: a bad output count, size or comment.
    #[error("frame header")]
    FrameHeader,
    /// The frame header's check byte does not match.
    #[error("frame header checksum")]
    HeaderChecksum,
    /// A malformed chunk header.
    #[error("chunk header")]
    ChunkHeader,
    /// The chunk's decoding map does not wire its streams consistently.
    #[error("chunk stream graph")]
    Graph,
    /// A codec's stream is malformed.
    #[error("codec stream")]
    CodecStream,
    /// A codec names a dictionary index past the end of the frame's bundle.
    #[error("dictionary index")]
    DictionaryIndex,
    /// A frame ends without any chunk, which the reference rejects even when
    /// it declares no content.
    #[error("frame without chunks")]
    EmptyFrame,
}

/// A valid OpenZL feature this decoder does not implement yet.
///
/// Carried by [`DecodeError::Unsupported`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[non_exhaustive]
pub enum Unsupported {
    /// A format version outside `21..=27`.
    #[error("format version {version}")]
    FormatVersion {
        /// The frame's version.
        version: u32,
    },
    /// A standard codec other than those of the default graph.
    #[error("standard codec {id}")]
    Codec {
        /// The codec's ID.
        id: u32,
    },
    /// A codec registered by the compressing application.
    #[error("custom codec {id}")]
    CustomCodec {
        /// The codec's ID.
        id: u32,
    },
    /// A frame that does not record its decompressed size, which the
    /// reference decoder rejects too.
    #[error("frames of unknown size")]
    UnknownSize,
    /// A frame with several outputs.
    #[error("frames with {outputs} outputs")]
    MultipleOutputs {
        /// The frame's output count.
        outputs: usize,
    },
    /// A frame whose output is struct, numeric or string data.
    #[error("typed outputs")]
    TypedOutput,
}

/// A decompression failure.
///
/// One-shot methods return it directly; sessions wrap it in
/// [`super::DecodeFailure`] together with the progress of the failing call.
/// The `std::io` adapters wrap it in `std::io::Error` (with `std`), from
/// which `get_ref` and `downcast_ref::<DecodeError>()` recover it.
///
/// # Examples
///
/// ```
/// use mopenzl::{DecodeError, Decompressor, InvalidDataKind};
///
/// let mut decoder = Decompressor::default();
/// assert!(matches!(
///     decoder.decompress(b"not openzl"),
///     Err(DecodeError::InvalidData { kind: InvalidDataKind::Magic })
/// ));
/// assert!(matches!(
///     decoder.decompress(&[0xdb, 0xa5, 0xb1, 0xd7]),
///     Err(DecodeError::UnexpectedEndOfInput)
/// ));
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// A registered application decoder failed.
    #[cfg(feature = "custom-codec")]
    #[error("custom codec {id} failed: {source}")]
    CustomCodec {
        /// The application codec's wire ID.
        id: u32,
        /// The callback or output-validation failure.
        #[source]
        source: crate::custom_codec::CodecError,
    },
    /// The input violates the OpenZL format.
    #[error("invalid OpenZL {kind}")]
    InvalidData {
        /// The malformed structure.
        #[source]
        kind: InvalidDataKind,
    },
    /// The frame is valid but uses something this decoder lacks.
    #[error("unsupported OpenZL feature: {feature}")]
    Unsupported {
        /// What is missing.
        #[source]
        feature: Unsupported,
    },
    /// The input ended inside a frame, or before any frame.
    #[error("unexpected end of compressed input")]
    UnexpectedEndOfInput,
    /// Bytes followed the frame of a [`super::FrameMode::Single`] operation.
    #[error("trailing data at byte {offset}")]
    TrailingData {
        /// Offset of the first byte after the frame.
        offset: u64,
    },
    /// The caller's output slice is full.
    #[error("output slice is full after {written} bytes")]
    OutputTooSmall {
        /// Bytes written before the slice filled.
        written: usize,
    },
    /// A chunk's content checksum does not match its output.
    #[error("content checksum mismatch: stored {expected:#010x}, computed {computed:#010x}")]
    ContentChecksumMismatch {
        /// The checksum stored in the chunk.
        expected: u32,
        /// The low 32 bits of the XXH3 of the decoded chunk.
        computed: u32,
    },
    /// A chunk's compressed checksum does not match its bytes.
    #[error("compressed checksum mismatch: stored {expected:#010x}, computed {computed:#010x}")]
    CompressedChecksumMismatch {
        /// The checksum stored in the chunk.
        expected: u32,
        /// The low 32 bits of the XXH3 of the chunk's bytes.
        computed: u32,
    },
    /// The frame regenerated a different size than its header declares.
    #[error("frame declares {declared} content bytes, decoded {decoded}")]
    ContentSizeMismatch {
        /// The size in the frame header.
        declared: u64,
        /// Bytes regenerated, or a lower bound once it exceeds `declared`.
        decoded: u64,
    },
    /// More input than [`super::DecodeLimits::max_input_bytes`].
    #[error("input exceeds the limit of {limit} bytes")]
    InputLimitExceeded {
        /// The configured limit.
        limit: u64,
    },
    /// More output than [`super::DecodeLimits::max_output_bytes`].
    #[error("output exceeds the limit of {limit} bytes")]
    OutputLimitExceeded {
        /// The configured limit.
        limit: u64,
    },
    /// The frame was compressed with a dictionary bundle the decompressor
    /// does not hold: none was given with
    /// [`DecompressorBuilder::with_dictionaries`](super::DecompressorBuilder::with_dictionaries),
    /// or the store lacks this bundle.
    #[error("dictionary bundle {bundle} is not available")]
    MissingBundle {
        /// The bundle the frame header names.
        bundle: BundleId,
    },
    /// The session already failed or finished.
    #[error("the session cannot accept this operation in its current state")]
    InvalidState,
}

impl DecodeError {
    /// [`Self::InvalidData`] of `kind`.
    pub(crate) const fn invalid(kind: InvalidDataKind) -> Self {
        Self::InvalidData { kind }
    }

    /// [`Self::Unsupported`] of `feature`.
    pub(crate) const fn unsupported(feature: Unsupported) -> Self {
        Self::Unsupported { feature }
    }
}

#[cfg(not(feature = "no_std"))]
impl From<DecodeError> for std::io::Error {
    /// Maps format errors to `InvalidData`, truncation to `UnexpectedEof`,
    /// contract violations to `InvalidInput`, missing features to
    /// `Unsupported` and exhausted limits to `Other`.
    fn from(error: DecodeError) -> Self {
        use std::io::ErrorKind;
        let kind = match &error {
            DecodeError::InvalidData { .. }
            | DecodeError::TrailingData { .. }
            | DecodeError::ContentChecksumMismatch { .. }
            | DecodeError::CompressedChecksumMismatch { .. }
            | DecodeError::ContentSizeMismatch { .. } => ErrorKind::InvalidData,
            #[cfg(feature = "custom-codec")]
            DecodeError::CustomCodec { .. } => ErrorKind::InvalidData,
            DecodeError::Unsupported { .. } => ErrorKind::Unsupported,
            DecodeError::MissingBundle { .. } => ErrorKind::NotFound,
            DecodeError::UnexpectedEndOfInput => ErrorKind::UnexpectedEof,
            DecodeError::OutputTooSmall { .. } | DecodeError::InvalidState => {
                ErrorKind::InvalidInput
            }
            DecodeError::InputLimitExceeded { .. } | DecodeError::OutputLimitExceeded { .. } => {
                ErrorKind::Other
            }
        };
        Self::new(kind, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn messages_name_the_structure_and_feature() {
        assert_eq!(
            DecodeError::invalid(InvalidDataKind::ChunkHeader).to_string(),
            "invalid OpenZL chunk header"
        );
        assert_eq!(
            DecodeError::unsupported(Unsupported::Codec { id: 49 }).to_string(),
            "unsupported OpenZL feature: standard codec 49"
        );
        assert_eq!(
            DecodeError::invalid(InvalidDataKind::DictionaryIndex).to_string(),
            "invalid OpenZL dictionary index"
        );
        assert!(
            DecodeError::MissingBundle {
                bundle: BundleId::from([0xab; 32])
            }
            .to_string()
            .contains(&"ab".repeat(32))
        );
        assert!(
            Unsupported::MultipleOutputs { outputs: 3 }
                .to_string()
                .contains('3')
        );
        assert!(
            DecodeError::ContentChecksumMismatch {
                expected: 1,
                computed: 2
            }
            .to_string()
            .contains("0x00000001")
        );
    }

    #[cfg(not(feature = "no_std"))]
    #[test]
    fn io_errors_keep_the_decode_error() {
        use std::io::ErrorKind;
        let cases = [
            (
                DecodeError::invalid(InvalidDataKind::Magic),
                ErrorKind::InvalidData,
            ),
            (
                DecodeError::unsupported(Unsupported::UnknownSize),
                ErrorKind::Unsupported,
            ),
            (
                DecodeError::MissingBundle {
                    bundle: BundleId::from([1; 32]),
                },
                ErrorKind::NotFound,
            ),
            (DecodeError::UnexpectedEndOfInput, ErrorKind::UnexpectedEof),
            (DecodeError::InvalidState, ErrorKind::InvalidInput),
            (
                DecodeError::OutputLimitExceeded { limit: 1 },
                ErrorKind::Other,
            ),
        ];
        for (error, kind) in cases {
            let io = std::io::Error::from(error.clone());
            assert_eq!(io.kind(), kind);
            assert_eq!(
                io.get_ref().and_then(|e| e.downcast_ref::<DecodeError>()),
                Some(&error)
            );
        }
    }
}
