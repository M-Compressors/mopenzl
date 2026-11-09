//! Byte identity of the numeric node encoders and graphs with the reference.
//!
//! Every node runs alone (`Graph::node(id, [STORE...])` in C, the equivalent
//! static graph here) over every element width and a set of data shapes, at
//! format versions 21, 24 and 27, with the default minimum stream size and
//! with a minimum of one byte so tiny inputs reach the node. A pair of runs
//! must agree: the same frame, or a failure on both sides. Every frame
//! decodes back to its input with this crate's decoder and the C decoder.
#![cfg(not(miri))]
#![cfg(all(
    feature = "compression",
    feature = "decompression",
    feature = "conversion"
))]
#![cfg(any(
    feature = "delta",
    feature = "zigzag",
    feature = "range-pack",
    feature = "divide-by",
    feature = "merge-sorted",
    feature = "partition",
    feature = "sparse-num",
    feature = "sentinel",
    feature = "bitsplit",
    feature = "float-deconstruct"
))]

mod support;

#[cfg(any(feature = "partition", feature = "merge-sorted"))]
use mopenzl::graph::GraphParams;
use mopenzl::graph::{CompressorGraph, GraphBuilder, LocalParams, StandardGraph, StandardNode};
use mopenzl::input::{Input, Numbers};
use mopenzl::{Compressor, Decompressor, EncodeError, EncoderConfig, FormatVersion, Level};
use openzl_ffi::graph::{self, Graph, Input as CInput, Params, cparam};

const VERSIONS: [u32; 3] = [21, 24, 27];
/// `(min stream size, store on expansion)`: the defaults, then a setup in
/// which every input reaches its node and the node's frame is always kept.
const MODES: [(u32, bool); 2] = [(10, true), (1, false)];

/// A deterministic xorshift stream.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn numbers(width: usize, values: impl IntoIterator<Item = u64>) -> Vec<u8> {
    values
        .into_iter()
        .flat_map(|value| value.to_le_bytes()[..width].to_vec())
        .collect()
}

fn random(width: usize, count: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed | 1);
    numbers(width, (0..count).map(|_| rng.next()))
}

#[cfg(any(
    feature = "delta",
    feature = "zigzag",
    feature = "range-pack",
    feature = "divide-by",
    feature = "partition",
    feature = "sparse-num",
    feature = "sentinel",
    feature = "bitsplit",
    feature = "float-deconstruct"
))]
/// Data shapes for a width: empty, tiny, ordered, constant, extreme, random.
fn shapes(width: usize) -> Vec<Vec<u8>> {
    let max = u64::MAX >> (64 - 8 * width);
    let n = if cfg!(miri) { 40 } else { 500 };
    vec![
        Vec::new(),
        random(width, 1, 1),
        random(width, 2, 2),
        numbers(width, (0..n as u64).map(|i| i * 3 + 7)),
        numbers(width, (0..n as u64 / 2).map(|i| 1000 - i * 2)),
        random(width, n, 3),
        vec![0xff; width * 33],
        vec![0; width * 40],
        numbers(width, (0..n as u64).map(|i| (i * 7919) % 200 + 100)),
        numbers(width, [0, max, 1, max - 1, max / 2, 0, 0, 5, 5, 5, max]),
    ]
}

/// A node under test with its parameters.
#[derive(Clone, Debug)]
struct NodeCase {
    node: u32,
    ints: Vec<(i32, i32)>,
    copies: Vec<(i32, Vec<u8>)>,
    outputs: usize,
}

impl NodeCase {
    fn new(node: u32, outputs: usize) -> Self {
        Self {
            node,
            ints: Vec::new(),
            copies: Vec::new(),
            outputs,
        }
    }

    #[cfg(any(
        feature = "divide-by",
        feature = "partition",
        feature = "sparse-num",
        feature = "sentinel",
        feature = "bitsplit"
    ))]
    fn copy(mut self, id: i32, bytes: Vec<u8>) -> Self {
        self.copies.push((id, bytes));
        self
    }

    #[cfg(feature = "partition")]
    fn int(mut self, id: i32, value: i32) -> Self {
        self.ints.push((id, value));
        self
    }

    fn c_graph(&self) -> Graph {
        Graph::Node {
            node: self.node,
            params: Params {
                ints: self.ints.clone(),
                copies: self.copies.clone(),
            },
            successors: vec![Graph::STORE; self.outputs],
        }
    }

    fn rust_graph(&self) -> CompressorGraph {
        let mut builder = GraphBuilder::new();
        let node = StandardNode::try_from(self.node).unwrap();
        let store = StandardGraph::Store.into();
        let graph = if self.ints.is_empty() && self.copies.is_empty() {
            builder.static_graph(node, &vec![store; self.outputs])
        } else {
            let mut params = LocalParams::default();
            for &(id, value) in &self.ints {
                params = params.with_int(id, value);
            }
            for (id, bytes) in &self.copies {
                params = params.with_bytes(*id, bytes.clone());
            }
            let node = builder.parameterize_node(node, params).unwrap();
            builder.static_graph(node, &vec![store; self.outputs])
        }
        .unwrap();
        builder.build(graph).unwrap()
    }
}

fn config(level: i32, version: u32, mode: (u32, bool)) -> EncoderConfig {
    EncoderConfig::default()
        .with_store_on_expansion(mode.1)
        .with_level(Level::try_from(level).unwrap())
        .with_format_version(FormatVersion::try_from(version).unwrap())
        .with_min_stream_size(mode.0)
}

fn c_params(config: &EncoderConfig) -> Vec<(i32, i32)> {
    vec![
        (cparam::LEVEL, config.level().get()),
        (
            cparam::FORMAT_VERSION,
            u32::from(config.format_version()) as i32,
        ),
        (cparam::MIN_STREAM_SIZE, config.min_stream_size() as i32),
        (
            cparam::STORE_ON_EXPANSION,
            1 + i32::from(!config.store_on_expansion()),
        ),
    ]
}

fn rust_frame(
    graph: &CompressorGraph,
    config: EncoderConfig,
    bytes: &[u8],
    width: usize,
) -> Result<Vec<u8>, EncodeError> {
    let mut compressor = Compressor::builder(config)
        .with_graph(graph.clone())
        .build()
        .unwrap();
    // Width 0 marks a serial input.
    let input = if width == 0 {
        Input::from(bytes)
    } else {
        Input::from(Numbers::try_from((bytes, width)).unwrap())
    };
    compressor.compress_inputs(&[input])
}

/// What one comparison found.
#[derive(Debug, Eq, PartialEq)]
enum Outcome {
    /// Both produced the same frame, which round-trips.
    Same,
    /// Both failed.
    BothFailed,
    /// Level 7 or above reaches the numeric transformer, which is not ported
    /// yet (the only comparison skipped).
    TransformerPending,
}

/// Compresses `bytes` through `c_graph` in C and `rust_graph` here and
/// checks the two agree.
fn compare(
    label: &str,
    c_graph: &Graph,
    rust_graph: &CompressorGraph,
    config: EncoderConfig,
    bytes: &[u8],
    width: usize,
) -> Outcome {
    let input = if width == 0 {
        CInput::Serial(bytes)
    } else {
        CInput::Numeric { bytes, width }
    };
    let reference = graph::compress(&[input], c_graph, &c_params(&config));
    let config_level = config.level().get();
    let ours = rust_frame(rust_graph, config, bytes, width);
    match (reference, ours) {
        (Ok(expected), Ok(frame)) => {
            assert_eq!(frame, expected, "{label}: frames differ");
            let decoded = Decompressor::default()
                .decompress_typed(&frame)
                .unwrap_or_else(|error| panic!("{label}: mopenzl cannot decode: {error}"));
            assert_eq!(decoded.len(), 1, "{label}");
            assert_eq!(decoded[0].content(), bytes, "{label}: round trip");
            let c_decoded = graph::decompress(&frame)
                .unwrap_or_else(|error| panic!("{label}: C cannot decode: {error:?}"));
            assert_eq!(c_decoded[0].bytes, bytes, "{label}: C round trip");
            Outcome::Same
        }
        (Err(_), Err(_)) => Outcome::BothFailed,
        (Ok(_), Err(_)) if config_level >= 7 && !support::transformer_ported() => {
            Outcome::TransformerPending
        }
        (Ok(_), Err(error)) => panic!("{label}: C succeeded, we failed: {error}"),
        // The oracle writes into a buffer of `compress_bound + 1024 + 64`
        // bytes, so a frame the node expands beyond it only fails there.
        (Err(_), Ok(frame)) if frame.len() > openzl_ffi::compress_bound(bytes.len()) + 1088 => {
            Outcome::BothFailed
        }
        (Err(code), Ok(_)) => panic!("{label}: C failed ({code:?}), we succeeded"),
    }
}

/// Runs `case` over `inputs` at every version and minimum stream size and
/// returns how many comparisons produced a frame.
fn run_case(label: &str, case: &NodeCase, inputs: &[(Vec<u8>, usize)]) -> usize {
    let c_graph = case.c_graph();
    let rust_graph = case.rust_graph();
    let mut frames = 0;
    for (bytes, width) in inputs {
        for version in VERSIONS {
            for mode in MODES {
                let name = format!(
                    "{label} width {width} len {} v{version} mode {mode:?}",
                    bytes.len()
                );
                let outcome = compare(
                    &name,
                    &c_graph,
                    &rust_graph,
                    config(6, version, mode),
                    bytes,
                    *width,
                );
                frames += usize::from(outcome == Outcome::Same);
            }
        }
    }
    frames
}

#[cfg(any(
    feature = "delta",
    feature = "zigzag",
    feature = "range-pack",
    feature = "divide-by",
    feature = "partition",
    feature = "sparse-num",
    feature = "sentinel",
    feature = "bitsplit",
    feature = "float-deconstruct"
))]
/// Every shape at every width.
fn all_shapes() -> Vec<(Vec<u8>, usize)> {
    [1usize, 2, 4, 8]
        .into_iter()
        .flat_map(|width| shapes(width).into_iter().map(move |bytes| (bytes, width)))
        .collect()
}

#[cfg(feature = "delta")]
#[test]
fn delta_int_is_byte_identical() {
    let frames = run_case("delta_int", &NodeCase::new(2, 1), &all_shapes());
    assert!(frames > 100);
}

#[cfg(feature = "zigzag")]
#[test]
fn zigzag_is_byte_identical() {
    let frames = run_case("zigzag", &NodeCase::new(4, 1), &all_shapes());
    assert!(frames > 100);
}

#[cfg(feature = "range-pack")]
#[test]
fn range_pack_is_byte_identical() {
    let mut inputs = all_shapes();
    for width in [2usize, 4, 8] {
        for minimum in [1u64, 1000, 1 << 20] {
            if width == 2 && minimum > 60_000 {
                continue;
            }
            let mut rng = Rng(minimum + width as u64);
            inputs.push((
                numbers(width, (0..300).map(|_| minimum + rng.next() % 200)),
                width,
            ));
        }
    }
    let frames = run_case("range_pack", &NodeCase::new(28, 1), &inputs);
    assert!(frames > 100);
}

#[cfg(feature = "divide-by")]
#[test]
fn divide_by_is_byte_identical_for_computed_and_given_divisors() {
    let computed = NodeCase::new(31, 1);
    let mut inputs = all_shapes();
    for width in [1usize, 2, 4, 8] {
        let max = u64::MAX >> (64 - 8 * width);
        inputs.push((
            numbers(width, (0..40u64).map(|i| (i * 6) % max.min(250))),
            width,
        ));
        inputs.push((numbers(width, [0, 0, 0]), width));
        inputs.push((numbers(width, [max / 5 * 5, 5, 10]), width));
        inputs.push((numbers(width, [12]), width));
        inputs.push((numbers(width, [0, 18, 30, 0, 42]), width));
    }
    assert!(run_case("divide_by auto", &computed, &inputs) > 100);
    for (divisor, name) in [
        (6u64, "6"),
        (7, "7"),
        (1, "1"),
        (0, "0"),
        (300, "300"),
        (70_000, "70000"),
    ] {
        let given = NodeCase::new(31, 1).copy(112, divisor.to_le_bytes().to_vec());
        run_case(&format!("divide_by given {name}"), &given, &inputs);
    }
    // A divisor that does not fit a 32-bit element, and a huge 64-bit one.
    for divisor in [1u64 << 33, u64::MAX, 1 << 63] {
        let given = NodeCase::new(31, 1).copy(112, divisor.to_le_bytes().to_vec());
        run_case("divide_by large", &given, &inputs);
    }
    // A parameter of the wrong size is an error for the node.
    for size in [0usize, 4, 9] {
        let wrong = NodeCase::new(31, 1).copy(112, vec![3; size]);
        run_case(&format!("divide_by {size}-byte param"), &wrong, &inputs);
    }
}

#[cfg(feature = "merge-sorted")]
#[test]
fn merge_sorted_is_byte_identical_for_every_run_count() {
    let case = NodeCase::new(29, 2);
    let mut inputs = Vec::new();
    for lists in [0usize, 1, 2, 7, 8, 9, 16, 17, 33, 63, 64, 65, 70] {
        let mut rng = Rng(lists as u64 + 5);
        let mut values = Vec::new();
        for _ in 0..lists {
            let mut next = rng.next() % 50;
            for _ in 0..(rng.next() % 40 + 1) {
                values.push(next);
                next += rng.next() % 30 + 1;
            }
        }
        inputs.push((numbers(4, values), 4));
    }
    inputs.push((numbers(4, [5, 5, 5, 5]), 4));
    inputs.push((numbers(4, [1, 2, 3, 3, 2, 1, 4]), 4));
    inputs.push((numbers(4, [u64::from(u32::MAX), 0, u64::from(u32::MAX)]), 4));
    // Other widths are rejected by the node.
    inputs.push((random(2, 50, 1), 2));
    inputs.push((random(8, 50, 2), 8));
    inputs.push((random(1, 50, 3), 1));
    let frames = run_case("merge_sorted", &case, &inputs);
    assert!(frames > 20);
}

#[cfg(feature = "partition")]
/// `ZL_PartitionParams` as the custom copy parameter: start, then sizes.
fn partition_param(start: u64, sizes: &[u64]) -> Vec<u8> {
    std::iter::once(start)
        .chain(sizes.iter().copied())
        .flat_map(u64::to_le_bytes)
        .collect()
}

#[cfg(feature = "partition")]
#[test]
fn partition_is_byte_identical_for_presets_and_custom_lists() {
    let mut inputs = all_shapes();
    for width in [2usize, 4, 8] {
        inputs.push((numbers(width, (1..400u64).map(|i| i * i % 5000 + 1)), width));
        inputs.push((numbers(width, (0..300u64).map(|i| i % 40)), width));
    }
    inputs.push((
        numbers(
            4,
            [
                1,
                2,
                3,
                4,
                1000,
                70_000,
                1 << 20,
                1 << 31,
                u64::from(u32::MAX),
            ],
        ),
        4,
    ));
    inputs.push((numbers(8, [1, 5, 1 << 40, 1 << 62, u64::MAX - 1]), 8));
    let mut frames = 0;
    for preset in [0, 1, 2, 3, 7, -1] {
        let case = NodeCase::new(48, 2).int(120, preset);
        frames += run_case(&format!("partition preset {preset}"), &case, &inputs);
    }
    let lists: Vec<(u64, Vec<u64>)> = vec![
        (0, vec![16, 16, 32, 64, 128]),
        (0, vec![256; 8]),
        (5, vec![1, 2, 4, 8, 16, 32, 64]),
        (0, vec![3, 5, 7, 11, 13, 100, 1000]),
        (10, vec![4, 8, 4]),
        (1, vec![1 << 63, 1 << 62]),
        (0, vec![1 << 63, 1 << 63]),
        (100, vec![1 << 20, 1 << 30, 1 << 40]),
        (0, vec![1; 256]),
        (0, vec![1; 257]),
        (0, vec![4]),
        (3, vec![1 << 14]),
        (0, vec![0, 4]),
        (u64::MAX, vec![1]),
        (u64::MAX, vec![2]),
        (0, vec![1 << 14; 3]),
        (7, (0..40).map(|i| 1u64 << (i % 20)).collect()),
    ];
    for (start, sizes) in &lists {
        let case = NodeCase::new(48, 2).copy(121, partition_param(*start, sizes));
        frames += run_case(
            &format!("partition {start} {sizes:?}")
                .chars()
                .take(60)
                .collect::<String>(),
            &case,
            &inputs,
        );
    }
    // Malformed custom parameters, and neither parameter.
    for bytes in [vec![], vec![1; 8], vec![1; 15], vec![0; 24], vec![0; 16]] {
        let case = NodeCase::new(48, 2).copy(121, bytes);
        run_case("partition malformed", &case, &inputs);
    }
    run_case(
        "partition without parameters",
        &NodeCase::new(48, 2),
        &inputs,
    );
    assert!(frames > 200, "{frames}");
}

#[cfg(feature = "sparse-num")]
#[test]
fn sparse_num_is_byte_identical() {
    let plain = NodeCase::new(54, 2);
    let mut inputs = all_shapes();
    for width in [1usize, 2, 4, 8] {
        let mut rng = Rng(width as u64 + 100);
        inputs.push((
            numbers(
                width,
                (0..3000).map(|_| {
                    if rng.next().is_multiple_of(9) {
                        rng.next() | 1
                    } else {
                        0
                    }
                }),
            ),
            width,
        ));
        inputs.push((
            numbers(width, vec![0; if width == 1 { 70_000 } else { 5_000 }]),
            width,
        ));
        inputs.push((numbers(width, [0, 0, 5]), width));
        inputs.push((numbers(width, [5, 0, 0]), width));
    }
    assert!(run_case("sparse_num", &plain, &inputs) > 100);
    // The automatic node: a vote, or an explicit dominant symbol.
    assert!(run_case("sparse_num_auto", &NodeCase::new(55, 2), &inputs) > 100);
    for dominant in [
        0u64,
        7,
        0x1234,
        0x12_3456,
        0x1234_5678,
        0x0123_4567_89ab_cdef,
        255,
        256,
    ] {
        let case = NodeCase::new(55, 2).copy(132, dominant.to_le_bytes().to_vec());
        let mut inputs = Vec::new();
        for width in [1usize, 2, 4, 8] {
            let mut rng = Rng(dominant + 1);
            inputs.push((
                numbers(
                    width,
                    (0..2000).map(|_| {
                        if rng.next().is_multiple_of(11) {
                            rng.next()
                        } else {
                            dominant
                        }
                    }),
                ),
                width,
            ));
            inputs.push((Vec::new(), width));
            inputs.push((numbers(width, [dominant]), width));
        }
        run_case(&format!("sparse_num_auto {dominant:#x}"), &case, &inputs);
    }
    for size in [0usize, 4, 9] {
        let case = NodeCase::new(55, 2).copy(132, vec![3; size]);
        run_case("sparse_num_auto wrong size", &case, &inputs);
    }
    // A dominant symbol on the plain node is ignored.
    let ignored = NodeCase::new(54, 2).copy(132, 5u64.to_le_bytes().to_vec());
    run_case("sparse_num ignores the parameter", &ignored, &inputs);
}

#[cfg(feature = "sentinel")]
#[test]
fn sentinel_nodes_are_byte_identical() {
    let mut inputs = all_shapes();
    for width in [2usize, 4, 8] {
        let mut rng = Rng(width as u64);
        inputs.push((
            numbers(
                width,
                (0..600).map(|_| {
                    if rng.next().is_multiple_of(20) {
                        rng.next()
                    } else {
                        rng.next() % 200
                    }
                }),
            ),
            width,
        ));
    }
    assert!(run_case("sentinel_byte", &NodeCase::new(50, 2), &inputs) > 50);
    let indices = |list: &[u64]| {
        list.iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<u8>>()
    };
    let mut frames = 0;
    for (list, name) in [
        (vec![], "none"),
        (vec![0u64], "first"),
        (vec![1, 5, 9], "three"),
        (vec![3, 1], "unordered"),
        (vec![2, 2], "repeated"),
        (vec![10_000], "out of range"),
        ((0..40).collect(), "prefix"),
    ] {
        let case = NodeCase::new(51, 2).copy(130, indices(&list));
        frames += run_case(&format!("sentinel_num {name}"), &case, &inputs);
        for value in [0u64, 7, 200, 255, 256, u64::MAX, 1 << 63] {
            let case = NodeCase::new(51, 2)
                .copy(130, indices(&list))
                .copy(131, value.to_le_bytes().to_vec());
            frames += run_case(
                &format!("sentinel_num {name} value {value:#x}"),
                &case,
                &inputs,
            );
        }
    }
    // Missing indices, a malformed sentinel value.
    run_case(
        "sentinel_num without indices",
        &NodeCase::new(51, 2),
        &inputs,
    );
    for size in [0usize, 4, 12] {
        let case = NodeCase::new(51, 2)
            .copy(130, indices(&[1]))
            .copy(131, vec![9; size]);
        run_case("sentinel_num wrong value size", &case, &inputs);
    }
    // Indices whose size is not a whole number of entries ignore the tail.
    let ragged = NodeCase::new(51, 2).copy(130, vec![1, 0, 0, 0, 0, 0, 0, 0, 9, 9]);
    run_case("sentinel_num ragged indices", &ragged, &inputs);
    assert!(frames > 100, "{frames}");
}

#[cfg(feature = "bitsplit")]
#[test]
fn bitsplit_nodes_are_byte_identical() {
    let mut inputs = all_shapes();
    for width in [1usize, 2, 4, 8] {
        inputs.push((random(width, 300, width as u64 + 40), width));
        inputs.push((numbers(width, (0..300u64).map(|i| i % 13)), width));
        inputs.push((numbers(width, (0..300u64).map(|i| i % 1000)), width));
    }
    let mut frames = 0;
    for (widths, name) in [
        (vec![8u8], "8"),
        (vec![4, 4], "4+4"),
        (vec![3, 5], "3+5"),
        (vec![1, 2, 4], "partial 1,2,4"),
        (vec![16], "16"),
        (vec![8, 8], "8+8"),
        (vec![12, 20], "12+20"),
        (vec![32], "32"),
        (vec![10, 22, 32], "10,22,32"),
        (vec![1; 64], "64 x 1"),
        (vec![1; 65], "65 x 1"),
        (vec![64], "64"),
        (vec![63, 1], "63+1"),
        (vec![17, 40, 7], "17,40,7"),
        (vec![0, 8], "zero width"),
        (vec![], "none"),
        (vec![9, 9, 9, 9, 9, 9, 9, 9], "8 x 9"),
        (vec![3; 21], "21 x 3"),
    ] {
        let case = NodeCase::new(75, 1).copy(701, widths);
        frames += run_case(&format!("bit_split {name}"), &case, &inputs);
    }
    run_case("bit_split without widths", &NodeCase::new(75, 1), &inputs);
    for node in [45, 46, 47] {
        frames += run_case(
            &format!("bitsplit node {node}"),
            &NodeCase::new(node, 1),
            &inputs,
        );
    }
    assert!(frames > 300, "{frames}");
}

#[cfg(feature = "float-deconstruct")]
#[test]
fn float_deconstruct_nodes_are_byte_identical() {
    let mut inputs = all_shapes();
    let mut rng = Rng(77);
    for width in [2usize, 4] {
        inputs.push((
            numbers(width, (0..500u64).map(|i| 0x3f80_0000 + i * 17)),
            width,
        ));
        inputs.push((numbers(width, (0..1000).map(|_| rng.next())), width));
    }
    let mut frames = 0;
    for node in [6, 7, 8] {
        frames += run_case(
            &format!("float deconstruct {node}"),
            &NodeCase::new(node, 2),
            &inputs,
        );
    }
    assert!(frames > 100, "{frames}");
}

#[cfg(any(
    feature = "partition",
    feature = "merge-sorted",
    all(feature = "delta", feature = "range-pack")
))]
const LEVELS: [i32; 6] = [1, 3, 6, 7, 10, 19];

#[cfg(any(
    feature = "partition",
    feature = "merge-sorted",
    all(feature = "delta", feature = "range-pack")
))]
/// Compares a standard graph run by `c_graph` and `rust_graph` over
/// `inputs` (width 0 is serial) at every level and version; returns the
/// outcomes seen.
fn run_graph(
    label: &str,
    c_graph: &Graph,
    rust_graph: &CompressorGraph,
    inputs: &[(Vec<u8>, usize)],
) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    for (bytes, width) in inputs {
        for level in LEVELS {
            for version in VERSIONS {
                let name = format!(
                    "{label} width {width} len {} level {level} v{version}",
                    bytes.len()
                );
                outcomes.push(compare(
                    &name,
                    c_graph,
                    rust_graph,
                    config(level, version, (10, true)),
                    bytes,
                    *width,
                ));
            }
        }
    }
    outcomes
}

#[cfg(any(
    feature = "partition",
    feature = "merge-sorted",
    all(feature = "delta", feature = "range-pack")
))]
/// A standard graph by ID on both sides.
fn standard(id: u32) -> (Graph, CompressorGraph) {
    (
        Graph::Id(id),
        CompressorGraph::standard(StandardGraph::try_from(id).unwrap()),
    )
}

#[cfg(any(feature = "partition", feature = "merge-sorted"))]
fn assert_some_identical(label: &str, outcomes: &[Outcome]) {
    let same = outcomes.iter().filter(|o| **o == Outcome::Same).count();
    eprintln!("{label}: {same} identical");
    assert!(same > 0, "{label}: no frame was compared");
}

#[cfg(feature = "partition")]
/// Numeric inputs of widths 2 and 4 with the distributions the partitioning
/// search reacts to.
fn partitionable() -> Vec<(Vec<u8>, usize)> {
    let mut inputs = Vec::new();
    for width in [2usize, 4] {
        let n = if cfg!(miri) { 60 } else { 12_000 };
        let mut rng = Rng(width as u64 * 31);
        let max = u64::MAX >> (64 - 8 * width);
        inputs.push((Vec::new(), width));
        inputs.push((random(width, 5, 1), width));
        inputs.push((random(width, 10, 2), width));
        inputs.push((random(width, n, 3), width));
        inputs.push((numbers(width, (0..n as u64).map(|i| i % 1000)), width));
        inputs.push((numbers(width, (0..n as u64).map(|i| (i * i) % 5000)), width));
        // Mostly tiny with rare huge values.
        inputs.push((
            numbers(
                width,
                (0..n).map(|_| {
                    if rng.next().is_multiple_of(50) {
                        max - rng.next() % 100
                    } else {
                        rng.next() % 5
                    }
                }),
            ),
            width,
        ));
        // Geometric: bit length uniform.
        inputs.push((
            numbers(
                width,
                (0..n).map(|_| {
                    rng.next() >> (64 - 8 * width as u32 + rng.next() as u32 % (8 * width as u32))
                }),
            ),
            width,
        ));
        inputs.push((numbers(width, vec![7; 2000]), width));
        inputs.push((numbers(width, (0..3000u64).map(|i| i % 3 * 20_000)), width));
    }
    inputs
}

#[cfg(feature = "partition")]
#[test]
fn partition_bitpack_graph_is_byte_identical() {
    let inputs = partitionable();
    let (c_graph, rust_graph) = standard(23);
    let outcomes = run_graph("partition_bitpack", &c_graph, &rust_graph, &inputs);
    assert_some_identical("partition_bitpack", &outcomes);
    // The optimal mode (int parameter 0 enabled).
    let c_optimal = Graph::Parameterized {
        graph: 23,
        params: Params {
            ints: vec![(0, 1)],
            copies: Vec::new(),
        },
        custom: Vec::new(),
    };
    let mut builder = GraphBuilder::new();
    let graph = builder
        .parameterize_graph(
            StandardGraph::PartitionBitpack,
            GraphParams::default().with_params(LocalParams::default().with_int(0, 1)),
        )
        .unwrap();
    let rust_optimal = builder.build(graph).unwrap();
    let outcomes = run_graph(
        "partition_bitpack optimal",
        &c_optimal,
        &rust_optimal,
        &inputs,
    );
    assert_some_identical("partition_bitpack optimal", &outcomes);
    // Widths the graph rejects.
    let others = [(random(1, 100, 1), 1), (random(8, 100, 2), 8)];
    run_graph(
        "partition_bitpack bad width",
        &c_graph,
        &rust_graph,
        &others,
    );
}

#[cfg(feature = "merge-sorted")]
#[test]
fn merge_sorted_graph_is_byte_identical() {
    let mut inputs = Vec::new();
    for lists in [0usize, 1, 3, 40, 64, 65, 100] {
        let mut rng = Rng(lists as u64 + 9);
        let mut values = Vec::new();
        for _ in 0..lists {
            let mut next = rng.next() % 50;
            for _ in 0..(rng.next() % 30 + 1) {
                values.push(next);
                next += rng.next() % 30 + 1;
            }
        }
        inputs.push((numbers(4, values), 4));
    }
    inputs.push((random(2, 200, 3), 2));
    inputs.push((random(8, 200, 4), 8));
    let c_graph = Graph::Parameterized {
        graph: 63,
        params: Params::default(),
        custom: vec![Graph::node(29, [Graph::STORE, Graph::STORE]), Graph::STORE],
    };
    let mut builder = GraphBuilder::new();
    let merge = builder
        .static_graph(
            StandardNode::MergeSorted,
            &[StandardGraph::Store.into(), StandardGraph::Store.into()],
        )
        .unwrap();
    let graph = builder
        .parameterize_graph(
            StandardGraph::MergeSorted,
            GraphParams::default().with_graphs(vec![merge, StandardGraph::Store.into()]),
        )
        .unwrap();
    let rust_graph = builder.build(graph).unwrap();
    let outcomes = run_graph("merge_sorted graph", &c_graph, &rust_graph, &inputs);
    assert_some_identical("merge_sorted graph", &outcomes);
    // Without its custom graphs the graph fails on both sides.
    let (bare_c, bare_rust) = standard(63);
    let outcomes = run_graph(
        "merge_sorted graph bare",
        &bare_c,
        &bare_rust,
        &inputs[2..4],
    );
    assert!(outcomes.iter().all(|o| *o == Outcome::BothFailed));
}

/// The static private graphs headed by the numeric nodes: delta, range pack
/// and the `interpret_numN_compress` readers.
#[cfg(all(feature = "delta", feature = "range-pack"))]
#[test]
fn static_numeric_graphs_are_byte_identical() {
    let numeric_inputs: Vec<(Vec<u8>, usize)> = [1usize, 2, 4, 8]
        .into_iter()
        .flat_map(|width| {
            [
                (random(width, 2000, 5), width),
                (numbers(width, (0..3000u64).map(|i| i * 3 + 100)), width),
                (numbers(width, (0..3000u64).map(|i| 1000 + i % 90)), width),
                (numbers(width, [9]), width),
                (Vec::new(), width),
            ]
        })
        .collect();
    let serial_inputs: Vec<(Vec<u8>, usize)> = [
        random(1, 4000, 6),
        numbers(1, (0..4000u64).map(|i| i % 200)),
        numbers(4, (0..1000u64).map(|i| i * 5)),
        Vec::new(),
        vec![3; 100],
    ]
    .into_iter()
    .map(|bytes| (bytes, 0))
    .collect();
    let mut compared = 0;
    for (id, name, serial) in [
        (47, "delta_huffman", true),
        (48, "delta_flatpack", true),
        (49, "delta_zstd", true),
        (50, "delta_huffman_internal", false),
        (51, "flatpack_internal", false),
        (52, "zstd_internal", false),
        (53, "delta_field_lz", false),
        (54, "range_pack", false),
        (55, "range_pack_zstd", false),
        (56, "tokenize_delta_field_lz", false),
        (65, "interpret_num8_compress", true),
        (66, "interpret_num16_compress", true),
        (67, "interpret_num32_compress", true),
        (68, "interpret_num64_compress", true),
    ] {
        let (c_graph, rust_graph) = standard(id);
        let inputs = if serial {
            &serial_inputs
        } else {
            &numeric_inputs
        };
        let outcomes = run_graph(name, &c_graph, &rust_graph, inputs);
        compared += outcomes.iter().filter(|o| **o == Outcome::Same).count();
    }
    assert!(compared > 0);
}
