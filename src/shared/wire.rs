//! Constants of the OpenZL frame format shared by the encoder and the decoder.
//!
//! The layout itself is documented in the reference's
//! `src/openzl/common/wire_format.h`; see also `architecture/frame-format.md`.

/// `ZSTRONG_MAGIC_NUMBER_BASE`: the magic of format version `v` is
/// `MAGIC_BASE + v`, stored little-endian.
pub(crate) const MAGIC_BASE: u32 = 0xD7B1_A5C0;

/// Versions after the newest supported one that still read as OpenZL magics
/// (`ZL_getFormatVersionFromMagic` pads its range by 16).
#[cfg(feature = "decompression")]
pub(crate) const MAGIC_PADDING: u32 = 16;

/// Frame property flag: each chunk carries a content checksum.
pub(crate) const FLAG_CONTENT_CHECKSUM: u8 = 1 << 0;
/// Frame property flag: each chunk carries a compressed checksum, and the
/// frame header a check byte.
pub(crate) const FLAG_COMPRESSED_CHECKSUM: u8 = 1 << 1;
/// Frame property flag (v22+): a comment follows the input sizes.
#[cfg(feature = "decompression")]
pub(crate) const FLAG_COMMENT: u8 = 1 << 2;
/// Frame property flag (v25+): a dictionary bundle ID follows the flags.
#[cfg(feature = "decompression")]
pub(crate) const FLAG_BUNDLE_ID: u8 = 1 << 3;

/// First version with header comments.
#[cfg(feature = "decompression")]
pub(crate) const COMMENT_VERSION_MIN: u32 = 22;
/// First version whose standard codec IDs take 7 bits instead of 6.
pub(crate) const WIDE_CODEC_ID_VERSION_MIN: u32 = 24;
/// First version with dictionary bundles.
#[cfg(feature = "decompression")]
pub(crate) const BUNDLE_VERSION_MIN: u32 = 25;
/// Longest header comment (`ZL_MAX_HEADER_COMMENT_SIZE_LIMIT`).
#[cfg(feature = "decompression")]
pub(crate) const COMMENT_MAX: u64 = 10_000;

/// End-of-frame marker: a chunk header starting with this byte ends the frame.
pub(crate) const END_OF_FRAME: u8 = 0;

/// `ZL_runtimeInputLimit` for v21+.
#[cfg(feature = "decompression")]
pub(crate) const INPUT_LIMIT: usize = 2048;
/// `ZL_runtimeNodeLimit` for v20+.
#[cfg(feature = "decompression")]
pub(crate) const NODE_LIMIT: u64 = 20_000;
/// `ZL_runtimeStreamLimit` for v16+.
#[cfg(feature = "decompression")]
pub(crate) const STREAM_LIMIT: u64 = 110_000;

/// `ZL_StandardTransformID_zstd`.
#[cfg(feature = "zstd")]
pub(crate) const CODEC_ZSTD: u32 = 22;

/// `ZL_MINSTREAMSIZE_DEFAULT`: smaller inputs are stored, not compressed.
#[cfg(all(feature = "compression", feature = "zstd"))]
pub(crate) const MIN_STREAM_SIZE: usize = 10;

/// Bits of a standard codec ID in format version `version`
/// (`ZL_StandardTransformID_numBits`).
pub(crate) const fn codec_id_bits(version: u32) -> u32 {
    if version < WIDE_CODEC_ID_VERSION_MIN {
        6
    } else {
        7
    }
}

/// Stream element types (`ZL_Type`) as two-bit codes.
#[cfg(feature = "decompression")]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum StreamType {
    /// Raw bytes.
    Serial,
    /// Fixed-width records.
    Struct,
    /// Little-endian integers.
    Numeric,
    /// Variable-length strings.
    String,
}

#[cfg(feature = "decompression")]
impl StreamType {
    /// The type of two-bit code `code`.
    pub(crate) const fn from_code(code: u8) -> Self {
        match code & 3 {
            0 => Self::Serial,
            1 => Self::Struct,
            2 => Self::Numeric,
            _ => Self::String,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_id_width_grows_at_version_24() {
        assert_eq!(codec_id_bits(21), 6);
        assert_eq!(codec_id_bits(23), 6);
        assert_eq!(codec_id_bits(24), 7);
        assert_eq!(codec_id_bits(27), 7);
    }

    #[cfg(feature = "decompression")]
    #[test]
    fn type_codes_follow_the_reference_order() {
        assert_eq!(StreamType::from_code(0), StreamType::Serial);
        assert_eq!(StreamType::from_code(1), StreamType::Struct);
        assert_eq!(StreamType::from_code(2), StreamType::Numeric);
        assert_eq!(StreamType::from_code(3), StreamType::String);
    }
}
