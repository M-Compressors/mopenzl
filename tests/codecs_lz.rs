//! The LZ family (`lz4`, `field_lz`, `lz`) against the C library.
#![cfg(all(
    feature = "decompression",
    feature = "lz4",
    feature = "field-lz",
    feature = "lz"
))]

use mopenzl::{DataType, Decompressor};
use openzl_ffi::graph::{self, Graph, Input, cparam};

/// `ZL_StandardNodeID_field_lz` and `ZL_StandardNodeID_lz`.
const NODE_FIELD_LZ: u32 = 9;
const NODE_LZ: u32 = 52;
/// `ZL_StandardGraphID_lz4`.
const GRAPH_LZ4: u32 = 22;

const NO_CHECKSUMS: [(i32, i32); 2] = [
    (cparam::CONTENT_CHECKSUM, 2),
    (cparam::COMPRESSED_CHECKSUM, 2),
];

fn params(version: i32) -> Vec<(i32, i32)> {
    let mut params = NO_CHECKSUMS.to_vec();
    params.push((cparam::FORMAT_VERSION, version));
    params
}

/// Deterministic pseudo-random bytes.
fn random(len: usize, mut seed: u64) -> Vec<u8> {
    (0..len)
        .map(|_| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 56) as u8
        })
        .collect()
}

/// Text-like, highly repetitive bytes.
fn text(len: usize) -> Vec<u8> {
    let words = [
        "alpha ",
        "beta ",
        "gamma ",
        "delta ",
        "lorem ipsum ",
        "dolor ",
    ];
    let mut out = Vec::new();
    let mut i = 0usize;
    while out.len() < len {
        out.extend_from_slice(words[(i * 7 + i / 3) % words.len()].as_bytes());
        i += 1;
    }
    out.truncate(len);
    out
}

fn sizes() -> Vec<usize> {
    let large = if cfg!(miri) { 4_000 } else { 300_000 };
    vec![0, 1, 3, 13, 64, 1000, large]
}

/// Checks that mopenzl and C decode `frame` to `expected` as one output.
fn agree(frame: &[u8], expected: &[u8], data_type: DataType, width: usize) {
    let theirs = graph::decompress(frame).expect("C decodes its frame");
    assert_eq!(theirs.len(), 1);
    assert_eq!(theirs[0].bytes, expected);
    let ours = Decompressor::default()
        .decompress_typed(frame)
        .expect("mopenzl decodes the frame");
    assert_eq!(ours.len(), 1);
    assert_eq!(ours[0].data_type(), data_type);
    assert_eq!(ours[0].content(), expected);
    if !ours[0].is_empty() {
        assert_eq!(ours[0].width(), width);
    }
}

/// Decoding the damaged frame must fail or succeed, never panic.
fn damage_never_panics(frame: &[u8]) {
    let mut decompressor = Decompressor::default();
    for at in 0..frame.len() {
        for flip in [0x01, 0x80, 0xff] {
            let mut damaged = frame.to_vec();
            damaged[at] ^= flip;
            let _ = decompressor.decompress_typed(&damaged);
        }
        let _ = decompressor.decompress_typed(&frame[..at]);
    }
}

fn lz4_frame(data: &[u8], version: i32) -> Vec<u8> {
    graph::compress(
        &[Input::Serial(data)],
        &Graph::Id(GRAPH_LZ4),
        &params(version),
    )
    .unwrap()
}

fn lz_frame(data: &[u8], version: i32) -> Vec<u8> {
    graph::compress(
        &[Input::Serial(data)],
        &Graph::node(NODE_LZ, [Graph::STORE; 4]),
        &params(version),
    )
    .unwrap()
}

fn field_frame(data: &[u8], width: usize, version: i32) -> Vec<u8> {
    graph::compress(
        &[Input::Struct { bytes: data, width }],
        &Graph::node(NODE_FIELD_LZ, [Graph::STORE; 5]),
        &params(version),
    )
    .unwrap()
}

#[test]
fn lz4_frames_decode_like_c() {
    for version in [23, 24, 27] {
        for size in sizes() {
            for data in [text(size), random(size, 5), vec![7u8; size]] {
                let frame = lz4_frame(&data, version);
                agree(&frame, &data, DataType::Serial, 1);
            }
        }
    }
}

#[test]
fn lz_frames_decode_like_c() {
    for version in [24, 27] {
        for size in sizes() {
            for data in [text(size), random(size, 9), vec![0u8; size]] {
                let frame = lz_frame(&data, version);
                agree(&frame, &data, DataType::Serial, 1);
            }
        }
    }
}

#[test]
fn field_lz_frames_decode_like_c() {
    for version in [21, 24, 27] {
        for width in [2usize, 4, 8] {
            for count in [0usize, 1, 2, 7, 100, if cfg!(miri) { 500 } else { 20_000 }] {
                let bytes = count * width;
                for data in [random(bytes, 3), text(bytes), vec![1u8; bytes]] {
                    let frame = field_frame(&data, width, version);
                    agree(&frame, &data, DataType::Struct, width);
                }
            }
        }
    }
}

#[test]
fn damaged_frames_never_panic() {
    let data = text(400);
    damage_never_panics(&lz4_frame(&data, 27));
    damage_never_panics(&lz_frame(&data, 27));
    damage_never_panics(&field_frame(&data[..400], 4, 27));
    damage_never_panics(&field_frame(&data[..400], 8, 24));
}

#[test]
fn damaged_frames_agree_with_c_on_acceptance() {
    // Whatever mopenzl accepts after damage, C accepts with the same bytes
    // and what C rejects mopenzl rejects; mopenzl may reject more, only for
    // offset 0 (which no encoder writes).
    let data = text(300);
    let frames = [
        lz4_frame(&data, 27),
        lz_frame(&data, 27),
        field_frame(&data, 2, 27),
    ];
    for frame in frames {
        for at in 0..frame.len() {
            let mut damaged = frame.clone();
            damaged[at] ^= 0x41;
            let theirs = graph::decompress(&damaged);
            let ours = Decompressor::default().decompress_typed(&damaged);
            match (theirs, ours) {
                (Ok(theirs), Ok(ours)) => {
                    assert_eq!(theirs.len(), ours.len());
                    for (theirs, ours) in theirs.iter().zip(&ours) {
                        assert_eq!(ours.content(), theirs.bytes);
                    }
                }
                (Err(_), Err(_)) => {}
                // The reference copies unwritten bytes for offset 0, which
                // mopenzl rejects.
                (Ok(_), Err(_)) => {}
                (theirs, ours) => {
                    panic!(
                        "frame {:02x?} byte {at}: C {:?}, mopenzl {:?}",
                        damaged,
                        theirs.map(|o| o[0].bytes.len()),
                        ours.map(|_| ())
                    )
                }
            }
        }
    }
}
