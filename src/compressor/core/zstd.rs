//! The Zstandard node of the default graph (`EI_zstd`).
//!
//! The reference resets a `ZSTD_CCtx`, selects the magicless frame format and
//! the global compression level, and calls `ZSTD_compress2` with a capacity
//! of at least the compress bound. `mzstd` writes exactly the bytes of that
//! call with the magic number; the magicless frame is the same frame without
//! its first four bytes. The codec's output stream is that frame preceded by
//! the element width as a varint, which for serial input is the single byte
//! `0x01`, so the stream is assembled in place over the magic's last byte.

use alloc::vec::Vec;

use crate::Backend;

/// A reusable Zstandard encoder and its output buffer.
#[derive(Debug)]
pub(crate) struct ZstdNode {
    encoder: mzstd::Compressor,
    output: Vec<u8>,
}

/// Bytes of the Zstandard magic number the magicless format omits.
const MAGIC_LEN: usize = 4;

/// The `mzstd` backend running the same SIMD level as `backend`.
pub(crate) fn zstd_backend(backend: Backend) -> mzstd::Backend {
    mzstd::Backend::available()
        .into_iter()
        .find(|candidate| candidate.name() == backend.name())
        .unwrap_or_default()
}

/// The `mzstd` configuration of `ZSTD_compress2` at `level`.
fn zstd_config(level: i32) -> mzstd::EncoderConfig {
    // Every OpenZL level is a valid Zstandard level; see `Level`.
    let level = mzstd::Level::try_from(level).unwrap_or_default();
    mzstd::EncoderConfig::default().with_level(level)
}

impl ZstdNode {
    pub(crate) fn new(level: i32, backend: Backend) -> Self {
        let encoder = mzstd::Compressor::builder(zstd_config(level))
            .with_backend(zstd_backend(backend))
            .build()
            .unwrap_or_default();
        Self {
            encoder,
            output: Vec::new(),
        }
    }

    /// Switches to `level` for the next chunk.
    pub(crate) fn set_level(&mut self, level: i32) {
        // A level-only configuration always validates.
        let _ = self.encoder.reconfigure(zstd_config(level));
    }

    /// Compresses `input` and returns the codec's output stream: the element
    /// width followed by the magicless frame.
    #[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
    pub(crate) fn encode(&mut self, input: &[u8]) -> Option<&[u8]> {
        self.output.clear();
        self.encoder.compress_into(input, &mut self.output).ok()?;
        let stream = self.output.get_mut(MAGIC_LEN - 1..)?;
        // The element width of serial data, as a one-byte varint.
        stream[0] = 1;
        Some(stream)
    }

    /// Bytes the node keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.output.capacity() + self.encoder.retained_bytes()
    }

    /// Releases what `policy` does not allow to keep.
    pub(crate) fn trim(&mut self, policy: crate::RetentionPolicy) {
        if !policy.allows(self.retained_bytes()) {
            self.output = Vec::new();
            self.encoder.trim(mzstd::RetentionPolicy::ReleaseAll);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_is_the_width_then_the_magicless_frame() {
        let input = alloc::vec![7u8; 4096];
        let mut node = ZstdNode::new(6, Backend::default());
        let mut reference = mzstd::Compressor::new(zstd_config(6)).expect("zstd config");
        let frame = reference.compress(&input).expect("zstd compress");
        let stream = node.encode(&input).expect("encode").to_vec();
        assert_eq!(stream[0], 1);
        assert_eq!(&stream[1..], &frame[MAGIC_LEN..]);
    }

    #[test]
    fn level_changes_apply_to_the_next_chunk() {
        let input: alloc::vec::Vec<u8> = (0..20_000u32)
            .map(|i| b"abcdefgh"[(i.wrapping_mul(2_654_435_761) >> 29) as usize])
            .collect();
        let mut node = ZstdNode::new(1, Backend::default());
        let fast = node.encode(&input).unwrap().to_vec();
        node.set_level(19);
        let strong = node.encode(&input).unwrap().to_vec();
        assert_ne!(fast, strong);
        assert!(node.retained_bytes() > 0);
        node.trim(crate::RetentionPolicy::ReleaseAll);
        assert_eq!(node.output.capacity(), 0);
    }

    #[test]
    fn backends_map_to_the_same_named_zstd_backend() {
        for backend in Backend::available() {
            let mapped = zstd_backend(backend);
            if backend != Backend::SCALAR {
                assert_eq!(mapped.name(), backend.name());
            }
        }
    }
}
