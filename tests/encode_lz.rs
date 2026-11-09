//! Byte identity of the LZ-family node encoders (`field_lz`, `lz`,
//! `mux_lengths`, `quantize_offsets`, `quantize_lengths`) with the C
//! library, and round trips through both decoders.
//!
//! Every node runs as `Graph::node(id, [STORE, ...])` in C and as a static
//! graph over the node in Rust, so the frame holds exactly the node's
//! outputs. Inputs C rejects must be rejected by both sides.
// The shared helpers serve the whole family, so the file needs all of it.
#![cfg(all(
    feature = "compression",
    feature = "decompression",
    feature = "conversion",
    feature = "field-lz",
    feature = "lz",
    feature = "mux-lengths",
    feature = "quantize"
))]

mod support;

use mopenzl::graph::{GraphBuilder, LocalParams, StandardGraph, StandardNode};
use mopenzl::input::{Input, Numbers, Records};
use mopenzl::{Compressor, DataType, Decompressor, EncoderConfig, FormatVersion, Level};
use openzl_ffi::graph::{self, Graph, Input as CInput, cparam};

/// One typed test input in both representations.
struct Data {
    bytes: Vec<u8>,
    width: usize,
    kind: DataType,
}

impl Data {
    #[cfg(feature = "field-lz")]
    fn records(bytes: Vec<u8>, width: usize) -> Self {
        Self {
            bytes,
            width,
            kind: DataType::Struct,
        }
    }

    #[cfg(any(feature = "field-lz", feature = "mux-lengths", feature = "quantize"))]
    fn numbers(bytes: Vec<u8>, width: usize) -> Self {
        Self {
            bytes,
            width,
            kind: DataType::Numeric,
        }
    }

    #[cfg(feature = "lz")]
    fn serial(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            width: 1,
            kind: DataType::Serial,
        }
    }

    fn input(&self) -> Input<'_> {
        match self.kind {
            DataType::Struct => {
                Input::from(Records::try_from((&self.bytes[..], self.width)).unwrap())
            }
            DataType::Numeric => {
                Input::from(Numbers::try_from((&self.bytes[..], self.width)).unwrap())
            }
            _ => Input::from(&self.bytes[..]),
        }
    }

    fn c_input(&self) -> CInput<'_> {
        match self.kind {
            DataType::Struct => CInput::Struct {
                bytes: &self.bytes,
                width: self.width,
            },
            DataType::Numeric => CInput::Numeric {
                bytes: &self.bytes,
                width: self.width,
            },
            _ => CInput::Serial(&self.bytes),
        }
    }
}

fn config(level: i32, version: u32) -> EncoderConfig {
    EncoderConfig::default()
        .with_level(Level::try_from(level).unwrap())
        .with_format_version(FormatVersion::try_from(version).unwrap())
        .with_content_checksum(false)
        .with_compressed_checksum(false)
        .with_min_stream_size(0)
        .with_store_on_expansion(false)
}

fn c_params(config: &EncoderConfig) -> Vec<(i32, i32)> {
    vec![
        (cparam::LEVEL, config.level().get()),
        (
            cparam::FORMAT_VERSION,
            u32::from(config.format_version()) as i32,
        ),
        (cparam::CONTENT_CHECKSUM, 2),
        (cparam::COMPRESSED_CHECKSUM, 2),
        (cparam::MIN_STREAM_SIZE, 0),
        (cparam::STORE_ON_EXPANSION, 2),
    ]
}

/// The frame of `node` with `ints` stored, from this crate.
fn rust_frame(
    node: StandardNode,
    ints: &[(i32, i32)],
    outputs: usize,
    inputs: &[&Data],
    config: EncoderConfig,
) -> Option<Vec<u8>> {
    let mut builder = GraphBuilder::new();
    let head = if ints.is_empty() {
        node.into()
    } else {
        let params = ints.iter().fold(LocalParams::default(), |p, &(id, value)| {
            p.with_int(id, value)
        });
        builder.parameterize_node(node, params).unwrap()
    };
    let store = vec![StandardGraph::Store.into(); outputs];
    let graph = builder.static_graph(head, &store).unwrap();
    let mut compressor = Compressor::builder(config)
        .with_graph(builder.build(graph).unwrap())
        .build()
        .unwrap();
    let inputs: Vec<Input<'_>> = inputs.iter().map(|data| data.input()).collect();
    compressor.compress_inputs(&inputs).ok()
}

/// The frame of node `id` with `ints` stored, from the C library.
fn c_frame(
    id: u32,
    ints: &[(i32, i32)],
    outputs: usize,
    inputs: &[&Data],
    config: &EncoderConfig,
) -> Option<Vec<u8>> {
    let successors = vec![Graph::STORE; outputs];
    let graph = if ints.is_empty() {
        Graph::node(id, successors)
    } else {
        Graph::node_with(id, ints.iter().copied(), successors)
    };
    let inputs: Vec<CInput<'_>> = inputs.iter().map(|data| data.c_input()).collect();
    graph::compress(&inputs, &graph, &c_params(config)).ok()
}

/// Both sides must produce the same frame or both fail; returns the frame.
fn same(
    label: &str,
    id: u32,
    node: StandardNode,
    ints: &[(i32, i32)],
    outputs: usize,
    inputs: &[&Data],
    config: EncoderConfig,
) -> Option<Vec<u8>> {
    let expected = c_frame(id, ints, outputs, inputs, &config);
    let actual = rust_frame(node, ints, outputs, inputs, config);
    match (&expected, &actual) {
        (Some(c), Some(rust)) => assert_eq!(rust, c, "{label}: frames differ"),
        (None, None) => {}
        _ => panic!(
            "{label}: C {} but Rust {}",
            if expected.is_some() {
                "succeeds"
            } else {
                "fails"
            },
            if actual.is_some() {
                "succeeds"
            } else {
                "fails"
            },
        ),
    }
    if let Some(frame) = &actual {
        let decoded = Decompressor::default()
            .decompress_typed(frame)
            .unwrap_or_else(|e| panic!("{label}: our decoder: {e}"));
        let c_decoded = graph::decompress(frame).unwrap_or_else(|_| panic!("{label}: C decoder"));
        assert_eq!(decoded.len(), inputs.len(), "{label}");
        assert_eq!(c_decoded.len(), inputs.len(), "{label}");
        for (i, data) in inputs.iter().enumerate() {
            assert_eq!(decoded[i].content(), &data.bytes[..], "{label}: input {i}");
            assert_eq!(c_decoded[i].bytes, data.bytes, "{label}: C input {i}");
        }
    }
    actual
}

#[cfg(any(feature = "field-lz", feature = "lz"))]
/// Deterministic data of `n` fields of `width` bytes in a given shape.
fn fields(shape: &str, n: usize, width: usize, seed: u32) -> Vec<u8> {
    let len = n * width;
    match shape {
        "random" => support::noise(len, seed),
        "text" => support::text(len, seed),
        "zeros" => vec![0; len],
        // A short period with a rare change: many repcode matches.
        "period" => {
            let unit = support::noise(7 * width, seed);
            let mut out: Vec<u8> = (0..len).map(|i| unit[i % unit.len()]).collect();
            for i in (0..len).step_by(97 * width.max(1) + 1) {
                out[i] ^= 0x55;
            }
            out
        }
        // Counters: no byte-exact repeats but a regular structure.
        "counter" => (0..n)
            .flat_map(|i| {
                let value = (i as u64).wrapping_mul(3) | 1;
                value.to_le_bytes()[..width].to_vec()
            })
            .collect(),
        // Long literal runs, long matches and short matches interleaved.
        "mixed" => {
            let block = support::noise(300 * width, seed);
            let mut out = Vec::with_capacity(len);
            let mut round = 0u32;
            while out.len() < len {
                out.extend_from_slice(&support::noise(40 * width, seed + round));
                out.extend_from_slice(&block);
                out.extend_from_slice(&support::text(9 * width, round));
                round += 1;
            }
            out.truncate(len);
            out
        }
        other => panic!("unknown shape {other}"),
    }
}

#[cfg(feature = "field-lz")]
const SHAPES: [&str; 7] = [
    "random", "text", "zeros", "period", "counter", "mixed", "mixed",
];

#[cfg(feature = "field-lz")]
fn sizes() -> Vec<usize> {
    if cfg!(miri) {
        vec![0, 1, 9, 80]
    } else {
        vec![0, 1, 2, 3, 9, 17, 33, 64, 300, 4_000, 40_000]
    }
}

#[cfg(feature = "field-lz")]
mod field_lz {
    use super::*;

    const ID: u32 = 9;
    const OVERRIDE: i32 = 181;

    fn run(
        label: &str,
        ints: &[(i32, i32)],
        data: &Data,
        config: EncoderConfig,
    ) -> Option<Vec<u8>> {
        same(label, ID, StandardNode::FieldLz, ints, 5, &[data], config)
    }

    #[test]
    fn every_width_shape_and_size_matches_at_the_default_level() {
        for width in [2usize, 4, 8] {
            for (s, shape) in SHAPES.iter().enumerate() {
                for n in sizes() {
                    let data = Data::records(fields(shape, n, width, s as u32 + 1), width);
                    let label = format!("w{width} {shape} n{n}");
                    assert!(run(&label, &[], &data, config(3, 27)).is_some(), "{label}");
                }
            }
        }
    }

    #[test]
    fn every_level_class_matches() {
        let n = if cfg!(miri) { 80 } else { 6_000 };
        for width in [2usize, 4, 8] {
            for shape in ["text", "period", "mixed", "random"] {
                let data = Data::records(fields(shape, n, width, 5), width);
                // Levels <= 0 behave as 3; 1 has no long hash; 2 and 3 use
                // the fast finder; 4 and above the greedy finder; 5+ clamp.
                for level in [-7, 0, 1, 2, 3, 4, 5, 6, 10, 19, 22] {
                    let label = format!("w{width} {shape} level {level}");
                    assert!(run(&label, &[], &data, config(level, 27)).is_some());
                }
            }
        }
    }

    #[test]
    fn the_override_parameter_replaces_the_global_level() {
        let n = if cfg!(miri) { 80 } else { 6_000 };
        let data = Data::records(fields("mixed", n, 4, 6), 4);
        let mut frames = std::collections::BTreeSet::new();
        for global in [1, 6] {
            for level in [-3, 0, 1, 2, 3, 4, 5, 9] {
                let label = format!("global {global} override {level}");
                let frame = run(&label, &[(OVERRIDE, level)], &data, config(global, 27)).unwrap();
                frames.insert((level, frame));
            }
        }
        // The override level, not the global one, decides: both globals give
        // the same frame per override.
        let distinct: std::collections::BTreeSet<_> = frames.iter().map(|(l, _)| *l).collect();
        assert_eq!(frames.len(), distinct.len());
    }

    #[test]
    fn every_format_version_matches() {
        let data = Data::records(fields("text", 3_000, 4, 3), 4);
        for version in [21, 22, 23, 24, 25, 26, 27] {
            let label = format!("version {version}");
            assert!(
                run(&label, &[], &data, config(4, version)).is_some(),
                "{label}"
            );
        }
    }

    #[test]
    fn widths_the_node_cannot_take_fail_on_both_sides() {
        for width in [1usize, 3, 5, 6, 12, 16] {
            let data = Data::records(fields("text", 200, width, 3), width);
            assert!(run(&format!("width {width}"), &[], &data, config(3, 27)).is_none());
        }
    }

    #[test]
    fn numeric_input_is_converted_by_the_engine_like_the_reference() {
        for width in [2usize, 4, 8] {
            let data = Data::numbers(fields("period", 500, width, 8), width);
            same(
                &format!("numeric {width}"),
                ID,
                StandardNode::FieldLz,
                &[],
                5,
                &[&data],
                config(3, 27),
            );
        }
    }
}

#[cfg(feature = "lz")]
mod lz {
    use super::*;

    const ID: u32 = 52;
    // `ZL_LzParam_*`.
    const LEVEL: i32 = 1;
    const ACCELERATION: i32 = 100;
    const WINDOW_LOG: i32 = 101;
    const STRATEGY: i32 = 102;
    const HASH_LOG1: i32 = 103;
    const HASH_LOG2: i32 = 104;
    const HASH_LENGTH: i32 = 105;
    const SEARCH_LOG: i32 = 106;

    fn run(
        label: &str,
        ints: &[(i32, i32)],
        bytes: &[u8],
        config: EncoderConfig,
    ) -> Option<Vec<u8>> {
        let data = Data::serial(bytes.to_vec());
        same(label, ID, StandardNode::Lz, ints, 4, &[&data], config)
    }

    fn corpus(n: usize) -> Vec<(&'static str, Vec<u8>)> {
        let mut long_run = support::text(n.max(1), 4);
        // A literal run beyond 65535 bytes and a match beyond 65535 bytes.
        let noise = support::noise(70_000, 9);
        long_run.splice(0..0, noise.iter().copied());
        long_run.extend_from_slice(&noise);
        long_run.extend_from_slice(&noise);
        vec![
            ("text", support::text(n, 1)),
            ("noise", support::noise(n, 2)),
            ("zeros", vec![0; n]),
            ("period", fields("period", n, 1, 3)),
            ("mixed", fields("mixed", n, 1, 4)),
            (
                "long",
                if cfg!(miri) {
                    support::text(n, 1)
                } else {
                    long_run
                },
            ),
        ]
    }

    #[test]
    fn every_level_matches_on_every_shape() {
        let n = if cfg!(miri) { 120 } else { 20_000 };
        for (name, bytes) in corpus(n) {
            // Negative levels accelerate; 0 takes the global level.
            for level in [-100, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9] {
                let label = format!("{name} node level {level}");
                assert!(run(&label, &[(LEVEL, level)], &bytes, config(3, 27)).is_some());
            }
            for global in [-5, 1, 3, 5, 6, 7, 10, 19] {
                let label = format!("{name} global level {global}");
                assert!(run(&label, &[], &bytes, config(global, 27)).is_some());
            }
        }
    }

    #[test]
    fn every_size_matches_across_the_small_input_boundaries() {
        for n in [
            0usize, 1, 2, 3, 4, 5, 15, 16, 17, 31, 32, 33, 34, 35, 64, 65, 200, 1_000,
        ] {
            for (name, bytes) in corpus(n) {
                if bytes.len() > 2_000 {
                    continue;
                }
                for level in [1, 3, 5, 6, 7] {
                    let label = format!("{name} n{n} level {level}");
                    assert!(run(&label, &[(LEVEL, level)], &bytes, config(3, 27)).is_some());
                }
            }
        }
    }

    #[test]
    fn every_parameter_and_clamp_matches() {
        let n = if cfg!(miri) { 120 } else { 30_000 };
        let bytes = fields("mixed", n, 1, 11);
        let cases: Vec<(i32, Vec<i32>)> = vec![
            (STRATEGY, vec![-1, 0, 1, 2, 3, 4, 5, 6, 100]),
            (WINDOW_LOG, vec![-1, 0, 5, 10, 12, 16, 17, 20, 28, 40]),
            (HASH_LOG1, vec![-1, 0, 4, 10, 12, 18, 23, 30]),
            (HASH_LOG2, vec![0, 4, 10, 12, 18, 23, 30]),
            (HASH_LENGTH, vec![0, 1, 4, 5, 6, 7, 8, 100]),
            (ACCELERATION, vec![0, 1, 2, 7, 100, 10_000, 100_000, -1]),
            (SEARCH_LOG, vec![0, 1, 2, 4, 9]),
        ];
        for level in [1, 3, 6] {
            for (param, values) in &cases {
                for &value in values {
                    let label = format!("level {level} param {param}={value}");
                    let ints = [(LEVEL, level), (*param, value)];
                    assert!(
                        run(&label, &ints, &bytes, config(3, 27)).is_some(),
                        "{label}"
                    );
                }
            }
        }
    }

    #[test]
    fn strategies_and_window_logs_together_match() {
        let n = if cfg!(miri) { 120 } else { 20_000 };
        let bytes = fields("text", n, 1, 12);
        for strategy in 1..=5 {
            for window in [10, 15, 16, 17, 22] {
                for hash_length in [4, 5, 6, 7] {
                    let label = format!("strategy {strategy} window {window} hash {hash_length}");
                    let ints = [
                        (STRATEGY, strategy),
                        (WINDOW_LOG, window),
                        (HASH_LENGTH, hash_length),
                        (SEARCH_LOG, 1 + hash_length % 4),
                    ];
                    assert!(
                        run(&label, &ints, &bytes, config(3, 27)).is_some(),
                        "{label}"
                    );
                }
            }
        }
    }

    #[test]
    fn later_duplicates_of_a_parameter_win_like_the_reference() {
        let bytes = fields("text", 2_000, 1, 13);
        let ints = [
            (LEVEL, 7),
            (LEVEL, 2),
            (STRATEGY, 3),
            (STRATEGY, 1),
            (999, 5),
        ];
        let label = "duplicates";
        assert!(run(label, &ints, &bytes, config(3, 27)).is_some());
    }

    #[test]
    fn every_supported_format_version_matches() {
        let bytes = fields("text", 3_000, 1, 14);
        for version in [24, 25, 26, 27] {
            let label = format!("version {version}");
            assert!(
                run(&label, &[], &bytes, config(5, version)).is_some(),
                "{label}"
            );
        }
    }

    #[test]
    fn older_format_versions_fail_on_both_sides() {
        let bytes = fields("text", 3_000, 1, 14);
        for version in [21, 22, 23] {
            let label = format!("version {version}");
            assert!(
                run(&label, &[], &bytes, config(5, version)).is_none(),
                "{label}"
            );
        }
    }
}

#[cfg(feature = "mux-lengths")]
mod mux_lengths {
    use super::*;

    const ID: u32 = 53;
    const SPLIT_POINT: i32 = 0;
    const BIAS: i32 = 1;

    /// A pair of length streams with small, medium and huge values.
    fn lengths(n: usize, width: usize, seed: u32) -> (Data, Data) {
        let noise = support::noise(n * 4, seed);
        let value = |i: usize, salt: usize| -> u64 {
            let r = u64::from(noise[(i * 2 + salt) % noise.len().max(1)]);
            match r % 8 {
                0..=4 => r % 7,
                5 => r % 40,
                6 => 100 + r,
                _ if width >= 4 => 70_000 + r * 1000,
                _ => 250 + r % 5,
            }
        };
        let make = |salt: usize| {
            (0..n)
                .flat_map(|i| value(i, salt).to_le_bytes()[..width].to_vec())
                .collect::<Vec<u8>>()
        };
        (Data::numbers(make(0), width), Data::numbers(make(1), width))
    }

    fn run(
        label: &str,
        ints: &[(i32, i32)],
        a: &Data,
        b: &Data,
        config: EncoderConfig,
    ) -> Option<Vec<u8>> {
        same(
            label,
            ID,
            StandardNode::MuxLengths,
            ints,
            2,
            &[a, b],
            config,
        )
    }

    #[test]
    fn automatic_split_points_match_for_every_width_and_size() {
        for width in [1usize, 2, 4, 8] {
            for n in [0usize, 1, 2, 7, 8, 9, 16, 17, 100, 1_000, 5_000] {
                let (ll, ml) = lengths(n, width, 3);
                let label = format!("width {width} n {n}");
                assert!(
                    run(&label, &[], &ll, &ml, config(3, 27)).is_some(),
                    "{label}"
                );
            }
        }
    }

    #[test]
    fn every_explicit_split_point_and_bias_matches() {
        for width in [1usize, 2, 4, 8] {
            let (ll, ml) = lengths(500, width, 5);
            for split in 0..=8 {
                for bias in [0, 1, 4, 15] {
                    let label = format!("width {width} split {split} bias {bias}");
                    let ints = [(SPLIT_POINT, split), (BIAS, bias)];
                    assert!(
                        run(&label, &ints, &ll, &ml, config(3, 27)).is_some(),
                        "{label}"
                    );
                }
            }
        }
    }

    #[test]
    fn out_of_range_parameters_fail_on_both_sides() {
        let (ll, ml) = lengths(100, 2, 6);
        for ints in [
            vec![(SPLIT_POINT, 9)],
            vec![(SPLIT_POINT, -1)],
            vec![(BIAS, 16)],
            vec![(BIAS, -1)],
        ] {
            assert!(run(&format!("{ints:?}"), &ints, &ll, &ml, config(3, 27)).is_none());
        }
    }

    #[test]
    fn mismatched_streams_fail_on_both_sides() {
        let (ll2, ml2) = lengths(100, 2, 6);
        let (ll4, _) = lengths(100, 4, 6);
        let (short, _) = lengths(50, 2, 6);
        assert!(run("width", &[], &ll2, &lengths(100, 4, 1).1, config(3, 27)).is_none());
        assert!(run("count", &[], &short, &ml2, config(3, 27)).is_none());
        let _ = ll4;
    }

    #[test]
    fn every_format_version_matches_or_fails_alike() {
        let (ll, ml) = lengths(300, 2, 7);
        for version in [21, 23, 24, 27] {
            let label = format!("version {version}");
            let frame = run(&label, &[], &ll, &ml, config(3, version));
            assert_eq!(frame.is_some(), version >= 24, "{label}");
        }
    }
}

#[cfg(feature = "quantize")]
mod quantize {
    use super::*;

    /// Values that hit every code: small, powers of two and their neighbours.
    fn values(n: usize, with_zero: bool) -> Data {
        let noise = support::noise(n * 4, 21);
        let mut out = Vec::with_capacity(n * 4);
        for i in 0..n {
            let word = u32::from_le_bytes([
                noise[4 * i],
                noise[4 * i + 1],
                noise[4 * i + 2],
                noise[4 * i + 3],
            ]);
            let value = match i % 6 {
                0 => word >> (word % 32),
                1 => 1u32 << (word % 32),
                2 => (1u32 << (word % 32)).wrapping_sub(1),
                3 => word % 40,
                4 => u32::MAX - (word % 3),
                _ => word,
            };
            let value = if with_zero || value != 0 { value } else { 1 };
            out.extend_from_slice(&value.to_le_bytes());
        }
        Data::numbers(out, 4)
    }

    fn run(id: u32, node: StandardNode, label: &str, data: &Data, version: u32) -> Option<Vec<u8>> {
        same(label, id, node, &[], 2, &[data], config(3, version))
    }

    #[test]
    fn both_nodes_match_for_every_size_and_version() {
        for (id, node) in [
            (43, StandardNode::QuantizeOffsets),
            (44, StandardNode::QuantizeLengths),
        ] {
            for n in [0usize, 1, 2, 7, 8, 9, 33, 500, 10_000] {
                for version in [21, 24, 27] {
                    let data = values(n, id == 44);
                    let label = format!("node {id} n {n} v{version}");
                    assert!(run(id, node, &label, &data, version).is_some(), "{label}");
                }
            }
        }
    }

    #[test]
    fn lengths_accept_zero_and_every_boundary_value() {
        let boundary: Vec<u32> = (0..70)
            .chain([255, 256, 65_535, 65_536, u32::MAX - 1, u32::MAX])
            .chain((4..32).flat_map(|b| [(1u32 << b) - 1, 1 << b, (1 << b) + 1]))
            .collect();
        let bytes: Vec<u8> = boundary.iter().flat_map(|v| v.to_le_bytes()).collect();
        let data = Data::numbers(bytes, 4);
        assert!(run(44, StandardNode::QuantizeLengths, "boundaries", &data, 27).is_some());
        let nonzero: Vec<u8> = boundary
            .iter()
            .filter(|&&v| v != 0)
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let data = Data::numbers(nonzero, 4);
        assert!(
            run(
                43,
                StandardNode::QuantizeOffsets,
                "offset boundaries",
                &data,
                27
            )
            .is_some()
        );
    }

    #[test]
    fn a_zero_offset_fails_on_both_sides() {
        let data = Data::numbers(
            [5u32, 0, 9].iter().flat_map(|v| v.to_le_bytes()).collect(),
            4,
        );
        assert!(run(43, StandardNode::QuantizeOffsets, "zero offset", &data, 27).is_none());
    }

    #[test]
    fn widths_other_than_four_fail_on_both_sides() {
        for width in [1usize, 2, 8] {
            let data = Data::numbers(support::noise(64 * width, 3), width);
            for (id, node) in [
                (43, StandardNode::QuantizeOffsets),
                (44, StandardNode::QuantizeLengths),
            ] {
                assert!(run(id, node, &format!("width {width}"), &data, 27).is_none());
            }
        }
    }
}
