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
use crate::dict::BundleId;
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
    /// The decompressed size: content bytes over every output.
    pub(crate) size: u64,
    /// Bytes of the header.
    pub(crate) header_len: usize,
    /// Number of outputs: the last streams of every chunk.
    pub(crate) outputs: usize,
    /// Strings over every string output.
    pub(crate) strings: u64,
    /// The dictionary bundle the frame was compressed with (v25+).
    pub(crate) bundle: Option<BundleId>,
    /// The header comment as `(offset, length)` in the frame (v22+).
    pub(crate) comment: Option<(usize, usize)>,
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

/// One output of a frame, as its header declares it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct OutputSpec {
    /// The output's type.
    pub(crate) ty: StreamType,
    /// Its content bytes.
    pub(crate) size: u64,
    /// Its strings, for string outputs.
    pub(crate) strings: u64,
}

/// Parses the frame header at the front of `bytes`, for callers that take
/// one serial output.
///
/// `verify` checks the header's check byte.
///
/// # Errors
///
/// [`Unsupported::MultipleOutputs`] and [`Unsupported::TypedOutput`] for
/// frames with other outputs, which only the typed decoder reads.
pub(crate) fn parse(bytes: &[u8], simd: Level, verify: bool) -> Result<FrameInfo, Parse> {
    let mut first = None;
    let info = parse_with(bytes, simd, verify, |spec| {
        first.get_or_insert(spec);
    })?;
    if info.outputs > 1 {
        let outputs = info.outputs;
        return Err(DecodeError::unsupported(Unsupported::MultipleOutputs { outputs }).into());
    }
    if first.map(|spec| spec.ty) != Some(StreamType::Serial) {
        return Err(DecodeError::unsupported(Unsupported::TypedOutput).into());
    }
    Ok(info)
}

/// Parses the frame header at the front of `bytes`, passing each output's
/// declaration to `output` in order.
pub(crate) fn parse_with(
    bytes: &[u8],
    simd: Level,
    verify: bool,
    mut output: impl FnMut(OutputSpec),
) -> Result<FrameInfo, Parse> {
    let mut reader = Reader::new(bytes, InvalidDataKind::FrameHeader);
    let magic = u32::from_le_bytes(reader.take(4)?.try_into().map_err(|_| Parse::Truncated)?);
    let version = version(magic)?;
    let flags = reader.byte()?;
    let bundle = if version >= BUNDLE_VERSION_MIN && flags & FLAG_BUNDLE_ID != 0 {
        Some(read_bundle_id(&mut reader)?)
    } else {
        None
    };
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
    // `DFH_decoderOutputTypes`: up to 14 outputs keep the first two types
    // in the token; the rest follow, four per byte.
    let mut types = alloc::vec::Vec::with_capacity(outputs);
    let mut done = 0;
    if outputs <= 14 {
        for n in 0..outputs.min(2) {
            types.push(StreamType::from_code(token >> (4 + 2 * n)));
        }
        done = types.len();
    }
    let mut packed = 0u8;
    for n in done..outputs {
        let shift = (n - done) % 4 * 2;
        if shift == 0 {
            packed = reader.byte()?;
        }
        types.push(StreamType::from_code(packed >> shift));
    }
    let mut sizes = alloc::vec::Vec::with_capacity(outputs);
    for _ in 0..outputs {
        sizes.push(match reader.varint()? {
            0 => return Err(DecodeError::unsupported(Unsupported::UnknownSize).into()),
            encoded => encoded - 1,
        });
    }
    let mut size = 0u64;
    let mut strings = 0u64;
    for (&ty, &bytes) in types.iter().zip(&sizes) {
        let count = if ty == StreamType::String {
            reader.varint()?
        } else {
            0
        };
        size = size.saturating_add(bytes);
        strings = strings.saturating_add(count);
        output(OutputSpec {
            ty,
            size: bytes,
            strings: count,
        });
    }
    let mut comment = None;
    if version >= COMMENT_VERSION_MIN && flags & FLAG_COMMENT != 0 {
        let len = reader.varint()?;
        if len == 0 || len > COMMENT_MAX {
            return Err(reader.invalid());
        }
        let start = reader.pos();
        reader.take(len as usize)?;
        comment = Some((start, len as usize));
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
        outputs,
        strings,
        bundle,
        comment,
    })
}

/// The bundle ID: a length byte, then that many leading bytes of the ID; the
/// rest are zero, and an all-zero ID is invalid.
fn read_bundle_id(reader: &mut Reader<'_>) -> Result<BundleId, Parse> {
    let len = usize::from(reader.byte()?);
    if len == 0 || len > 32 {
        return Err(reader.invalid());
    }
    let mut id = [0u8; 32];
    id[..len].copy_from_slice(reader.take(len)?);
    if id == [0; 32] {
        return Err(reader.invalid());
    }
    Ok(BundleId::from(id))
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
                outputs: 1,
                strings: 0,
                bundle: None,
                comment: None,
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
    fn bundle_ids_are_read_without_their_trailing_zeros() {
        let base = [0xdb, 0xa5, 0xb1, 0xd7, 0x08];
        let with = |rest: &[u8]| [&base[..], rest].concat();
        // A two-byte ID, then one output of one byte.
        let info = parse_default(&with(&[2, 0xaa, 0xbb, 0x01, 0x02])).unwrap();
        let mut expected = [0u8; 32];
        expected[..2].copy_from_slice(&[0xaa, 0xbb]);
        assert_eq!(info.bundle, Some(BundleId::from(expected)));
        assert_eq!(info.header_len, 10);
        // A full 32-byte ID.
        let full = [&[32u8][..], &[7; 32], &[0x01, 0x02]].concat();
        assert_eq!(
            parse_default(&with(&full)).unwrap().bundle,
            Some(BundleId::from([7; 32]))
        );
        // Zero length, over-long, all-zero IDs are invalid; short is truncated.
        for rest in [&[0u8][..], &[33], &[2, 0, 0, 1, 2]] {
            assert_eq!(
                parse_default(&with(rest)),
                failed(DecodeError::invalid(InvalidDataKind::FrameHeader))
            );
        }
        assert_eq!(parse_default(&with(&[2, 0xaa])), Err(Parse::Truncated));
        assert_eq!(parse_default(&with(&[])), Err(Parse::Truncated));
        // Below v25 the flag is ignored.
        let v24 = [0xd8, 0xa5, 0xb1, 0xd7, 0x08, 0x01, 0x02];
        assert_eq!(parse_default(&v24).unwrap().bundle, None);
    }

    #[test]
    fn unsupported_shapes_are_named() {
        let base = [0xdb, 0xa5, 0xb1, 0xd7];
        let with = |rest: &[u8]| [&base[..], rest].concat();
        assert_eq!(
            parse_default(&with(&[0x00, 0x02, 0x01, 0x01])),
            failed(DecodeError::unsupported(Unsupported::MultipleOutputs {
                outputs: 2
            }))
        );
        assert_eq!(
            parse_default(&with(
                &[&[0x00, 0x0f, 0x00][..], &[0; 4], &[1; 15]].concat()
            )),
            failed(DecodeError::unsupported(Unsupported::MultipleOutputs {
                outputs: 15
            }))
        );
        assert_eq!(
            parse_default(&with(&[0x00, 0x21, 0x01])),
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
    fn typed_outputs_declare_types_sizes_and_strings() {
        let base = [0xdb, 0xa5, 0xb1, 0xd7, 0x00];
        let with = |rest: &[u8]| [&base[..], rest].concat();
        let collect = |bytes: &[u8]| {
            let mut specs = alloc::vec::Vec::new();
            parse_with(bytes, Backend::default().0, true, |spec| specs.push(spec))
                .map(|info| (info, specs))
        };
        // Three outputs: numeric (token), string (token), struct (next byte).
        let (info, specs) = collect(&with(&[0xe3, 0x01, 5, 7, 4, 3])).unwrap();
        assert_eq!((info.outputs, info.size, info.strings), (3, 13, 3));
        let types: alloc::vec::Vec<_> = specs.iter().map(|spec| spec.ty).collect();
        assert_eq!(
            types,
            [StreamType::Numeric, StreamType::String, StreamType::Struct]
        );
        assert_eq!(specs[1].strings, 3);
        assert_eq!(collect(&with(&[0xe3])).unwrap_err(), Parse::Truncated);
        // Fifteen outputs keep every type after the count.
        let mut fifteen = alloc::vec![0x0f, 0x00, 0b1001_0011, 0, 0, 0];
        fifteen.extend([1u8; 15]);
        fifteen.push(0);
        let (info, specs) = collect(&with(&fifteen)).unwrap();
        assert_eq!(info.outputs, 15);
        assert_eq!(specs[0].ty, StreamType::String);
        assert_eq!(specs[2].ty, StreamType::Struct);
        assert_eq!(specs[3].ty, StreamType::Numeric);
    }

    #[test]
    fn comments_are_skipped_and_bounded() {
        let base = [0xdb, 0xa5, 0xb1, 0xd7, 0x04, 0x01, 0x01];
        let with = |rest: &[u8]| [&base[..], rest].concat();
        let info = parse_default(&with(&[2, b'h', b'i'])).unwrap();
        assert_eq!(info.header_len, 10);
        assert_eq!(info.comment, Some((7 + 1, 2)));
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
