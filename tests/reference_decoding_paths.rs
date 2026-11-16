//! Frames the C decoder takes through paths the codecs alone do not
//! describe: frames without chunks, Zstandard streams of several frames, and
//! `ZL_AppendToOutputOptimization`, which replaces a codec regenerating a
//! chunk output. Each was a `decompress` or `decompress_typed` fuzz finding.
#![cfg(feature = "decompression")]

use mopenzl::{DataType, DecodeError, DecodeOperation, Decompressor, InvalidDataKind, TypedOutput};

fn decode_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

/// A v27 frame without checksums of one serial output of `size` bytes,
/// holding `chunk` (header and payload).
#[cfg(any(feature = "zstd", all(feature = "conversion", feature = "split")))]
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

/// The C shim's code for `data_type`.
const fn c_kind(data_type: DataType) -> u32 {
    match data_type {
        DataType::Serial => 0,
        DataType::Struct => 1,
        DataType::Numeric => 2,
        DataType::String => 3,
    }
}

/// Decodes `src` with both libraries' typed decoders and checks they agree.
fn typed_agree(src: &[u8]) -> Vec<TypedOutput> {
    let theirs = openzl_ffi::graph::decompress(src).expect("C decodes the frame");
    let ours = Decompressor::default()
        .decompress_typed(src)
        .expect("mopenzl decodes the frame");
    assert_eq!(ours.len(), theirs.len());
    for (ours, theirs) in ours.iter().zip(&theirs) {
        assert_eq!(ours.content(), theirs.bytes);
        assert_eq!(ours.string_lens(), theirs.lens);
        if theirs.kind == 0 && theirs.width == 0 {
            // A struct or numeric output no chunk typed stays
            // `ZL_Type_unassigned` in C, which the shim reports as code 0.
            assert!(matches!(
                ours.data_type(),
                DataType::Struct | DataType::Numeric
            ));
            assert_eq!(ours.width(), 0);
        } else {
            assert_eq!(c_kind(ours.data_type()), theirs.kind);
        }
    }
    ours
}

/// Decodes `src` with both libraries' byte decoders and checks they agree.
#[cfg(any(feature = "zstd", feature = "conversion"))]
fn bytes_agree(src: &[u8]) -> Vec<u8> {
    let theirs = openzl_ffi::decompress(src).expect("C decodes the frame");
    let ours = Decompressor::default()
        .decompress(src)
        .expect("mopenzl decodes the frame");
    assert_eq!(ours, theirs);
    ours
}

#[test]
fn typed_frames_without_chunks_decode_like_c() {
    // The fuzz finding (unused flag bits, a comment), then a commented frame
    // of each output type and one of two outputs.
    let cases = [
        (
            "dba5b1d774010103c4100c00".to_owned(),
            vec![DataType::Serial],
        ),
        ("dba5b1d7040101016100".to_owned(), vec![DataType::Serial]),
        ("dba5b1d7041101016100".to_owned(), vec![DataType::Struct]),
        ("dba5b1d7042101016100".to_owned(), vec![DataType::Numeric]),
        ("dba5b1d704310100016100".to_owned(), vec![DataType::String]),
        (
            "dba5b1d704020101016100".to_owned(),
            vec![DataType::Serial, DataType::Serial],
        ),
    ];
    for (hex, types) in cases {
        let outputs = typed_agree(&decode_hex(&hex));
        let decoded: Vec<_> = outputs.iter().map(TypedOutput::data_type).collect();
        assert_eq!(decoded, types, "{hex}");
        assert!(outputs.iter().all(TypedOutput::is_empty), "{hex}");
    }
}

#[test]
fn short_typed_frames_without_chunks_are_rejected_like_c() {
    // `DFH_decodeNbOutputs` wants five bytes from the inputs token on.
    for hex in ["dba5b1d700010100", "dba5b1d70002010100"] {
        let src = decode_hex(hex);
        assert_eq!(
            Decompressor::default().decompress_typed(&src),
            Err(DecodeError::InvalidData {
                kind: InvalidDataKind::EmptyFrame
            }),
            "{hex}"
        );
        assert!(openzl_ffi::graph::decompress(&src).is_err(), "{hex}");
    }
}

#[test]
fn byte_decoders_reject_frames_without_chunks_like_c() {
    // `ZL_DCtx_decompress` never sees its output typed: `outputNotReserved`.
    let src = decode_hex("dba5b1d7040101016100");
    assert_eq!(
        Decompressor::default().decompress(&src),
        Err(DecodeError::InvalidData {
            kind: InvalidDataKind::EmptyFrame
        })
    );
    let mut session = Decompressor::default().into_session();
    let mut out = [0u8; 4];
    assert!(
        session
            .process(&src, &mut out, DecodeOperation::Finish)
            .is_err()
    );
    assert!(openzl_ffi::decompress(&src).is_err());
}

#[test]
fn typed_frames_without_chunks_declare_no_content() {
    let src = decode_hex("dba5b1d7040106016100");
    assert_eq!(
        Decompressor::default().decompress_typed(&src),
        Err(DecodeError::ContentSizeMismatch {
            declared: 5,
            decoded: 0
        })
    );
    assert!(openzl_ffi::graph::decompress(&src).is_err());
}

#[cfg(feature = "zstd")]
mod zstd {
    use super::*;

    /// A frame of one Zstandard chunk whose stream is `frames`, magicless.
    fn zstd_frame(size: u64, frames: &[&[u8]]) -> Vec<u8> {
        let mut stream = vec![1u8];
        for content in frames {
            let mut encoder = mzstd::Compressor::default();
            stream.extend_from_slice(&encoder.compress(content).unwrap()[4..]);
        }
        let mut chunk = vec![0x02, 0x01, 0x00, 0x16, 0x00, 0x00, 0x00, 0x00];
        let mut value = stream.len();
        while value >= 0x80 {
            chunk.push(value as u8 | 0x80);
            value >>= 7;
        }
        chunk.push(value as u8);
        chunk.extend_from_slice(&stream);
        frame(size, &chunk)
    }

    #[test]
    fn every_frame_of_a_zstd_stream_is_decoded_like_c() {
        // `ZSTD_decompress_usingDDict` decodes frame after frame; the first
        // one's content size sizes the output.
        assert_eq!(bytes_agree(&zstd_frame(3, &[b"abc", b""])), b"abc");
        assert_eq!(bytes_agree(&zstd_frame(3, &[b"abc", b"", b""])), b"abc");
        let overflowing = zstd_frame(6, &[b"abc", b"def"]);
        assert!(Decompressor::default().decompress(&overflowing).is_err());
        assert!(openzl_ffi::decompress(&overflowing).is_err());
    }

    #[test]
    fn zstd_fuzz_finding_decodes_like_c() {
        // A one-block frame, then blocks of nothing as more frames.
        let src = decode_hex(concat!(
            "dba5b1d70001818004020100160000000050016000ff4d000008000100fc",
            "7f7427040000000000000000000000000000000000000000000000000000",
            "000000000000000000000000000000000000000000030000000000140000",
            "000000000001000000",
        ));
        assert_eq!(bytes_agree(&src).len(), 1 << 16);
    }
}

#[cfg(feature = "conversion")]
#[test]
fn convert_serial_to_struct_appends_a_serial_input_like_c() {
    // The fuzz finding: `convert_serial_to_struct` regenerating the output
    // from a stored (serial) stream, which its signature types as struct.
    let src = decode_hex("dba5b1d70001810002010485000000860000");
    assert!(bytes_agree(&src).is_empty());
    assert!(typed_agree(&src)[0].is_empty());
}

#[cfg(all(feature = "conversion", feature = "split"))]
mod splitn {
    use super::*;

    /// `splitn` (40) regenerating the output from the stream `id`
    /// regenerates from the stored `abcdef` with the one-byte header
    /// `header`.
    fn frame_of(id: u8, header: u8) -> Vec<u8> {
        // Two codecs, one stored stream; IDs `id` and 40 in seven bits each.
        let ids = u16::from(id) | 40 << 7;
        let [low, high] = ids.to_le_bytes();
        let chunk = [
            0x03, 0x01, 0x00, low, high, // codecs, stored, custom lane, IDs
            0x01, 0x00, // `id` has a one-byte header
            0x02, 0x00, // `splitn` has one variable input
            0x00, 0x00, 0x06, // one regen each, distances 0, stored size
            header, b'a', b'b', b'c', b'd', b'e', b'f',
        ];
        frame(6, &chunk)
    }

    #[test]
    fn splitn_appends_a_struct_input_like_c() {
        // `convert_struct_to_serial` (6) makes structs of width 2.
        let src = frame_of(6, 0x02);
        assert_eq!(bytes_agree(&src), b"abcdef");
        assert_eq!(typed_agree(&src)[0].content(), b"abcdef");
    }

    #[test]
    fn splitn_refuses_a_numeric_input() {
        // `convert_num_to_serial_le` (10) makes numbers of width 2. The
        // reference allocates them (and refuses them) or references its
        // input (and takes them), as the input's address is aligned.
        let src = frame_of(10, 0x01);
        assert_eq!(
            Decompressor::default().decompress(&src),
            Err(DecodeError::InvalidData {
                kind: InvalidDataKind::Graph
            })
        );
    }
}
