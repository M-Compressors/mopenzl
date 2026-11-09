//! Byte identity of the structure encoders (tokenize, split, split-by-struct,
//! dispatch, concat, dedup, interleave, prefix, parse-int, transpose) and of
//! their graphs with the reference, and round trips through both decoders.
//!
//! Every node is run from a function graph that stores all its outputs, so the
//! frame holds exactly the node's output streams, in order, and its codec
//! header. The reference side runs the same node from an identical function
//! graph (`openzl_ffi::structure`).
#![cfg(all(
    feature = "compression",
    feature = "decompression",
    feature = "zstd",
    feature = "conversion",
    feature = "tokenize",
    feature = "split",
    feature = "split-by-struct",
    feature = "dispatch",
    feature = "concat",
    feature = "dedup",
    feature = "interleave",
    feature = "prefix",
    feature = "parse-int",
    feature = "transpose"
))]

mod support;

use mopenzl::graph::{
    CompressorGraph, DispatchInstructions, Edge, FunctionGraph, GraphBuilder, GraphContext,
    GraphError, GraphParams, LocalParams, StandardGraph, StandardNode, TypeSet,
};
use mopenzl::input::{Input, Numbers, Records, Strings};
use mopenzl::{
    Compressor, DataType, Decompressor, EncodeError, EncoderConfig, FormatVersion, Level,
};
use openzl_ffi::enc_structure::{self, SplitGraph};
use openzl_ffi::graph::{self, Graph, Input as CInput, cparam};
use openzl_ffi::structure::{self, Node, Run};

const VERSIONS: [u32; 3] = [21, 24, 27];

fn config(level: i32, version: u32) -> EncoderConfig {
    EncoderConfig::default()
        .with_level(Level::try_from(level).unwrap())
        .with_format_version(FormatVersion::try_from(version).unwrap())
        // A lone stream below the minimum goes straight to the store, never
        // reaching the node under test.
        .with_min_stream_size(1)
}

fn c_params(config: &EncoderConfig) -> Vec<(i32, i32)> {
    vec![
        (cparam::LEVEL, config.level().get()),
        (
            cparam::FORMAT_VERSION,
            u32::from(config.format_version()) as i32,
        ),
        (
            cparam::CONTENT_CHECKSUM,
            1 + i32::from(!config.content_checksum()),
        ),
        (
            cparam::COMPRESSED_CHECKSUM,
            1 + i32::from(!config.compressed_checksum()),
        ),
        (cparam::PERMISSIVE, 1 + i32::from(!config.permissive())),
        (cparam::MIN_STREAM_SIZE, config.min_stream_size() as i32),
        (
            cparam::STORE_ON_EXPANSION,
            1 + i32::from(!config.store_on_expansion()),
        ),
    ]
}

/// One typed input in both representations.
#[derive(Clone, Debug)]
enum In {
    Serial(Vec<u8>),
    Struct(Vec<u8>, usize),
    Numeric(Vec<u8>, usize),
    Strings(Vec<u8>, Vec<u32>),
}

impl In {
    fn rust(&self) -> Input<'_> {
        match self {
            In::Serial(bytes) => Input::from(&bytes[..]),
            In::Struct(bytes, width) => {
                Input::from(Records::try_from((&bytes[..], *width)).unwrap())
            }
            In::Numeric(bytes, width) => {
                Input::from(Numbers::try_from((&bytes[..], *width)).unwrap())
            }
            In::Strings(bytes, lens) => {
                Input::from(Strings::try_from((&bytes[..], &lens[..])).unwrap())
            }
        }
    }

    fn c(&self) -> CInput<'_> {
        match self {
            In::Serial(bytes) => CInput::Serial(bytes),
            In::Struct(bytes, width) => CInput::Struct {
                bytes,
                width: *width,
            },
            In::Numeric(bytes, width) => CInput::Numeric {
                bytes,
                width: *width,
            },
            In::Strings(bytes, lens) => CInput::String { bytes, lens },
        }
    }
}

fn rust_inputs(inputs: &[In]) -> Vec<Input<'_>> {
    inputs.iter().map(In::rust).collect()
}

fn c_inputs(inputs: &[In]) -> Vec<CInput<'_>> {
    inputs.iter().map(In::c).collect()
}

/// Runs a node over all inputs and stores every output.
struct RunNode {
    node: StandardNode,
    params: Option<LocalParams>,
}

fn store_all(cx: &mut GraphContext<'_>, outputs: Vec<Edge>) -> Result<(), GraphError> {
    for output in outputs {
        cx.set_destination(&[output], StandardGraph::Store)?;
    }
    Ok(())
}

impl FunctionGraph for RunNode {
    fn compress(&self, cx: &mut GraphContext<'_>, inputs: &[Edge]) -> Result<(), GraphError> {
        let outputs = match &self.params {
            Some(params) => cx.run_node_with(self.node, inputs, params)?,
            None => cx.run_node(self.node, inputs)?,
        };
        store_all(cx, outputs)
    }
}

/// Runs `GraphContext::run_split_node`.
struct RunSplit(Vec<usize>);

impl FunctionGraph for RunSplit {
    fn compress(&self, cx: &mut GraphContext<'_>, inputs: &[Edge]) -> Result<(), GraphError> {
        let outputs = cx.run_split_node(inputs[0], &self.0)?;
        store_all(cx, outputs)
    }
}

/// Runs `GraphContext::run_dispatch_node`.
struct RunDispatch(DispatchInstructions);

impl FunctionGraph for RunDispatch {
    fn compress(&self, cx: &mut GraphContext<'_>, inputs: &[Edge]) -> Result<(), GraphError> {
        let outputs = cx.run_dispatch_node(inputs[0], &self.0)?;
        store_all(cx, outputs)
    }
}

/// Runs `GraphContext::run_dispatch_string_node`.
struct RunDispatchString(usize, Vec<u16>);

impl FunctionGraph for RunDispatchString {
    fn compress(&self, cx: &mut GraphContext<'_>, inputs: &[Edge]) -> Result<(), GraphError> {
        let outputs = cx.run_dispatch_string_node(inputs[0], self.0, &self.1)?;
        store_all(cx, outputs)
    }
}

fn rust_function(
    function: impl FunctionGraph,
    config: EncoderConfig,
    inputs: &[In],
) -> Result<Vec<u8>, EncodeError> {
    let mut builder = GraphBuilder::new();
    let graph = builder
        .function_graph("structure", function, &[TypeSet::ANY], true, &[], &[])
        .unwrap();
    let mut compressor = Compressor::builder(config)
        .with_graph(builder.build(graph).unwrap())
        .build()
        .unwrap();
    compressor.compress_inputs(&rust_inputs(inputs))
}

fn node(id: u32) -> StandardNode {
    StandardNode::try_from(id).unwrap()
}

fn byte_params(ints: &[(i32, i32)], copies: &[(i32, Vec<u8>)]) -> Option<LocalParams> {
    if ints.is_empty() && copies.is_empty() {
        return None;
    }
    let mut params = LocalParams::default();
    for &(id, value) in ints {
        params = params.with_int(id, value);
    }
    for (id, bytes) in copies {
        params = params.with_bytes(*id, bytes.clone());
    }
    Some(params)
}

/// Checks the frame decodes back to `inputs` with both decoders.
fn assert_round_trip(frame: &[u8], inputs: &[In], what: &str) {
    let ours = Decompressor::default()
        .decompress_typed(frame)
        .unwrap_or_else(|error| panic!("{what}: mopenzl cannot decode: {error}"));
    let theirs = graph::decompress(frame)
        .unwrap_or_else(|error| panic!("{what}: the reference cannot decode: {error:?}"));
    assert_eq!(ours.len(), inputs.len(), "{what}");
    assert_eq!(theirs.len(), inputs.len(), "{what}");
    for (n, input) in inputs.iter().enumerate() {
        let (ty, bytes, lens): (DataType, &[u8], &[u32]) = match input {
            In::Serial(bytes) => (DataType::Serial, bytes, &[]),
            In::Struct(bytes, _) => (DataType::Struct, bytes, &[]),
            In::Numeric(bytes, _) => (DataType::Numeric, bytes, &[]),
            In::Strings(bytes, lens) => (DataType::String, bytes, lens),
        };
        assert_eq!(ours[n].data_type(), ty, "{what} output {n}");
        assert_eq!(ours[n].content(), bytes, "{what} output {n}");
        assert_eq!(ours[n].string_lens(), lens, "{what} output {n}");
        assert_eq!(theirs[n].bytes, bytes, "{what} output {n} (reference)");
        assert_eq!(theirs[n].lens, lens, "{what} output {n} (reference)");
    }
}

/// Compares the frames of `rust` and `reference`: equal bytes, or both failing.
/// Returns whether they succeeded.
fn compare(
    what: &str,
    reference: Result<Vec<u8>, openzl_ffi::Error>,
    rust: Result<Vec<u8>, EncodeError>,
    inputs: &[In],
) -> bool {
    match (reference, rust) {
        (Ok(expected), Ok(frame)) => {
            assert_eq!(frame, expected, "{what}");
            assert_round_trip(&frame, inputs, what);
            true
        }
        (Err(c), Err(r)) => {
            eprintln!("{what}: both failed: {c:?} / {r}");
            false
        }
        (Ok(_), Err(error)) => panic!("{what}: only mopenzl failed: {error}"),
        (Err(error), Ok(_)) => panic!("{what}: only the reference failed: {error:?}"),
    }
}

/// Runs node `id` in both libraries at every format version.
fn check_node(
    what: &str,
    id: u32,
    ints: &[(i32, i32)],
    copies: &[(i32, Vec<u8>)],
    inputs: &[In],
) -> bool {
    let mut ok = false;
    for version in VERSIONS {
        let cfg = config(3, version);
        let reference = structure::compress(
            &c_inputs(inputs),
            &Run::RawNode {
                node: id,
                ints: ints.to_vec(),
                copies: copies.to_vec(),
            },
            &c_params(&cfg),
        );
        let rust = rust_function(
            RunNode {
                node: node(id),
                params: byte_params(ints, copies),
            },
            cfg,
            inputs,
        );
        ok = compare(&format!("{what} v{version}"), reference, rust, inputs);
    }
    ok
}

// -----------------------------------------------------------------------
// Data
// -----------------------------------------------------------------------

fn words(n: usize, seed: u32) -> Vec<u64> {
    let bytes = support::noise(n * 8, seed);
    bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|chunk| u64::from_le_bytes(*chunk))
        .collect()
}

fn le_bytes(values: &[u64], width: usize) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes()[..width].to_vec())
        .collect()
}

fn size_bytes(sizes: &[usize]) -> Vec<u8> {
    sizes.iter().flat_map(|size| size.to_ne_bytes()).collect()
}

fn scale(full: usize, small: usize) -> usize {
    if cfg!(miri) { small } else { full }
}

/// Numeric streams of width `width` with an alphabet of every interesting size.
fn numeric_cases(width: usize) -> Vec<(String, Vec<u8>)> {
    let n = scale(3000, 60);
    let max = if width == 8 {
        u64::MAX
    } else {
        (1u64 << (width * 8)) - 1
    };
    let mut cases: Vec<(String, Vec<u64>)> = vec![
        ("empty".into(), vec![]),
        ("one".into(), vec![7]),
        ("constant".into(), vec![max / 3; 40]),
        (
            "few-descending".into(),
            (0..n as u64).map(|i| 9 - i % 10).collect(),
        ),
        (
            "alphabet-200".into(),
            words(n, 3).iter().map(|v| v % 200).collect(),
        ),
        (
            "alphabet-256".into(),
            (0..n as u64).map(|i| (i * 7) % 256).collect(),
        ),
        (
            "alphabet-257".into(),
            (0..n as u64).map(|i| (i * 7) % 257).collect(),
        ),
        (
            "random".into(),
            words(n, 4).iter().map(|v| v & max).collect(),
        ),
    ];
    if width >= 2 {
        cases.push((
            "alphabet-65536".into(),
            (0..scale(70_000, 100) as u64)
                .map(|i| (i * 5) % 65_536)
                .collect(),
        ));
        cases.push((
            "alphabet-65537".into(),
            (0..scale(70_000, 100) as u64)
                .map(|i| (i * 5) % 65_537)
                .collect(),
        ));
    }
    if width == 2 {
        // The reference has a specialized kernel for 5000 elements or more.
        cases.push((
            "kernel-100".into(),
            (0..scale(6000, 100) as u64)
                .map(|i| (i * 31) % 100 + 5)
                .collect(),
        ));
        cases.push((
            "kernel-300".into(),
            (0..scale(6000, 100) as u64)
                .map(|i| (i * 31) % 300 + 5)
                .collect(),
        ));
    }
    cases
        .into_iter()
        .map(|(name, values)| {
            (
                name,
                le_bytes(&values.iter().map(|v| v & max).collect::<Vec<_>>(), width),
            )
        })
        .collect()
}

// -----------------------------------------------------------------------
// Tokenize
// -----------------------------------------------------------------------

const TOKENIZE_STRUCT: u32 = 40;
const TOKENIZE_NUMERIC: u32 = 41;
const TOKENIZE_STRING: u32 = 42;
const TOKENIZE_SORTED: u32 = 72;
const TOKENIZE_STRING_SORTED: u32 = 73;

#[test]
fn tokenize_numeric_matches_the_reference_at_every_width() {
    for width in [1, 2, 4, 8] {
        for (name, bytes) in numeric_cases(width) {
            let input = [In::Numeric(bytes, width)];
            for (id, ints) in [
                (TOKENIZE_NUMERIC, vec![]),
                (TOKENIZE_SORTED, vec![]),
                (TOKENIZE_NUMERIC, vec![(0, 1)]),
                (TOKENIZE_NUMERIC, vec![(0, 0)]),
                (TOKENIZE_SORTED, vec![(0, 0)]),
            ] {
                assert!(
                    check_node(
                        &format!("tokenize {id} {ints:?} w{width} {name}"),
                        id,
                        &ints,
                        &[],
                        &input
                    ),
                    "tokenize {id} {ints:?} w{width} {name} must succeed"
                );
            }
        }
    }
}

#[test]
fn tokenize_struct_matches_the_reference() {
    for width in [1, 2, 4, 8] {
        for (name, bytes) in numeric_cases(width) {
            let input = [In::Struct(bytes, width)];
            assert!(check_node(
                &format!("tokenize_struct w{width} {name}"),
                TOKENIZE_STRUCT,
                &[],
                &[],
                &input
            ));
            // The sort parameter only works on numerics. (An empty lone input
            // never reaches the node.)
            assert!(
                (name == "empty")
                    == check_node(
                        &format!("tokenize_struct sorted w{width} {name}"),
                        TOKENIZE_STRUCT,
                        &[(0, 1)],
                        &[],
                        &input
                    ),
                "sorted struct w{width} {name}"
            );
        }
    }
}

#[test]
fn tokenize_struct_rejects_unsupported_widths() {
    for width in [3, 5, 16] {
        let input = [In::Struct(support::noise(width * 40, 5), width)];
        assert!(!check_node(
            &format!("tokenize_struct w{width}"),
            TOKENIZE_STRUCT,
            &[],
            &[],
            &input
        ));
    }
}

fn string_cases() -> Vec<(String, Vec<u8>, Vec<u32>)> {
    let split = |parts: &[&str]| -> (Vec<u8>, Vec<u32>) {
        (
            parts.concat().into_bytes(),
            parts.iter().map(|part| part.len() as u32).collect(),
        )
    };
    let mut cases = Vec::new();
    for (name, parts) in [
        ("empty", vec![]),
        ("one-empty-string", vec![""]),
        (
            "mixed",
            vec!["b", "a", "", "ab", "b", "a", "abc", "", "ba", "a"],
        ),
        ("prefixes", vec!["abc", "ab", "abcd", "a", "abc", "ab"]),
        ("high-bytes", vec!["\u{e9}a", "\u{e9}", "z", "\u{e9}a"]),
    ] {
        let (bytes, lens) = split(&parts);
        cases.push((name.to_string(), bytes, lens));
    }
    let n = scale(4000, 80);
    let text = support::text(n * 6, 7);
    let lens: Vec<u32> = (0..n).map(|i| (i % 7) as u32).collect();
    let total: usize = lens.iter().map(|&l| l as usize).sum();
    cases.push(("text-pieces".into(), text[..total].to_vec(), lens));
    // More than 256 and more than 65536 distinct strings.
    for (name, count) in [
        ("distinct-300", 300usize),
        ("distinct-66000", scale(66_000, 400)),
    ] {
        let mut bytes = Vec::new();
        let mut lens = Vec::new();
        for i in 0..count * 2 {
            let piece = format!("k{}", (i * 2_654_435_761usize) % count);
            bytes.extend_from_slice(piece.as_bytes());
            lens.push(piece.len() as u32);
        }
        cases.push((name.to_string(), bytes, lens));
    }
    cases
}

#[test]
fn tokenize_string_matches_the_reference() {
    for (name, bytes, lens) in string_cases() {
        let input = [In::Strings(bytes, lens)];
        for (id, ints) in [
            (TOKENIZE_STRING, vec![]),
            (TOKENIZE_STRING_SORTED, vec![]),
            (TOKENIZE_STRING, vec![(0, 1)]),
            (TOKENIZE_STRING_SORTED, vec![(0, 0)]),
        ] {
            assert!(
                check_node(
                    &format!("tokenize_string {id} {ints:?} {name}"),
                    id,
                    &ints,
                    &[],
                    &input
                ),
                "{name}"
            );
        }
    }
}

// -----------------------------------------------------------------------
// Split
// -----------------------------------------------------------------------

const SPLIT_N: u32 = 66;
const SPLIT_N_STRUCT: u32 = 67;
const SPLIT_N_NUM: u32 = 68;
const SPLIT_BYRANGE: u32 = 49;
const SEGMENT_SIZES_PID: i32 = 323;

fn split_input(kind: u32, n_elts: usize, width: usize) -> In {
    let bytes = support::noise(n_elts * width, 11 + kind);
    match kind {
        SPLIT_N => In::Serial(bytes),
        SPLIT_N_STRUCT => In::Struct(bytes, width),
        _ => In::Numeric(bytes, width),
    }
}

#[test]
fn split_n_matches_the_reference() {
    let sizes_cases: [(&str, usize, Vec<usize>); 9] = [
        ("halves", 100, vec![50, 50]),
        ("last-takes-the-rest", 100, vec![20, 30, 0]),
        ("one-segment", 100, vec![100]),
        ("empty-segments", 100, vec![0, 100, 0, 0]),
        ("empty-input-one-empty", 0, vec![0]),
        ("empty-input-no-segments", 0, vec![]),
        ("too-long", 100, vec![60, 60]),
        ("does-not-cover", 100, vec![10, 20]),
        ("no-segments", 100, vec![]),
    ];
    for (kind, width) in [
        (SPLIT_N, 1),
        (SPLIT_N_STRUCT, 3),
        (SPLIT_N_STRUCT, 8),
        (SPLIT_N_NUM, 2),
        (SPLIT_N_NUM, 8),
    ] {
        for (name, n_elts, sizes) in &sizes_cases {
            let inputs = [split_input(kind, *n_elts, width)];
            let copies = [(SEGMENT_SIZES_PID, size_bytes(sizes))];
            let expected_ok =
                !name.starts_with("too-") && !name.starts_with("does-") && *name != "no-segments";
            let ok = check_node(
                &format!("splitN {kind} w{width} {name}"),
                kind,
                &[],
                &copies,
                &inputs,
            );
            assert_eq!(ok, expected_ok, "splitN {kind} w{width} {name}");
        }
    }
}

#[test]
fn run_split_node_matches_the_reference() {
    for (kind, width) in [(SPLIT_N, 1), (SPLIT_N_STRUCT, 5), (SPLIT_N_NUM, 4)] {
        for sizes in [vec![10, 0], vec![1, 2, 3, 94], vec![100], vec![50, 49]] {
            let inputs = [split_input(kind, 100, width)];
            for version in VERSIONS {
                let cfg = config(3, version);
                let reference = structure::compress(
                    &c_inputs(&inputs),
                    &Run::RawNode {
                        node: kind,
                        ints: vec![],
                        copies: vec![(SEGMENT_SIZES_PID, size_bytes(&sizes))],
                    },
                    &c_params(&cfg),
                );
                let rust = rust_function(RunSplit(sizes.clone()), cfg, &inputs);
                compare(
                    &format!("run_split {kind} {sizes:?} v{version}"),
                    reference,
                    rust,
                    &inputs,
                );
            }
        }
    }
    // There is no split node for strings.
    let strings = [In::Strings(b"abc".to_vec(), vec![1, 2])];
    assert!(rust_function(RunSplit(vec![2]), config(3, 27), &strings).is_err());
}

/// Ranges that jump: `segments` of `(elements, low, span)`.
fn ranged(width: usize, segments: &[(usize, u64, u64)], seed: u32) -> Vec<u8> {
    let mut values = Vec::new();
    let noise = words(segments.iter().map(|s| s.0).sum::<usize>() + 1, seed);
    for &(count, low, span) in segments {
        for _ in 0..count {
            values.push(low + noise[values.len()] % (span + 1));
        }
    }
    le_bytes(&values, width)
}

#[test]
fn split_byrange_matches_the_reference() {
    let big = scale(1000, 300);
    for width in [1, 2, 4, 8] {
        let max = if width == 8 {
            u64::MAX / 2
        } else {
            (1u64 << (width * 8)) - 1
        };
        let q = max / 4;
        let ramp: Vec<u64> = (0..big as u64 * 3).map(|i| i % (max.min(60_000))).collect();
        let cases: Vec<(String, Vec<u8>)> = vec![
            ("empty".into(), vec![]),
            ("tiny".into(), le_bytes(&[3, 4, 5], width)),
            ("constant".into(), le_bytes(&vec![9u64; big * 2], width)),
            (
                "three-ranges".into(),
                ranged(
                    width,
                    &[(big, 0, q / 20), (big, q * 2, q / 20), (big, q, q / 20)],
                    21,
                ),
            ),
            (
                "two-ranges-wide".into(),
                ranged(width, &[(big * 2, 0, q / 4), (big * 2, q * 3, q / 4)], 22),
            ),
            (
                "noise".into(),
                le_bytes(
                    &words(big * 3, 23)
                        .iter()
                        .map(|v| v & max)
                        .collect::<Vec<_>>(),
                    width,
                ),
            ),
            ("ramp".into(), le_bytes(&ramp, width)),
            (
                "many-short-ranges".into(),
                ranged(
                    width,
                    &(0..10)
                        .map(|i| (big / 2, (i % 3) * q, q / 50))
                        .collect::<Vec<_>>(),
                    24,
                ),
            ),
        ];
        for (name, bytes) in cases {
            let input = [In::Numeric(bytes, width)];
            for ints in [
                vec![],
                vec![(324, 8)],
                vec![(324, 64)],
                vec![(324, 1)],
                vec![(324, 100_000)],
            ] {
                assert!(
                    check_node(
                        &format!("split_byrange w{width} {name} {ints:?}"),
                        SPLIT_BYRANGE,
                        &ints,
                        &[],
                        &input
                    ),
                    "split_byrange w{width} {name} {ints:?}"
                );
            }
        }
    }
}

#[test]
fn split_byrange_with_a_negative_minimum_matches_the_reference() {
    let input = [In::Numeric(
        ranged(2, &[(300, 0, 10), (300, 5000, 10)], 31),
        2,
    )];
    check_node("split_byrange -1", SPLIT_BYRANGE, &[(324, -1)], &[], &input);
    check_node("split_byrange 0", SPLIT_BYRANGE, &[(324, 0)], &[], &input);
}

// -----------------------------------------------------------------------
// Split by struct
// -----------------------------------------------------------------------

const SPLIT_BY_STRUCT: u32 = 69;
const FIELD_SIZES_PID: i32 = 387;

#[test]
fn split_by_struct_matches_the_reference() {
    let cases: [(&str, usize, Vec<usize>); 9] = [
        ("three-fields", 40, vec![1, 2, 5]),
        ("one-field", 33, vec![3]),
        ("wide-fields", 17, vec![9, 9, 16]),
        ("small-fields", 100, vec![1, 1, 2, 4, 8]),
        ("empty-input", 0, vec![4, 4]),
        ("one-struct", 1, vec![2, 3, 4]),
        ("zero-sum", 10, vec![]),
        ("zero-field", 10, vec![4, 0, 4]),
        ("not-a-multiple", 0, vec![3]),
    ];
    for (name, count, fields) in cases {
        let size: usize = fields.iter().sum();
        let mut bytes = support::noise(count * size, 41);
        if name == "not-a-multiple" {
            bytes = support::noise(10, 42);
        }
        let inputs = [In::Serial(bytes)];
        let ok = check_node(
            &format!("split_by_struct {name}"),
            SPLIT_BY_STRUCT,
            &[],
            &[(FIELD_SIZES_PID, size_bytes(&fields))],
            &inputs,
        );
        assert_eq!(
            ok,
            !matches!(name, "zero-field" | "not-a-multiple"), // zero-sum: empty input is stored
            "{name}"
        );
    }
    // The parameter is required.
    assert!(!check_node(
        "split_by_struct without parameter",
        SPLIT_BY_STRUCT,
        &[],
        &[],
        &[In::Serial(vec![1, 2, 3])]
    ));
}

// -----------------------------------------------------------------------
// Dispatch
// -----------------------------------------------------------------------

const DISPATCH_STRING: u32 = 32;

/// `(name, input size, segment sizes, tags, tag count, expected success)`.
type DispatchCase = (&'static str, usize, Vec<usize>, Vec<u32>, usize, bool);

fn dispatch_cases() -> Vec<DispatchCase> {
    let many: Vec<usize> = vec![1; 700];
    let many_tags: Vec<u32> = (0..700).map(|i| (i % 300) as u32).collect();
    vec![
        ("two-tags", 100, vec![30, 20, 50], vec![0, 1, 0], 2, true),
        ("unused-tag", 10, vec![10], vec![0], 3, true),
        (
            "empty-segments",
            10,
            vec![0, 5, 0, 5],
            vec![1, 0, 1, 0],
            2,
            true,
        ),
        ("wide-sizes", 70_000, vec![300, 69_700], vec![1, 0], 2, true),
        ("many-tags", 700, many, many_tags, 300, true),
        ("empty-input", 0, vec![], vec![], 0, true),
        ("empty-input-ignores-tags", 0, vec![], vec![], 5, true),
        ("bad-total", 10, vec![4, 4], vec![0, 0], 1, false),
        ("segment-too-big", 10, vec![11], vec![0], 1, false),
        ("tag-out-of-range", 10, vec![10], vec![2], 2, false),
        ("no-segments", 10, vec![], vec![], 1, false),
        ("65536-tags", 10, vec![10], vec![0], 65_536, false),
    ]
}

#[test]
fn dispatch_n_by_tag_matches_the_reference() {
    for (name, size, sizes, tags, nb_tags, expected_ok) in dispatch_cases() {
        let inputs = [In::Serial(support::noise(size, 51))];
        let mut any_ok = false;
        for version in VERSIONS {
            let cfg = config(3, version);
            let reference = structure::compress(
                &c_inputs(&inputs),
                &Run::Dispatch {
                    sizes: sizes.clone(),
                    tags: tags.clone(),
                    tags_count: nb_tags as u32,
                },
                &c_params(&cfg),
            );
            let rust = rust_function(
                RunDispatch(DispatchInstructions {
                    segment_sizes: sizes.clone(),
                    tags: tags.clone(),
                    tag_count: nb_tags,
                }),
                cfg,
                &inputs,
            );
            any_ok = compare(
                &format!("dispatch {name} v{version}"),
                reference,
                rust,
                &inputs,
            );
        }
        assert_eq!(any_ok, expected_ok, "dispatch {name}");
    }
}

#[test]
fn dispatch_string_matches_the_reference() {
    let n = scale(500, 40);
    let text = support::text(n * 5, 61);
    let lens: Vec<u32> = (0..n).map(|i| (i % 6) as u32).collect();
    let total: usize = lens.iter().map(|&l| l as usize).sum();
    let bytes = text[..total].to_vec();
    let cases: Vec<(&str, usize, Vec<u16>, bool)> = vec![
        ("one-output", 1, vec![0; n], true),
        (
            "three-outputs",
            3,
            (0..n).map(|i| (i % 3) as u16).collect(),
            true,
        ),
        ("unused-outputs", 5, vec![4; n], true),
        (
            "many-outputs",
            2048,
            (0..n).map(|i| (i * 7 % 2048) as u16).collect(),
            true,
        ),
        ("too-many-outputs", 2049, vec![0; n], false),
        ("index-out-of-range", 3, vec![3; n], false),
        ("zero-outputs", 0, vec![0; n], false),
    ];
    for (name, outputs, indices, expected_ok) in cases {
        let inputs = [In::Strings(bytes.clone(), lens.clone())];
        let index_bytes: Vec<u8> = indices.iter().flat_map(|i| i.to_le_bytes()).collect();
        let ok = check_node(
            &format!("dispatch_string {name}"),
            DISPATCH_STRING,
            &[(47, outputs as i32)],
            &[(48, index_bytes)],
            &inputs,
        );
        assert_eq!(ok, expected_ok, "dispatch_string {name}");
        // The same through the graph helper.
        for version in VERSIONS {
            let cfg = config(3, version);
            let reference = structure::compress(
                &c_inputs(&inputs),
                &Run::RawNode {
                    node: DISPATCH_STRING,
                    ints: vec![(47, outputs as i32)],
                    copies: vec![(48, indices.iter().flat_map(|i| i.to_le_bytes()).collect())],
                },
                &c_params(&cfg),
            );
            let rust = rust_function(RunDispatchString(outputs, indices.clone()), cfg, &inputs);
            compare(
                &format!("run_dispatch_string {name} v{version}"),
                reference,
                rust,
                &inputs,
            );
        }
    }
    // Empty strings input, with and without indices.
    let empty = [In::Strings(vec![], vec![])];
    assert!(check_node(
        "dispatch_string empty",
        DISPATCH_STRING,
        &[(47, 0)],
        &[(48, vec![])],
        &empty
    ));
    assert!(!check_node(
        "dispatch_string no indices",
        DISPATCH_STRING,
        &[(47, 2)],
        &[],
        &empty
    ));
    // Missing count (indices shorter than the strings are undefined
    // behaviour in the reference and an error here).
    let some = [In::Strings(b"ab".to_vec(), vec![1, 1])];
    assert!(!check_node(
        "dispatch_string no count",
        DISPATCH_STRING,
        &[],
        &[(48, vec![0, 0, 1, 0])],
        &some
    ));
}

// -----------------------------------------------------------------------
// Concat, dedup, interleave
// -----------------------------------------------------------------------

#[test]
fn concat_matches_the_reference() {
    let serial = |n: usize, seed| In::Serial(support::noise(n, seed));
    let cases: Vec<(&str, u32, Vec<In>, bool)> = vec![
        (
            "serial",
            33,
            vec![serial(10, 1), serial(0, 2), serial(300, 3)],
            true,
        ),
        ("serial-one", 33, vec![serial(7, 1)], true),
        ("serial-empty", 33, vec![serial(0, 1), serial(0, 2)], true),
        (
            "num",
            34,
            vec![
                In::Numeric(le_bytes(&[1, 2, 3], 4), 4),
                In::Numeric(vec![], 4),
                In::Numeric(le_bytes(&[9; 50], 4), 4),
            ],
            true,
        ),
        (
            "num-mixed-widths",
            34,
            vec![
                In::Numeric(le_bytes(&[1, 2, 3], 4), 4),
                In::Numeric(le_bytes(&[1], 2), 2),
            ],
            false,
        ),
        (
            "struct",
            35,
            vec![
                In::Struct(support::noise(30, 4), 5),
                In::Struct(support::noise(15, 5), 5),
            ],
            true,
        ),
        (
            "struct-mixed-widths",
            35,
            vec![
                In::Struct(support::noise(30, 4), 5),
                In::Struct(support::noise(30, 5), 3),
            ],
            false,
        ),
        (
            "string",
            36,
            vec![
                In::Strings(b"abcdef".to_vec(), vec![1, 2, 3]),
                In::Strings(vec![], vec![]),
                In::Strings(b"xy".to_vec(), vec![0, 2, 0]),
            ],
            true,
        ),
    ];
    for (name, id, inputs, expected_ok) in cases {
        assert_eq!(
            check_node(&format!("concat {name}"), id, &[], &[], &inputs),
            expected_ok,
            "{name}"
        );
    }
}

#[test]
fn dedup_matches_the_reference() {
    let same = le_bytes(&words(50, 71), 8);
    let other = le_bytes(&words(50, 72), 8);
    let num = |bytes: &Vec<u8>, w| In::Numeric(bytes.clone(), w);
    // dedup_num verifies unless told to trust; the private node never checks.
    type DedupCase = (&'static str, u32, Vec<(i32, i32)>, Vec<In>, bool);
    let cases: Vec<DedupCase> = vec![
        (
            "identical",
            37,
            vec![],
            vec![num(&same, 8), num(&same, 8), num(&same, 8)],
            true,
        ),
        ("single", 37, vec![], vec![num(&same, 8)], true),
        (
            "empty",
            37,
            vec![],
            vec![In::Numeric(vec![], 4), In::Numeric(vec![], 4)],
            true,
        ),
        (
            "different",
            37,
            vec![],
            vec![num(&same, 8), num(&other, 8)],
            false,
        ),
        (
            "different-width",
            37,
            vec![],
            vec![num(&same, 8), In::Numeric(same.clone(), 4)],
            false,
        ),
        (
            "different-length",
            37,
            vec![],
            vec![num(&same, 8), num(&same[..40].to_vec(), 8)],
            false,
        ),
        (
            "trusted-param",
            37,
            vec![(9438, 1)],
            vec![num(&same, 8), num(&same, 8)],
            true,
        ),
        (
            "trusted-param-zero",
            37,
            vec![(9438, 0)],
            vec![num(&same, 8), num(&other, 8)],
            false,
        ),
        (
            "trusted-node",
            74,
            vec![],
            vec![num(&same, 8), num(&same, 8)],
            true,
        ),
    ];
    for (name, id, ints, inputs, expected_ok) in cases {
        assert_eq!(
            check_node(&format!("dedup {name}"), id, &ints, &[], &inputs),
            expected_ok,
            "{name}"
        );
    }
}

#[test]
fn interleave_matches_the_reference() {
    let strings = |n: usize, seed: u32| {
        let text = support::text(n * 4, seed);
        let lens: Vec<u32> = (0..n).map(|i| (i % 5) as u32).collect();
        let total: usize = lens.iter().map(|&l| l as usize).sum();
        In::Strings(text[..total].to_vec(), lens)
    };
    let cases: Vec<(&str, Vec<In>, bool)> = vec![
        ("two", vec![strings(30, 1), strings(30, 2)], true),
        ("one", vec![strings(30, 1)], true),
        ("five", (0..5).map(|i| strings(17, i)).collect(), true),
        ("empty", vec![strings(0, 1), strings(0, 2)], true),
        (
            "different-counts",
            vec![strings(30, 1), strings(29, 2)],
            false,
        ),
        ("many", (0..64).map(|i| strings(3, i)).collect(), true),
    ];
    for (name, inputs, expected_ok) in cases {
        assert_eq!(
            check_node(&format!("interleave {name}"), 39, &[], &[], &inputs),
            expected_ok,
            "{name}"
        );
    }
}

// -----------------------------------------------------------------------
// Prefix, parse_int, transpose
// -----------------------------------------------------------------------

#[test]
fn prefix_matches_the_reference() {
    let sorted: Vec<String> = {
        let mut names: Vec<String> = (0..scale(500, 40))
            .map(|i| format!("/usr/lib/{}/file{}", i % 13, i * 17 % 91))
            .collect();
        names.sort();
        names
    };
    let build = |parts: &[String]| {
        In::Strings(
            parts.concat().into_bytes(),
            parts.iter().map(|p| p.len() as u32).collect(),
        )
    };
    let words: Vec<String> = ["", "a", "ab", "abc", "abd", "", "abd", "b", "ba", "ba"]
        .iter()
        .map(ToString::to_string)
        .collect();
    for (name, input) in [
        ("paths", build(&sorted)),
        ("words", build(&words)),
        ("empty", build(&[])),
        ("one", build(&["x".to_string()])),
        ("text", In::Strings(support::text(2000, 3), vec![50; 40])),
    ] {
        assert!(
            check_node(&format!("prefix {name}"), 30, &[], &[], &[input]),
            "{name}"
        );
    }
}

#[test]
fn parse_int_matches_the_reference() {
    let valid: Vec<String> = [
        "0",
        "1",
        "-1",
        "123456789",
        "-9223372036854775808",
        "9223372036854775807",
        "42",
        "-77",
        "1000000",
        "999",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    let many: Vec<String> = (0..scale(3000, 50))
        .map(|i| format!("{}", i as i64 * 7919 - 5000))
        .collect();
    let invalid: [&str; 14] = [
        "",
        "-",
        "+1",
        "01",
        "-0",
        "1a",
        " 1",
        "1 ",
        "9223372036854775808",
        "-9223372036854775809",
        "123456789012345678901",
        "99999999999999999999",
        "\u{e9}",
        "1.5",
    ];
    let build = |parts: &[String]| {
        In::Strings(
            parts.concat().into_bytes(),
            parts.iter().map(|p| p.len() as u32).collect(),
        )
    };
    assert!(check_node(
        "parse_int valid",
        38,
        &[],
        &[],
        &[build(&valid)]
    ));
    assert!(check_node("parse_int many", 38, &[], &[], &[build(&many)]));
    assert!(check_node("parse_int empty", 38, &[], &[], &[build(&[])]));
    for bad in invalid {
        let mut parts = valid.clone();
        parts.push(bad.to_string());
        assert!(
            !check_node(
                &format!("parse_int {bad:?}"),
                38,
                &[],
                &[],
                &[build(&parts)]
            ),
            "{bad:?}"
        );
    }
    // Pre-parsed integers replace parsing, and must cover every string.
    let values: Vec<u8> = valid
        .iter()
        .flat_map(|v| v.parse::<i64>().unwrap().to_le_bytes())
        .collect();
    assert!(check_node(
        "parse_int preparsed",
        38,
        &[],
        &[(350, values.clone())],
        &[build(&valid)]
    ));
    assert!(!check_node(
        "parse_int preparsed short",
        38,
        &[],
        &[(350, values[8..].to_vec())],
        &[build(&valid)]
    ));
    assert!(!check_node(
        "parse_int preparsed empty",
        38,
        &[],
        &[(350, vec![])],
        &[build(&valid)]
    ));
}

#[test]
fn transpose_split_matches_the_reference() {
    for width in [1, 2, 3, 4, 7, 8, 16, 33] {
        for n in [0, 1, 5, scale(1000, 20)] {
            let input = [In::Struct(support::noise(n * width, 81), width)];
            assert!(check_node(
                &format!("transpose_split w{width} n{n}"),
                3,
                &[],
                &[],
                &input
            ));
        }
    }
}

#[test]
fn node_ids_match_the_reference() {
    assert_eq!(Node::ConcatSerial.id(), 33);
    assert_eq!(Node::DedupNumeric.id(), 37);
    assert_eq!(Node::InterleaveString.id(), 39);
    assert_eq!(Node::TransposeSplit.id(), 3);
    assert_eq!(Node::DispatchString.id(), 32);
    assert_eq!(Node::SplitByStruct.id(), 69);
    assert_eq!(Node::SplitN.id(), 66);
    assert_eq!(Node::SplitNStruct.id(), 67);
    assert_eq!(Node::SplitNNum.id(), 68);
}

// -----------------------------------------------------------------------
// Graphs
// -----------------------------------------------------------------------

fn graph_config(level: i32, version: u32) -> EncoderConfig {
    config(level, version)
}

fn rust_graph(
    build: impl FnOnce(&mut GraphBuilder) -> mopenzl::graph::GraphId,
    config: EncoderConfig,
    inputs: &[In],
) -> Result<Vec<u8>, EncodeError> {
    let mut builder = GraphBuilder::new();
    let start = build(&mut builder);
    let graph: CompressorGraph = builder.build(start).unwrap();
    let mut compressor = Compressor::builder(config)
        .with_graph(graph)
        .build()
        .unwrap();
    compressor.compress_inputs(&rust_inputs(inputs))
}

const LEVELS: [i32; 6] = [1, 3, 6, 7, 10, 19];

#[test]
fn try_parse_int_matches_the_reference() {
    let build = |parts: &[&str]| {
        In::Strings(
            parts.concat().into_bytes(),
            parts.iter().map(|p| p.len() as u32).collect(),
        )
    };
    let many: Vec<String> = (0..scale(800, 40)).map(|i| format!("{}", i * 31)).collect();
    let many_refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let mut mixed: Vec<&str> = many_refs.clone();
    mixed.insert(3, "n/a");
    mixed.insert(10, "007");
    mixed.push("x");
    let cases: Vec<(&str, In)> = vec![
        ("all-ints", build(&many_refs)),
        ("none", build(&["a", "b", "c", "dd", "", "-"])),
        ("mixed", build(&mixed)),
        ("empty", build(&[])),
        ("one-exception", build(&["1", "2", "no", "4"])),
        ("single", build(&["5"])),
    ];
    // (numbers, exceptions): the first run uses plain stores, the second
    // compresses both.
    for (name, input) in &cases {
        for successors in [
            [StandardGraph::Store, StandardGraph::Store],
            [StandardGraph::Zstd, StandardGraph::Zstd],
            [StandardGraph::Store, StandardGraph::Zstd],
        ] {
            for level in LEVELS {
                for version in VERSIONS {
                    let cfg = graph_config(level, version);
                    let ids: Vec<u32> = successors.iter().map(|&g| u32::from(g)).collect();
                    let reference = graph::compress(
                        &[input.c()],
                        &Graph::Parameterized {
                            graph: 19,
                            params: graph::Params::default(),
                            custom: vec![Graph::Id(ids[0]), Graph::Id(ids[1]), Graph::STORE],
                        },
                        &c_params(&cfg),
                    );
                    let rust = rust_graph(
                        |b| {
                            b.parameterize_graph(
                                StandardGraph::TryParseInt,
                                GraphParams::default().with_graphs(vec![
                                    successors[0].into(),
                                    successors[1].into(),
                                    StandardGraph::Store.into(),
                                ]),
                            )
                            .unwrap()
                        },
                        cfg,
                        std::slice::from_ref(input),
                    );
                    compare(
                        &format!("try_parse_int {name} {ids:?} l{level} v{version}"),
                        reference,
                        rust,
                        std::slice::from_ref(input),
                    );
                }
            }
        }
    }
}

#[test]
fn try_parse_int_needs_three_graphs() {
    let input = [In::Strings(b"12".to_vec(), vec![1, 1])];
    let reference = graph::compress(
        &[input[0].c()],
        &Graph::Parameterized {
            graph: 19,
            params: graph::Params::default(),
            custom: vec![Graph::STORE, Graph::STORE],
        },
        &c_params(&config(3, 27)),
    );
    let rust = rust_graph(
        |b| {
            b.parameterize_graph(
                StandardGraph::TryParseInt,
                GraphParams::default().with_graphs(vec![
                    StandardGraph::Store.into(),
                    StandardGraph::Store.into(),
                ]),
            )
            .unwrap()
        },
        config(3, 27),
        &input,
    );
    assert!(!compare(
        "try_parse_int two graphs",
        reference,
        rust,
        &input
    ));
}

#[test]
fn n_to_n_matches_the_reference() {
    let inputs = [
        In::Serial(support::text(3000, 91)),
        In::Serial(support::noise(2000, 92)),
        In::Numeric(le_bytes(&words(300, 93), 8), 8),
    ];
    for successors in [
        [
            StandardGraph::Zstd,
            StandardGraph::Store,
            StandardGraph::Store,
        ],
        [
            StandardGraph::Store,
            StandardGraph::Zstd,
            StandardGraph::Zstd,
        ],
    ] {
        for version in VERSIONS {
            let cfg = config(3, version);
            let ids: Vec<u32> = successors.iter().map(|&g| u32::from(g)).collect();
            let reference = graph::compress(
                &c_inputs(&inputs),
                &Graph::Parameterized {
                    graph: 62,
                    params: graph::Params::default(),
                    custom: ids.iter().map(|&id| Graph::Id(id)).collect(),
                },
                &c_params(&cfg),
            );
            let rust = rust_graph(
                |b| {
                    b.parameterize_graph(
                        StandardGraph::NToN,
                        GraphParams::default()
                            .with_graphs(successors.iter().map(|&g| g.into()).collect::<Vec<_>>()),
                    )
                    .unwrap()
                },
                cfg,
                &inputs,
            );
            assert!(compare(
                &format!("n_to_n {ids:?} v{version}"),
                reference,
                rust,
                &inputs
            ));
        }
    }
    // A count mismatch fails on both sides.
    let reference = graph::compress(
        &c_inputs(&inputs),
        &Graph::Parameterized {
            graph: 62,
            params: graph::Params::default(),
            custom: vec![Graph::STORE],
        },
        &c_params(&config(3, 27)),
    );
    let rust = rust_graph(
        |b| {
            b.parameterize_graph(
                StandardGraph::NToN,
                GraphParams::default().with_graphs(vec![StandardGraph::Store.into()]),
            )
            .unwrap()
        },
        config(3, 27),
        &inputs,
    );
    assert!(!compare("n_to_n mismatch", reference, rust, &inputs));
}

fn split_input_for(graph: StandardGraph, n: usize) -> In {
    match graph {
        StandardGraph::SplitSerial => In::Serial(support::text(n, 101)),
        StandardGraph::SplitStruct => In::Struct(support::noise(n * 4, 102), 4),
        _ => In::Numeric(le_bytes(&words(n, 103), 8), 8),
    }
}

#[test]
fn split_graphs_match_the_reference() {
    let node_of = |graph: StandardGraph| match graph {
        StandardGraph::SplitSerial => StandardNode::SplitN,
        StandardGraph::SplitStruct => StandardNode::SplitNStruct,
        _ => StandardNode::SplitNNum,
    };
    let type_of = |graph: StandardGraph| match graph {
        StandardGraph::SplitSerial => 0,
        StandardGraph::SplitStruct => 1,
        _ => 2,
    };
    for graph in [
        StandardGraph::SplitSerial,
        StandardGraph::SplitStruct,
        StandardGraph::SplitNumeric,
    ] {
        for sizes in [vec![100, 200, 0], vec![150, 150], vec![300]] {
            let input = split_input_for(graph, 300);
            for version in VERSIONS {
                let cfg = config(3, version);
                let reference = enc_structure::split_graph(
                    input.c(),
                    SplitGraph::Split,
                    &sizes,
                    &c_params(&cfg),
                );
                let rust = rust_graph(
                    |b| {
                        let split = b
                            .parameterize_node(
                                node_of(graph),
                                LocalParams::default()
                                    .with_bytes(SEGMENT_SIZES_PID, size_bytes(&sizes)),
                            )
                            .unwrap();
                        b.parameterize_graph(
                            graph,
                            GraphParams::default()
                                .with_graphs(vec![StandardGraph::Store.into(); sizes.len()])
                                .with_nodes(vec![split]),
                        )
                        .unwrap()
                    },
                    cfg,
                    std::slice::from_ref(&input),
                );
                let _ = type_of(graph);
                assert!(compare(
                    &format!("{graph:?} {sizes:?} v{version}"),
                    reference,
                    rust,
                    std::slice::from_ref(&input)
                ));
            }
        }
    }
}

#[test]
fn split_by_struct_graph_matches_the_reference() {
    for fields in [vec![1, 2, 5], vec![4], vec![8, 8, 3]] {
        let size: usize = fields.iter().sum();
        let input = In::Serial(support::noise(size * 50, 111));
        for version in VERSIONS {
            let cfg = config(3, version);
            let reference = enc_structure::split_graph(
                input.c(),
                SplitGraph::SplitByStruct,
                &fields,
                &c_params(&cfg),
            );
            let rust = rust_graph(
                |b| {
                    let split = b
                        .parameterize_node(
                            StandardNode::SplitByStruct,
                            LocalParams::default().with_bytes(FIELD_SIZES_PID, size_bytes(&fields)),
                        )
                        .unwrap();
                    b.parameterize_graph(
                        StandardGraph::SplitSerial,
                        GraphParams::default()
                            .with_graphs(vec![StandardGraph::Store.into(); fields.len()])
                            .with_nodes(vec![split]),
                    )
                    .unwrap()
                },
                cfg,
                std::slice::from_ref(&input),
            );
            assert!(compare(
                &format!("split_by_struct {fields:?} v{version}"),
                reference,
                rust,
                std::slice::from_ref(&input)
            ));
        }
    }
}

#[test]
fn transpose_split_graph_matches_the_reference() {
    for width in [1, 2, 3, 4, 8, 12] {
        let input = In::Struct(support::noise(width * 200, 121), width);
        for version in VERSIONS {
            let cfg = config(3, version);
            let reference = enc_structure::split_graph(
                input.c(),
                SplitGraph::TransposeSplit,
                &[],
                &c_params(&cfg),
            );
            let rust = rust_graph(
                |b| {
                    let store: mopenzl::graph::GraphId = StandardGraph::Store.into();
                    let split = b
                        .static_graph(StandardNode::TransposeSplit, &[store])
                        .unwrap();
                    b.parameterize_graph(
                        StandardGraph::TransposeSplit,
                        GraphParams::default().with_graphs(vec![store, store, store, store, split]),
                    )
                    .unwrap()
                },
                cfg,
                std::slice::from_ref(&input),
            );
            assert!(compare(
                &format!("transpose_split graph w{width} v{version}"),
                reference,
                rust,
                std::slice::from_ref(&input)
            ));
        }
    }
}
