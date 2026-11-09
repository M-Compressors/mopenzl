//! The Zstandard decoder of the default graph (`DI_zstd`).
//!
//! The codec's stream is the element width as a varint followed by one
//! magicless Zstandard frame that records its content size. `mzstd` decodes
//! frames with a magic number, so the frame is staged behind one in a reused
//! buffer; the content size is read from the frame header first, so the
//! output is sized before decoding and the decoder writes straight into it.

use alloc::vec::Vec;

use crate::Backend;
use crate::decompressor::error::{DecodeError, InvalidDataKind};
use crate::shared::varint;

/// The Zstandard magic number, little-endian.
const MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// A malformed codec stream.
const fn bad_stream() -> DecodeError {
    DecodeError::invalid(InvalidDataKind::CodecStream)
}

/// The element width and frame of a codec stream.
pub(crate) fn split(stream: &[u8]) -> Result<(u64, &[u8]), DecodeError> {
    let (width, len) = varint::decode(stream).map_err(|_| bad_stream())?;
    if width == 0 {
        return Err(bad_stream());
    }
    Ok((width, &stream[len..]))
}

/// The content size a magicless frame header records
/// (`ZSTD_getFrameHeader_advanced` with `ZSTD_f_zstd1_magicless`).
pub(crate) fn content_size(frame: &[u8]) -> Result<u64, DecodeError> {
    let &descriptor = frame.first().ok_or_else(bad_stream)?;
    if descriptor & 0x08 != 0 {
        return Err(bad_stream());
    }
    let single_segment = descriptor & 0x20 != 0;
    let size_len = match descriptor >> 6 {
        0 if single_segment => 1,
        0 => return Err(bad_stream()),
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let dictionary_len = [0, 1, 2, 4][usize::from(descriptor & 3)];
    let start = 1 + usize::from(!single_segment) + dictionary_len;
    let bytes = frame.get(start..start + size_len).ok_or_else(bad_stream)?;
    let mut value = [0u8; 8];
    value[..size_len].copy_from_slice(bytes);
    let size = u64::from_le_bytes(value);
    Ok(if size_len == 2 { size + 256 } else { size })
}

/// A reusable Zstandard decoder and its staging buffer.
#[derive(Debug)]
pub(crate) struct ZstdDecoder {
    decoder: mzstd::Decompressor,
    staged: Vec<u8>,
}

impl ZstdDecoder {
    pub(crate) fn new(backend: Backend) -> Self {
        let zstd_backend = mzstd::Backend::available()
            .into_iter()
            .find(|candidate| candidate.name() == backend.name())
            .unwrap_or_default();
        let config = mzstd::DecoderConfig::default().with_frame_mode(mzstd::FrameMode::Single);
        Self {
            decoder: mzstd::Decompressor::builder(config)
                .with_backend(zstd_backend)
                .build(),
            staged: Vec::new(),
        }
    }

    /// Decodes `frame` (magicless) into all of `dst`, whose length is the
    /// frame's content size.
    #[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
    pub(crate) fn decode(&mut self, frame: &[u8], dst: &mut [u8]) -> Result<(), DecodeError> {
        self.staged.clear();
        self.staged.extend_from_slice(&MAGIC);
        self.staged.extend_from_slice(frame);
        match self.decoder.decompress_to_slice(&self.staged, dst) {
            Ok(written) if written == dst.len() => Ok(()),
            _ => Err(bad_stream()),
        }
    }

    /// Appends the `size` bytes `frame` (magicless) decodes to onto `dst`,
    /// leaving `dst` as it was on failure.
    pub(crate) fn decode_append(
        &mut self,
        frame: &[u8],
        dst: &mut Vec<u8>,
        size: usize,
    ) -> Result<(), DecodeError> {
        self.staged.clear();
        self.staged.extend_from_slice(&MAGIC);
        self.staged.extend_from_slice(frame);
        let start = dst.len();
        match self.decoder.decompress_into(&self.staged, dst) {
            Ok(range) if range.len() == size => Ok(()),
            _ => {
                dst.truncate(start);
                Err(bad_stream())
            }
        }
    }

    /// Bytes kept allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.staged.capacity() + self.decoder.retained_bytes()
    }

    /// Releases the buffers.
    pub(crate) fn release(&mut self) {
        self.staged = Vec::new();
        self.decoder.trim(mzstd::RetentionPolicy::ReleaseAll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(input: &[u8]) -> Vec<u8> {
        let mut encoder = mzstd::Compressor::default();
        encoder.compress(input).unwrap()[4..].to_vec()
    }

    #[test]
    fn content_sizes_of_every_field_width_are_read() {
        for len in [0usize, 200, 300, 70_000] {
            assert_eq!(
                content_size(&frame(&alloc::vec![1u8; len])),
                Ok(len as u64),
                "{len}"
            );
        }
        // No content size, a reserved bit, a short header.
        assert_eq!(content_size(&[0x00, 0x00]), Err(bad_stream()));
        assert_eq!(content_size(&[0x28]), Err(bad_stream()));
        assert_eq!(content_size(&[0xc0, 0x00]), Err(bad_stream()));
        assert_eq!(content_size(&[]), Err(bad_stream()));
        // Eight-byte sizes.
        assert_eq!(content_size(&[0xe0, 1, 0, 0, 0, 0, 0, 0, 0]), Ok(1));
    }

    #[test]
    fn frames_decode_into_exactly_sized_outputs() {
        let input: Vec<u8> = (0..5000u32).map(|i| (i % 13) as u8).collect();
        let frame = frame(&input);
        let mut decoder = ZstdDecoder::new(Backend::default());
        let mut out = alloc::vec![0u8; input.len()];
        decoder.decode(&frame, &mut out).unwrap();
        assert_eq!(out, input);
        let mut short = alloc::vec![0u8; input.len() - 1];
        assert_eq!(decoder.decode(&frame, &mut short), Err(bad_stream()));
        let mut long = alloc::vec![0u8; input.len() + 1];
        assert_eq!(decoder.decode(&frame, &mut long), Err(bad_stream()));
        let mut appended = b"head".to_vec();
        decoder
            .decode_append(&frame, &mut appended, input.len())
            .unwrap();
        assert_eq!(&appended[4..], input.as_slice());
        assert_eq!(
            decoder.decode_append(&frame, &mut appended, 1),
            Err(bad_stream())
        );
        assert_eq!(appended.len(), 4 + input.len());
        assert!(decoder.retained_bytes() > 0);
        decoder.release();
        assert_eq!(decoder.staged.capacity(), 0);
    }

    #[test]
    fn streams_split_into_width_and_frame() {
        assert_eq!(split(&[1, 9, 9]), Ok((1, &[9u8, 9][..])));
        assert_eq!(split(&[0, 9]), Err(bad_stream()));
        assert_eq!(split(&[]), Err(bad_stream()));
    }
}
