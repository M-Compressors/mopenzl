//! Error contracts of the public API on hand-made frames.
#![cfg(all(feature = "compression", feature = "decompression"))]

use mopenzl::{
    Compressor, DecodeError, DecodeLimits, DecoderConfig, Decompressor, InvalidDataKind,
    Unsupported,
};

/// A frame of `chunk` (header and payload) at v27 without checksums.
fn frame(size: u64, chunk: &[u8]) -> Vec<u8> {
    let mut out = vec![0xdb, 0xa5, 0xb1, 0xd7, 0x00, 0x01];
    let mut value = size + 1;
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
    out.extend_from_slice(chunk);
    out.push(0);
    out
}

#[test]
fn hand_made_stored_frames_decode() {
    let src = frame(5, &[0x01, 0x01, 0x05, b'h', b'e', b'l', b'l', b'o']);
    assert_eq!(Decompressor::default().decompress(&src).unwrap(), b"hello");
    assert_eq!(openzl_ffi::decompress(&src).unwrap(), b"hello");
}

#[test]
fn frames_with_other_codecs_are_unsupported() {
    // One FSE (49) node over a stored stream.
    let src = frame(
        3,
        &[0x02, 0x01, 0x00, 0x31, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00],
    );
    assert_eq!(
        Decompressor::default().decompress(&src),
        Err(DecodeError::Unsupported {
            feature: Unsupported::Codec { id: 49 }
        })
    );
}

#[test]
fn size_mismatches_and_bad_graphs_are_invalid() {
    let src = frame(4, &[0x01, 0x01, 0x05, b'h', b'e', b'l', b'l', b'o']);
    assert!(matches!(
        Decompressor::default().decompress(&src),
        Err(DecodeError::ContentSizeMismatch { declared: 4, .. })
    ));
    let src = frame(6, &[0x01, 0x01, 0x05, b'h', b'e', b'l', b'l', b'o']);
    assert_eq!(
        Decompressor::default().decompress(&src),
        Err(DecodeError::ContentSizeMismatch {
            declared: 6,
            decoded: 5
        })
    );
    let src = frame(0, &[0x01, 0x00]);
    assert_eq!(
        Decompressor::default().decompress(&src),
        Err(DecodeError::InvalidData {
            kind: InvalidDataKind::Graph
        })
    );
}

#[test]
fn corrupted_reference_frames_are_rejected() {
    let input: Vec<u8> = (0..20_000u32).map(|i| (i % 97) as u8).collect();
    let frame = Compressor::default().compress(&input).unwrap();
    let mut decoder = Decompressor::default();
    for position in (0..frame.len()).step_by(3) {
        let mut damaged = frame.clone();
        damaged[position] ^= 0x10;
        assert!(decoder.decompress(&damaged).is_err(), "flip at {position}");
    }
    // Without checksum checks, payload damage surfaces as stream errors or
    // differing content, never as a panic.
    let mut lenient = Decompressor::new(
        DecoderConfig::default()
            .with_content_checksum_check(false)
            .with_compressed_checksum_check(false),
    );
    for position in 0..frame.len() {
        let mut damaged = frame.clone();
        damaged[position] = damaged[position].wrapping_add(1);
        let _ = lenient.decompress(&damaged);
    }
}

#[test]
fn frames_the_reference_rejects_are_rejected() {
    // Found by the `decompress` fuzz target: an empty frame without its
    // chunk, the same with a content checksum flag, and unused type bits.
    for hex in ["dba5b1d700010100", "dba5b1d7010181800000"] {
        let src = decode_hex(hex);
        assert_eq!(
            Decompressor::default().decompress(&src),
            Err(DecodeError::InvalidData {
                kind: InvalidDataKind::EmptyFrame
            }),
            "{hex}"
        );
        assert!(openzl_ffi::decompress(&src).is_err());
        let mut session = Decompressor::default().into_session();
        let mut out = [0u8; 4];
        assert!(
            session
                .process(&src, &mut out, mopenzl::DecodeOperation::Finish)
                .is_err()
        );
    }
    let src = decode_hex("dba5b1d700410101010000");
    assert_eq!(
        Decompressor::default().decompress(&src),
        Err(DecodeError::InvalidData {
            kind: InvalidDataKind::FrameHeader
        })
    );
    assert!(openzl_ffi::decompress(&src).is_err());
}

fn decode_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn output_limits_reject_large_declarations_before_decoding() {
    let src = frame(1 << 40, &[]);
    let mut decoder = Decompressor::new(
        DecodeLimits::default()
            .with_max_output_bytes(Some(1 << 20))
            .into(),
    );
    assert_eq!(
        decoder.decompress(&src),
        Err(DecodeError::OutputLimitExceeded { limit: 1 << 20 })
    );
}

#[cfg(not(feature = "zstd"))]
#[test]
fn without_the_zstd_feature_frames_are_stored_and_zstd_chunks_unsupported() {
    let input = vec![b'z'; 10_000];
    let frame = Compressor::default().compress(&input).unwrap();
    assert!(frame.len() > input.len(), "every chunk is stored");
    assert_eq!(openzl_ffi::decompress(&frame).unwrap(), input);
    assert_eq!(Decompressor::default().decompress(&frame).unwrap(), input);
    let reference = openzl_ffi::compress(&input, Default::default()).unwrap();
    assert_eq!(
        Decompressor::default().decompress(&reference),
        Err(DecodeError::Unsupported {
            feature: Unsupported::Codec { id: 22 }
        })
    );
}
