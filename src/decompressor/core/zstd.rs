//! The Zstandard decoder of the default graph (`DI_zstd`,
//! decode_zstd_binding.c).
//!
//! The codec's stream is the element width as a varint followed by one
//! magicless Zstandard frame that records its content size. `mzstd` decodes
//! magicless frames natively (`ZSTD_d_format = ZSTD_f_zstd1_magicless`), so
//! the frame is decoded where it lies: the content size is read from the
//! frame header first (`ZSTD_getFrameHeader_advanced`), the output is sized
//! before decoding and the decoder writes straight into it.

use alloc::vec::Vec;

use mzstd::dictionary::DecodeDictionary;

use crate::Backend;
use crate::decompressor::error::{DecodeError, InvalidDataKind};
use crate::dict::core::Materialized;
use crate::shared::varint;

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
/// (`ZSTD_getFrameHeader_advanced` with `ZSTD_f_zstd1_magicless`). A header
/// that cannot be read, or that omits the size, is a corrupt stream.
pub(crate) fn content_size(frame: &[u8]) -> Result<u64, DecodeError> {
    mzstd::decompressor::FrameHeaderInfo::parse(frame, mzstd::FrameFormat::Magicless)
        .ok()
        .and_then(|header| header.content_size)
        .ok_or_else(bad_stream)
}

/// The `ZSTD_DDict` of a materialized dictionary.
pub(crate) const fn zstd_dictionary(
    dict: Option<&Materialized>,
) -> Option<&mzstd::dictionary::DecodeDictionary> {
    match dict {
        Some(Materialized::Zstd(dictionary)) => Some(dictionary),
        None => None,
    }
}

/// A reusable magicless Zstandard decoder.
#[derive(Debug)]
pub(crate) struct ZstdDecoder {
    decoder: mzstd::Decompressor,
}

impl ZstdDecoder {
    pub(crate) fn new(backend: Backend) -> Self {
        let zstd_backend = mzstd::Backend::available()
            .into_iter()
            .find(|candidate| candidate.name() == backend.name())
            .unwrap_or_default();
        let config = mzstd::DecoderConfig::default()
            .with_frame_mode(mzstd::FrameMode::Single)
            .with_format(mzstd::FrameFormat::Magicless);
        Self {
            decoder: mzstd::Decompressor::builder(config)
                .with_backend(zstd_backend)
                .build(),
        }
    }

    /// Decodes `frame` (magicless) into all of `dst`, whose length is the
    /// frame's content size.
    #[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
    pub(crate) fn decode(
        &mut self,
        frame: &[u8],
        dst: &mut [u8],
        dict: Option<&DecodeDictionary>,
    ) -> Result<(), DecodeError> {
        let result = match dict {
            Some(dict) => self
                .decoder
                .decompress_with_dictionary_to_slice(dict, frame, dst),
            None => self.decoder.decompress_to_slice(frame, dst),
        };
        match result {
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
        dict: Option<&DecodeDictionary>,
    ) -> Result<(), DecodeError> {
        let start = dst.len();
        let result = match dict {
            Some(dict) => self
                .decoder
                .decompress_with_dictionary_into(dict, frame, dst),
            None => self.decoder.decompress_into(frame, dst),
        };
        match result {
            Ok(range) if range.len() == size => Ok(()),
            _ => {
                dst.truncate(start);
                Err(bad_stream())
            }
        }
    }

    /// Bytes kept allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.decoder.retained_bytes()
    }

    /// Releases the buffers.
    pub(crate) fn release(&mut self) {
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
        decoder.decode(&frame, &mut out, None).unwrap();
        assert_eq!(out, input);
        let mut short = alloc::vec![0u8; input.len() - 1];
        assert_eq!(decoder.decode(&frame, &mut short, None), Err(bad_stream()));
        let mut long = alloc::vec![0u8; input.len() + 1];
        assert_eq!(decoder.decode(&frame, &mut long, None), Err(bad_stream()));
        let mut appended = b"head".to_vec();
        decoder
            .decode_append(&frame, &mut appended, input.len(), None)
            .unwrap();
        assert_eq!(&appended[4..], input.as_slice());
        assert_eq!(
            decoder.decode_append(&frame, &mut appended, 1, None),
            Err(bad_stream())
        );
        assert_eq!(appended.len(), 4 + input.len());
        assert!(decoder.retained_bytes() > 0);
        decoder.release();
        assert_eq!(decoder.retained_bytes(), 0);
    }

    /// Bytes that are neither constant nor random.
    #[cfg(not(miri))]
    fn mixed(len: usize) -> Vec<u8> {
        (0..len as u32)
            .map(|i| {
                let h = i.wrapping_mul(2_654_435_761) >> 20;
                if h.is_multiple_of(5) {
                    h as u8
                } else {
                    b"abcdefgh"[(h & 7) as usize]
                }
            })
            .collect()
    }

    #[cfg(not(miri))]
    #[test]
    fn reference_streams_decode_with_block_splits_and_raw_literals() {
        let mut decoder = ZstdDecoder::new(Backend::default());
        /// (width, elements, decompression level, local parameters)
        type Case = (usize, usize, i32, &'static [(i32, i32)]);
        let cases: [Case; 6] = [
            (1, 5000, 3, &[]),
            (2048, 3, 3, &[]),
            (1024, 8, 1, &[]),
            (4096, 5, 1, &[(100, 12), (105, 4)]),
            (1500, 1, 1, &[]),
            (3000, 2, 3, &[(1002, 2), (201, 1)]),
        ];
        for (width, n_elts, level, ints) in cases {
            let input = mixed(width * n_elts);
            let stream =
                openzl_ffi::enc_zstd::zstd_node(&input, width, 5, level, ints).expect("reference");
            let (decoded_width, frame) = split(&stream).unwrap();
            assert_eq!(decoded_width, width as u64);
            let size = content_size(frame).unwrap();
            assert_eq!(size, input.len() as u64);
            let mut out = alloc::vec![0u8; input.len()];
            decoder.decode(frame, &mut out, None).unwrap();
            assert_eq!(out, input, "width {width} x{n_elts}");
            let mut appended = Vec::new();
            decoder
                .decode_append(frame, &mut appended, input.len(), None)
                .unwrap();
            assert_eq!(appended, input);
        }
    }

    #[test]
    fn headers_without_a_readable_size_are_corrupt_streams() {
        // A frame header whose window exceeds 2^31 (window byte 0xff), and a
        // frame cut inside its header.
        assert_eq!(content_size(&[0x00, 0xff]), Err(bad_stream()));
        assert_eq!(content_size(&[0x60, 0x00]), Err(bad_stream()));
    }

    #[test]
    fn streams_split_into_width_and_frame() {
        assert_eq!(split(&[1, 9, 9]), Ok((1, &[9u8, 9][..])));
        assert_eq!(split(&[0, 9]), Err(bad_stream()));
        assert_eq!(split(&[]), Err(bad_stream()));
    }
}
