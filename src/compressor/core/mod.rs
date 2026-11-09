//! The OpenZL encoder: frame headers, chunks and the session state machine.
//!
//! [`Engine`] owns the reusable Zstandard node and the scratch the chunk
//! header is assembled in, and the SIMD level selected once when the
//! compressor was built. Every checksum below it runs at that level.

pub(crate) mod chunk;
pub(crate) mod frame;
pub(crate) mod sink;
pub(crate) mod stream;
#[cfg(feature = "zstd")]
pub(crate) mod zstd;

use alloc::vec::Vec;

use fearless_simd::Level;

use super::config::EncoderConfig;
use crate::{Backend, RetentionPolicy};
use sink::{Full, Sink};
#[cfg(feature = "zstd")]
use zstd::ZstdNode;

/// The resolved global parameters of a frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Params {
    /// The wire-format version.
    pub(crate) version: u32,
    /// The compression level handed to the Zstandard node.
    pub(crate) level: i32,
    /// Chunks carry content checksums.
    pub(crate) content_checksum: bool,
    /// Chunks carry compressed checksums, the header a check byte.
    pub(crate) compressed_checksum: bool,
    /// The most input bytes one chunk takes.
    pub(crate) chunk_limit: usize,
}

impl From<&EncoderConfig> for Params {
    fn from(config: &EncoderConfig) -> Self {
        Self {
            version: config.format_version().get(),
            level: config.level().get(),
            content_checksum: config.content_checksum(),
            compressed_checksum: config.compressed_checksum(),
            chunk_limit: config.chunking().limit(),
        }
    }
}

/// Why the core could not encode.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum CoreError {
    /// The destination is full.
    OutputFull,
    /// The Zstandard backend failed, which it never should.
    #[cfg(feature = "zstd")]
    Internal,
}

impl From<Full> for CoreError {
    fn from(_: Full) -> Self {
        Self::OutputFull
    }
}

/// Reusable encoding state.
#[derive(Debug)]
pub(crate) struct Engine {
    #[cfg(feature = "zstd")]
    zstd: ZstdNode,
    header: Vec<u8>,
    simd: Level,
}

impl Engine {
    pub(crate) fn new(params: &Params, backend: Backend) -> Self {
        #[cfg(not(feature = "zstd"))]
        let _ = params;
        Self {
            #[cfg(feature = "zstd")]
            zstd: ZstdNode::new(params.level, backend),
            header: Vec::new(),
            simd: backend.0,
        }
    }

    /// Adopts new parameters for the next frame.
    pub(crate) fn reconfigure(&mut self, params: &Params) {
        #[cfg(feature = "zstd")]
        self.zstd.set_level(params.level);
        #[cfg(not(feature = "zstd"))]
        let _ = params;
    }

    /// Writes a frame header announcing `total` input bytes.
    pub(crate) fn header(
        &self,
        params: &Params,
        total: u64,
        sink: &mut impl Sink,
    ) -> Result<(), CoreError> {
        Ok(frame::write_header(params, self.simd, total, sink)?)
    }

    /// Compresses one chunk.
    pub(crate) fn chunk(
        &mut self,
        params: &Params,
        input: &[u8],
        sink: &mut impl Sink,
    ) -> Result<(), CoreError> {
        chunk::write_chunk(self, params, input, sink)
    }

    /// Compresses a whole frame of `input`, chunked as the segmenter would.
    pub(crate) fn frame(
        &mut self,
        params: &Params,
        input: &[u8],
        sink: &mut impl Sink,
    ) -> Result<(), CoreError> {
        self.header(params, input.len() as u64, sink)?;
        if input.is_empty() {
            self.chunk(params, input, sink)?;
        }
        for piece in input.chunks(params.chunk_limit) {
            self.chunk(params, piece, sink)?;
        }
        Ok(frame::write_end(sink)?)
    }

    /// Bytes kept allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        #[cfg(feature = "zstd")]
        let codecs = self.zstd.retained_bytes();
        #[cfg(not(feature = "zstd"))]
        let codecs = 0;
        codecs + self.header.capacity()
    }

    /// Releases what `policy` does not allow to keep.
    pub(crate) fn trim(&mut self, policy: RetentionPolicy) {
        #[cfg(feature = "zstd")]
        self.zstd.trim(policy);
        if !policy.allows(self.retained_bytes()) {
            self.header = Vec::new();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sink::{SliceSink, VecSink};
    use super::*;

    #[cfg(feature = "zstd")]
    #[test]
    fn whole_frames_split_at_the_chunk_limit() {
        let mut params = Params::from(&EncoderConfig::default());
        params.chunk_limit = 32 << 10;
        let input = alloc::vec![3u8; (64 << 10) + 1];
        let mut engine = Engine::new(&params, Backend::default());
        let mut out = Vec::new();
        engine
            .frame(&params, &input, &mut VecSink::new(&mut out))
            .unwrap();
        let expected = openzl_ffi::compress(
            &input,
            openzl_ffi::Params {
                chunk_size: 32 << 10,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out, expected);
    }

    #[test]
    fn a_full_slice_reports_output_full() {
        let params = Params::from(&EncoderConfig::default());
        let mut engine = Engine::new(&params, Backend::default());
        let mut buffer = [0u8; 12];
        let error = engine.frame(
            &params,
            b"0123456789abcdef",
            &mut SliceSink::new(&mut buffer),
        );
        assert_eq!(error, Err(CoreError::OutputFull));
        assert_eq!(CoreError::from(Full), CoreError::OutputFull);
    }

    #[test]
    fn trimming_releases_scratch() {
        let params = Params::from(&EncoderConfig::default());
        let mut engine = Engine::new(&params, Backend::default());
        let mut out = Vec::new();
        engine
            .frame(&params, &[1u8; 5000], &mut VecSink::new(&mut out))
            .unwrap();
        assert!(engine.retained_bytes() > 0);
        engine.trim(RetentionPolicy::ReleaseAll);
        assert_eq!(engine.header.capacity(), 0);
    }
}
