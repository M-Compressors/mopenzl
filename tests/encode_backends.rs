//! Byte identity of the backend encoders (`lz4`, `bitpack`, `bitunpack`,
//! `flatpack`) and their graphs with the reference, and round trips through
//! this crate's decoder and the reference's.
#![cfg(all(
    feature = "compression",
    feature = "decompression",
    feature = "zstd",
    feature = "conversion",
    feature = "lz4",
    feature = "bitpack",
    feature = "bitunpack",
    feature = "flatpack"
))]

mod support;

use mopenzl::graph::{GraphBuilder, LocalParams, StandardGraph, StandardNode};
use mopenzl::input::{Input, Numbers};
use mopenzl::{Compressor, Decompressor, EncoderConfig, FormatVersion, Level};
use openzl_ffi::graph::{self, Graph, Input as CInput, cparam};

fn config(level: i32, version: u32) -> EncoderConfig {
    EncoderConfig::default()
        .with_level(Level::try_from(level).unwrap())
        .with_format_version(FormatVersion::try_from(version).unwrap())
        .with_min_stream_size(0)
}

fn c_params(config: &EncoderConfig) -> Vec<(i32, i32)> {
    vec![
        (cparam::LEVEL, config.level().get()),
        (
            cparam::FORMAT_VERSION,
            u32::from(config.format_version()) as i32,
        ),
        (cparam::MIN_STREAM_SIZE, config.min_stream_size() as i32),
    ]
}

/// One typed input in both representations.
#[derive(Clone)]
struct Data {
    name: String,
    bytes: Vec<u8>,
    /// 0 for serial data, else the numeric element width.
    width: usize,
}

impl Data {
    fn serial(name: &str, bytes: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            bytes,
            width: 0,
        }
    }

    fn numeric(name: &str, bytes: Vec<u8>, width: usize) -> Self {
        Self {
            name: name.into(),
            bytes,
            width,
        }
    }

    fn input(&self) -> Input<'_> {
        if self.width == 0 {
            Input::from(&self.bytes[..])
        } else {
            Input::from(Numbers::try_from((&self.bytes[..], self.width)).unwrap())
        }
    }

    fn c_input(&self) -> CInput<'_> {
        if self.width == 0 {
            CInput::Serial(&self.bytes)
        } else {
            CInput::Numeric {
                bytes: &self.bytes,
                width: self.width,
            }
        }
    }
}

/// Compresses with both implementations and compares: equal frames, or both
/// failing. Returns the frame, if any, after checking it decodes with both
/// decoders to the input.
fn compare(
    data: &Data,
    cfg: EncoderConfig,
    c_graph: &Graph,
    rust_graph: impl FnOnce(&mut GraphBuilder) -> mopenzl::graph::GraphId,
    label: &str,
) -> Option<Vec<u8>> {
    let expected = graph::compress(&[data.c_input()], c_graph, &c_params(&cfg));
    let mut builder = GraphBuilder::new();
    let start = rust_graph(&mut builder);
    let mut compressor = Compressor::builder(cfg)
        .with_graph(builder.build(start).unwrap())
        .build()
        .unwrap();
    let frame = compressor.compress_inputs(&[data.input()]);
    match (expected, frame) {
        (Ok(expected), Ok(frame)) => {
            assert_eq!(frame, expected, "{label} on {}", data.name);
            let outputs = Decompressor::default()
                .decompress_typed(&frame)
                .unwrap_or_else(|error| panic!("{label} on {}: our decoder: {error}", data.name));
            assert_eq!(outputs.len(), 1);
            assert_eq!(
                outputs[0].content(),
                &data.bytes[..],
                "{label} on {}",
                data.name
            );
            let c = graph::decompress(&frame).expect("C decodes our frame");
            assert_eq!(
                c[0].bytes, data.bytes,
                "{label} on {} (C decoder)",
                data.name
            );
            Some(frame)
        }
        (Err(_), Err(_)) => None,
        (Ok(_), Err(error)) => panic!("{label} on {}: C succeeds, we fail: {error}", data.name),
        (Err(error), Ok(_)) => panic!("{label} on {}: we succeed, C fails: {error:?}", data.name),
    }
}

/// A node with local integer parameters followed by `outputs` stores.
fn node_pair(
    id: u32,
    node: StandardNode,
    ints: &[(i32, i32)],
    outputs: usize,
) -> (
    Graph,
    impl FnOnce(&mut GraphBuilder) -> mopenzl::graph::GraphId + use<>,
) {
    let c = Graph::node_with(id, ints.iter().copied(), vec![Graph::STORE; outputs]);
    let ints = ints.to_vec();
    let rust = move |builder: &mut GraphBuilder| {
        let stores = vec![StandardGraph::Store.into(); outputs];
        if ints.is_empty() {
            builder.static_graph(node, &stores).unwrap()
        } else {
            let mut params = LocalParams::default();
            for (id, value) in ints {
                params = params.with_int(id, value);
            }
            let node = builder.parameterize_node(node, params).unwrap();
            builder.static_graph(node, &stores).unwrap()
        }
    };
    (c, rust)
}

fn patterns(len: usize, seed: u32) -> Vec<u8> {
    let noise = support::noise(len + 64, seed);
    let mut out = Vec::new();
    let mut at = 0;
    while out.len() < len {
        let pick = noise[at % noise.len()];
        at += 1;
        let pattern: &[u8] = match pick % 4 {
            0 => &[0],
            1 => &[1, 2],
            2 => &[7, 7, 7, 8],
            _ => &noise[at % 32..at % 32 + 5],
        };
        for n in 0..1 + usize::from(pick) * 2 {
            out.push(pattern[n % pattern.len()]);
        }
        out.push(pick);
    }
    out.truncate(len);
    out
}

/// Little-endian elements below `1 << bits`.
fn values(count: usize, width: usize, bits: u32, seed: u32) -> Vec<u8> {
    let noise = support::noise(count * 8, seed);
    let mut out = Vec::with_capacity(count * width);
    for chunk in noise.as_chunks::<8>().0 {
        let value = u64::from_le_bytes(*chunk);
        let value = if bits >= 64 {
            value
        } else {
            value & ((1u64 << bits) - 1)
        };
        out.extend_from_slice(&value.to_le_bytes()[..width]);
    }
    out
}

fn lz4_inputs() -> Vec<Data> {
    let big = if cfg!(miri) { 3000 } else { 70_000 };
    vec![
        Data::serial("empty", vec![]),
        Data::serial("one", vec![9]),
        Data::serial("tiny", b"abcabcabcabcabc".to_vec()),
        Data::serial("text-small", support::text(300, 1)),
        Data::serial("text", support::text(5000, 2)),
        Data::serial("zeros", vec![0; 3000]),
        Data::serial("noise", support::noise(3000, 3)),
        Data::serial("patterns", patterns(9000, 4)),
        Data::serial("text-64k", support::text(big, 5)),
        Data::serial("patterns-64k", patterns(big, 6)),
    ]
}

#[test]
fn lz4_node_matches_the_reference_at_every_level_class() {
    let levels: &[i32] = if cfg!(miri) {
        &[-5, 1, 3, 10]
    } else {
        &[-10000, -5, -1, 0, 1, 2, 3, 6, 7, 9, 10, 11, 12, 19]
    };
    for data in lz4_inputs() {
        for &level in levels {
            // Level 11 and above on a long input is slow in debug builds.
            if data.bytes.len() > 10_000 && level > 10 {
                continue;
            }
            for version in [23, 24, 27] {
                let (c, rust) = node_pair(86, StandardNode::Lz4, &[], 1);
                let label = format!("lz4 level {level} v{version}");
                compare(&data, config(level, version), &c, rust, &label);
            }
        }
    }
}

#[test]
fn lz4_node_rejects_formats_before_it_exists_like_the_reference() {
    for version in [21, 22] {
        let (c, rust) = node_pair(86, StandardNode::Lz4, &[], 1);
        let data = Data::serial("text", support::text(500, 1));
        assert!(compare(&data, config(3, version), &c, rust, "lz4").is_none());
    }
}

#[test]
fn lz4_level_override_parameter_wins_over_the_global_level() {
    let data = Data::serial("text", support::text(8000, 7));
    for (global, local) in [(3, 1), (3, 12), (1, 9), (12, -7), (6, 0), (6, 10)] {
        let (c, rust) = node_pair(86, StandardNode::Lz4, &[(0, local)], 1);
        let frame = compare(&data, config(global, 27), &c, rust, "lz4 override").unwrap();
        // The override really changes the output.
        let (c2, rust2) = node_pair(86, StandardNode::Lz4, &[], 1);
        let plain = compare(&data, config(global, 27), &c2, rust2, "lz4 plain").unwrap();
        if local != global {
            assert!(
                frame != plain || (global, local) == (12, -7),
                "{global} {local}"
            );
        }
    }
}

#[test]
fn lz4_graph_matches_the_reference_by_id() {
    for data in lz4_inputs().into_iter().take(8) {
        for level in [1, 3, 8, 10] {
            let label = format!("lz4 graph level {level}");
            compare(
                &data,
                config(level, 27),
                &Graph::Id(22),
                |_| StandardGraph::Lz4.into(),
                &label,
            );
        }
    }
}

fn bitpack_inputs() -> Vec<Data> {
    let mut inputs = vec![
        Data::serial("serial-empty", vec![]),
        Data::serial("serial-zeros", vec![0; 100]),
        Data::serial("serial-ones", vec![1; 37]),
        Data::serial("serial-one", vec![5]),
        Data::serial("serial-full", support::noise(333, 1)),
    ];
    for bits in [1u32, 2, 3, 5, 7, 8] {
        inputs.push(Data::serial(
            &format!("serial-{bits}b"),
            values(129, 1, bits, bits),
        ));
    }
    for width in [1usize, 2, 4, 8] {
        let max = width as u32 * 8;
        let mut widths: Vec<u32> = vec![1, 2, 3, 5, 7, 9, 12, 13, 15, 17, 24, 31, 33, 47, 57, 63];
        widths.retain(|&bits| bits <= max);
        widths.push(max);
        inputs.push(Data::numeric(&format!("num{width}-empty"), vec![], width));
        inputs.push(Data::numeric(
            &format!("num{width}-zeros"),
            vec![0; 16 * width],
            width,
        ));
        for bits in widths {
            for count in [1usize, 3, 8, 9, 100] {
                inputs.push(Data::numeric(
                    &format!("num{width}-{bits}b-{count}"),
                    values(count, width, bits, bits + 31 * count as u32),
                    width,
                ));
            }
        }
    }
    inputs
}

#[test]
fn bitpack_nodes_and_graphs_match_the_reference() {
    for data in bitpack_inputs() {
        for version in [24, 27] {
            let cfg = config(6, version);
            let (id, node, graph_id) = if data.width == 0 {
                (63, StandardNode::BitpackSerial, 40)
            } else {
                (64, StandardNode::BitpackInt, 41)
            };
            let (c, rust) = node_pair(id, node, &[], 1);
            compare(&data, cfg, &c, rust, "bitpack node");
            let graph = if data.width == 0 {
                StandardGraph::BitpackSerial
            } else {
                StandardGraph::BitpackInt
            };
            compare(
                &data,
                cfg,
                &Graph::Id(graph_id),
                |_| graph.into(),
                "private bitpack graph",
            );
            compare(
                &data,
                cfg,
                &Graph::Id(10),
                |_| StandardGraph::Bitpack.into(),
                "bitpack selector",
            );
        }
    }
}

#[test]
fn bitpack_node_rejects_the_wrong_input_type_like_the_reference() {
    // A numeric stream into the serial node, and a serial one into the
    // integer node, fail on both sides (the engine inserts no conversion
    // for a type the port does not accept... unless it can convert).
    let numeric = Data::numeric("num4", values(10, 4, 9, 1), 4);
    let serial = Data::serial("serial", support::noise(40, 2));
    let (c, rust) = node_pair(63, StandardNode::BitpackSerial, &[], 1);
    compare(
        &numeric,
        config(6, 27),
        &c,
        rust,
        "bitpack_serial on numeric",
    );
    let (c, rust) = node_pair(64, StandardNode::BitpackInt, &[], 1);
    compare(&serial, config(6, 27), &c, rust, "bitpack_int on serial");
}

fn flatpack_inputs() -> Vec<Data> {
    let mut inputs = vec![
        Data::serial("empty", vec![]),
        Data::serial("one-symbol", vec![42; 100]),
        Data::serial("two-symbols", b"abbaabbbabab".to_vec()),
        Data::serial("noise", support::noise(5000, 1)),
        Data::serial("text", support::text(4000, 2)),
        Data::serial("single", vec![0]),
        Data::serial("all-bytes", (0..=255).cycle().take(1000).collect()),
    ];
    for alphabet in [2u32, 3, 4, 5, 8, 9, 16, 17, 100, 128, 129, 200, 255, 256] {
        for len in [1usize, 7, 8, 9, 64, 1000] {
            let bytes = support::noise(len, alphabet)
                .iter()
                .map(|&b| (u32::from(b) % alphabet) as u8)
                .collect();
            inputs.push(Data::serial(&format!("alphabet-{alphabet}-{len}"), bytes));
        }
    }
    inputs
}

#[test]
fn flatpack_node_and_graph_match_the_reference() {
    for data in flatpack_inputs() {
        for version in [24, 27] {
            let cfg = config(6, version);
            let (c, rust) = node_pair(65, StandardNode::Flatpack, &[], 2);
            compare(&data, cfg, &c, rust, "flatpack node");
            compare(
                &data,
                cfg,
                &Graph::Id(11),
                |_| StandardGraph::Flatpack.into(),
                "flatpack graph",
            );
        }
    }
}

/// The reference's check on `bitunpack` input, restated.
fn fits(len: usize, bits: usize) -> bool {
    (len * 8 / bits * bits).div_ceil(8) == len
}

#[test]
fn bitunpack_node_matches_the_reference_for_every_bit_count() {
    for bits in 1..=64i32 {
        for len in [0usize, 1, 2, 3, 5, 8, 9, 17, 64, 100] {
            for (kind, bytes) in [
                ("noise", support::noise(len, bits as u32)),
                ("zeros", vec![0; len]),
                ("ones", vec![0xFF; len]),
            ] {
                let data = Data::serial(&format!("{kind}-{len}"), bytes);
                for version in [24, 27] {
                    let (c, rust) = node_pair(27, StandardNode::Bitunpack, &[(1, bits)], 1);
                    let frame = compare(
                        &data,
                        config(6, version),
                        &c,
                        rust,
                        &format!("bitunpack {bits} bits"),
                    );
                    // A fitting input must compress. One that does not fit
                    // fails the node, which both sides survive identically
                    // (the chunk is stored).
                    if fits(len, bits as usize) {
                        assert!(frame.is_some(), "{bits} bits, {len} bytes");
                    }
                }
            }
        }
    }
}

#[test]
fn bitunpack_node_rejects_missing_and_out_of_range_parameters() {
    let data = Data::serial("noise", support::noise(16, 1));
    let (c, rust) = node_pair(27, StandardNode::Bitunpack, &[], 1);
    assert!(compare(&data, config(6, 27), &c, rust, "no parameter").is_none());
    for bits in [0, -1, 65, 1000] {
        let (c, rust) = node_pair(27, StandardNode::Bitunpack, &[(1, bits)], 1);
        assert!(compare(&data, config(6, 27), &c, rust, "bad bits").is_none());
    }
}

#[test]
fn the_nodes_really_run_instead_of_falling_back_to_store() {
    let text = Data::serial("text", support::text(5000, 2));
    let (c, rust) = node_pair(86, StandardNode::Lz4, &[], 1);
    let frame = compare(&text, config(6, 27), &c, rust, "lz4").unwrap();
    assert!(frame.len() < 2500, "lz4 frame of {} bytes", frame.len());

    let four = Data::serial(
        "four",
        support::noise(4000, 1).iter().map(|b| b & 3).collect(),
    );
    let (c, rust) = node_pair(65, StandardNode::Flatpack, &[], 2);
    let frame = compare(&four, config(6, 27), &c, rust, "flatpack").unwrap();
    assert!(
        frame.len() < 1100,
        "flatpack frame of {} bytes",
        frame.len()
    );
    let (c, rust) = node_pair(63, StandardNode::BitpackSerial, &[], 1);
    let frame = compare(&four, config(6, 27), &c, rust, "bitpack").unwrap();
    assert!(frame.len() < 1100, "bitpack frame of {} bytes", frame.len());

    // The decoder hands back the serial bytes bitunpack was given.
    let packed = Data::serial("packed", support::noise(10, 3));
    let (c, rust) = node_pair(27, StandardNode::Bitunpack, &[(1, 5)], 1);
    let frame = compare(&packed, config(6, 27), &c, rust, "bitunpack").unwrap();
    let outputs = Decompressor::default().decompress_typed(&frame).unwrap();
    assert_eq!(outputs[0].data_type(), mopenzl::DataType::Serial);
}
