//! Frame headers (`DFH_FrameInfo_decodeFrameHeader`, v21+).
//!
//! ```text
//! magic (4, LE) | flags (1) | [bundle ID: len (1) + bytes, v25+]
//!   | inputs token: count in bits 0-3 (15: one more byte), types from bit 4
//!   | [types of inputs 3+, 2 bits each] | varint(size + 1) per input
//!   | [comment: varint(len) + bytes, v22+] | [check byte]
//! ```

use fearless_simd::Level;

use super::reader::{Parse, Reader};
use crate::FormatVersion;
use crate::decompressor::error::{DecodeError, InvalidDataKind, Unsupported};
use crate::shared::wire::{
    BUNDLE_VERSION_MIN, COMMENT_MAX, COMMENT_VERSION_MIN, FLAG_BUNDLE_ID, FLAG_COMMENT,
    FLAG_COMPRESSED_CHECKSUM, FLAG_CONTENT_CHECKSUM, INPUT_LIMIT, MAGIC_BASE, MAGIC_PADDING,
    StreamType,
};
use crate::shared::xxh3;

/// What a frame header says about its frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct FrameInfo {
    /// The wire-format version.
    pub(crate) version: u32,
    /// Chunks carry content checksums.
    pub(crate) content_checksum: bool,
    /// Chunks carry compressed checksums.
    pub(crate) compressed_checksum: bool,
    /// The decompressed size.
    pub(crate) size: u64,
    /// Bytes of the header.
    pub(crate) header_len: usize,
}

/// Reads the version from a magic number.
fn version(magic: u32) -> Result<u32, DecodeError> {
    let newest = FormatVersion::MAX.get();
    if magic < MAGIC_BASE || magic > MAGIC_BASE + newest + MAGIC_PADDING {
        return Err(DecodeError::invalid(InvalidDataKind::Magic));
    }
    let version = magic - MAGIC_BASE;
    if FormatVersion::new(version).is_none() {
        // Versions 8-20 and the padding above 27 are OpenZL, but not ours.
        return Err(DecodeError::unsupported(Unsupported::FormatVersion {
            version,
        }));
    }
    Ok(version)
}

/// Parses the frame header at the front of `bytes`.
///
/// `verify` checks the header's check byte.
pub(crate) fn parse(bytes: &[u8], simd: Level, verify: bool) -> Result<FrameInfo, Parse> {
    let mut reader = Reader::new(bytes, InvalidDataKind::FrameHeader);
    let magic = u32::from_le_bytes(reader.take(4)?.try_into().map_err(|_| Parse::Truncated)?);
    let version = version(magic)?;
    let flags = reader.byte()?;
    if version >= BUNDLE_VERSION_MIN && flags & FLAG_BUNDLE_ID != 0 {
        return Err(DecodeError::unsupported(Unsupported::Dictionary).into());
    }
    let token = reader.byte()?;
    let mut outputs = usize::from(token & 15);
    if outputs == 15 {
        outputs = (usize::from(reader.byte()?) << 4) + usize::from(token >> 4) + 15;
    }
    if outputs == 0 || outputs > INPUT_LIMIT {
        return Err(reader.invalid());
    }
    if outputs == 1 && token >> 6 != 0 {
        // The type bits of a second output that does not exist
        // (`ZL_getDecompressedSize` rejects them).
        return Err(reader.invalid());
    }
    if outputs > 1 {
        return Err(DecodeError::unsupported(Unsupported::MultipleOutputs { outputs }).into());
    }
    if StreamType::from_code(token >> 4) != StreamType::Serial {
        return Err(DecodeError::unsupported(Unsupported::TypedOutput).into());
    }
    let size = match reader.varint()? {
        0 => return Err(DecodeError::unsupported(Unsupported::UnknownSize).into()),
        encoded => encoded - 1,
    };
    if version >= COMMENT_VERSION_MIN && flags & FLAG_COMMENT != 0 {
        let len = reader.varint()?;
        if len == 0 || len > COMMENT_MAX {
            return Err(reader.invalid());
        }
        reader.take(len as usize)?;
    }
    let compressed_checksum = flags & FLAG_COMPRESSED_CHECKSUM != 0;
    if compressed_checksum {
        let expected = xxh3::hash64(simd, reader.consumed()) as u8;
        let stored = reader.byte()?;
        if verify && stored != expected {
            return Err(DecodeError::invalid(InvalidDataKind::HeaderChecksum).into());
        }
    }
    Ok(FrameInfo {
        version,
        content_checksum: flags & FLAG_CONTENT_CHECKSUM != 0,
        compressed_checksum,
        size,
        header_len: reader.pos(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Backend;

    fn parse_default(bytes: &[u8]) -> Result<FrameInfo, Parse> {
        parse(bytes, Backend::default().0, true)
    }

    fn failed(error: DecodeError) -> Result<FrameInfo, Parse> {
        Err(Parse::Failed(error))
    }

    #[test]
    fn reference_headers_parse() {
        let header = [0xdb, 0xa5, 0xb1, 0xd7, 0x03, 0x01, 0xad, 0x02, 0x03];
        assert_eq!(
            parse_default(&header),
            Ok(FrameInfo {
                version: 27,
                content_checksum: true,
                compressed_checksum: true,
                size: 300,
                header_len: 9,
            })
        );
        for len in 0..header.len() {
            assert_eq!(parse_default(&header[..len]), Err(Parse::Truncated));
        }
        let unchecked = [0xd5, 0xa5, 0xb1, 0xd7, 0x00, 0x01, 0x01];
        assert_eq!(parse_default(&unchecked).unwrap().size, 0);
    }

    #[test]
    fn check_byte_is_verified_on_request() {
        let header = [0xdb, 0xa5, 0xb1, 0xd7, 0x03, 0x01, 0xad, 0x02, 0x04];
        assert_eq!(
            parse_default(&header),
            failed(DecodeError::invalid(InvalidDataKind::HeaderChecksum))
        );
        assert!(parse(&header, Backend::default().0, false).is_ok());
    }

    #[test]
    fn magics_split_into_foreign_and_unsupported() {
        assert_eq!(
            parse_default(&[0x28, 0xb5, 0x2f, 0xfd, 0]),
            failed(DecodeError::invalid(InvalidDataKind::Magic))
        );
        let v20 = (MAGIC_BASE + 20).to_le_bytes();
        assert_eq!(
            parse_default(&v20),
            failed(DecodeError::unsupported(Unsupported::FormatVersion {
                version: 20
            }))
        );
        let v43 = (MAGIC_BASE + 43).to_le_bytes();
        assert!(matches!(
            parse_default(&v43),
            Err(Parse::Failed(DecodeError::Unsupported { .. }))
        ));
        let v44 = (MAGIC_BASE + 44).to_le_bytes();
        assert_eq!(
            parse_default(&v44),
            failed(DecodeError::invalid(InvalidDataKind::Magic))
        );
    }

    #[test]
    fn unsupported_shapes_are_named() {
        let base = [0xdb, 0xa5, 0xb1, 0xd7];
        let with = |rest: &[u8]| [&base[..], rest].concat();
        assert_eq!(
            parse_default(&with(&[0x08, 1])),
            failed(DecodeError::unsupported(Unsupported::Dictionary))
        );
        assert_eq!(
            parse_default(&with(&[0x00, 0x02])),
            failed(DecodeError::unsupported(Unsupported::MultipleOutputs {
                outputs: 2
            }))
        );
        assert_eq!(
            parse_default(&with(&[0x00, 0x0f, 0x00])),
            failed(DecodeError::unsupported(Unsupported::MultipleOutputs {
                outputs: 15
            }))
        );
        assert_eq!(
            parse_default(&with(&[0x00, 0x21])),
            failed(DecodeError::unsupported(Unsupported::TypedOutput))
        );
        assert_eq!(
            parse_default(&with(&[0x00, 0x01, 0x00])),
            failed(DecodeError::unsupported(Unsupported::UnknownSize))
        );
        assert_eq!(
            parse_default(&with(&[0x00, 0x00])),
            failed(DecodeError::invalid(InvalidDataKind::FrameHeader))
        );
        assert_eq!(
            parse_default(&with(&[0x00, 0x41, 0x01])),
            failed(DecodeError::invalid(InvalidDataKind::FrameHeader))
        );
    }

    #[test]
    fn comments_are_skipped_and_bounded() {
        let base = [0xdb, 0xa5, 0xb1, 0xd7, 0x04, 0x01, 0x01];
        let with = |rest: &[u8]| [&base[..], rest].concat();
        assert_eq!(
            parse_default(&with(&[2, b'h', b'i'])).unwrap().header_len,
            10
        );
        assert_eq!(parse_default(&with(&[2, b'h'])), Err(Parse::Truncated));
        assert_eq!(
            parse_default(&with(&[0])),
            failed(DecodeError::invalid(InvalidDataKind::FrameHeader))
        );
        assert_eq!(
            parse_default(&with(&[0x91, 0x4e])),
            failed(DecodeError::invalid(InvalidDataKind::FrameHeader))
        );
    }
}
