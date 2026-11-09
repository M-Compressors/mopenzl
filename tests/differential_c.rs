//! Byte identity with, and compatibility against, the reference C library.
#![cfg(all(feature = "compression", feature = "decompression", feature = "zstd"))]

mod support;

use mopenzl::{ChunkSize, Chunking, Compressor, Decompressor, EncoderConfig, FormatVersion, Level};
use openzl_ffi::{Params, Ternary};

fn ternary(enabled: bool) -> Ternary {
    if enabled {
        Ternary::Enable
    } else {
        Ternary::Disable
    }
}

fn reference(input: &[u8], config: &EncoderConfig) -> Vec<u8> {
    let chunk_size = match config.chunking() {
        Chunking::Whole => 0,
        Chunking::Fixed(size) => usize::from(size),
    };
    openzl_ffi::compress(
        input,
        Params {
            level: config.level().get(),
            format_version: u32::from(config.format_version()),
            content_checksum: ternary(config.content_checksum()),
            compressed_checksum: ternary(config.compressed_checksum()),
            chunk_size,
        },
    )
    .expect("reference compression")
}

fn check(config: EncoderConfig) {
    let mut encoder = Compressor::new(config).expect("config");
    let mut decoder = Decompressor::default();
    for (name, input) in support::corpus() {
        let expected = reference(&input, &config);
        let frame = encoder.compress(&input).expect("compress");
        assert_eq!(frame, expected, "{name} with {config:?}");
        assert_eq!(
            decoder.decompress(&expected).expect("decode reference"),
            input,
            "{name}"
        );
        assert_eq!(
            openzl_ffi::decompress(&frame).expect("C decodes ours"),
            input,
            "{name}"
        );
    }
}

#[test]
fn default_configuration_is_byte_identical() {
    check(EncoderConfig::default());
}

#[test]
fn every_format_version_is_byte_identical() {
    for version in 21..=27 {
        let version = FormatVersion::try_from(version).unwrap();
        check(EncoderConfig::default().with_format_version(version));
    }
}

#[test]
fn levels_are_byte_identical() {
    let levels: &[i32] = if cfg!(miri) {
        &[1]
    } else {
        &[-50, -1, 1, 3, 5, 9, 12, 16, 19]
    };
    for &level in levels {
        check(EncoderConfig::from(Level::try_from(level).unwrap()));
    }
}

#[test]
fn checksum_switches_are_byte_identical() {
    for (content, compressed) in [(false, false), (true, false), (false, true)] {
        check(
            EncoderConfig::default()
                .with_content_checksum(content)
                .with_compressed_checksum(compressed),
        );
    }
}

#[test]
fn segmented_frames_are_byte_identical() {
    for size in [ChunkSize::MIN, ChunkSize::try_from(100_000usize).unwrap()] {
        check(EncoderConfig::default().with_chunking(Chunking::Fixed(size)));
        check(
            EncoderConfig::default()
                .with_chunking(Chunking::Fixed(size))
                .with_format_version(FormatVersion::MIN)
                .with_level(Level::try_from(1).unwrap()),
        );
    }
}

#[test]
fn strongest_levels_are_byte_identical_on_text() {
    let input = support::text(if cfg!(miri) { 2000 } else { 200_000 }, 9);
    for level in [20, 22] {
        let config = EncoderConfig::from(Level::try_from(level).unwrap());
        let frame = Compressor::new(config).unwrap().compress(&input).unwrap();
        assert_eq!(frame, reference(&input, &config), "level {level}");
    }
}
