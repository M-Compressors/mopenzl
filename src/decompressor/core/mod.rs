//! The OpenZL decoder: frame headers, chunk graphs and the session state
//! machine.
//!
//! [`Engine`] owns the reusable chunk layout, the Zstandard decoder and the
//! buffers of intermediate streams, plus the SIMD level selected once when
//! the decompressor was built; every checksum below it runs at that level.

pub(crate) mod chunk;
pub(crate) mod frame;
pub(crate) mod out;
pub(crate) mod reader;
pub(crate) mod stream;
#[cfg(feature = "zstd")]
pub(crate) mod zstd;

use alloc::vec::Vec;

use fearless_simd::Level;

use super::config::{DecoderConfig, FrameMode};
use super::error::{DecodeError, InvalidDataKind};
use crate::shared::wire::END_OF_FRAME;
use crate::shared::xxh3;
use crate::{Backend, RetentionPolicy};
#[cfg(feature = "zstd")]
use chunk::Codec;
use chunk::{Layout, Source};
use frame::FrameInfo;
use out::Out;
use reader::Parse;
#[cfg(feature = "zstd")]
use zstd::ZstdDecoder;

#[cfg(feature = "zstd")]
/// Bytes an intermediate stream may exceed the output it leads to by:
/// a Zstandard frame never grows its content by more than a 1/128 share
/// and a block header per block, so the default graph stays far below.
const INTERMEDIATE_SLACK: u64 = 64 << 10;

/// What the next unit of a frame is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Unit {
    /// A chunk of this many bytes.
    Chunk(usize),
    /// The end-of-frame marker.
    End,
}

/// Reusable decoding state.
#[derive(Debug)]
pub(crate) struct Engine {
    layout: Layout,
    #[cfg(feature = "zstd")]
    zstd: ZstdDecoder,
    /// Intermediate streams of the current chunk, by stream index.
    buffers: Vec<Vec<u8>>,
    simd: Level,
}

/// `ZL_readCE32` at `at`.
fn read_le32(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(word)
}

impl Engine {
    pub(crate) fn new(backend: Backend) -> Self {
        Self {
            layout: Layout::default(),
            #[cfg(feature = "zstd")]
            zstd: ZstdDecoder::new(backend),
            buffers: Vec::new(),
            simd: backend.0,
        }
    }

    /// The SIMD level checksums run at.
    pub(crate) const fn simd(&self) -> Level {
        self.simd
    }

    /// What the unit at the front of `bytes` is, from its header alone.
    pub(crate) fn measure(&mut self, bytes: &[u8], frame: &FrameInfo) -> Result<Unit, Parse> {
        match bytes.first() {
            None => Err(Parse::Truncated),
            Some(&END_OF_FRAME) => Ok(Unit::End),
            Some(_) => {
                self.layout.parse(bytes, frame)?;
                Ok(Unit::Chunk(self.layout.len))
            }
        }
    }

    /// Decodes the whole chunk `chunk` into `out` and returns the bytes it
    /// regenerated, at most `remaining`.
    #[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
    pub(crate) fn decode_chunk(
        &mut self,
        chunk: &[u8],
        frame: &FrameInfo,
        config: &DecoderConfig,
        remaining: u64,
        out: &mut impl Out,
    ) -> Result<u64, DecodeError> {
        match self.layout.parse(chunk, frame) {
            Ok(()) if self.layout.len == chunk.len() => {}
            Ok(()) | Err(Parse::Truncated) => {
                return Err(DecodeError::invalid(InvalidDataKind::ChunkHeader));
            }
            Err(Parse::Failed(error)) => return Err(error),
        }
        if let Some(at) = self.layout.compressed_checksum
            && config.compressed_checksum_check()
        {
            let expected = read_le32(chunk, at);
            let computed = xxh3::hash32(self.simd, &chunk[..at]);
            if expected != computed {
                return Err(DecodeError::CompressedChecksumMismatch { expected, computed });
            }
        }
        let start = out.len();
        let last = self.layout.streams.len() - 1;
        if self.buffers.len() < self.layout.streams.len() {
            self.buffers
                .resize_with(self.layout.streams.len(), Vec::new);
        }
        // Without codecs every chunk with a node is rejected while parsing.
        #[cfg(feature = "zstd")]
        for index in 0..self.layout.nodes.len() {
            match self.layout.nodes[index].codec {
                #[cfg(feature = "zstd")]
                Codec::Zstd => self.run_zstd(index, chunk, frame, remaining, out)?,
            }
        }
        if let Source::Stored(range) = &self.layout.streams[last] {
            let stored = &chunk[range.clone()];
            if stored.len() as u64 > remaining {
                return Err(DecodeError::ContentSizeMismatch {
                    declared: frame.size,
                    decoded: frame.size - remaining + stored.len() as u64,
                });
            }
            out.put(stored)
                .map_err(|_| DecodeError::OutputTooSmall { written: out.len() })?;
        }
        if let Some(at) = self.layout.content_checksum
            && config.content_checksum_check()
        {
            let expected = read_le32(chunk, at);
            let computed = xxh3::hash32(self.simd, out.since(start));
            if expected != computed {
                return Err(DecodeError::ContentChecksumMismatch { expected, computed });
            }
        }
        Ok((out.len() - start) as u64)
    }

    /// Runs Zstandard node `index` of the current layout over `chunk`,
    /// writing the frame's output to `out` and intermediate streams to
    /// `buffers`.
    #[cfg(feature = "zstd")]
    fn run_zstd(
        &mut self,
        index: usize,
        chunk: &[u8],
        frame: &FrameInfo,
        remaining: u64,
        out: &mut impl Out,
    ) -> Result<(), DecodeError> {
        let node = &self.layout.nodes[index];
        let last = self.layout.streams.len() - 1;
        let (input, output) = (node.inputs.start, node.output);
        let mut target = core::mem::take(&mut self.buffers[output]);
        let stream: &[u8] = match &self.layout.streams[input] {
            Source::Stored(range) => &chunk[range.clone()],
            Source::Node(_) => &self.buffers[input],
        };
        let (width, frame_bytes) = zstd::split(stream)?;
        if width != 1 {
            // Only serial streams feed the codecs this decoder runs.
            return Err(DecodeError::invalid(InvalidDataKind::Graph));
        }
        let size = zstd::content_size(frame_bytes)?;
        if output == last {
            if size > remaining {
                // The codec would regenerate at least one byte too many.
                return Err(DecodeError::ContentSizeMismatch {
                    declared: frame.size,
                    decoded: frame.size.saturating_add(1),
                });
            }
            out.zstd(&mut self.zstd, frame_bytes, size as usize)?;
        } else {
            let bound = remaining
                .saturating_add(remaining / 128)
                .saturating_add(INTERMEDIATE_SLACK);
            if size > bound {
                return Err(DecodeError::invalid(InvalidDataKind::CodecStream));
            }
            target.clear();
            target.resize(size as usize, 0);
            self.zstd.decode(frame_bytes, &mut target)?;
        }
        self.buffers[output] = target;
        Ok(())
    }

    /// Decodes the frames at the front of `src` into `out`, as `config`
    /// says, and returns the bytes consumed.
    pub(crate) fn decode_frames(
        &mut self,
        config: &DecoderConfig,
        src: &[u8],
        out: &mut impl Out,
    ) -> Result<usize, DecodeError> {
        let limits = config.limits();
        let truncated = |error: Parse| match error {
            Parse::Truncated => DecodeError::UnexpectedEndOfInput,
            Parse::Failed(error) => error,
        };
        let mut pos = 0usize;
        let mut total_out = 0u64;
        loop {
            let info = frame::parse(&src[pos..], self.simd, config.compressed_checksum_check())
                .map_err(truncated)?;
            if let Some(limit) = limits.max_output_bytes()
                && total_out.saturating_add(info.size) > limit
            {
                return Err(DecodeError::OutputLimitExceeded { limit });
            }
            pos += info.header_len;
            let mut decoded = 0u64;
            let mut chunks = 0u64;
            loop {
                if let Some(limit) = limits.max_input_bytes()
                    && pos as u64 > limit
                {
                    return Err(DecodeError::InputLimitExceeded { limit });
                }
                match self.measure(&src[pos..], &info).map_err(truncated)? {
                    Unit::End if chunks == 0 => {
                        return Err(DecodeError::invalid(InvalidDataKind::EmptyFrame));
                    }
                    Unit::End => {
                        pos += 1;
                        break;
                    }
                    Unit::Chunk(len) => {
                        let chunk = src
                            .get(pos..pos.saturating_add(len))
                            .ok_or(DecodeError::UnexpectedEndOfInput)?;
                        decoded +=
                            self.decode_chunk(chunk, &info, config, info.size - decoded, out)?;
                        pos += len;
                        chunks += 1;
                    }
                }
            }
            if let Some(limit) = limits.max_input_bytes()
                && pos as u64 > limit
            {
                return Err(DecodeError::InputLimitExceeded { limit });
            }
            if decoded != info.size {
                return Err(DecodeError::ContentSizeMismatch {
                    declared: info.size,
                    decoded,
                });
            }
            total_out += decoded;
            if config.frame_mode() == FrameMode::Single || pos == src.len() {
                return Ok(pos);
            }
        }
    }

    /// Bytes kept allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        #[cfg(feature = "zstd")]
        let codecs = self.zstd.retained_bytes();
        #[cfg(not(feature = "zstd"))]
        let codecs = 0;
        self.layout.retained_bytes()
            + codecs
            + self.buffers.iter().map(Vec::capacity).sum::<usize>()
            + self.buffers.capacity() * core::mem::size_of::<Vec<u8>>()
    }

    /// Releases what `policy` does not allow to keep.
    pub(crate) fn trim(&mut self, policy: RetentionPolicy) {
        if !policy.allows(self.retained_bytes()) {
            self.layout.release();
            #[cfg(feature = "zstd")]
            self.zstd.release();
            self.buffers = Vec::new();
        }
    }
}

// Every case here decodes reference frames, which use Zstandard.
#[cfg(all(test, feature = "zstd"))]
mod tests {
    use super::out::{SliceOut, VecOut};
    use super::*;

    fn engine() -> Engine {
        Engine::new(Backend::default())
    }

    fn reference(input: &[u8]) -> Vec<u8> {
        openzl_ffi::compress(input, openzl_ffi::Params::default()).unwrap()
    }

    fn decode(engine: &mut Engine, src: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut out = Vec::new();
        engine.decode_frames(&DecoderConfig::default(), src, &mut VecOut::new(&mut out))?;
        Ok(out)
    }

    #[test]
    fn reference_frames_decode() {
        let mut engine = engine();
        let text: Vec<u8> = (0..50_000u32)
            .map(|i| b"openzl"[i as usize % 6] ^ (i / 997) as u8)
            .collect();
        for input in [&b""[..], b"hello", b"0123456789", &text] {
            assert_eq!(decode(&mut engine, &reference(input)).unwrap(), input);
        }
    }

    #[test]
    fn corrupted_chunks_fail_their_checksums() {
        let mut engine = engine();
        let input = alloc::vec![9u8; 4000];
        let frame = reference(&input);
        let mut damaged = frame.clone();
        damaged[20] ^= 1;
        assert!(matches!(
            decode(&mut engine, &damaged),
            Err(DecodeError::CompressedChecksumMismatch { .. })
        ));
        // Same damage with the compressed checksum ignored.
        let config = DecoderConfig::default().with_compressed_checksum_check(false);
        let mut out = Vec::new();
        assert!(
            engine
                .decode_frames(&config, &damaged, &mut VecOut::new(&mut out))
                .is_err()
        );
    }

    #[test]
    fn truncation_limits_and_small_slices_are_reported() {
        let mut engine = engine();
        let frame = reference(&alloc::vec![1u8; 3000]);
        for cut in [0, 5, frame.len() - 1] {
            assert_eq!(
                decode(&mut engine, &frame[..cut]),
                Err(DecodeError::UnexpectedEndOfInput)
            );
        }
        let stored = reference(b"stored payload");
        let mut slice = [0u8; 3];
        assert!(matches!(
            engine.decode_frames(
                &DecoderConfig::default(),
                &stored,
                &mut SliceOut::new(&mut slice)
            ),
            Err(DecodeError::OutputTooSmall { .. })
        ));
        let mut slice = [0u8; 100];
        assert!(matches!(
            engine.decode_frames(
                &DecoderConfig::default(),
                &frame,
                &mut SliceOut::new(&mut slice)
            ),
            Err(DecodeError::OutputTooSmall { .. })
        ));
        let limited = DecoderConfig::from(
            super::super::config::DecodeLimits::DEFAULT.with_max_output_bytes(Some(10)),
        );
        let mut out = Vec::new();
        assert_eq!(
            engine.decode_frames(&limited, &frame, &mut VecOut::new(&mut out)),
            Err(DecodeError::OutputLimitExceeded { limit: 10 })
        );
        let limited = DecoderConfig::from(
            super::super::config::DecodeLimits::DEFAULT.with_max_input_bytes(Some(10)),
        );
        assert_eq!(
            engine.decode_frames(&limited, &frame, &mut VecOut::new(&mut out)),
            Err(DecodeError::InputLimitExceeded { limit: 10 })
        );
    }

    #[test]
    fn trimming_releases_buffers() {
        let mut engine = engine();
        decode(&mut engine, &reference(&alloc::vec![3u8; 5000])).unwrap();
        assert!(engine.retained_bytes() > 0);
        engine.trim(RetentionPolicy::ReleaseAll);
        assert!(engine.buffers.is_empty());
        assert_eq!(Backend(engine.simd()), Backend::default());
    }
}
