//! The Zstandard node (`EI_zstd`, encode_zstd_binding.c) and the Zstandard
//! encoder behind it.
//!
//! The reference resets a `ZSTD_CCtx`, selects the magicless frame format,
//! sets the compression level and the node's local parameters, then either
//! calls `ZSTD_compress2` or, for input of at most eight elements of at least
//! 1024 bytes, streams one flushed block per element. `mzstd` reproduces both
//! calls byte for byte; this module owns the reusable encoder and its output
//! buffer, and the node in `codecs::zstd_node` decides the configuration.
//! The codec's output stream is the element width as a varint followed by the
//! magicless frame, which for serial input is the single byte `0x01`.

use alloc::vec::Vec;

use crate::Backend;
use crate::shared::varint;

/// A reusable Zstandard encoder and its output buffer.
#[derive(Debug)]
pub(crate) struct ZstdNode {
    encoder: mzstd::Compressor,
    output: Vec<u8>,
    config: mzstd::EncoderConfig,
}

/// `kMaxNbElts` of `EI_zstd_shouldCutBlocks`.
const MAX_BLOCK_ELTS: usize = 8;
/// `kMinBlockSize` of `EI_zstd_shouldCutBlocks`.
const MIN_BLOCK_WIDTH: usize = 1024;

/// The `mzstd` backend running the same SIMD level as `backend`.
pub(crate) fn zstd_backend(backend: Backend) -> mzstd::Backend {
    mzstd::Backend::available()
        .into_iter()
        .find(|candidate| candidate.name() == backend.name())
        .unwrap_or_default()
}

/// The configuration of a node run before its local parameters: the
/// magicless format and `level`. Every OpenZL level is a valid Zstandard
/// level; see `Level`.
pub(crate) fn zstd_config(level: i32) -> mzstd::EncoderConfig {
    let level = mzstd::Level::try_from(level).unwrap_or_default();
    mzstd::EncoderConfig::default()
        .with_format(mzstd::FrameFormat::Magicless)
        .with_level(level)
}

/// `EI_zstd_shouldCutBlocks`: whether a stream of `n_elts` elements of
/// `width` bytes gets one block per element.
const fn should_cut_blocks(width: usize, n_elts: usize) -> bool {
    n_elts > 0 && width >= MIN_BLOCK_WIDTH && n_elts <= MAX_BLOCK_ELTS
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
            config: zstd_config(level),
        }
    }

    /// Switches to `level` for the next chunk.
    pub(crate) fn set_level(&mut self, level: i32) {
        // A level-only configuration always validates.
        let _ = self.set_config(zstd_config(level));
    }

    /// Switches to `config` for the next chunk; a no-op when it is already
    /// in use. Fails when `config` does not validate.
    pub(crate) fn set_config(&mut self, config: mzstd::EncoderConfig) -> Result<(), ()> {
        if config != self.config {
            self.encoder.reconfigure(config).map_err(|_| ())?;
            self.config = config;
        }
        Ok(())
    }

    /// Compresses serial `input` and returns the codec's output stream: the
    /// element width (1) followed by the magicless frame.
    #[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
    pub(crate) fn encode(&mut self, input: &[u8]) -> Option<&[u8]> {
        self.output.clear();
        // The element width of serial data, as a one-byte varint.
        self.output.push(1);
        self.encoder.compress_into(input, &mut self.output).ok()?;
        Some(&self.output)
    }

    /// Compresses `input`, `n_elts` elements of `width` bytes, and returns
    /// the codec's output stream (`EI_zstdWithCCtx`): `varint(width)` then
    /// the magicless frame. Input of at most eight elements of at least 1024
    /// bytes is streamed with one flushed block per element and a pledged
    /// size; anything else is one `ZSTD_compress2` call.
    #[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
    pub(crate) fn encode_elements(
        &mut self,
        input: &[u8],
        width: usize,
        n_elts: usize,
        dictionary: Option<&mzstd::dictionary::PreparedDictionary>,
    ) -> Option<&[u8]> {
        self.output.clear();
        varint::push(&mut self.output, width as u64);
        // `blockSize == srcSize` for a single element.
        if !should_cut_blocks(width, n_elts) || n_elts == 1 {
            // `ZSTD_CCtx_refCDict` + `ZSTD_compress2`; the dictionary is not
            // used when blocks are cut.
            match dictionary {
                Some(dictionary) => self
                    .encoder
                    .compress_with_dictionary_into(dictionary, input, &mut self.output)
                    .ok()?,
                None => self.encoder.compress_into(input, &mut self.output).ok()?,
            };
            return Some(&self.output);
        }
        let header = self.output.len();
        // The bound, and 3 bytes for each extra block header.
        let capacity = mzstd::Compressor::max_compressed_size(input.len())
            .ok()?
            .checked_add(n_elts * 3)?;
        self.output.resize(header.checked_add(capacity)?, 0);
        let mut session = self
            .encoder
            .start(mzstd::InputSize::Exact(input.len() as u64).into());
        let mut position = header;
        for (index, element) in input.chunks_exact(width).enumerate() {
            let operation = if index + 1 == n_elts {
                mzstd::Operation::End
            } else {
                mzstd::Operation::Flush
            };
            let progress = session
                .process(element, &mut self.output[position..], operation)
                .ok()?;
            if progress.consumed != element.len() {
                return None;
            }
            position += progress.produced;
            if operation == mzstd::Operation::End
                && progress.status != mzstd::EncoderStatus::Finished
            {
                return None;
            }
        }
        self.output.truncate(position);
        Some(&self.output)
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

    /// The standard-format frame `mzstd` writes for `input` at `level`,
    /// without its magic number.
    fn magicless_frame(input: &[u8], level: i32) -> Vec<u8> {
        let level = mzstd::Level::try_from(level).expect("level");
        let config = mzstd::EncoderConfig::default().with_level(level);
        let mut reference = mzstd::Compressor::new(config).expect("zstd config");
        reference.compress(input).expect("zstd compress")[4..].to_vec()
    }

    #[test]
    fn stream_is_the_width_then_the_magicless_frame() {
        let input = alloc::vec![7u8; 4096];
        let mut node = ZstdNode::new(6, Backend::default());
        let stream = node.encode(&input).expect("encode").to_vec();
        assert_eq!(stream[0], 1);
        assert_eq!(&stream[1..], &magicless_frame(&input, 6)[..]);
        // The element-aware entry point writes the same stream for serial data.
        assert_eq!(
            node.encode_elements(&input, 1, input.len(), None).unwrap(),
            &stream[..]
        );
    }

    #[test]
    fn a_single_wide_element_is_one_frame_after_its_width() {
        let input = alloc::vec![9u8; 3000];
        let mut node = ZstdNode::new(3, Backend::default());
        let stream = node
            .encode_elements(&input, 3000, 1, None)
            .unwrap()
            .to_vec();
        // varint(3000) takes two bytes.
        assert_eq!(&stream[..2], &[0xb8, 0x17]);
        assert_eq!(&stream[2..], &magicless_frame(&input, 3)[..]);
    }

    #[test]
    fn wide_elements_get_one_block_each() {
        let width = 2048;
        let input: Vec<u8> = (0..(width * 3) as u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let mut node = ZstdNode::new(3, Backend::default());
        let stream = node
            .encode_elements(&input, width, 3, None)
            .unwrap()
            .to_vec();
        let mut decoder = mzstd::Decompressor::builder(
            mzstd::DecoderConfig::default().with_format(mzstd::FrameFormat::Magicless),
        )
        .build();
        // The stream starts with the element width as a varint.
        let mut expected = alloc::vec::Vec::new();
        crate::shared::varint::push(&mut expected, width as u64);
        let len = expected.len();
        assert_eq!(&stream[..len], expected.as_slice());
        assert_eq!(decoder.decompress(&stream[len..]).unwrap(), input);
        // Flushing ends a block per element: at least three blocks, so the
        // stream differs from the one-call frame of the same data.
        assert_ne!(&stream[len..], &magicless_frame(&input, 3)[..]);
        // More than eight elements, or narrow ones, are not cut.
        let one_call = node
            .encode_elements(&input, 512, 12, None)
            .unwrap()
            .to_vec();
        assert_eq!(&one_call[2..], &magicless_frame(&input, 3)[..]);
    }

    #[test]
    fn empty_input_is_an_empty_frame() {
        let mut node = ZstdNode::new(3, Backend::default());
        let stream = node.encode_elements(&[], 4096, 0, None).unwrap().to_vec();
        assert_eq!(&stream[2..], &magicless_frame(&[], 3)[..]);
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
    fn configurations_that_do_not_validate_are_refused() {
        let mut node = ZstdNode::new(3, Backend::default());
        let params = mzstd::CompressionParams::default().with_window_log(5);
        assert!(node.set_config(zstd_config(3).with_params(params)).is_err());
        // The node keeps working with its previous configuration.
        assert!(node.encode(b"still working fine").is_some());
    }

    /// Input of `len` bytes that is neither constant nor random.
    #[cfg(not(miri))]
    fn mixed(len: usize, seed: u32) -> Vec<u8> {
        (0..len as u32)
            .map(|i| {
                let h = (i ^ seed).wrapping_mul(2_654_435_761) >> 20;
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
    fn elements_match_the_reference_binding_at_every_level() {
        let shapes = [
            (1, 5000),
            (1024, 1),
            (1024, 2),
            (2048, 3),
            (1500, 8),
            (1024, 9),
            (4096, 8),
            (512, 12),
            (4096, 0),
            (70_000, 3),
        ];
        for level in [-5, -1, 0, 1, 3, 6, 9, 12, 19, 22] {
            let mut node = ZstdNode::new(level, Backend::default());
            for (width, n_elts) in shapes {
                let input = mixed(width * n_elts, (width + n_elts) as u32);
                let expected = openzl_ffi::enc_zstd::zstd_node(&input, width, level, 3, &[]);
                let actual = node
                    .encode_elements(&input, width, n_elts, None)
                    .map(<[u8]>::to_vec);
                assert_eq!(actual, expected, "level {level} width {width} x{n_elts}");
            }
        }
    }

    #[cfg(not(miri))]
    #[test]
    fn literal_mode_and_parameters_match_the_reference_binding() {
        use mzstd::CParameter;
        let input = mixed(3 * 2048, 7);
        let mut node = ZstdNode::new(3, Backend::default());
        for ints in [
            &[][..],
            &[(100, 7), (101, 15), (107, 4)][..],
            &[(1002, 2)][..],
            &[(201, 1), (202, 1), (160, 1)][..],
        ] {
            let mut config = zstd_config(3);
            for &(id, value) in ints {
                config = config.with_parameter(CParameter::try_from((id, value)).unwrap());
            }
            node.set_config(config).unwrap();
            let expected = openzl_ffi::enc_zstd::zstd_node(&input, 2048, 3, 3, ints);
            let actual = node
                .encode_elements(&input, 2048, 3, None)
                .map(<[u8]>::to_vec);
            assert_eq!(actual, expected, "{ints:?}");
        }
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
