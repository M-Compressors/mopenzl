//! `FrameInfo` against the reference's `ZL_FrameInfo_*` getters.
#![cfg(all(feature = "compression", feature = "decompression"))]

use mopenzl::graph::Comment;
use mopenzl::{
    Compressor, DataType, DecodeError, Decompressor, EncoderConfig, FormatVersion, FrameInfo,
    InvalidDataKind,
};
use openzl_ffi::frame_info as c_info;
use openzl_ffi::graph::{self, Graph, Input as CInput};

fn data_type(ty: usize) -> DataType {
    match ty {
        1 => DataType::Serial,
        2 => DataType::Struct,
        4 => DataType::Numeric,
        _ => DataType::String,
    }
}

/// Reads `frame` with both libraries and checks every getter agrees.
fn agree(frame: &[u8]) -> FrameInfo {
    let expected = c_info::frame_info(frame).expect("C reads its frame");
    let info = Decompressor::default()
        .frame_info(frame)
        .expect("mopenzl reads the frame");
    assert_eq!(FrameInfo::try_from(frame).as_ref(), Ok(&info));
    assert_eq!(
        usize::try_from(u32::from(info.version())),
        Ok(expected.version)
    );
    assert_eq!(info.outputs().len(), expected.outputs.len());
    for (ours, theirs) in info.outputs().iter().zip(&expected.outputs) {
        assert_eq!(ours.data_type(), data_type(theirs.ty));
        assert_eq!(ours.size(), theirs.size as u64);
        assert_eq!(ours.num_elts(), theirs.num_elts.map(|n| n as u64));
    }
    assert_eq!(info.comment().unwrap_or_default(), expected.comment);
    assert_eq!(
        info.bundle_id()
            .map(|id| <[u8; 32]>::try_from(id.as_ref()).unwrap()),
        expected.bundle_id
    );
    info
}

#[test]
fn serial_frames_report_the_same_header_at_every_version() {
    let data = b"frame info frame info frame info".repeat(20);
    for version in 21..=27u32 {
        let config =
            EncoderConfig::default().with_format_version(FormatVersion::try_from(version).unwrap());
        let frame = Compressor::builder(config)
            .build()
            .unwrap()
            .compress(&data)
            .unwrap();
        let info = agree(&frame);
        assert_eq!(u32::from(info.version()), version);
        assert_eq!(info.decompressed_size(), data.len() as u64);
        assert_eq!(info.comment(), None);
        assert!(info.header_len() > 4 && info.header_len() < frame.len());
    }
}

#[test]
fn comments_are_read_from_rust_and_c_frames() {
    let comment = b"produced by the frame info test";
    for version in 22..=27u32 {
        let config =
            EncoderConfig::default().with_format_version(FormatVersion::try_from(version).unwrap());
        let mut compressor = Compressor::builder(config).build().unwrap();
        compressor
            .set_comment(Some(Comment::try_from(&comment[..]).unwrap()))
            .unwrap();
        let frame = compressor.compress(b"payload").unwrap();
        assert_eq!(agree(&frame).comment(), Some(&comment[..]));

        let from_c = graph::compress_with_comment(
            &[CInput::Serial(b"payload")],
            &Graph::Id(13),
            &[(graph::cparam::FORMAT_VERSION, version as i32)],
            comment,
        )
        .unwrap();
        assert_eq!(agree(&from_c).comment(), Some(&comment[..]));
    }
}

#[test]
fn typed_outputs_report_types_sizes_and_string_counts() {
    let numbers: Vec<u8> = (0..300u32).flat_map(u32::to_le_bytes).collect();
    let records: Vec<u8> = (0..90u8).collect();
    let inputs = [
        CInput::Serial(b"serial bytes"),
        CInput::Struct {
            bytes: &records,
            width: 3,
        },
        CInput::Numeric {
            bytes: &numbers,
            width: 4,
        },
        CInput::String {
            bytes: b"onetwothree",
            lens: &[3, 3, 0, 5],
        },
    ];
    let frame = graph::compress(&inputs, &Graph::STORE, &[]).unwrap();
    let info = agree(&frame);
    let outputs = info.outputs();
    assert_eq!(outputs.len(), 4);
    assert_eq!(outputs[0].num_elts(), Some(12));
    assert_eq!(outputs[1].num_elts(), None);
    assert_eq!(outputs[2].size(), 1200);
    assert_eq!(outputs[3].num_elts(), Some(4));
    assert_eq!(outputs[3].size(), 11);
    assert_eq!(info.decompressed_size(), 12 + 90 + 1200 + 11);
}

#[test]
fn checksum_flags_follow_the_encoder_configuration() {
    for (content, compressed) in [(true, true), (false, true), (true, false), (false, false)] {
        let config = EncoderConfig::default()
            .with_content_checksum(content)
            .with_compressed_checksum(compressed);
        let frame = Compressor::builder(config)
            .build()
            .unwrap()
            .compress(b"flags")
            .unwrap();
        let info = agree(&frame);
        assert_eq!(info.content_checksum(), content);
        assert_eq!(info.compressed_checksum(), compressed);
    }
}

#[test]
fn only_the_header_is_read() {
    let frame = Compressor::default().compress(b"abcdef").unwrap();
    let header = FrameInfo::try_from(&frame[..]).unwrap();
    let mut damaged = frame.clone();
    // Damage a chunk byte past the header, and append another frame.
    let last = damaged.len() - 2;
    damaged[last] ^= 0xff;
    damaged.extend_from_slice(&frame);
    assert_eq!(FrameInfo::try_from(&damaged[..]).unwrap(), header);
    assert!(Decompressor::default().decompress(&damaged).is_err());
    // The header alone is enough.
    let prefix = &frame[..header.header_len()];
    assert_eq!(FrameInfo::try_from(prefix).unwrap(), header);
}

#[test]
fn bad_headers_are_reported() {
    let frame = Compressor::default().compress(b"abcdef").unwrap();
    let header_len = FrameInfo::try_from(&frame[..]).unwrap().header_len();
    for len in 0..header_len {
        assert_eq!(
            FrameInfo::try_from(&frame[..len]),
            Err(DecodeError::UnexpectedEndOfInput),
            "{len}"
        );
    }
    assert_eq!(
        FrameInfo::try_from(&b"\x28\xb5\x2f\xfd\0"[..]),
        Err(DecodeError::InvalidData {
            kind: InvalidDataKind::Magic
        })
    );
    // A flipped check byte fails the default read and passes when unchecked.
    let mut flipped = frame.clone();
    flipped[header_len - 1] ^= 1;
    assert_eq!(
        FrameInfo::try_from(&flipped[..]),
        Err(DecodeError::InvalidData {
            kind: InvalidDataKind::HeaderChecksum
        })
    );
    let lenient =
        Decompressor::new(mopenzl::DecoderConfig::default().with_compressed_checksum_check(false));
    assert!(lenient.frame_info(&flipped).is_ok());
}

#[cfg(feature = "zstd")]
#[test]
fn bundle_ids_are_reported() {
    use mopenzl::dict::{Dict, DictBundle};
    use mopenzl::graph::{GraphBuilder, NodeParams, StandardGraph, StandardNode};

    let bundle = DictBundle::try_from(vec![Dict::zstd(b"dictionary content", 3).unwrap()]).unwrap();
    let mut builder = GraphBuilder::new();
    let node = builder
        .parameterize_node_with(
            StandardNode::Zstd,
            NodeParams::default().with_dictionary(bundle.dicts()[0].id()),
        )
        .unwrap();
    builder.load_dictionaries(&bundle).unwrap();
    let start = builder
        .static_graph(node, &[StandardGraph::Store.into()])
        .unwrap();
    let graph = builder.build(start).unwrap();
    let frame = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()
        .unwrap()
        .compress(b"dictionary content, again")
        .unwrap();
    let info = agree(&frame);
    assert_eq!(info.bundle_id(), Some(bundle.id()));
}
