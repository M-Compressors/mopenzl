//! Byte identity of the LZ graphs with the reference: `field_lz` (12), its
//! private literals graph (45) and channel selector (46), `lz` (28),
//! `compress_small_lengths` (69), and `compress_generic` (13) on the struct
//! and numeric inputs that route through `field_lz`.
//!
//! Every comparison builds the same graph in C (`openzl_ffi::graph`) and in
//! this crate, with the same global parameters, and requires the same frame
//! or a failure on both sides. Every frame decodes back to its input with
//! this crate's decoder and the reference's.
//!
//! The only comparisons skipped are numeric inputs at level 7 or above
//! through `compress_generic`/`select_numeric`, which reach the numeric
//! transformer; they run as soon as the transformer is ported.
#![cfg(not(miri))]
#![cfg(all(
    feature = "compression",
    feature = "decompression",
    feature = "conversion",
    feature = "field-lz",
    feature = "lz",
    feature = "entropy"
))]

mod support;

use mopenzl::graph::StandardGraph;
use mopenzl::graph::{CompressorGraph, GraphBuilder, GraphId, GraphParams, LocalParams};
use mopenzl::input::{Input, Numbers, Records};
use mopenzl::{Compressor, DataType, Decompressor, EncoderConfig, FormatVersion, Level};
use openzl_ffi::graph::{self, Graph, Input as CInput, Params, cparam};

/// `ZL_CParam_decompressionLevel`.
const DECOMPRESSION_LEVEL: i32 = 3;
/// `ZL_FIELD_LZ_COMPRESSION_LEVEL_OVERRIDE_PID`.
const FIELD_LZ_LEVEL_OVERRIDE: i32 = 181;
const VERSIONS: [u32; 3] = [21, 24, 27];

/// One typed test input in both representations.
struct Data {
    name: String,
    bytes: Vec<u8>,
    width: usize,
    kind: DataType,
}

impl Data {
    fn new(name: impl Into<String>, kind: DataType, width: usize, bytes: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            bytes,
            width,
            kind,
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

/// Global parameters of one run.
#[derive(Copy, Clone, Debug)]
struct Cfg {
    level: i32,
    version: u32,
    min_stream_size: u32,
    decompression_level: u8,
}

impl Cfg {
    const fn new(level: i32, version: u32) -> Self {
        Self {
            level,
            version,
            min_stream_size: 10,
            decompression_level: 3,
        }
    }

    fn config(self) -> EncoderConfig {
        EncoderConfig::default()
            .with_level(Level::try_from(self.level).unwrap())
            .with_format_version(FormatVersion::try_from(self.version).unwrap())
            .with_min_stream_size(self.min_stream_size)
            .with_decompression_level(self.decompression_level)
    }

    fn c_params(self) -> Vec<(i32, i32)> {
        let config = self.config();
        vec![
            (cparam::LEVEL, self.level),
            (DECOMPRESSION_LEVEL, i32::from(self.decompression_level)),
            (cparam::FORMAT_VERSION, self.version as i32),
            (
                cparam::CONTENT_CHECKSUM,
                1 + i32::from(!config.content_checksum()),
            ),
            (
                cparam::COMPRESSED_CHECKSUM,
                1 + i32::from(!config.compressed_checksum()),
            ),
            (cparam::PERMISSIVE, 1 + i32::from(!config.permissive())),
            (cparam::MIN_STREAM_SIZE, self.min_stream_size as i32),
            (
                cparam::STORE_ON_EXPANSION,
                1 + i32::from(!config.store_on_expansion()),
            ),
        ]
    }
}

/// A standard graph with optional integer parameters and custom graphs (by
/// standard graph ID), on both sides.
#[derive(Clone, Debug)]
struct Spec {
    id: u32,
    ints: Vec<(i32, i32)>,
    custom: Vec<u32>,
}

impl Spec {
    fn plain(id: u32) -> Self {
        Self {
            id,
            ints: Vec::new(),
            custom: Vec::new(),
        }
    }

    fn with(id: u32, ints: &[(i32, i32)], custom: &[u32]) -> Self {
        Self {
            id,
            ints: ints.to_vec(),
            custom: custom.to_vec(),
        }
    }

    fn c_graph(&self) -> Graph {
        if self.ints.is_empty() && self.custom.is_empty() {
            return Graph::Id(self.id);
        }
        Graph::Parameterized {
            graph: self.id,
            params: Params {
                ints: self.ints.clone(),
                copies: Vec::new(),
            },
            custom: self.custom.iter().map(|&id| Graph::Id(id)).collect(),
        }
    }

    fn rust_graph(&self) -> CompressorGraph {
        let standard = StandardGraph::try_from(self.id).unwrap();
        if self.ints.is_empty() && self.custom.is_empty() {
            return CompressorGraph::standard(standard);
        }
        let mut params = GraphParams::default();
        if !self.ints.is_empty() {
            params = params.with_params(
                self.ints
                    .iter()
                    .fold(LocalParams::default(), |p, &(id, value)| {
                        p.with_int(id, value)
                    }),
            );
        }
        if !self.custom.is_empty() {
            let graphs: Vec<GraphId> = self
                .custom
                .iter()
                .map(|&id| StandardGraph::try_from(id).unwrap().into())
                .collect();
            params = params.with_graphs(graphs);
        }
        let mut builder = GraphBuilder::new();
        let graph = builder.parameterize_graph(standard, params).unwrap();
        builder.build(graph).unwrap()
    }
}

/// How one comparison ended.
#[derive(Debug, Eq, PartialEq)]
enum Outcome {
    /// Both produced this frame, which round-trips.
    Identical(Vec<u8>),
    /// Both failed.
    BothFailed,
    /// The numeric transformer (level 7 and above) is not ported yet.
    TransformerPending,
}

/// Compresses `data` through `spec` in C and here and compares.
/// `via_transformer` marks graphs whose numeric input reaches the numeric
/// transformer from level 7.
fn compare(label: &str, spec: &Spec, cfg: Cfg, data: &Data, via_transformer: bool) -> Outcome {
    let label = format!(
        "{label}: graph {} on {} (v{} l{} dl{} min{})",
        spec.id, data.name, cfg.version, cfg.level, cfg.decompression_level, cfg.min_stream_size
    );
    let expected = graph::compress(&[data.c_input()], &spec.c_graph(), &cfg.c_params());
    let actual = Compressor::builder(cfg.config())
        .with_graph(spec.rust_graph())
        .build()
        .unwrap()
        .compress_inputs(&[data.input()]);
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => {
            assert_eq!(actual, expected, "{label}: frames differ");
            let decoded = Decompressor::default()
                .decompress_typed(&actual)
                .unwrap_or_else(|error| panic!("{label}: our decoder: {error}"));
            assert_eq!(decoded.len(), 1, "{label}");
            assert_eq!(
                decoded[0].content(),
                &data.bytes[..],
                "{label}: our decoder"
            );
            let c = graph::decompress(&actual)
                .unwrap_or_else(|error| panic!("{label}: C decoder: {error:?}"));
            assert_eq!(c[0].bytes, data.bytes, "{label}: C decoder");
            Outcome::Identical(actual)
        }
        (Err(_), Err(_)) => Outcome::BothFailed,
        (Ok(_), Err(_))
            if via_transformer
                && cfg.level >= 7
                && data.kind == DataType::Numeric
                && !support::transformer_ported() =>
        {
            Outcome::TransformerPending
        }
        (Ok(expected), Err(error)) => panic!(
            "{label}: C produced {} bytes, we failed: {error}",
            expected.len()
        ),
        (Err(error), Ok(actual)) => panic!(
            "{label}: C failed ({error:?}), we produced {} bytes",
            actual.len()
        ),
    }
}

/// Runs the whole matrix and returns how many frames were identical.
fn matrix(
    spec: &Spec,
    data: &[Data],
    levels: &[i32],
    versions: &[u32],
    modes: &[(u32, u8)],
    via_transformer: bool,
) -> usize {
    let mut identical = 0;
    for data in data {
        for &version in versions {
            for &level in levels {
                for &(min_stream_size, decompression_level) in modes {
                    let cfg = Cfg {
                        level,
                        version,
                        min_stream_size,
                        decompression_level,
                    };
                    if matches!(
                        compare("matrix", spec, cfg, data, via_transformer),
                        Outcome::Identical(_)
                    ) {
                        identical += 1;
                    }
                }
            }
        }
    }
    identical
}

/// Default minimum stream size and decompression level.
const DEFAULT_MODE: [(u32, u8); 1] = [(10, 3)];

/// `value` as `width` little-endian bytes (zero-extended past 8).
fn padded(value: u64, width: usize) -> Vec<u8> {
    let mut bytes = value.to_le_bytes().to_vec();
    bytes.resize(width, 0);
    bytes.truncate(width);
    bytes
}

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
                padded(value, width)
            })
            .collect(),
        // Small values, few distinct: literals that entropy-code well.
        "small" => (0..n)
            .flat_map(|i| {
                let value = (i as u64).wrapping_mul(2_654_435_761) % 19;
                padded(value, width)
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

const SHAPES: [&str; 8] = [
    "random", "text", "zeros", "period", "counter", "small", "mixed", "mixed",
];

/// Typed inputs of one kind and width over the shapes and sizes.
fn typed(kind: DataType, width: usize, sizes: &[usize]) -> Vec<Data> {
    let mut out = Vec::new();
    for (s, shape) in SHAPES.iter().enumerate() {
        for &n in sizes {
            let bytes = fields(shape, n, width, s as u32 + 1);
            out.push(Data::new(
                format!("{kind:?}-w{width}-{shape}-n{n}"),
                kind,
                width,
                bytes,
            ));
        }
    }
    out
}

fn serial(sizes: &[usize]) -> Vec<Data> {
    typed(DataType::Serial, 1, sizes)
}

const SMALL_SIZES: [usize; 7] = [0, 1, 3, 17, 64, 300, 2_500];
const LEVELS: [i32; 6] = [1, 3, 6, 7, 10, 19];
const GENERIC_LEVELS: [i32; 3] = [1, 3, 6];

#[test]
fn compress_generic_matches_on_numeric_inputs() {
    let spec = Spec::plain(13);
    let mut identical = 0;
    for width in [1usize, 2, 4, 8] {
        let data = typed(DataType::Numeric, width, &SMALL_SIZES);
        identical += matrix(
            &spec,
            &data,
            &GENERIC_LEVELS,
            &VERSIONS,
            &DEFAULT_MODE,
            true,
        );
    }
    assert!(identical > 500, "{identical}");
}

#[test]
fn compress_generic_matches_on_numeric_inputs_above_level_6() {
    // Levels from 7 reach the numeric transformer; the comparisons run once it
    // is ported and are skipped (and counted as such) until then.
    let spec = Spec::plain(13);
    for width in [1usize, 2, 4, 8] {
        let data = typed(DataType::Numeric, width, &[0, 17, 300, 2_500]);
        matrix(&spec, &data, &[7, 10, 19], &VERSIONS, &DEFAULT_MODE, true);
    }
}

#[test]
fn compress_generic_matches_on_struct_inputs() {
    let spec = Spec::plain(13);
    let mut identical = 0;
    for width in [1usize, 2, 3, 4, 8, 12] {
        let data = typed(DataType::Struct, width, &SMALL_SIZES);
        identical += matrix(
            &spec,
            &data,
            &GENERIC_LEVELS,
            &VERSIONS,
            &DEFAULT_MODE,
            true,
        );
    }
    assert!(identical > 500, "{identical}");
}

#[test]
fn compress_generic_matches_on_struct_inputs_above_level_6() {
    let spec = Spec::plain(13);
    for width in [2usize, 3, 4, 8, 12] {
        let data = typed(DataType::Struct, width, &[0, 17, 300, 2_500]);
        let identical = matrix(&spec, &data, &[7, 10, 19], &VERSIONS, &DEFAULT_MODE, true);
        assert!(identical > 50, "{identical}");
    }
}

#[test]
fn compress_generic_matches_on_large_struct_and_numeric_inputs() {
    let spec = Spec::plain(13);
    for (kind, width) in [
        (DataType::Struct, 4),
        (DataType::Struct, 12),
        (DataType::Numeric, 2),
        (DataType::Numeric, 8),
    ] {
        let data = typed(kind, width, &[30_000]);
        let data: Vec<Data> = data
            .into_iter()
            .filter(|d| d.name.contains("mixed") || d.name.contains("text"))
            .collect();
        matrix(&spec, &data, &[1, 6], &[27], &DEFAULT_MODE, true);
    }
}

#[test]
fn field_lz_graph_matches_on_struct_inputs() {
    let spec = Spec::plain(12);
    let mut identical = 0;
    for width in [1usize, 2, 3, 4, 5, 8, 12, 16] {
        let data = typed(DataType::Struct, width, &SMALL_SIZES);
        identical += matrix(&spec, &data, &LEVELS, &VERSIONS, &DEFAULT_MODE, false);
    }
    assert!(identical > 800, "{identical}");
}

#[test]
fn field_lz_graph_matches_on_numeric_inputs() {
    let spec = Spec::plain(12);
    let mut identical = 0;
    for width in [1usize, 2, 4, 8] {
        let data = typed(DataType::Numeric, width, &SMALL_SIZES);
        identical += matrix(&spec, &data, &LEVELS, &VERSIONS, &DEFAULT_MODE, false);
    }
    assert!(identical > 500, "{identical}");
}

#[test]
fn field_lz_graph_matches_on_large_inputs() {
    let spec = Spec::plain(12);
    for (kind, width) in [
        (DataType::Struct, 2),
        (DataType::Struct, 4),
        (DataType::Struct, 8),
        (DataType::Numeric, 4),
    ] {
        let data: Vec<Data> = typed(kind, width, &[60_000])
            .into_iter()
            .filter(|d| !d.name.contains("zeros") && !d.name.contains("counter"))
            .collect();
        let identical = matrix(
            &spec,
            &data,
            &[1, 3, 6, 10],
            &[24, 27],
            &DEFAULT_MODE,
            false,
        );
        assert!(identical > 20, "{identical}");
    }
}

#[test]
fn field_lz_graph_follows_the_minimum_stream_size_and_decompression_level() {
    let spec = Spec::plain(12);
    let modes = [
        (0, 3),
        (1, 3),
        (10, 1),
        (100, 3),
        (1000, 3),
        (u32::MAX / 2, 3),
        (0, 1),
        (0, 2),
        (0, 5),
        (50, 9),
    ];
    for width in [2usize, 4, 8] {
        let data = typed(DataType::Struct, width, &[40, 300, 5_000]);
        let identical = matrix(&spec, &data, &[1, 6], &[24, 27], &modes, false);
        assert!(identical > 200, "{identical}");
    }
}

#[test]
fn field_lz_graph_honours_its_level_override_and_custom_graphs() {
    // Override of the node's level.
    for level_override in [-3, 0, 1, 3, 4, 9, 30] {
        let spec = Spec::with(12, &[(FIELD_LZ_LEVEL_OVERRIDE, level_override)], &[]);
        let data = typed(DataType::Struct, 4, &[300, 5_000]);
        matrix(&spec, &data, &[1, 6, 19], &[27], &DEFAULT_MODE, false);
    }
    // Custom graphs replacing each successor: store (2), zstd (9), the
    // entropy graph (7), bitpack (10), fse (3) and the field_lz literals
    // graph itself (45); an index past the list and a negative index fail.
    let customs = [2u32, 9, 7, 10, 3, 4, 11];
    let data = typed(DataType::Struct, 4, &[300, 5_000]);
    for slot in 0..5 {
        for index in [0, 1, 2, 3, 4, 5, 6] {
            let spec = Spec::with(12, &[(slot, index)], &customs);
            matrix(&spec, &data, &[3], &[27], &DEFAULT_MODE, false);
        }
        for bad in [7, 100, -1] {
            let spec = Spec::with(12, &[(slot, bad)], &customs);
            let data = &data[..4];
            for datum in data {
                let cfg = Cfg::new(3, 27);
                let outcome = compare("bad index", &spec, cfg, datum, false);
                // Streams under the minimum size never read the index.
                let _ = outcome;
            }
        }
    }
    // Several slots at once, with a minimum stream size that stores some.
    let spec = Spec::with(12, &[(0, 0), (2, 1), (4, 2)], &customs);
    let mut cfg = Cfg::new(6, 27);
    for min in [0, 50, 500, 5_000] {
        cfg.min_stream_size = min;
        for datum in &data {
            compare("multi slot", &spec, cfg, datum, false);
        }
    }
}

#[test]
fn field_lz_literals_graph_matches() {
    // The private literals graph takes struct input of any width.
    let spec = Spec::plain(45);
    let mut identical = 0;
    for width in [1usize, 2, 3, 4, 8, 12] {
        let data = typed(DataType::Struct, width, &[0, 17, 300, 5_000]);
        for decompression_level in [1u8, 3] {
            let mode = [(10, decompression_level)];
            identical += matrix(&spec, &data, &LEVELS, &[21, 24, 27], &mode, false);
        }
    }
    assert!(identical > 500, "{identical}");
}

/// Serial inputs that pick each branch of the channel selector.
fn channel_inputs() -> Vec<Data> {
    let mut out = serial(&[0, 1, 2, 19, 20, 21, 199, 200, 201, 1_000, 6_000, 40_000]);
    let mut add =
        |name: &str, bytes: Vec<u8>| out.push(Data::new(name, DataType::Serial, 1, bytes));
    add("constant-5", vec![5; 5]);
    add("constant-30", vec![9; 30]);
    add("constant-3000", vec![0; 3000]);
    // Monotonic: delta coding wins.
    add(
        "ramp",
        (0..4000u32).map(|i| (i / 3) as u8).collect::<Vec<u8>>(),
    );
    add(
        "steps",
        (0..4000u32)
            .map(|i| (i / 40 * 5) as u8)
            .collect::<Vec<u8>>(),
    );
    // Skewed: Huffman wins; flat: store or flatpack.
    add(
        "skewed",
        (0..8000u32)
            .map(|i| {
                let r = i.wrapping_mul(2_654_435_761) >> 24;
                if r < 200 { 0 } else { (r % 7) as u8 }
            })
            .collect(),
    );
    add(
        "two-symbols",
        (0..3000u32)
            .map(|i| {
                if i.wrapping_mul(2_654_435_761) >> 31 == 0 {
                    3
                } else {
                    250
                }
            })
            .collect(),
    );
    add(
        "sixteen-symbols",
        (0..3000u32)
            .map(|i| ((i.wrapping_mul(2_654_435_761) >> 20) % 16) as u8 * 16)
            .collect(),
    );
    // Text compresses better with zstd than with Huffman alone.
    add("text", support::text(9_000, 11));
    out
}

#[test]
fn field_lz_literals_channel_selector_matches() {
    let spec = Spec::plain(46);
    let data = channel_inputs();
    let mut identical = 0;
    for decompression_level in [1u8, 2, 3, 5] {
        let mode = [(10, decompression_level), (0, decompression_level)];
        identical += matrix(&spec, &data, &LEVELS, &[21, 24, 27], &mode, false);
    }
    assert!(identical > 1000, "{identical}");
}

#[test]
fn lz_graph_matches_on_serial_inputs() {
    let spec = Spec::plain(28);
    let mut identical = 0;
    let data = serial(&[0, 1, 5, 13, 64, 300, 2_500, 12_000]);
    identical += matrix(&spec, &data, &LEVELS, &VERSIONS, &DEFAULT_MODE, false);
    assert!(identical > 400, "{identical}");
}

#[test]
fn lz_graph_matches_on_large_inputs() {
    let spec = Spec::plain(28);
    // Above 8 KiB the offsets go to partition_bitpack; above 16 KiB of
    // literals or muxed bytes the pivco Huffman applies at format 27.
    let mut data = serial(&[100_000]);
    data.retain(|d| !d.name.contains("zeros"));
    data.push(Data::new(
        "text-300k",
        DataType::Serial,
        1,
        support::text(300_000, 21),
    ));
    let identical = matrix(&spec, &data, &[1, 3, 6], &[24, 27], &DEFAULT_MODE, false);
    assert!(identical > 20, "{identical}");
    let identical = matrix(&spec, &data[..3], &[10, 19], &[27], &DEFAULT_MODE, false);
    assert!(identical > 0, "{identical}");
}

#[test]
fn lz_graph_honours_its_parameters() {
    let data: Vec<Data> = serial(&[300, 9_000])
        .into_iter()
        .filter(|d| !d.name.contains("zeros"))
        .collect();
    // LZ node parameters forwarded by the graph: level, acceleration, window
    // log, strategy, hash logs and lengths, search log.
    for ints in [
        vec![(1, 1)],
        vec![(1, 5)],
        vec![(1, 12)],
        vec![(1, -3)],
        vec![(100, 4)],
        vec![(101, 12)],
        vec![(101, 18)],
        vec![(102, 1)],
        vec![(102, 3)],
        vec![(102, 5)],
        vec![(103, 10)],
        vec![(104, 12)],
        vec![(105, 6)],
        vec![(106, 5)],
        vec![(1, 4), (101, 20), (102, 4)],
    ] {
        let spec = Spec::with(28, &ints, &[]);
        matrix(&spec, &data, &[3, 10], &[27], &DEFAULT_MODE, false);
    }
    // Minimum Huffman gains.
    for ints in [
        vec![(10000, 0)],
        vec![(10000, -5)],
        vec![(10000, 100_000)],
        vec![(10001, 0)],
        vec![(10001, 50)],
        vec![(10001, 400)],
        vec![(10000, 10), (10001, 10)],
    ] {
        let spec = Spec::with(28, &ints, &[]);
        matrix(&spec, &data, &[3, 6], &[27], &DEFAULT_MODE, false);
    }
}

#[test]
fn lz_graph_honours_its_custom_graph_indices() {
    let data: Vec<Data> = serial(&[300, 9_000])
        .into_iter()
        .filter(|d| !d.name.contains("zeros"))
        .collect();
    // Custom graphs: store (2), huffman (4), fse (3), bitpack (10),
    // compress_small_lengths (69), zstd (9).
    let customs = [2u32, 4, 3, 10, 69, 9];
    // Literals, offsets, muxed bytes, overflow lengths.
    for key in [1000, 1001, 1002, 1003] {
        for index in [0, 1, 2, 3, 4, 5, 6, 99, -1] {
            let spec = Spec::with(28, &[(key, index)], &customs);
            matrix(&spec, &data, &[3], &[27], &DEFAULT_MODE, false);
        }
    }
    // All lengths to one graph: it needs two inputs, so it fails or not on
    // both sides alike.
    for index in [0, 1, 5, 6, -1] {
        let spec = Spec::with(28, &[(1004, index)], &customs);
        matrix(&spec, &data, &[3], &[27], &DEFAULT_MODE, false);
    }
    // Everything at once.
    let spec = Spec::with(28, &[(1000, 1), (1001, 3), (1002, 0), (1003, 4)], &customs);
    matrix(&spec, &data, &[1, 6, 19], &[24, 27], &DEFAULT_MODE, false);
}

/// Lengths-like numeric data: mostly small, some overflow values.
fn lengths(width: usize, n: usize, kind: &str) -> Vec<u8> {
    let noise = support::noise(n * 4, 31);
    let mut values: Vec<u64> = (0..n)
        .map(|i| {
            let r = u64::from(noise[i * 4]) | u64::from(noise[i * 4 + 1]) << 8;
            match kind {
                "tiny" => r % 3,
                "bytes" => r % 255,
                "sentinel" => 250 + r % 10,
                "outliers" => {
                    if r % 17 == 0 {
                        300 + r * 7 % 60_000
                    } else {
                        r % 40
                    }
                }
                "wide" => r * 1_000 + (r >> 3),
                _ => r,
            }
        })
        .collect();
    if width < 8 {
        let max = (1u64 << (8 * width)) - 1;
        for v in &mut values {
            *v = (*v).min(max);
        }
    }
    values
        .iter()
        .flat_map(|v| v.to_le_bytes()[..width].to_vec())
        .collect()
}

#[test]
// Formats without `sentinel_byte` (21) compress generically, which reaches the
// numeric transformer from level 7.
fn compress_small_lengths_matches() {
    let spec = Spec::plain(69);
    let mut identical = 0;
    for width in [1usize, 2, 4, 8] {
        let mut data = Vec::new();
        for n in [0usize, 1, 49, 50, 51, 99, 100, 101, 400, 5_000] {
            for kind in ["tiny", "bytes", "sentinel", "outliers", "wide", "random"] {
                data.push(Data::new(
                    format!("lengths-w{width}-{kind}-n{n}"),
                    DataType::Numeric,
                    width,
                    lengths(width, n, kind),
                ));
            }
        }
        identical += matrix(&spec, &data, &[1, 3, 6, 10], &VERSIONS, &DEFAULT_MODE, true);
    }
    assert!(identical > 500, "{identical}");
}

#[test]
fn compress_small_lengths_forwards_its_parameters_to_huffman() {
    let mut data = Vec::new();
    for width in [1usize, 2, 4] {
        for kind in ["tiny", "outliers"] {
            data.push(Data::new(
                format!("lengths-w{width}-{kind}"),
                DataType::Numeric,
                width,
                lengths(width, 2_000, kind),
            ));
        }
    }
    for ints in [
        vec![(66, 0), (80, 0)],
        vec![(66, 100_000)],
        vec![(80, 100)],
        vec![(66, 5), (80, 5)],
    ] {
        let spec = Spec::with(69, &ints, &[]);
        let identical = matrix(&spec, &data, &[3], &[24, 27], &DEFAULT_MODE, false);
        assert!(identical > 0);
    }
}
