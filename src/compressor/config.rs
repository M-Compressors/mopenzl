//! Compression configuration: what the reference calls global parameters.

use core::fmt;

use thiserror::Error;

use crate::FormatVersion;

/// A compression level (`ZL_CParam_compressionLevel`).
///
/// The default graph hands the level to its Zstandard backend unchanged, so
/// the valid range is Zstandard's, `-131072..=22`. As in the reference, `0`
/// selects the default, [`Self::DEFAULT`] (6), rather than Zstandard's own
/// default.
///
/// # Examples
///
/// ```
/// use mopenzl::Level;
///
/// let level = Level::try_from(19)?;
/// assert_eq!(i32::from(level), 19);
/// assert_eq!(Level::try_from(0)?, Level::DEFAULT);
/// assert!(Level::try_from(23).is_err());
/// # Ok::<(), mopenzl::ConfigError>(())
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Level(i32);

impl Level {
    /// The fastest level, `-131072`.
    pub const MIN: Self = Self(-131_072);
    /// The strongest level, `22`.
    pub const MAX: Self = Self(22);
    /// The reference's default level, `6` (`ZL_COMPRESSIONLEVEL_DEFAULT`).
    pub const DEFAULT: Self = Self(6);

    /// The level as the reference's integer.
    ///
    /// # Examples
    ///
    /// ```
    /// assert_eq!(mopenzl::Level::MAX.get(), 22);
    /// ```
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

impl Default for Level {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<i32> for Level {
    type Error = ConfigError;

    /// Validates a level.
    ///
    /// # Errors
    ///
    /// [`ConfigError::OutOfRange`] outside `-131072..=22`.
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        if value == 0 {
            return Ok(Self::DEFAULT);
        }
        if !(Self::MIN.0..=Self::MAX.0).contains(&value) {
            return Err(ConfigError::OutOfRange {
                parameter: Parameter::Level,
                value: i64::from(value),
            });
        }
        Ok(Self(value))
    }
}

impl From<Level> for i32 {
    fn from(level: Level) -> Self {
        level.0
    }
}

/// The input size of one chunk when a frame is segmented.
///
/// The reference's serial segmenter (`ZL_Compressor_buildSerialSegmenter`)
/// accepts `32 KiB..=i32::MAX` bytes and defaults to 16 MiB.
///
/// # Examples
///
/// ```
/// use mopenzl::ChunkSize;
///
/// let size = ChunkSize::try_from(1usize << 20)?;
/// assert_eq!(usize::from(size), 1 << 20);
/// assert_eq!(usize::from(ChunkSize::DEFAULT), 16 << 20);
/// assert!(ChunkSize::try_from(4096usize).is_err());
/// # Ok::<(), mopenzl::ConfigError>(())
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ChunkSize(u32);

impl ChunkSize {
    /// The smallest chunk, 32 KiB (`ZL_MIN_CHUNK_SIZE`).
    pub const MIN: Self = Self(32 << 10);
    /// The largest chunk, `i32::MAX` bytes.
    pub const MAX: Self = Self(i32::MAX as u32);
    /// The reference's default, 16 MiB (`ZL_DEFAULT_SEGMENTER_CHUNK_BYTE_SIZE`).
    pub const DEFAULT: Self = Self(16 << 20);

    /// The size in bytes.
    pub(crate) const fn bytes(self) -> usize {
        self.0 as usize
    }
}

impl Default for ChunkSize {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<usize> for ChunkSize {
    type Error = ConfigError;

    /// Validates a chunk size.
    ///
    /// # Errors
    ///
    /// [`ConfigError::OutOfRange`] outside `32768..=2147483647`.
    fn try_from(value: usize) -> Result<Self, Self::Error> {
        if value < Self::MIN.bytes() || value > Self::MAX.bytes() {
            return Err(ConfigError::OutOfRange {
                parameter: Parameter::ChunkSize,
                value: i64::try_from(value).unwrap_or(i64::MAX),
            });
        }
        Ok(Self(value as u32))
    }
}

impl From<ChunkSize> for usize {
    fn from(size: ChunkSize) -> Self {
        size.bytes()
    }
}

/// How a frame's input is cut into independently compressed chunks.
///
/// [`Self::Whole`] is what `ZL_CCtx_compress` does with its default graph:
/// the whole input is one chunk, so a session buffers everything until the
/// frame ends. [`Self::Fixed`] is the serial segmenter of the reference CLI's
/// `serial` profile: a session holds at most one chunk of input, and decoders
/// can deliver output chunk by chunk. Both are byte-identical to the
/// reference configured the same way.
///
/// Whole inputs above 2 GiB are still cut every 2 GiB, because the format
/// caps a stored stream below 4 GiB.
///
/// # Examples
///
/// ```
/// use mopenzl::{ChunkSize, Chunking, EncoderConfig};
///
/// assert_eq!(Chunking::default(), Chunking::Whole);
/// let config = EncoderConfig::default().with_chunking(Chunking::Fixed(ChunkSize::DEFAULT));
/// assert_eq!(config.chunking(), Chunking::Fixed(ChunkSize::DEFAULT));
/// ```
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Chunking {
    /// The whole input is one chunk.
    #[default]
    Whole,
    /// Chunks of this many input bytes; the last one may be shorter.
    Fixed(ChunkSize),
}

impl From<ChunkSize> for Chunking {
    fn from(size: ChunkSize) -> Self {
        Self::Fixed(size)
    }
}

/// The largest chunk [`Chunking::Whole`] writes.
pub(crate) const WHOLE_CHUNK_LIMIT: usize = 1 << 31;

impl Chunking {
    /// The most input bytes one chunk takes.
    pub(crate) const fn limit(self) -> usize {
        match self {
            Self::Whole => WHOLE_CHUNK_LIMIT,
            Self::Fixed(size) => size.bytes(),
        }
    }
}

/// Everything that shapes the bytes of a frame.
///
/// The defaults are the reference's: level 6, the newest format version,
/// content and compressed checksums on, and the whole input as one chunk.
///
/// # Examples
///
/// ```
/// use mopenzl::{EncoderConfig, FormatVersion, Level};
///
/// let config = EncoderConfig::default()
///     .with_level(Level::try_from(3)?)
///     .with_format_version(FormatVersion::try_from(25)?)
///     .with_content_checksum(false);
/// assert_eq!(config.level().get(), 3);
/// assert_eq!(u32::from(config.format_version()), 25);
/// assert!(!config.content_checksum());
/// assert!(config.compressed_checksum());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct EncoderConfig {
    level: Level,
    format_version: FormatVersion,
    chunking: Chunking,
    content_checksum: bool,
    compressed_checksum: bool,
    decompression_level: u8,
    min_stream_size: u32,
    store_on_expansion: bool,
    permissive: bool,
}

impl EncoderConfig {
    /// The reference's defaults.
    pub const DEFAULT: Self = Self {
        level: Level::DEFAULT,
        format_version: FormatVersion::MAX,
        chunking: Chunking::Whole,
        content_checksum: true,
        compressed_checksum: true,
        decompression_level: Self::DEFAULT_DECOMPRESSION_LEVEL,
        min_stream_size: Self::DEFAULT_MIN_STREAM_SIZE,
        store_on_expansion: true,
        permissive: false,
    };

    /// The reference's default decompression level (`ZL_DECOMPRESSIONLEVEL_DEFAULT`).
    pub const DEFAULT_DECOMPRESSION_LEVEL: u8 = 3;

    /// The reference's default minimum stream size
    /// (`ZL_MINSTREAMSIZE_DEFAULT`): smaller streams are stored.
    pub const DEFAULT_MIN_STREAM_SIZE: u32 = 10;

    /// Sets the compression level.
    #[must_use]
    pub const fn with_level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// Sets the format version frames are written in.
    #[must_use]
    pub const fn with_format_version(mut self, version: FormatVersion) -> Self {
        self.format_version = version;
        self
    }

    /// Sets how the input is cut into chunks.
    #[must_use]
    pub const fn with_chunking(mut self, chunking: Chunking) -> Self {
        self.chunking = chunking;
        self
    }

    /// Whether every chunk records a checksum of its content
    /// (`ZL_CParam_contentChecksum`).
    #[must_use]
    pub const fn with_content_checksum(mut self, enabled: bool) -> Self {
        self.content_checksum = enabled;
        self
    }

    /// Whether every chunk records a checksum of its compressed bytes, and the
    /// frame header a check byte (`ZL_CParam_compressedChecksum`).
    #[must_use]
    pub const fn with_compressed_checksum(mut self, enabled: bool) -> Self {
        self.compressed_checksum = enabled;
        self
    }

    /// Sets the decompression level (`ZL_CParam_decompressionLevel`), which
    /// trades compression for decoder speed in some codecs. As in the
    /// reference, `0` selects the default,
    /// [`Self::DEFAULT_DECOMPRESSION_LEVEL`].
    #[must_use]
    pub const fn with_decompression_level(mut self, level: u8) -> Self {
        self.decompression_level = if level == 0 {
            Self::DEFAULT_DECOMPRESSION_LEVEL
        } else {
            level
        };
        self
    }

    /// Sets the size below which a lone stream is stored without being
    /// compressed (`ZL_CParam_minStreamSize`). As in the reference, `0`
    /// selects the default, [`Self::DEFAULT_MIN_STREAM_SIZE`].
    #[must_use]
    pub const fn with_min_stream_size(mut self, size: u32) -> Self {
        self.min_stream_size = if size == 0 {
            Self::DEFAULT_MIN_STREAM_SIZE
        } else {
            size
        };
        self
    }

    /// Whether a chunk that would not be smaller than its input is stored
    /// instead (`ZL_CParam_storeOnExpansion`).
    #[must_use]
    pub const fn with_store_on_expansion(mut self, enabled: bool) -> Self {
        self.store_on_expansion = enabled;
        self
    }

    /// Whether a failing successor is replaced by `compress_generic`
    /// (`ZL_CParam_permissiveCompression`).
    #[must_use]
    pub const fn with_permissive(mut self, enabled: bool) -> Self {
        self.permissive = enabled;
        self
    }

    /// The decompression level.
    #[must_use]
    pub const fn decompression_level(&self) -> u8 {
        self.decompression_level
    }

    /// The minimum stream size.
    #[must_use]
    pub const fn min_stream_size(&self) -> u32 {
        self.min_stream_size
    }

    /// Whether expanding chunks are stored.
    #[must_use]
    pub const fn store_on_expansion(&self) -> bool {
        self.store_on_expansion
    }

    /// Whether permissive compression is on.
    #[must_use]
    pub const fn permissive(&self) -> bool {
        self.permissive
    }

    /// The compression level.
    #[must_use]
    pub const fn level(&self) -> Level {
        self.level
    }

    /// The format version.
    #[must_use]
    pub const fn format_version(&self) -> FormatVersion {
        self.format_version
    }

    /// How the input is cut into chunks.
    #[must_use]
    pub const fn chunking(&self) -> Chunking {
        self.chunking
    }

    /// Whether chunks carry content checksums.
    #[must_use]
    pub const fn content_checksum(&self) -> bool {
        self.content_checksum
    }

    /// Whether chunks carry compressed checksums.
    #[must_use]
    pub const fn compressed_checksum(&self) -> bool {
        self.compressed_checksum
    }
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl From<Level> for EncoderConfig {
    fn from(level: Level) -> Self {
        Self::DEFAULT.with_level(level)
    }
}

/// The configuration value a [`ConfigError`] is about.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum Parameter {
    /// [`Level`].
    Level,
    /// [`ChunkSize`].
    ChunkSize,
}

impl fmt::Display for Parameter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Level => "compression level",
            Self::ChunkSize => "chunk size",
        })
    }
}

/// A configuration value outside its valid range.
///
/// # Examples
///
/// ```
/// use mopenzl::{ConfigError, Level, Parameter};
///
/// assert_eq!(
///     Level::try_from(99),
///     Err(ConfigError::OutOfRange { parameter: Parameter::Level, value: 99 })
/// );
/// ```
#[derive(Error, Debug, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
pub enum ConfigError {
    /// The value lies outside the parameter's range.
    #[error("{parameter} {value} is out of range")]
    OutOfRange {
        /// Which parameter.
        parameter: Parameter,
        /// The rejected value.
        value: i64,
    },
    /// Header comments need format version 22 or newer.
    #[error("format version {version} predates header comments (22)")]
    CommentUnsupported {
        /// The format version in use.
        version: u32,
    },
}

/// A compressed-size bound that does not fit in `usize`.
///
/// # Examples
///
/// ```
/// use mopenzl::{Compressor, SizeOverflow};
///
/// assert_eq!(Compressor::max_compressed_size(usize::MAX), Err(SizeOverflow));
/// ```
#[derive(Error, Debug, Clone, Copy, Eq, PartialEq)]
#[error("the compressed-size bound overflows usize")]
pub struct SizeOverflow;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn levels_follow_the_reference_range_and_zero_default() {
        assert_eq!(Level::try_from(0), Ok(Level::DEFAULT));
        assert_eq!(Level::try_from(-131_072), Ok(Level::MIN));
        assert_eq!(Level::try_from(22), Ok(Level::MAX));
        assert!(Level::try_from(-131_073).is_err());
        assert_eq!(Level::default().get(), 6);
        assert_eq!(i32::from(Level::try_from(-5).unwrap()), -5);
    }

    #[test]
    fn chunk_sizes_follow_the_segmenter_range() {
        assert_eq!(ChunkSize::try_from(32_768), Ok(ChunkSize::MIN));
        assert_eq!(ChunkSize::try_from(i32::MAX as usize), Ok(ChunkSize::MAX));
        assert_eq!(
            ChunkSize::try_from(32_767),
            Err(ConfigError::OutOfRange {
                parameter: Parameter::ChunkSize,
                value: 32_767,
            })
        );
        assert!(ChunkSize::try_from(1usize << 31).is_err());
        assert_eq!(ChunkSize::default(), ChunkSize::DEFAULT);
        assert_eq!(
            Chunking::from(ChunkSize::MIN),
            Chunking::Fixed(ChunkSize::MIN)
        );
        assert_eq!(Chunking::Whole.limit(), WHOLE_CHUNK_LIMIT);
        assert_eq!(Chunking::Fixed(ChunkSize::MIN).limit(), 32_768);
    }

    #[test]
    fn config_builders_set_every_field() {
        let config = EncoderConfig::from(Level::MAX)
            .with_format_version(FormatVersion::MIN)
            .with_chunking(Chunking::Fixed(ChunkSize::MIN))
            .with_content_checksum(false)
            .with_compressed_checksum(false);
        assert_eq!(config.level(), Level::MAX);
        assert_eq!(config.format_version(), FormatVersion::MIN);
        assert_eq!(config.chunking(), Chunking::Fixed(ChunkSize::MIN));
        assert!(!config.content_checksum());
        assert!(!config.compressed_checksum());
        assert_eq!(EncoderConfig::default(), EncoderConfig::DEFAULT);
    }

    #[test]
    fn engine_parameters_default_to_the_reference_and_zero_means_default() {
        let config = EncoderConfig::default();
        assert_eq!(config.decompression_level(), 3);
        assert_eq!(config.min_stream_size(), 10);
        assert!(config.store_on_expansion());
        assert!(!config.permissive());
        let config = config
            .with_decompression_level(1)
            .with_min_stream_size(100)
            .with_store_on_expansion(false)
            .with_permissive(true);
        assert_eq!(config.decompression_level(), 1);
        assert_eq!(config.min_stream_size(), 100);
        assert!(!config.store_on_expansion());
        assert!(config.permissive());
        let config = config.with_decompression_level(0).with_min_stream_size(0);
        assert_eq!(
            (config.decompression_level(), config.min_stream_size()),
            (3, 10)
        );
    }

    #[test]
    fn errors_name_the_parameter() {
        let error = ConfigError::OutOfRange {
            parameter: Parameter::ChunkSize,
            value: 3,
        };
        assert_eq!(error.to_string(), "chunk size 3 is out of range");
        assert_eq!(Parameter::Level.to_string(), "compression level");
        assert!(SizeOverflow.to_string().contains("overflows"));
    }
}
