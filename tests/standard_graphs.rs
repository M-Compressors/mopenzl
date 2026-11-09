//! Frames of every standard graph the C library builds, decoded by mopenzl,
//! and produced again by mopenzl's encoder.
//!
//! Each graph runs over serial, struct, numeric and string inputs at
//! several levels and format versions. Whatever C produces must decode to
//! the same typed outputs here, and mopenzl's encoder (through
//! [`StandardGraph`]) must write the very same bytes. Graphs C cannot apply
//! to an input are skipped. The graphs another encoder package ports
//! (`ml_selector` 17, `cluster` 18, `sddl` 20, `sddl2` 21, and
//! `transformer_numeric` 30) are left out, and so are the comparisons that
//! reach the numeric transformer from level 7 until it is ported.
#![cfg(all(feature = "decompression", feature = "all-codecs"))]

mod support;

use mopenzl::graph::StandardGraph;
use mopenzl::input::{Input as RustInput, Numbers, Records, Strings};
use mopenzl::{Compressor, DataType, Decompressor, EncoderConfig, FormatVersion, Level};
use openzl_ffi::graph::{self, Graph, Input, cparam};

/// Standard graphs runnable without extra configuration.
const GRAPHS: &[(u32, &str)] = &[
    (2, "store"),
    (3, "fse"),
    (4, "huffman"),
    (5, "huffman_huf0"),
    (6, "huffman_pivco"),
    (7, "entropy"),
    (8, "constant"),
    (9, "zstd"),
    (10, "bitpack"),
    (11, "flatpack"),
    (12, "field_lz"),
    (13, "compress_generic"),
    (14, "select_generic_lz_backend"),
    (15, "segment_numeric"),
    (16, "select_numeric"),
    (19, "try_parse_int"),
    (22, "lz4"),
    (23, "partition_bitpack"),
    (24, "segment_num8_from_serial"),
    (25, "segment_num16_from_serial"),
    (26, "segment_num32_from_serial"),
    (27, "segment_num64_from_serial"),
    (28, "lz"),
    (29, "segment_serial"),
    (45, "field_lz_literals"),
    (46, "field_lz_literals_channel"),
    (69, "compress_small_lengths"),
];

/// The graphs that reach the numeric transformer from level 7 through
/// `select_numeric` (`compress_small_lengths` falls back to `compress_generic`
/// for 8-byte lengths).
const VIA_TRANSFORMER: &[u32] = &[13, 15, 16, 24, 25, 26, 27, 69];

fn data_type(kind: u32) -> DataType {
    match kind {
        0 => DataType::Serial,
        1 => DataType::Struct,
        2 => DataType::Numeric,
        _ => DataType::String,
    }
}

struct Case {
    name: String,
    bytes: Vec<u8>,
    width: usize,
    lens: Vec<u32>,
    kind: u8,
}

impl Case {
    fn rust_input(&self) -> RustInput<'_> {
        match self.kind {
            0 => RustInput::from(&self.bytes[..]),
            1 => RustInput::from(Records::try_from((&self.bytes[..], self.width)).unwrap()),
            2 => RustInput::from(Numbers::try_from((&self.bytes[..], self.width)).unwrap()),
            _ => RustInput::from(Strings::try_from((&self.bytes[..], &self.lens[..])).unwrap()),
        }
    }

    fn input(&self) -> Input<'_> {
        match self.kind {
            0 => Input::Serial(&self.bytes),
            1 => Input::Struct {
                bytes: &self.bytes,
                width: self.width,
            },
            2 => Input::Numeric {
                bytes: &self.bytes,
                width: self.width,
            },
            _ => Input::String {
                bytes: &self.bytes,
                lens: &self.lens,
            },
        }
    }
}

fn cases() -> Vec<Case> {
    let n = if cfg!(miri) { 64 } else { 6000 };
    let text = support::text(n * 4, 3);
    let noise = support::noise(n, 7);
    let mut cases = vec![
        Case {
            name: "serial text".into(),
            bytes: text.clone(),
            width: 1,
            lens: Vec::new(),
            kind: 0,
        },
        Case {
            name: "serial noise".into(),
            bytes: noise.clone(),
            width: 1,
            lens: Vec::new(),
            kind: 0,
        },
        Case {
            name: "serial constant".into(),
            bytes: vec![b'z'; n],
            width: 1,
            lens: Vec::new(),
            kind: 0,
        },
    ];
    for width in [1usize, 2, 4, 8] {
        let ramp: Vec<u8> = (0..n as u64)
            .flat_map(|i| (i * 3 + (i % 7) * 1000).to_le_bytes()[..width].to_vec())
            .collect();
        cases.push(Case {
            name: format!("numeric ramp w{width}"),
            bytes: ramp,
            width,
            lens: Vec::new(),
            kind: 2,
        });
        let small: Vec<u8> = (0..n as u64)
            .flat_map(|i| ((i * 2_654_435_761) % 97).to_le_bytes()[..width].to_vec())
            .collect();
        cases.push(Case {
            name: format!("numeric small w{width}"),
            bytes: small,
            width,
            lens: Vec::new(),
            kind: 2,
        });
    }
    // Inputs rich in matches, for the LZ graphs.
    for width in [1usize, 2, 4, 8] {
        let unit = support::noise(37 * width, 5);
        let period: Vec<u8> = (0..n * width).map(|i| unit[i % unit.len()]).collect();
        cases.push(Case {
            name: format!("numeric period w{width}"),
            bytes: period,
            width,
            lens: Vec::new(),
            kind: 2,
        });
    }
    for width in [3usize, 4, 12] {
        let unit = support::text(29 * width, 6);
        let period: Vec<u8> = (0..n * width).map(|i| unit[i % unit.len()]).collect();
        cases.push(Case {
            name: format!("struct period w{width}"),
            bytes: period,
            width,
            lens: Vec::new(),
            kind: 1,
        });
    }
    cases.push(Case {
        name: "serial period".into(),
        bytes: (0..n * 4).map(|i| text[i % 777]).collect(),
        width: 1,
        lens: Vec::new(),
        kind: 0,
    });
    for width in [3usize, 4, 12] {
        let records: Vec<u8> = (0..n)
            .flat_map(|i| (0..width).map(move |j| ((i / 3) as u8).wrapping_add(j as u8)))
            .collect();
        cases.push(Case {
            name: format!("struct w{width}"),
            bytes: records,
            width,
            lens: Vec::new(),
            kind: 1,
        });
    }
    let words: Vec<String> = (0..n / 4).map(|i| format!("{}", i * 37 % 1013)).collect();
    cases.push(Case {
        name: "strings of digits".into(),
        bytes: words.concat().into_bytes(),
        width: 1,
        lens: words.iter().map(|w| w.len() as u32).collect(),
        kind: 3,
    });
    let names: Vec<&[u8]> = text.split(|&b| b == b' ').take(n / 4).collect();
    cases.push(Case {
        name: "strings of words".into(),
        bytes: names.concat(),
        width: 1,
        lens: names.iter().map(|w| w.len() as u32).collect(),
        kind: 3,
    });
    cases
}

#[test]
fn every_standard_graph_decodes_like_the_reference() {
    let mut decoder = Decompressor::default();
    let mut decoded = 0usize;
    let mut encoded = 0usize;
    let mut failures = Vec::new();
    for case in cases() {
        for &(graph, name) in GRAPHS {
            for (level, version) in [(1, 21), (6, 24), (10, 27)] {
                let params = [(cparam::LEVEL, level), (cparam::FORMAT_VERSION, version)];
                let Ok(frame) = graph::compress(&[case.input()], &Graph::Id(graph), &params) else {
                    continue;
                };
                let label = format!("{name} on {} at level {level}, v{version}", case.name);
                let config = EncoderConfig::default()
                    .with_level(Level::try_from(level).unwrap())
                    .with_format_version(FormatVersion::try_from(version as u32).unwrap());
                let ours = Compressor::builder(config)
                    .with_graph(StandardGraph::try_from(graph).unwrap().into())
                    .build()
                    .unwrap()
                    .compress_inputs(&[case.rust_input()]);
                match ours {
                    Ok(bytes) => {
                        assert_eq!(bytes, frame, "{label}: the encoder writes other bytes");
                        encoded += 1;
                    }
                    Err(_)
                        if level >= 7
                            && VIA_TRANSFORMER.contains(&graph)
                            && !support::transformer_ported() => {}
                    Err(error) => panic!("{label}: C encodes it, we fail: {error}"),
                }
                let expected = graph::decompress(&frame).expect("C decodes its own frame");
                match decoder.decompress_typed(&frame) {
                    Ok(outputs) => {
                        assert_eq!(outputs.len(), expected.len(), "{label}");
                        for (ours, theirs) in outputs.iter().zip(&expected) {
                            assert_eq!(ours.data_type(), data_type(theirs.kind), "{label}");
                            assert_eq!(ours.content(), theirs.bytes, "{label}");
                            assert_eq!(ours.string_lens(), theirs.lens, "{label}");
                        }
                        decoded += 1;
                    }
                    Err(error) => failures.push(format!("{label}: {error}")),
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} frames failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(decoded > 300, "only {decoded} frames were produced");
    assert!(encoded > 250, "only {encoded} frames were re-encoded");
}
