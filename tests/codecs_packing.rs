//! The packing family (`bitpack`, `bitunpack`, `flatpack`, `tokenize`,
//! `parse_int`, `partition`) against frames the C library writes.
// The shared helpers serve the whole family, so the file needs all of it.
#![cfg(all(
    feature = "decompression",
    feature = "conversion",
    feature = "bitpack",
    feature = "bitunpack",
    feature = "flatpack",
    feature = "tokenize",
    feature = "parse-int",
    feature = "partition"
))]

#[cfg(any(
    feature = "bitpack",
    feature = "bitunpack",
    feature = "flatpack",
    feature = "tokenize",
    feature = "parse-int",
    feature = "partition"
))]
use mopenzl::{DataType, Decompressor};
#[cfg(all(feature = "partition", feature = "conversion"))]
use openzl_ffi::graph::Params;
#[cfg(any(
    feature = "bitpack",
    feature = "bitunpack",
    feature = "flatpack",
    feature = "tokenize",
    feature = "parse-int",
    feature = "partition"
))]
use openzl_ffi::graph::{self, Graph, Input};

#[cfg(feature = "bitunpack")]
const NODE_BITUNPACK: u32 = 27;
#[cfg(feature = "parse-int")]
const NODE_PARSE_INT: u32 = 38;
#[cfg(feature = "tokenize")]
const NODE_TOKENIZE_STRUCT: u32 = 40;
#[cfg(feature = "tokenize")]
const NODE_TOKENIZE_NUMERIC: u32 = 41;
#[cfg(feature = "tokenize")]
const NODE_TOKENIZE_STRING: u32 = 42;
#[cfg(feature = "partition")]
const NODE_PARTITION: u32 = 48;
#[cfg(feature = "bitpack")]
const NODE_BITPACK_SERIAL: u32 = 63;
#[cfg(feature = "bitpack")]
const NODE_BITPACK_INT: u32 = 64;
#[cfg(feature = "flatpack")]
const NODE_FLATPACK: u32 = 65;

#[cfg(any(
    feature = "bitpack",
    feature = "bitunpack",
    feature = "flatpack",
    feature = "tokenize",
    feature = "parse-int",
    feature = "partition"
))]
fn data_type(kind: u32) -> DataType {
    match kind {
        0 => DataType::Serial,
        1 => DataType::Struct,
        2 => DataType::Numeric,
        _ => DataType::String,
    }
}

/// A deterministic xorshift stream.
#[cfg(any(
    feature = "bitpack",
    feature = "bitunpack",
    feature = "flatpack",
    feature = "tokenize",
    feature = "parse-int",
    feature = "partition"
))]
fn random(seed: u64, count: usize) -> Vec<u64> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..count)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        })
        .collect()
}

#[cfg(any(
    feature = "bitpack",
    feature = "bitunpack",
    feature = "flatpack",
    feature = "tokenize",
    feature = "parse-int",
    feature = "partition"
))]
fn numbers(values: &[u64], width: usize) -> Vec<u8> {
    values
        .iter()
        .flat_map(|v| v.to_le_bytes()[..width].to_vec())
        .collect()
}

#[cfg(any(
    feature = "bitpack",
    feature = "bitunpack",
    feature = "flatpack",
    feature = "tokenize",
    feature = "parse-int",
    feature = "partition"
))]
fn compress(inputs: &[Input<'_>], graph: &Graph, version: i32) -> Option<Vec<u8>> {
    graph::compress(
        inputs,
        graph,
        &[
            (graph::cparam::FORMAT_VERSION, version),
            (graph::cparam::CONTENT_CHECKSUM, 2),
            (graph::cparam::COMPRESSED_CHECKSUM, 2),
        ],
    )
    .ok()
}

/// Decodes `frame` with both libraries, checks they agree and returns the
/// outputs.
#[cfg(any(
    feature = "bitpack",
    feature = "bitunpack",
    feature = "flatpack",
    feature = "tokenize",
    feature = "parse-int",
    feature = "partition"
))]
fn agree(frame: &[u8]) -> Vec<mopenzl::TypedOutput> {
    let expected = graph::decompress(frame).expect("C decodes its frame");
    let outputs = Decompressor::default()
        .decompress_typed(frame)
        .expect("mopenzl decodes the frame");
    assert_eq!(outputs.len(), expected.len());
    for (ours, theirs) in outputs.iter().zip(&expected) {
        assert_eq!(ours.data_type(), data_type(theirs.kind));
        assert_eq!(ours.content(), theirs.bytes);
        assert_eq!(ours.string_lens(), theirs.lens);
        assert_eq!(ours.len(), theirs.elts);
        if !ours.is_empty() && ours.data_type() != DataType::String {
            assert_eq!(ours.width(), theirs.width);
        }
    }
    outputs
}

/// Corrupted frames must fail or decode, never panic.
#[cfg(any(
    feature = "bitpack",
    feature = "bitunpack",
    feature = "flatpack",
    feature = "tokenize",
    feature = "parse-int",
    feature = "partition"
))]
fn survives_damage(frame: &[u8]) {
    let mut decoder = Decompressor::default();
    for cut in (0..frame.len()).step_by(frame.len().div_ceil(40).max(1)) {
        let _ = decoder.decompress_typed(&frame[..cut]);
    }
    for at in 0..frame.len() {
        for flip in [0x01u8, 0x80, 0xff] {
            let mut damaged = frame.to_vec();
            damaged[at] ^= flip;
            let _ = decoder.decompress_typed(&damaged);
        }
    }
}

/// Compresses, decodes against C and the expected content, then damages.
/// Returns whether the encoder accepted the graph.
#[cfg(any(
    feature = "bitpack",
    feature = "bitunpack",
    feature = "flatpack",
    feature = "tokenize",
    feature = "parse-int",
    feature = "partition"
))]
fn round_trip(inputs: &[Input<'_>], graph: &Graph, version: i32, wanted: &[&[u8]]) -> bool {
    let Some(frame) = compress(inputs, graph, version) else {
        return false;
    };
    let outputs = agree(&frame);
    assert_eq!(outputs.len(), wanted.len());
    for (output, want) in outputs.iter().zip(wanted) {
        assert_eq!(output.content(), *want);
    }
    if frame.len() < 1500 {
        survives_damage(&frame);
    }
    true
}

#[cfg(any(
    feature = "bitpack",
    feature = "bitunpack",
    feature = "flatpack",
    feature = "tokenize",
    feature = "parse-int",
    feature = "partition"
))]
const VERSIONS: [i32; 3] = [21, 24, 27];

#[cfg(any(
    feature = "bitpack",
    feature = "bitunpack",
    feature = "flatpack",
    feature = "tokenize",
    feature = "parse-int",
    feature = "partition"
))]
fn store2() -> [Graph; 2] {
    [Graph::STORE, Graph::STORE]
}

#[cfg(feature = "bitpack")]
#[test]
fn bitpack_serial_matches_c() {
    let mut accepted = 0;
    for version in VERSIONS {
        for bits in [1u32, 2, 3, 5, 7, 8] {
            for count in [0usize, 1, 2, 7, 8, 9, 100, 5000] {
                let data: Vec<u8> = random(u64::from(bits), count)
                    .iter()
                    .map(|v| (v & ((1 << bits) - 1)) as u8)
                    .collect();
                let graph = Graph::node(NODE_BITPACK_SERIAL, [Graph::STORE]);
                accepted += usize::from(round_trip(
                    &[Input::Serial(&data)],
                    &graph,
                    version,
                    &[&data],
                ));
            }
        }
    }
    assert!(accepted > 0);
}

#[cfg(all(feature = "bitpack", feature = "conversion"))]
#[test]
fn bitpack_int_matches_c_for_every_width() {
    let mut accepted = 0;
    for version in VERSIONS {
        for width in [1usize, 2, 4, 8] {
            for bits in [1u32, 3, 8, 9, 15, 16, 17, 31, 32, 33, 56, 57, 63, 64] {
                if bits as usize > width * 8 {
                    continue;
                }
                for count in [0usize, 1, 5, 64, 1000] {
                    let mask = if bits == 64 {
                        u64::MAX
                    } else {
                        (1 << bits) - 1
                    };
                    let values: Vec<u64> = random(u64::from(bits) + 7, count)
                        .iter()
                        .map(|v| v & mask)
                        .collect();
                    let data = numbers(&values, width);
                    let graph = Graph::node(NODE_BITPACK_INT, [Graph::STORE]);
                    let input = Input::Numeric {
                        bytes: &data,
                        width,
                    };
                    accepted += usize::from(round_trip(&[input], &graph, version, &[&data]));
                }
            }
        }
    }
    assert!(accepted > 0);
}

#[cfg(feature = "bitunpack")]
#[test]
fn bitunpack_matches_c() {
    let mut accepted = 0;
    for version in VERSIONS {
        for bits in [
            1i32, 2, 3, 5, 7, 8, 9, 12, 16, 17, 24, 31, 32, 33, 48, 63, 64,
        ] {
            for len in [0usize, 1, 2, 3, 5, 8, 13, 100, 777] {
                // Random bytes: the last byte may carry bits that fill no
                // element, which the header must keep.
                let mut data: Vec<u8> = random(bits as u64 * 31 + len as u64, len.div_ceil(8) + 1)
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .take(len)
                    .collect();
                // Alternate between zero and non-zero leftover bits.
                let spare = len * 8 - (len * 8 / bits as usize) * bits as usize;
                if len > 0 && spare < 8 && len % 2 == 0 {
                    *data.last_mut().unwrap() &= 0xff >> spare;
                }
                let graph = Graph::node_with(NODE_BITUNPACK, [(1, bits)], [Graph::STORE]);
                accepted += usize::from(round_trip(
                    &[Input::Serial(&data)],
                    &graph,
                    version,
                    &[&data],
                ));
            }
        }
    }
    assert!(accepted > 50);
}

#[cfg(feature = "flatpack")]
#[test]
fn flatpack_matches_c() {
    let mut accepted = 0;
    for version in VERSIONS {
        for size in [
            1usize, 2, 3, 4, 5, 16, 17, 32, 33, 64, 65, 100, 128, 129, 200, 256,
        ] {
            for count in [0usize, 1, 2, 15, 16, 17, 100, 4096] {
                let alphabet: Vec<u8> = (0..size).map(|i| (i * 13 + 5) as u8).collect();
                let data: Vec<u8> = random(size as u64, count)
                    .iter()
                    .map(|v| alphabet[(*v % size as u64) as usize])
                    .collect();
                let graph = Graph::node(NODE_FLATPACK, store2());
                accepted += usize::from(round_trip(
                    &[Input::Serial(&data)],
                    &graph,
                    version,
                    &[&data],
                ));
            }
        }
    }
    assert!(accepted > 0);
}

#[cfg(all(feature = "tokenize", feature = "conversion"))]
#[test]
fn tokenize_struct_and_numeric_match_c() {
    let mut accepted = 0;
    for version in VERSIONS {
        for distinct in [1u64, 2, 7, 300, 70_000] {
            for count in [0usize, 1, 10, 3000] {
                let values: Vec<u64> = random(distinct, count)
                    .iter()
                    .map(|v| v % distinct)
                    .collect();
                for width in [1usize, 2, 4, 8] {
                    if width == 1 && distinct > 256 || width == 2 && distinct > 65_536 {
                        continue;
                    }
                    let data = numbers(&values, width);
                    let numeric = Graph::node(NODE_TOKENIZE_NUMERIC, store2());
                    let input = Input::Numeric {
                        bytes: &data,
                        width,
                    };
                    accepted += usize::from(round_trip(&[input], &numeric, version, &[&data]));
                }
                let records = numbers(&values, 3);
                let structs = Graph::node(NODE_TOKENIZE_STRUCT, store2());
                let input = Input::Struct {
                    bytes: &records,
                    width: 3,
                };
                accepted += usize::from(round_trip(&[input], &structs, version, &[&records]));
            }
        }
    }
    assert!(accepted > 0);
}

#[cfg(all(feature = "tokenize", feature = "conversion"))]
#[test]
fn tokenize_string_matches_c() {
    let mut accepted = 0;
    for version in [24, 27] {
        for distinct in [1u64, 3, 40, 500] {
            for count in [0usize, 1, 5, 2000] {
                let words: Vec<String> = random(distinct, count)
                    .iter()
                    .map(|v| "x".repeat((v % distinct % 9) as usize) + &(v % distinct).to_string())
                    .collect();
                let bytes: Vec<u8> = words.iter().flat_map(|w| w.bytes()).collect();
                let lens: Vec<u32> = words.iter().map(|w| w.len() as u32).collect();
                let graph = Graph::node(NODE_TOKENIZE_STRING, store2());
                let input = Input::String {
                    bytes: &bytes,
                    lens: &lens,
                };
                if let Some(frame) = compress(&[input], &graph, version) {
                    let outputs = agree(&frame);
                    assert_eq!(outputs[0].content(), bytes);
                    assert_eq!(outputs[0].string_lens(), lens);
                    if frame.len() < 800 {
                        survives_damage(&frame);
                    }
                    accepted += 1;
                }
            }
        }
    }
    assert!(accepted > 0);
}

#[cfg(all(feature = "parse-int", feature = "conversion"))]
#[test]
fn parse_int_matches_c() {
    let mut accepted = 0;
    for version in [21, 24, 27] {
        let sets: Vec<Vec<i64>> = vec![
            vec![],
            vec![0],
            vec![-1, 1, 10, -10, 99, 100],
            random(3, 1000).iter().map(|v| *v as i64).collect(),
            random(4, 1000)
                .iter()
                .map(|v| (*v % 100_000) as i64 - 50_000)
                .collect(),
            vec![i64::MAX, i64::MIN, i64::MIN + 1, 0, -0],
        ];
        for values in sets {
            let words: Vec<String> = values.iter().map(i64::to_string).collect();
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.bytes()).collect();
            let lens: Vec<u32> = words.iter().map(|w| w.len() as u32).collect();
            let graph = Graph::node(NODE_PARSE_INT, [Graph::STORE]);
            let input = Input::String {
                bytes: &bytes,
                lens: &lens,
            };
            if let Some(frame) = compress(&[input], &graph, version) {
                let outputs = agree(&frame);
                assert_eq!(outputs[0].content(), bytes);
                assert_eq!(outputs[0].string_lens(), lens);
                if frame.len() < 800 {
                    survives_damage(&frame);
                }
                accepted += 1;
            }
        }
    }
    assert!(accepted > 0);
}

#[cfg(all(feature = "partition", feature = "conversion"))]
#[test]
fn partition_matches_c() {
    let custom = |start: u64, sizes: &[u64]| {
        let mut bytes = start.to_le_bytes().to_vec();
        for size in sizes {
            bytes.extend(size.to_le_bytes());
        }
        Graph::Node {
            node: NODE_PARTITION,
            params: Params {
                ints: vec![],
                copies: vec![(121, bytes)],
            },
            successors: store2().to_vec(),
        }
    };
    let preset = |id: i32| Graph::Node {
        node: NODE_PARTITION,
        params: Params {
            ints: vec![(120, id)],
            copies: vec![],
        },
        successors: store2().to_vec(),
    };
    let mut accepted = 0;
    for version in [24, 27] {
        for width in [1usize, 2, 4, 8] {
            for count in [0usize, 1, 9, 2000] {
                let limit: u64 = if width == 1 { 256 } else { 1 << 20 };
                let values: Vec<u64> = random(width as u64, count)
                    .iter()
                    .map(|v| (v >> (v % 7)) % limit + 1)
                    .collect();
                let data = numbers(&values, width);
                let input = Input::Numeric {
                    bytes: &data,
                    width,
                };
                let wide: [u64; 4] = [64, 64, 128, 1 << 20];
                let graphs = [
                    preset(0),
                    preset(1),
                    preset(2),
                    // Power-of-two sizes, then general sizes, start 0 and not.
                    custom(1, &[1, 2, 4, 8, 16, 32, 64, 128]),
                    custom(0, &[16, 16, 16, 208]),
                    custom(0, &[3, 5, 100, 150]),
                    custom(100, &[28, 128, 1 << 12, 1 << 22]),
                    custom(0, &wide),
                    custom(0, &[1 << 63, 1 << 63]),
                ];
                for graph in &graphs {
                    accepted += usize::from(round_trip(&[input], graph, version, &[&data]));
                }
            }
        }
    }
    assert!(accepted > 20, "{accepted}");
}
