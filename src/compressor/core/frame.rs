//! The frame header and footer (`writeFrameHeader_internal`, v21+).
//!
//! ```text
//! magic (4, LE) | flags (1) | inputs token (1) | varint(size + 1)
//!   | [check byte: XXH3 & 0xFF, with compressed checksums]
//! ... chunks ...
//! end-of-frame marker (1, 0x00)
//! ```
//!
//! The encoder writes one serial input of known size, without a comment or
//! dictionary bundle.

use fearless_simd::Level;

use super::Params;
use super::sink::{Full, Sink};
use crate::shared::wire::{
    END_OF_FRAME, FLAG_COMPRESSED_CHECKSUM, FLAG_CONTENT_CHECKSUM, MAGIC_BASE,
};
use crate::shared::{varint, xxh3};

/// The longest header this encoder writes.
pub(crate) const HEADER_MAX: usize = 4 + 1 + 1 + varint::MAX_LEN + 1;

/// Writes the header of a frame of `total` input bytes.
pub(crate) fn write_header(
    params: &Params,
    simd: Level,
    total: u64,
    sink: &mut impl Sink,
) -> Result<(), Full> {
    let mut header = [0u8; HEADER_MAX];
    header[..4].copy_from_slice(&(MAGIC_BASE + params.version).to_le_bytes());
    let mut flags = 0;
    if params.content_checksum {
        flags |= FLAG_CONTENT_CHECKSUM;
    }
    if params.compressed_checksum {
        flags |= FLAG_COMPRESSED_CHECKSUM;
    }
    header[4] = flags;
    // One input; its type, serial, is code 0 in bits 4-5.
    header[5] = 1;
    let (size, size_len) = varint::encode(total.saturating_add(1));
    let mut len = 6 + size_len;
    header[6..len].copy_from_slice(&size[..size_len]);
    if params.compressed_checksum {
        header[len] = xxh3::hash64(simd, &header[..len]) as u8;
        len += 1;
    }
    sink.put(&header[..len])
}

/// Writes the end-of-frame marker.
pub(crate) fn write_end(sink: &mut impl Sink) -> Result<(), Full> {
    sink.put(&[END_OF_FRAME])
}

#[cfg(test)]
mod tests {
    use super::super::sink::VecSink;
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn params(version: u32, checksums: bool) -> Params {
        Params {
            version,
            level: 6,
            content_checksum: checksums,
            compressed_checksum: checksums,
            chunk_limit: 1 << 31,
        }
    }

    #[test]
    fn header_matches_the_reference_bytes() {
        // `ZL_compress` of 300 bytes at v27, and of nothing at v21.
        let mut out = Vec::new();
        write_header(
            &params(27, true),
            Backend::default().0,
            300,
            &mut VecSink::new(&mut out),
        )
        .unwrap();
        assert_eq!(out, [0xdb, 0xa5, 0xb1, 0xd7, 0x03, 0x01, 0xad, 0x02, 0x03]);
        let mut out = Vec::new();
        write_header(
            &params(21, true),
            Backend::default().0,
            0,
            &mut VecSink::new(&mut out),
        )
        .unwrap();
        assert_eq!(out, [0xd5, 0xa5, 0xb1, 0xd7, 0x03, 0x01, 0x01, 0xb2]);
    }

    #[test]
    fn header_without_checksums_has_no_check_byte() {
        let mut out = Vec::new();
        write_header(
            &params(27, false),
            Backend::default().0,
            300,
            &mut VecSink::new(&mut out),
        )
        .unwrap();
        assert_eq!(out, [0xdb, 0xa5, 0xb1, 0xd7, 0x00, 0x01, 0xad, 0x02]);
        write_end(&mut VecSink::new(&mut out)).unwrap();
        assert_eq!(out.last(), Some(&0));
    }
}
