//! Frame metadata read from a frame header without decoding any chunk.

use alloc::boxed::Box;
use alloc::vec::Vec;

use fearless_simd::Level;

use super::core::frame::{self, OutputSpec};
use super::core::reader::Parse;
use super::error::{DecodeError, Unsupported};
use crate::dict::BundleId;
use crate::{Backend, DataType, FormatVersion};

/// What a frame header says about one output of the frame
/// (`ZL_FrameInfo_getOutputType`, `getDecompressedSize`, `getNumElts`).
///
/// # Examples
///
/// ```
/// use mopenzl::{Compressor, DataType, Decompressor};
///
/// let frame = Compressor::default().compress(b"abc")?;
/// let info = Decompressor::default().frame_info(&frame)?;
/// let output = info.outputs()[0];
/// assert_eq!(output.data_type(), DataType::Serial);
/// assert_eq!(output.size(), 3);
/// assert_eq!(output.num_elts(), Some(3));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OutputInfo {
    data_type: DataType,
    size: u64,
    strings: u64,
}

impl OutputInfo {
    /// The type of the output.
    #[must_use]
    pub const fn data_type(&self) -> DataType {
        self.data_type
    }

    /// The decompressed size of the output in bytes: for strings, the bytes
    /// of all strings, without their lengths.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// The number of elements, where the header records it: the bytes of a
    /// serial output and the strings of a string output. Like the reference
    /// (`ZL_FrameInfo_getNumElts`) it is `None` for struct and numeric
    /// outputs, whose element width is not in the header.
    #[must_use]
    pub const fn num_elts(&self) -> Option<u64> {
        match self.data_type {
            DataType::Serial => Some(self.size),
            DataType::String => Some(self.strings),
            DataType::Struct | DataType::Numeric => None,
        }
    }
}

/// The metadata of a frame header (`ZL_FrameInfo`): format version, outputs,
/// checksum flags, header comment and dictionary bundle.
///
/// Reading it parses the header only; no chunk is decoded, so it is cheap and
/// works on frames whose codecs this build does not support. Get one from
/// [`Decompressor::frame_info`](super::Decompressor::frame_info) or
/// [`TryFrom<&[u8]>`].
///
/// # Examples
///
/// ```
/// use mopenzl::graph::Comment;
/// use mopenzl::{Compressor, FormatVersion, FrameInfo};
///
/// let mut compressor = Compressor::default();
/// compressor.set_comment(Some(Comment::try_from(&b"made by a test"[..])?))?;
/// let frame = compressor.compress(b"payload")?;
/// let info = FrameInfo::try_from(&frame[..])?;
/// assert_eq!(info.version(), FormatVersion::MAX);
/// assert_eq!(info.comment(), Some(&b"made by a test"[..]));
/// assert_eq!(info.decompressed_size(), 7);
/// assert!(info.content_checksum() && info.compressed_checksum());
/// assert_eq!(info.bundle_id(), None);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameInfo {
    version: FormatVersion,
    outputs: Box<[OutputInfo]>,
    content_checksum: bool,
    compressed_checksum: bool,
    bundle: Option<BundleId>,
    comment: Option<Box<[u8]>>,
    header_len: usize,
}

impl FrameInfo {
    /// Parses the header at the front of `src`; `verify` checks the header's
    /// check byte.
    pub(crate) fn parse(src: &[u8], simd: Level, verify: bool) -> Result<Self, DecodeError> {
        let mut specs: Vec<OutputSpec> = Vec::new();
        let info =
            frame::parse_with(src, simd, verify, |spec| specs.push(spec)).map_err(|error| {
                match error {
                    Parse::Truncated => DecodeError::UnexpectedEndOfInput,
                    Parse::Failed(error) => error,
                }
            })?;
        // `frame::parse_with` only accepts supported versions.
        let Some(version) = FormatVersion::new(info.version) else {
            return Err(DecodeError::unsupported(Unsupported::FormatVersion {
                version: info.version,
            }));
        };
        Ok(Self {
            version,
            outputs: specs
                .into_iter()
                .map(|spec| OutputInfo {
                    data_type: spec.ty,
                    size: spec.size,
                    strings: spec.strings,
                })
                .collect(),
            content_checksum: info.content_checksum,
            compressed_checksum: info.compressed_checksum,
            bundle: info.bundle,
            comment: info
                .comment
                .and_then(|(start, len)| src.get(start..start + len))
                .map(Box::from),
            header_len: info.header_len,
        })
    }

    /// The wire-format version (`ZL_FrameInfo_getFormatVersion`).
    #[must_use]
    pub const fn version(&self) -> FormatVersion {
        self.version
    }

    /// The outputs the frame regenerates, in order
    /// (`ZL_FrameInfo_getNumOutputs` is the length).
    #[must_use]
    pub fn outputs(&self) -> &[OutputInfo] {
        &self.outputs
    }

    /// The total decompressed size over every output, in bytes
    /// (`ZL_getDecompressedSize` for a single output).
    #[must_use]
    pub fn decompressed_size(&self) -> u64 {
        self.outputs
            .iter()
            .fold(0, |total, output| total.saturating_add(output.size))
    }

    /// The header comment (`ZL_FrameInfo_getComment`), or `None` where the
    /// frame has none (and for formats older than version 22, which cannot
    /// carry one).
    #[must_use]
    pub fn comment(&self) -> Option<&[u8]> {
        self.comment.as_deref()
    }

    /// The dictionary bundle the frame was compressed with
    /// (`ZL_FrameInfo_getBundleID`), if any.
    #[must_use]
    pub const fn bundle_id(&self) -> Option<BundleId> {
        self.bundle
    }

    /// Whether the chunks carry content checksums.
    #[must_use]
    pub const fn content_checksum(&self) -> bool {
        self.content_checksum
    }

    /// Whether the chunks carry compressed checksums, and the header a
    /// check byte.
    #[must_use]
    pub const fn compressed_checksum(&self) -> bool {
        self.compressed_checksum
    }

    /// The size of the header in bytes: where the first chunk starts.
    #[must_use]
    pub const fn header_len(&self) -> usize {
        self.header_len
    }
}

impl TryFrom<&[u8]> for FrameInfo {
    type Error = DecodeError;

    /// Reads the header at the front of `src` on the host's best
    /// [`Backend`], verifying the header's check byte.
    ///
    /// # Errors
    ///
    /// [`DecodeError::UnexpectedEndOfInput`] for a truncated header,
    /// [`DecodeError::InvalidData`] for a bad magic number, header or check
    /// byte, and [`DecodeError::Unsupported`] for a format version or a
    /// header shape this crate does not read.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{DecodeError, FrameInfo};
    ///
    /// assert!(matches!(
    ///     FrameInfo::try_from(&b"\x28\xb5\x2f\xfd"[..]),
    ///     Err(DecodeError::InvalidData { .. })
    /// ));
    /// ```
    fn try_from(src: &[u8]) -> Result<Self, Self::Error> {
        Self::parse(src, Backend::default().0, true)
    }
}
