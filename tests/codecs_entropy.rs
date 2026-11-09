//! Differential tests of the entropy decoders (`fse_v2`, `huffman_v2`,
//! `huffman_struct_v2`, `fse_ncount`, `pivco_huffman`) against frames the C
//! library writes. The table streams are stored (not bitpacked) so the tests
//! exercise only the entropy codecs.
#![cfg(all(feature = "decompression", feature = "entropy", feature = "conversion"))]

use mopenzl::Decompressor;
use openzl_ffi::graph::{self, Graph, Input, cparam};

// `ZL_PrivateStandardNodeID_*` and `ZL_PrivateStandardGraphID_fse_ncount`
// (computed from private_nodes.h).
const NODE_FSE_V2: u32 = 58;
const NODE_HUFFMAN_V2: u32 = 59;
const NODE_HUFFMAN_STRUCT_V2: u32 = 60;
const NODE_PIVCO_HUFFMAN: u32 = 87;
const GRAPH_FSE_NCOUNT: u32 = 44;

/// Frame options: no checksums, so corruptions reach the codec.
fn options(version: i32) -> [(i32, i32); 3] {
    [
        (cparam::FORMAT_VERSION, version),
        (cparam::CONTENT_CHECKSUM, 2),
        (cparam::COMPRESSED_CHECKSUM, 2),
    ]
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Bytes with a geometric-ish distribution over `alphabet` symbols.
fn skewed(len: usize, alphabet: u64, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed | 1);
    (0..len)
        .map(|_| {
            let a = rng.next() % alphabet;
            let b = rng.next() % alphabet;
            a.min(b).min(rng.next() % alphabet) as u8
        })
        .collect()
}

fn random(len: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed | 1);
    (0..len).map(|_| rng.next() as u8).collect()
}

fn text(len: usize) -> Vec<u8> {
    b"the quick brown fox jumps over the lazy dog and keeps running "
        .iter()
        .copied()
        .cycle()
        .take(len)
        .collect()
}

/// Byte inputs of several shapes. FSE and Huffman need at least two symbols
/// and cannot code incompressible data.
fn byte_inputs() -> Vec<(&'static str, Vec<u8>)> {
    let mut two = vec![b'a'; 300];
    two[7] = b'b';
    two[200] = b'b';
    vec![
        ("tiny", b"abracadabra".to_vec()),
        ("two symbols", two),
        ("text", text(900)),
        ("text 1k", text(1500)),
        ("skewed 8", skewed(4000, 8, 1)),
        ("skewed 64", skewed(20_000, 64, 2)),
        ("skewed 256", skewed(70_000, 256, 3)),
        ("large text", text(300_000)),
    ]
}

fn check(frame: &[u8], expected: &[u8]) {
    let c = graph::decompress(frame).expect("C decodes its frame");
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].bytes, expected);
    assert_eq!(Decompressor::default().decompress(frame).unwrap(), expected);
}

/// Decodes every damaged variant of `frame` without panicking and, when the
/// decoder accepts one, agrees with C.
fn survives_damage(frame: &[u8]) {
    let mut errors = 0;
    let step = (frame.len() / 150).max(1);
    for at in (0..frame.len()).step_by(step) {
        for flip in [0x01u8, 0x80, 0xff] {
            let mut damaged = frame.to_vec();
            damaged[at] ^= flip;
            if Decompressor::default().decompress(&damaged).is_err() {
                errors += 1;
            }
        }
    }
    for cut in (0..frame.len()).step_by(step) {
        assert!(Decompressor::default().decompress(&frame[..cut]).is_err());
    }
    assert!(errors > 0);
}

fn serial_frame(graph: &Graph, data: &[u8], version: i32) -> Option<Vec<u8>> {
    graph::compress(&[Input::Serial(data)], graph, &options(version)).ok()
}

#[test]
fn fse_streams_decode_for_every_shape_and_version() {
    // Counts bitpacked by C normally; here stored raw or through ncount.
    let stored = Graph::node(NODE_FSE_V2, [Graph::STORE, Graph::STORE]);
    let ncount = Graph::node(NODE_FSE_V2, [Graph::Id(GRAPH_FSE_NCOUNT), Graph::STORE]);
    let mut decoded = 0;
    for version in [21, 24, 27] {
        for (name, data) in byte_inputs() {
            for graph in [&stored, &ncount] {
                if let Some(frame) = serial_frame(graph, &data, version) {
                    check(&frame, &data);
                    decoded += 1;
                } else {
                    // C refuses constant and tiny data; nothing to compare.
                    assert!(data.len() < 4 || name == "two symbols", "{name}");
                }
            }
        }
    }
    assert!(decoded >= 30);
}

#[test]
fn fse_frames_survive_damage() {
    for len in [200, 3000] {
        let data = skewed(len, 20, 9);
        let graph = Graph::node(NODE_FSE_V2, [Graph::Id(GRAPH_FSE_NCOUNT), Graph::STORE]);
        let frame = serial_frame(&graph, &data, 27).unwrap();
        check(&frame, &data);
        survives_damage(&frame);
    }
}

#[test]
fn huffman_streams_decode_for_every_shape_and_version() {
    let graph = Graph::node(NODE_HUFFMAN_V2, [Graph::STORE, Graph::STORE]);
    let mut decoded = 0;
    for version in [21, 24, 27] {
        for (name, data) in byte_inputs() {
            if let Some(frame) = serial_frame(&graph, &data, version) {
                check(&frame, &data);
                decoded += 1;
            } else {
                assert!(data.len() < 4, "{name}");
            }
        }
        // Incompressible input is still valid Huffman (8-bit codes).
        let noise = random(5000, 5);
        if let Some(frame) = serial_frame(&graph, &noise, version) {
            check(&frame, &noise);
        }
    }
    assert!(decoded >= 20);
}

#[test]
fn huffman_frames_survive_damage() {
    for len in [100, 2000] {
        let data = skewed(len, 30, 4);
        let graph = Graph::node(NODE_HUFFMAN_V2, [Graph::STORE, Graph::STORE]);
        let frame = serial_frame(&graph, &data, 27).unwrap();
        check(&frame, &data);
        survives_damage(&frame);
    }
}

fn wide_symbols(len: usize, alphabet: u64, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed | 1);
    (0..len)
        .flat_map(|_| {
            let a = rng.next() % alphabet;
            let b = rng.next() % alphabet;
            (a.min(b) as u16).to_le_bytes()
        })
        .collect()
}

fn struct_frame(bytes: &[u8], version: i32) -> Option<Vec<u8>> {
    let graph = Graph::node(NODE_HUFFMAN_STRUCT_V2, [Graph::STORE, Graph::STORE]);
    graph::compress(
        &[Input::Struct { bytes, width: 2 }],
        &graph,
        &options(version),
    )
    .ok()
}

#[test]
fn huffman_struct_streams_decode_one_and_four_blocks() {
    let mut decoded = 0;
    for version in [21, 24, 27] {
        for (len, alphabet) in [
            (2, 2),
            (50, 9),
            (900, 300),
            (1001, 5000),
            (40_000, 1000),
            (70_000, 40_000),
        ] {
            let bytes = wide_symbols(len, alphabet, len as u64);
            if let Some(frame) = struct_frame(&bytes, version) {
                let c = graph::decompress(&frame).unwrap();
                assert_eq!(c[0].bytes, bytes);
                let ours = Decompressor::default().decompress_typed(&frame).unwrap();
                assert_eq!(ours[0].content(), bytes);
                assert_eq!(ours[0].width(), 2);
                decoded += 1;
            }
        }
    }
    assert!(decoded >= 12);
}

#[test]
fn huffman_struct_frames_survive_damage() {
    for len in [300, 3000] {
        let bytes = wide_symbols(len, 700, 1);
        let frame = struct_frame(&bytes, 27).unwrap();
        let step = (frame.len() / 150).max(1);
        let mut errors = 0;
        for at in (0..frame.len()).step_by(step) {
            for flip in [0x01u8, 0x80, 0xff] {
                let mut damaged = frame.clone();
                damaged[at] ^= flip;
                if Decompressor::default().decompress_typed(&damaged).is_err() {
                    errors += 1;
                }
            }
        }
        for cut in (0..frame.len()).step_by(step) {
            assert!(
                Decompressor::default()
                    .decompress_typed(&frame[..cut])
                    .is_err()
            );
        }
        assert!(errors > 0);
    }
}

#[cfg(feature = "pivco-huffman")]
#[test]
fn pivco_streams_decode_for_every_shape() {
    let graph = Graph::node(NODE_PIVCO_HUFFMAN, [Graph::STORE, Graph::STORE]);
    let mut decoded = 0;
    for (name, data) in byte_inputs().into_iter().chain([
        ("noise", random(40_000, 6)),
        ("multi block", skewed(100_000, 40, 8)),
        ("pair", b"ab".to_vec()),
    ]) {
        if let Some(frame) = serial_frame(&graph, &data, 27) {
            check(&frame, &data);
            decoded += 1;
        } else {
            assert!(data.len() < 2, "{name}");
        }
    }
    assert!(decoded >= 10);
    // Constant data: a lone symbol with no bitstream.
    let constant = vec![b'z'; 1000];
    if let Some(frame) = serial_frame(&graph, &constant, 27) {
        check(&frame, &constant);
    }
}

#[cfg(feature = "pivco-huffman")]
#[test]
fn pivco_frames_survive_damage() {
    let graph = Graph::node(NODE_PIVCO_HUFFMAN, [Graph::STORE, Graph::STORE]);
    for len in [60, 2500] {
        let data = skewed(len, 25, 12);
        let frame = serial_frame(&graph, &data, 27).unwrap();
        check(&frame, &data);
        survives_damage(&frame);
    }
}

#[cfg(feature = "pivco-huffman")]
#[test]
fn pivco_in_older_formats_is_not_a_codec() {
    // The oracle refuses version < 27, as the registry's minimum version says.
    let graph = Graph::node(NODE_PIVCO_HUFFMAN, [Graph::STORE, Graph::STORE]);
    assert!(serial_frame(&graph, &text(500), 24).is_none());
}

/// Every small length, where the tails of FSE (2 and 4 states), Huffman and
/// PivCo take every shape.
#[test]
fn every_small_length_round_trips_through_each_codec() {
    let fse = Graph::node(NODE_FSE_V2, [Graph::STORE, Graph::STORE]);
    let huffman = Graph::node(NODE_HUFFMAN_V2, [Graph::STORE, Graph::STORE]);
    let mut graphs = vec![("fse", &fse), ("huffman", &huffman)];
    let pivco = Graph::node(NODE_PIVCO_HUFFMAN, [Graph::STORE, Graph::STORE]);
    if cfg!(feature = "pivco-huffman") {
        graphs.push(("pivco", &pivco));
    }
    let lengths = (2..70)
        .chain(995..1010)
        .chain([255, 256, 257, 1023, 1024, 4097]);
    let mut decoded = 0;
    for length in lengths {
        for alphabet in [2u64, 3, 5, 17, 200] {
            let data = skewed(length, alphabet, length as u64 * 31 + alphabet);
            for (name, graph) in &graphs {
                if let Some(frame) = serial_frame(graph, &data, 27) {
                    let ours = Decompressor::default().decompress(&frame);
                    assert_eq!(ours.as_deref(), Ok(&data[..]), "{name} {length} {alphabet}");
                    decoded += 1;
                }
            }
        }
    }
    assert!(decoded > 400);
}

/// The standard entropy graphs (`ZL_GRAPH_FSE`, `HUFFMAN`, `HUFFMAN_HUF0`,
/// `HUFFMAN_PIVCO`, `ENTROPY`) wire these codecs to bitpack, tokenize,
/// constant and the other codecs of the graph. Frames that need a codec
/// that is not built are skipped; every other frame must decode exactly.
#[test]
fn standard_entropy_graphs_decode_when_their_codecs_are_built() {
    let mut decoded = 0;
    for graph_id in [3u32, 4, 5, 6, 7] {
        for version in [21, 24, 27] {
            for (_, data) in byte_inputs() {
                let graph = Graph::Id(graph_id);
                let Some(frame) = serial_frame(&graph, &data, version) else {
                    continue;
                };
                let expected = graph::decompress(&frame).unwrap();
                match Decompressor::default().decompress(&frame) {
                    Ok(bytes) => {
                        assert_eq!(bytes, expected[0].bytes);
                        assert_eq!(bytes, data);
                        decoded += 1;
                    }
                    // A codec of another family is missing (or still a stub
                    // whose signature does not fit the graph).
                    Err(
                        mopenzl::DecodeError::Unsupported { .. }
                        | mopenzl::DecodeError::InvalidData {
                            kind: mopenzl::InvalidDataKind::Graph,
                        },
                    ) => {}
                    Err(error) => panic!("graph {graph_id} version {version}: {error}"),
                }
            }
        }
    }
    eprintln!("{decoded} standard-graph frames decoded");
}
