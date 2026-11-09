//! Byte identity of the ML selector (graph 17) and clustering (graph 18)
//! graphs with the reference, round trips through both decoders, and the
//! behaviour of their configurations.
//!
//! The reference side builds each graph in C (`openzl_ffi::enc_ml`): the ML
//! selector from a raw CBOR MParam blob, clustering both from a typed
//! `ZL_ClusteringConfig` (the reference serializes it) and from the CBOR this
//! crate writes (a copy parameter), so each configuration also proves the two
//! serializers interoperate.
//!
//! Tests that need the `field_lz` graph (the clustering graph sends the sizes
//! stream of every multi-input cluster to it) skip, with a `SKIP(field_lz)`
//! line, while that graph is not ported.
#![cfg(all(
    feature = "compression",
    feature = "decompression",
    feature = "zstd",
    feature = "conversion",
    feature = "concat",
    feature = "bitpack"
))]

mod support;

use mopenzl::graph::{
    ClusteringConfig, GbtModel, GbtNode, GraphBuilder, GraphId, MlSelectorConfig, NodeId,
    StandardGraph, StandardNode,
};
use mopenzl::input::{Input, Numbers, Records, Strings};
use mopenzl::{
    Compressor, DataType, Decompressor, EncodeError, EncoderConfig, FormatVersion, Level,
};
use openzl_ffi::enc_ml::{self, Cluster, MlGraph, MlInput, TypeDefault};
use openzl_ffi::graph::{self, Graph, Input as CInput, Params, cparam};

const VERSIONS: [u32; 3] = [21, 24, 27];
const LEVELS: [i32; 4] = [1, 3, 7, 19];

const STORE: u32 = 2;
const ZSTD: u32 = 9;
const BITPACK: u32 = 10;
const CONSTANT: u32 = 8;
const ML_SELECTOR: u32 = 17;
const CLUSTERING: u32 = 18;
const NODE_CONCAT: [u32; 4] = [33, 35, 34, 36];

fn config(level: i32, version: u32, permissive: bool) -> EncoderConfig {
    EncoderConfig::default()
        .with_level(Level::try_from(level).unwrap())
        .with_format_version(FormatVersion::try_from(version).unwrap())
        .with_min_stream_size(1)
        .with_permissive(permissive)
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

// -----------------------------------------------------------------------
// Inputs
// -----------------------------------------------------------------------

/// One typed input with its integer metadata, in both representations.
#[derive(Clone, Debug)]
struct In {
    data: Data,
    metadata: Vec<(i32, i32)>,
}

#[derive(Clone, Debug)]
enum Data {
    Serial(Vec<u8>),
    Struct(Vec<u8>, usize),
    Numeric(Vec<u8>, usize),
    Strings(Vec<u8>, Vec<u32>),
}

impl In {
    fn plain(data: Data) -> Self {
        Self {
            data,
            metadata: Vec::new(),
        }
    }

    fn tagged(data: Data, tag: i32) -> Self {
        Self {
            data,
            metadata: vec![(0, tag)],
        }
    }

    fn rust(&self) -> Input<'_> {
        let input = match &self.data {
            Data::Serial(bytes) => Input::from(&bytes[..]),
            Data::Struct(bytes, width) => {
                Input::from(Records::try_from((&bytes[..], *width)).unwrap())
            }
            Data::Numeric(bytes, width) => {
                Input::from(Numbers::try_from((&bytes[..], *width)).unwrap())
            }
            Data::Strings(bytes, lens) => {
                Input::from(Strings::try_from((&bytes[..], &lens[..])).unwrap())
            }
        };
        input.with_metadata(&self.metadata)
    }

    fn c(&self) -> MlInput<'_> {
        let input = match &self.data {
            Data::Serial(bytes) => CInput::Serial(bytes),
            Data::Struct(bytes, width) => CInput::Struct {
                bytes,
                width: *width,
            },
            Data::Numeric(bytes, width) => CInput::Numeric {
                bytes,
                width: *width,
            },
            Data::Strings(bytes, lens) => CInput::String { bytes, lens },
        };
        MlInput {
            input,
            metadata: &self.metadata,
        }
    }
}

fn numeric(values: &[u64], width: usize) -> Data {
    Data::Numeric(
        values
            .iter()
            .flat_map(|value| value.to_le_bytes()[..width].to_vec())
            .collect(),
        width,
    )
}

fn scale(full: usize, small: usize) -> usize {
    if cfg!(miri) { small } else { full }
}

fn assert_round_trip(frame: &[u8], inputs: &[In], what: &str) {
    let ours = Decompressor::default()
        .decompress_typed(frame)
        .unwrap_or_else(|error| panic!("{what}: mopenzl cannot decode: {error}"));
    let theirs = graph::decompress(frame)
        .unwrap_or_else(|error| panic!("{what}: the reference cannot decode: {error:?}"));
    assert_eq!(ours.len(), inputs.len(), "{what}");
    assert_eq!(theirs.len(), inputs.len(), "{what}");
    for (n, input) in inputs.iter().enumerate() {
        let (ty, bytes, lens): (DataType, &[u8], &[u32]) = match &input.data {
            Data::Serial(bytes) => (DataType::Serial, bytes, &[]),
            Data::Struct(bytes, _) => (DataType::Struct, bytes, &[]),
            Data::Numeric(bytes, _) => (DataType::Numeric, bytes, &[]),
            Data::Strings(bytes, lens) => (DataType::String, bytes, lens),
        };
        assert_eq!(ours[n].data_type(), ty, "{what} output {n}");
        assert_eq!(ours[n].content(), bytes, "{what} output {n}");
        assert_eq!(ours[n].string_lens(), lens, "{what} output {n}");
        assert_eq!(theirs[n].bytes, bytes, "{what} output {n} (reference)");
        assert_eq!(theirs[n].lens, lens, "{what} output {n} (reference)");
    }
}

/// How many comparisons of a test found each outcome.
#[derive(Debug, Default)]
struct Tally {
    identical: usize,
    failed: usize,
    skipped: usize,
}

impl Tally {
    fn add(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Identical => self.identical += 1,
            Outcome::BothFailed => self.failed += 1,
            Outcome::Skipped => self.skipped += 1,
        }
    }

    /// At least one byte-identical frame and no frame the libraries disagree
    /// on (skips stand for graphs of other packages).
    fn assert_identical(&self, what: &str) {
        assert!(self.identical > 0 && self.failed == 0, "{what}: {self:?}");
    }

    /// The strict-mode comparisons failed in both libraries. A permissive
    /// compression falls back to the generic graph and may still succeed, so
    /// only the failures are counted here.
    fn assert_failed(&self, what: &str) {
        assert!(self.failed >= 3, "{what}: {self:?}");
    }
}

/// What a comparison found.
#[derive(Debug, Eq, PartialEq)]
enum Outcome {
    Identical,
    BothFailed,
    /// The reference succeeded where mopenzl hit a graph that is not ported.
    Skipped,
}

/// Compares the frames of `rust` and `reference`: equal bytes, or both
/// failing. A graph that is not ported yet (`field_lz`) skips.
fn compare(
    what: &str,
    reference: Result<Vec<u8>, openzl_ffi::Error>,
    rust: Result<Vec<u8>, EncodeError>,
    inputs: &[In],
) -> Outcome {
    match (reference, rust) {
        (Ok(expected), Ok(frame)) => {
            assert_eq!(frame, expected, "{what}");
            assert_round_trip(&frame, inputs, what);
            Outcome::Identical
        }
        (Err(c), Err(r)) => {
            eprintln!("{what}: both failed: {c:?} / {r}");
            Outcome::BothFailed
        }
        (Ok(_), Err(error)) if error.to_string().contains("not ported") => {
            eprintln!("SKIP(field_lz) {what}: {error}");
            Outcome::Skipped
        }
        // Level 7 and up routes numeric data to the transformer graph, whose
        // package is separate; its stub is an invalid graph.
        (Ok(_), Err(error))
            if error.to_string().contains("invalid graph") && what.contains(" L19 ") =>
        {
            eprintln!("SKIP(transformer) {what}: {error}");
            Outcome::Skipped
        }
        (Ok(_), Err(error)) => panic!("{what}: only mopenzl failed: {error}"),
        (Err(error), Ok(_)) => panic!("{what}: only the reference failed: {error:?}"),
    }
}

fn rust_compress(
    builder: GraphBuilder,
    start: GraphId,
    config: EncoderConfig,
    inputs: &[In],
) -> Result<Vec<u8>, EncodeError> {
    let mut compressor = Compressor::builder(config)
        .with_graph(builder.build(start).unwrap())
        .build()
        .unwrap();
    let inputs: Vec<Input<'_>> = inputs.iter().map(In::rust).collect();
    compressor.compress_inputs(&inputs)
}

// -----------------------------------------------------------------------
// CBOR writer for hand-written models
// -----------------------------------------------------------------------

/// A minimal definite-length CBOR writer, independent of the crate's.
enum V {
    Int(i64),
    F64(f64),
    Text(&'static str),
    Arr(Vec<V>),
    Map(Vec<(&'static str, V)>),
}

fn head(out: &mut Vec<u8>, major: u8, n: u64) {
    match n {
        0..=23 => out.push(major << 5 | n as u8),
        24..=255 => out.extend([major << 5 | 24, n as u8]),
        256..=65535 => {
            out.push(major << 5 | 25);
            out.extend((n as u16).to_be_bytes());
        }
        _ => {
            out.push(major << 5 | 26);
            out.extend((n as u32).to_be_bytes());
        }
    }
}

fn write(out: &mut Vec<u8>, v: &V) {
    match v {
        V::Int(n) if *n >= 0 => head(out, 0, *n as u64),
        V::Int(n) => head(out, 1, !*n as u64),
        V::F64(x) => {
            out.push(0xFB);
            out.extend(x.to_bits().to_be_bytes());
        }
        V::Text(text) => {
            head(out, 3, text.len() as u64);
            out.extend(text.as_bytes());
        }
        V::Arr(items) => {
            head(out, 4, items.len() as u64);
            items.iter().for_each(|item| write(out, item));
        }
        V::Map(pairs) => {
            head(out, 5, pairs.len() as u64);
            for (key, value) in pairs {
                write(out, &V::Text(key));
                write(out, value);
            }
        }
    }
}

fn cbor(v: &V) -> Vec<u8> {
    let mut out = Vec::new();
    write(&mut out, v);
    out
}

/// A tree node: `(feature, value, left, right, missing)`.
type N = (i64, f64, usize, usize, usize);

fn node_v(n: &N) -> V {
    V::Map(vec![
        ("featureIdx", V::Int(n.0)),
        ("value", V::F64(n.1)),
        ("leftChildIdx", V::Int(n.2 as i64)),
        ("rightChildIdx", V::Int(n.3 as i64)),
        ("missingChildIdx", V::Int(n.4 as i64)),
    ])
}

/// The serialized model: `forests[f][t]` is a tree.
fn model_cbor(
    forests: &[Vec<Vec<N>>],
    labels: &[&'static str],
    successors: usize,
    generator: i64,
) -> Vec<u8> {
    let forests_v = forests
        .iter()
        .map(|forest| {
            V::Map(vec![
                ("numTrees", V::Int(forest.len() as i64)),
                (
                    "trees",
                    V::Arr(
                        forest
                            .iter()
                            .map(|tree| {
                                V::Map(vec![
                                    ("numNodes", V::Int(tree.len() as i64)),
                                    ("nodes", V::Arr(tree.iter().map(node_v).collect())),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ])
        })
        .collect();
    cbor(&V::Map(vec![
        ("model", V::Int(0)),
        (
            "runtimeConfig",
            V::Map(vec![
                (
                    "predictor",
                    V::Map(vec![
                        ("numForests", V::Int(forests.len() as i64)),
                        ("forests", V::Arr(forests_v)),
                    ]),
                ),
                ("featureGenerator", V::Int(generator)),
                ("nbSuccessors", V::Int(successors as i64)),
                ("nbFeatures", V::Int(labels.len() as i64)),
                (
                    "featureLabels",
                    V::Arr(labels.iter().map(|label| V::Text(label)).collect()),
                ),
            ]),
        ),
    ]))
}

const LEAF: i64 = -1;

fn leaf(score: f64) -> N {
    (LEAF, score, 0, 0, 0)
}

// -----------------------------------------------------------------------
// ML selector
// -----------------------------------------------------------------------

/// The Rust and C graph selecting among `successors` (graph IDs) with `blob`.
fn ml_graphs(
    blob: &[u8],
    successors: &[u32],
) -> (
    Result<(GraphBuilder, GraphId), mopenzl::graph::ConfigError>,
    MlGraph,
) {
    let rust = MlSelectorConfig::try_from(blob).map(|config| {
        let mut builder = GraphBuilder::new();
        let ids: Vec<GraphId> = successors
            .iter()
            .map(|&id| StandardGraph::try_from(id).unwrap().into())
            .collect();
        let graph = builder.ml_selector(&config, &ids).unwrap();
        (builder, graph)
    });
    let c = MlGraph::Parameterized {
        graph: ML_SELECTOR,
        params: Params::default(),
        custom: successors
            .iter()
            .map(|&id| MlGraph::Plain(Graph::Id(id)))
            .collect(),
        nodes: vec![],
        mparam: blob.to_vec(),
    };
    (rust, c)
}

/// Compresses `inputs` through the ML selector built from `blob` in both
/// libraries at every version, for each `(level, permissive)`.
fn check_ml(what: &str, blob: &[u8], successors: &[u32], inputs: &[In], levels: &[i32]) -> Tally {
    let mut tally = Tally::default();
    for &level in levels {
        for version in VERSIONS {
            for permissive in [false, true] {
                let cfg = config(level, version, permissive);
                let label = format!("{what} L{level} v{version} permissive={permissive}");
                let cinputs: Vec<MlInput<'_>> = inputs.iter().map(In::c).collect();
                let (rust, c_graph) = ml_graphs(blob, successors);
                let reference = enc_ml::compress(&cinputs, &c_graph, &c_params(&cfg));
                let Ok((builder, graph)) = rust else {
                    assert!(
                        reference.is_err(),
                        "{label}: the reference accepted what mopenzl rejects"
                    );
                    tally.add(Outcome::BothFailed);
                    continue;
                };
                let result = rust_compress(builder, graph, cfg, inputs);
                tally.add(compare(&label, reference, result, inputs));
            }
        }
    }
    tally
}

/// Numeric streams with different statistics.
fn ml_inputs(width: usize) -> Vec<(String, Data)> {
    let max = if width == 8 {
        u64::MAX
    } else {
        (1u64 << (width * 8)) - 1
    };
    let noise = |n: usize, seed: u32| -> Vec<u64> {
        support::noise(n * 8, seed)
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_le_bytes(*c) & max)
            .collect()
    };
    let n = scale(2000, 40);
    vec![
        ("empty".into(), numeric(&[], width)),
        ("one".into(), numeric(&[7], width)),
        ("two".into(), numeric(&[3, 900.min(max)], width)),
        ("constant".into(), numeric(&vec![5; n], width)),
        (
            "narrow".into(),
            numeric(
                &noise(n, 3).iter().map(|v| v % 12).collect::<Vec<_>>(),
                width,
            ),
        ),
        ("wide".into(), numeric(&noise(n, 4), width)),
        (
            "sorted".into(),
            numeric(
                &(0..n as u64)
                    .map(|v| v * 3 % max.max(1))
                    .collect::<Vec<_>>(),
                width,
            ),
        ),
        ("small-wide".into(), numeric(&noise(30, 5), width)),
    ]
}

/// A binary model over `nbElts`, `cardinality` and `range_size` using every
/// node kind: leaves, left and right branches, the missing child (a NaN
/// feature and an index past the features) and several trees per forest.
fn binary_model() -> Vec<u8> {
    // Features: 0 nbElts, 1 cardinality, 2 range_size, 3 unknown (NaN).
    let tree_a: Vec<N> = vec![
        (1, 8.0, 1, 2, 4),    // 0: cardinality < 8 ? 1 : 2 ; missing 4
        leaf(-2.0),           // 1
        (2, 5000.0, 3, 5, 5), // 2: range_size < 5000 ? 3 : 5
        leaf(1.5),            // 3
        leaf(0.25),           // 4
        leaf(3.0),            // 5
    ];
    let tree_b: Vec<N> = vec![
        (3, 1.0, 1, 2, 2),   // 0: unknown feature -> NaN -> missing (2)
        leaf(100.0),         // 1 (never reached)
        (0, 100.0, 3, 4, 4), // 2: nbElts < 100 ? 3 : 4
        leaf(-0.5),          // 3
        leaf(0.75),          // 4
    ];
    let tree_c: Vec<N> = vec![
        (9, 1.0, 1, 2, 1), // feature index past nbFeatures -> missing (1)
        leaf(-0.125),
        leaf(50.0),
    ];
    model_cbor(
        &[vec![tree_a, tree_b, tree_c]],
        &["nbElts", "cardinality", "range_size", "bogus"],
        2,
        0,
    )
}

#[test]
fn binary_model_selects_like_the_reference() {
    let blob = binary_model();
    let mut tally = Tally::default();
    for width in [1usize, 2, 4, 8] {
        for (name, data) in ml_inputs(width) {
            let inputs = [In::plain(data)];
            let this = check_ml(
                &format!("binary w{width} {name}"),
                &blob,
                &[STORE, ZSTD],
                &inputs,
                &LEVELS,
            );
            tally.identical += this.identical;
            tally.failed += this.failed;
        }
    }
    tally.assert_identical("binary model");
}

#[test]
fn multiclass_model_selects_like_the_reference() {
    // Three classes over nbElts: small streams -> class 1, huge ranges ->
    // class 2, otherwise class 0.
    let class0: Vec<Vec<N>> = vec![
        vec![leaf(0.5)],
        vec![(0, 50.0, 1, 2, 2), leaf(1.0), leaf(-1.0)],
    ];
    let class1: Vec<Vec<N>> = vec![vec![(0, 50.0, 1, 2, 2), leaf(2.0), leaf(-3.0)]];
    let class2: Vec<Vec<N>> = vec![vec![(2, 1_000_000.0, 1, 2, 2), leaf(-2.0), leaf(4.0)]];
    let blob = model_cbor(
        &[class0, class1, class2],
        &["nbElts", "cardinality", "range_size"],
        3,
        0,
    );
    for width in [1usize, 4, 8] {
        for (name, data) in ml_inputs(width) {
            let inputs = [In::plain(data)];
            check_ml(
                &format!("multiclass w{width} {name}"),
                &blob,
                &[STORE, ZSTD, BITPACK],
                &inputs,
                &LEVELS[1..3],
            );
        }
    }
}

#[test]
fn every_feature_can_drive_the_choice() {
    let labels = [
        "nbElts",
        "eltWidth",
        "cardinality",
        "cardinality_upper",
        "cardinality_lower",
        "range_size",
        "mean",
        "variance",
        "stddev",
        "skewness",
        "kurtosis",
    ];
    for (feature, label) in labels.iter().enumerate() {
        // Several thresholds per feature so streams land on both sides.
        for threshold in [0.5, 4.0, 40.0, 2000.0, 1.0e6] {
            let tree: Vec<N> = vec![(feature as i64, threshold, 1, 2, 2), leaf(-1.0), leaf(1.0)];
            let blob = model_cbor(&[vec![tree]], &labels, 2, 0);
            for width in [1usize, 2, 8] {
                for (name, data) in ml_inputs(width).into_iter().skip(2).step_by(2) {
                    let inputs = [In::plain(data)];
                    check_ml(
                        &format!("feature {label} < {threshold} w{width} {name}"),
                        &blob,
                        &[STORE, ZSTD],
                        &inputs,
                        &[3],
                    );
                }
            }
        }
    }
}

#[test]
fn successors_of_every_kind_are_reachable() {
    // Class 1 always: the second successor runs on the numeric input.
    let blob = model_cbor(&[vec![vec![leaf(1.0)]]], &["nbElts"], 2, 0);
    let data = ml_inputs(4).into_iter().nth(5).unwrap().1;
    for second in [STORE, ZSTD, BITPACK, CONSTANT, 7, 3, 4] {
        check_ml(
            &format!("successor {second}"),
            &blob,
            &[ZSTD, second],
            &[In::plain(data.clone())],
            &[3],
        );
    }
}

#[test]
fn successor_count_mismatch_fails_like_the_reference() {
    let blob = binary_model();
    let data = ml_inputs(2).into_iter().nth(5).unwrap().1;
    for successors in [&[STORE][..], &[STORE, ZSTD, BITPACK][..]] {
        let tally = check_ml(
            "count mismatch",
            &blob,
            successors,
            &[In::plain(data.clone())],
            &[3],
        );
        // Strict: both fail; permissive: both fall back to the generic graph
        // (the helper compares both modes).
        assert!(
            tally.failed > 0 && tally.identical + tally.skipped > 0,
            "{tally:?}"
        );
    }
}

#[test]
fn a_class_beyond_the_successors_fails_like_the_reference() {
    // One forest of a positive leaf says class 1, but the model claims 2
    // successors while nbSuccessors says 1: predict() rejects it.
    let blob = model_cbor(&[vec![vec![leaf(1.0)]]], &["nbElts"], 1, 0);
    let data = ml_inputs(2).into_iter().nth(5).unwrap().1;
    check_ml(
        "class past successors",
        &blob,
        &[STORE],
        &[In::plain(data)],
        &[3],
    );
}

#[test]
fn unusable_configurations_fail_like_the_reference() {
    let good = binary_model();
    // Unknown feature generator.
    let generator = model_cbor(&[vec![vec![leaf(1.0)]]], &["nbElts"], 2, 1);
    // A model type this library does not implement loads, then fails to run.
    let unsupported = cbor(&V::Map(vec![("model", V::Int(7))]));
    // Not CBOR, truncated CBOR, and a map missing its fields.
    let garbage = vec![0xFF, 0x00];
    let truncated = good[..good.len() / 2].to_vec();
    let empty_map = cbor(&V::Map(vec![]));
    let data = ml_inputs(4).into_iter().nth(5).unwrap().1;
    for (name, blob) in [
        ("generator", generator),
        ("unsupported", unsupported),
        ("garbage", garbage),
        ("truncated", truncated),
        ("empty map", empty_map),
    ] {
        let tally = check_ml(
            name,
            &blob,
            &[STORE, ZSTD],
            &[In::plain(data.clone())],
            &[3],
        );
        assert!(tally.failed > 0, "{name}: {tally:?}");
    }
}

#[test]
fn typed_models_serialize_to_blobs_the_reference_loads() {
    let model = GbtModel {
        forests: vec![vec![
            vec![
                GbtNode::split(0, 10.0, 1, 2, 2),
                GbtNode::leaf(-1.0),
                GbtNode::leaf(2.0),
            ],
            vec![GbtNode::leaf(0.5)],
        ]],
        successors: 2,
        features: vec!["nbElts".into(), "cardinality".into()],
    };
    let config = MlSelectorConfig::try_from(&model).unwrap();
    let blob = config.as_ref().to_vec();
    for width in [1usize, 4] {
        for (name, data) in ml_inputs(width) {
            check_ml(
                &format!("typed w{width} {name}"),
                &blob,
                &[STORE, ZSTD],
                &[In::plain(data)],
                &[3],
            );
        }
    }
}

#[test]
fn untrained_selector_always_picks_the_first_successor() {
    for successors in [2usize, 3, 4] {
        let ids: Vec<u32> = [ZSTD, STORE, BITPACK, CONSTANT][..successors].to_vec();
        let mut builder = GraphBuilder::new();
        let graphs: Vec<GraphId> = ids
            .iter()
            .map(|&id| StandardGraph::try_from(id).unwrap().into())
            .collect();
        let graph = builder.untrained_ml_selector(&graphs).unwrap();
        let data = ml_inputs(4).into_iter().nth(5).unwrap().1;
        let inputs = [In::plain(data)];
        let cfg = config(3, 27, false);
        let rust = rust_compress(builder, graph, cfg, &inputs);
        // Every forest is one leaf at -1: class 0 (binary) or the first forest
        // (multiclass, ties go to the first), i.e. the first successor alone.
        let expected = graph::compress(
            &[match &inputs[0].data {
                Data::Numeric(bytes, width) => CInput::Numeric {
                    bytes,
                    width: *width,
                },
                _ => unreachable!(),
            }],
            &Graph::Id(ids[0]),
            &c_params(&cfg),
        );
        assert_eq!(rust.unwrap(), expected.unwrap(), "{successors} successors");
    }
    let mut builder = GraphBuilder::new();
    assert!(
        builder
            .untrained_ml_selector(&[StandardGraph::Store.into()])
            .is_err()
    );
}

#[test]
fn malformed_trees_fail_instead_of_looping() {
    // The reference reads out of bounds or spins on these; mopenzl fails the
    // graph, and a permissive compression falls back to the generic graph.
    let data = ml_inputs(4).into_iter().nth(5).unwrap().1;
    let inputs = [In::plain(data)];
    let cyclic: Vec<N> = vec![(0, 10.0, 0, 0, 0)];
    let backwards: Vec<N> = vec![(0, 10.0, 1, 1, 1), (0, 10.0, 0, 0, 0)];
    let out_of_range: Vec<N> = vec![(0, 10.0, 5, 5, 5)];
    let no_nodes: Vec<N> = vec![];
    let below_minus_one: Vec<N> = vec![(-3, 1.0, 1, 1, 1), leaf(1.0)];
    for (name, tree) in [
        ("cyclic", cyclic),
        ("backwards", backwards),
        ("out of range", out_of_range),
        ("empty", no_nodes),
        ("feature below -1", below_minus_one),
    ] {
        let blob = model_cbor(&[vec![tree]], &["nbElts"], 2, 0);
        let config = MlSelectorConfig::try_from(&blob[..]).unwrap();
        let mut builder = GraphBuilder::new();
        let graph = builder
            .ml_selector(
                &config,
                &[StandardGraph::Store.into(), StandardGraph::Zstd.into()],
            )
            .unwrap();
        let strict = rust_compress(builder, graph, config_for_strict(), &inputs);
        assert!(strict.is_err(), "{name}: strict mode must fail");
    }
}

fn config_for_strict() -> EncoderConfig {
    config(3, 27, false)
}

#[test]
fn model_validation_matches_the_reference_rules() {
    let leaf_forest = vec![vec![GbtNode::leaf(0.0)]];
    let model = |forests: Vec<Vec<Vec<GbtNode>>>, successors, features: &[&str]| GbtModel {
        forests,
        successors,
        features: features.iter().map(|f| (*f).to_string()).collect(),
    };
    // No forests; one forest for 3 successors; 2 forests for 3 successors.
    assert!(MlSelectorConfig::try_from(&model(vec![], 2, &["a"])).is_err());
    assert!(MlSelectorConfig::try_from(&model(vec![leaf_forest.clone()], 3, &["a"])).is_err());
    assert!(MlSelectorConfig::try_from(&model(vec![leaf_forest.clone(); 2], 3, &["a"])).is_err());
    assert!(MlSelectorConfig::try_from(&model(vec![leaf_forest.clone(); 3], 3, &["a"])).is_ok());
    // Children must advance, stay in the tree, and thresholds be finite.
    let split = |feature, value, l, r, m| {
        vec![vec![vec![
            GbtNode::split(feature, value, l, r, m),
            GbtNode::leaf(0.0),
        ]]]
    };
    assert!(MlSelectorConfig::try_from(&model(split(0, 1.0, 1, 1, 1), 2, &["a"])).is_ok());
    assert!(MlSelectorConfig::try_from(&model(split(0, 1.0, 0, 1, 1), 2, &["a"])).is_err());
    assert!(MlSelectorConfig::try_from(&model(split(0, 1.0, 1, 2, 1), 2, &["a"])).is_err());
    assert!(MlSelectorConfig::try_from(&model(split(0, f32::NAN, 1, 1, 1), 2, &["a"])).is_err());
    assert!(
        MlSelectorConfig::try_from(&model(split(0, f32::INFINITY, 1, 1, 1), 2, &["a"])).is_err()
    );
    assert!(MlSelectorConfig::try_from(&model(split(1, 1.0, 1, 1, 1), 2, &["a"])).is_err());
    assert!(MlSelectorConfig::try_from(&model(split(-2, 1.0, 1, 1, 1), 2, &["a"])).is_err());
    assert!(
        MlSelectorConfig::try_from(&model(vec![vec![vec![]]], 2, &["a"])).is_err(),
        "an empty tree"
    );
}

#[test]
fn config_errors_are_typed_and_describe_the_problem() {
    use mopenzl::graph::ConfigError;
    let error = MlSelectorConfig::try_from(&[0xFFu8][..]).unwrap_err();
    assert!(matches!(error, ConfigError::Malformed(_)), "{error}");
    let error = MlSelectorConfig::try_from(&cbor(&V::Map(vec![]))[..]).unwrap_err();
    assert!(matches!(error, ConfigError::Invalid(_)), "{error}");
    let bad = GbtModel {
        forests: vec![],
        successors: 2,
        features: vec![],
    };
    let error = MlSelectorConfig::try_from(&bad).unwrap_err();
    assert!(matches!(error, ConfigError::Model(_)), "{error}");
    assert!(error.to_string().contains("forests"));
    let config = MlSelectorConfig::untrained(2).unwrap();
    assert!(format!("{config:?}").contains("MlSelectorConfig"));
    assert_eq!(config.clone(), config);
    assert!(MlSelectorConfig::untrained(1).is_err());
}

// -----------------------------------------------------------------------
// Clustering
// -----------------------------------------------------------------------

/// A clustering configuration for both libraries.
#[derive(Clone, Debug)]
struct Plan {
    clusters: Vec<(DataType, usize, usize, usize, Vec<i32>)>,
    defaults: Vec<(DataType, usize, usize, usize)>,
    successors: Vec<u32>,
}

fn type_code(ty: DataType) -> u32 {
    match ty {
        DataType::Serial => 1,
        DataType::Struct => 2,
        DataType::Numeric => 4,
        DataType::String => 8,
    }
}

impl Plan {
    fn rust_config(&self) -> ClusteringConfig {
        let mut config = ClusteringConfig::default();
        for (ty, width, successor, codec, tags) in &self.clusters {
            config = config.with_cluster(*ty, *width, *successor, *codec, tags.clone());
        }
        for &(ty, width, successor, codec) in &self.defaults {
            config = config.with_type_default(ty, width, successor, codec);
        }
        config
    }

    fn c_config(&self) -> enc_ml::ClusteringConfig {
        enc_ml::ClusteringConfig {
            clusters: self
                .clusters
                .iter()
                .map(|(ty, width, successor, codec, tags)| Cluster {
                    ty: type_code(*ty),
                    width: *width as u32,
                    successor: *successor as u32,
                    codec: *codec as u32,
                    tags: tags.clone(),
                })
                .collect(),
            defaults: self
                .defaults
                .iter()
                .map(|&(ty, width, successor, codec)| TypeDefault {
                    ty: type_code(ty),
                    width: width as u32,
                    successor: successor as u32,
                    codec: codec as u32,
                })
                .collect(),
        }
    }

    fn c_successors(&self) -> Vec<MlGraph> {
        self.successors
            .iter()
            .map(|&id| MlGraph::Plain(Graph::Id(id)))
            .collect()
    }

    /// The reference's typed registration with its four concat nodes.
    fn c_typed(&self) -> MlGraph {
        MlGraph::Clustering {
            config: self.c_config(),
            successors: self.c_successors(),
            nodes: vec![],
        }
    }

    /// The reference reading this crate's CBOR as a copy parameter.
    fn c_raw(&self, nodes: &[u32]) -> MlGraph {
        let blob: Vec<u8> = (&self.rust_config()).into();
        MlGraph::Parameterized {
            graph: CLUSTERING,
            params: Params {
                ints: vec![],
                copies: vec![(315, blob)],
            },
            custom: self.c_successors(),
            nodes: nodes.to_vec(),
            mparam: vec![],
        }
    }

    fn rust_graph(
        &self,
        codecs: Option<&[NodeId]>,
    ) -> Result<(GraphBuilder, GraphId), mopenzl::graph::GraphError> {
        let mut builder = GraphBuilder::new();
        let successors: Vec<GraphId> = self
            .successors
            .iter()
            .map(|&id| StandardGraph::try_from(id).unwrap().into())
            .collect();
        let config = self.rust_config();
        let graph = match codecs {
            None => builder.clustering(&config, &successors)?,
            Some(codecs) => builder.clustering_with_codecs(&config, &successors, codecs)?,
        };
        Ok((builder, graph))
    }
}

/// Compares the clustering graph of `plan` over `inputs` in both libraries
/// (both C registrations) at every version and a few levels.
fn check_clustering(what: &str, plan: &Plan, inputs: &[In]) -> Tally {
    let mut tally = Tally::default();
    let standard: Vec<NodeId> = [
        StandardNode::ConcatSerial,
        StandardNode::ConcatStruct,
        StandardNode::ConcatNum,
        StandardNode::ConcatString,
    ]
    .into_iter()
    .map(NodeId::from)
    .collect();
    for level in [3, 19] {
        for version in VERSIONS {
            for permissive in [false, true] {
                let cfg = config(level, version, permissive);
                let label = format!("{what} L{level} v{version} permissive={permissive}");
                let cinputs: Vec<MlInput<'_>> = inputs.iter().map(In::c).collect();
                let typed = enc_ml::compress(&cinputs, &plan.c_typed(), &c_params(&cfg));
                let raw = enc_ml::compress(&cinputs, &plan.c_raw(&NODE_CONCAT), &c_params(&cfg));
                assert_eq!(
                    typed.as_ref().ok(),
                    raw.as_ref().ok(),
                    "{label}: the reference's own serializer and this crate's differ"
                );
                let rust = plan
                    .rust_graph(None)
                    .map(|(builder, graph)| rust_compress(builder, graph, cfg, inputs));
                let rust = match rust {
                    Ok(result) => result,
                    Err(error) => {
                        assert!(typed.is_err(), "{label}: only mopenzl rejected: {error}");
                        tally.add(Outcome::BothFailed);
                        continue;
                    }
                };
                tally.add(compare(&label, typed, rust, inputs));
                // The explicit codec list must behave like the default one.
                let (builder, graph) = plan.rust_graph(Some(&standard)).unwrap();
                let explicit = rust_compress(builder, graph, cfg, inputs);
                let again = plan
                    .rust_graph(None)
                    .map(|(b, g)| rust_compress(b, g, cfg, inputs))
                    .unwrap();
                assert_eq!(explicit.ok(), again.ok(), "{label}: explicit codecs");
            }
        }
    }
    tally
}

fn serial(len: usize, seed: u32) -> Data {
    Data::Serial(support::text(len, seed))
}

fn words(n: usize, seed: u32, modulus: u64, width: usize) -> Data {
    let values: Vec<u64> = support::noise(n * 8, seed)
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| u64::from_le_bytes(*c) % modulus)
        .collect();
    numeric(&values, width)
}

fn strings(parts: &[&str]) -> Data {
    Data::Strings(
        parts.concat().into_bytes(),
        parts.iter().map(|part| part.len() as u32).collect(),
    )
}

#[test]
fn one_input_per_cluster_goes_straight_to_the_successor() {
    // No cluster has two members, so no concat node and no field_lz.
    let plan = Plan {
        clusters: vec![
            (DataType::Numeric, 4, 0, 0, vec![1]),
            (DataType::Numeric, 4, 1, 2, vec![2]),
            (DataType::Serial, 1, 1, 0, vec![5, 6]),
        ],
        defaults: vec![],
        successors: vec![STORE, ZSTD],
    };
    let inputs = [
        In::tagged(words(scale(500, 30), 1, 1000, 4), 1),
        In::tagged(words(scale(500, 30), 2, 1 << 30, 4), 2),
        In::tagged(serial(scale(2000, 100), 3), 5),
    ];
    let tally = check_clustering("straight", &plan, &inputs);
    tally.assert_identical("straight");
    assert_eq!(tally.skipped, 0);
}

#[test]
fn unconfigured_tags_get_their_own_clusters_and_the_generic_graph() {
    let plan = Plan {
        clusters: vec![],
        defaults: vec![],
        successors: vec![],
    };
    // Serial inputs go through zstd and concat_serial: byte identical.
    let serial_only = [
        In::tagged(serial(scale(900, 80), 2), -3),
        In::tagged(serial(scale(700, 60), 4), 12),
        In::tagged(serial(scale(500, 50), 5), 13),
    ];
    check_clustering("generic serial", &plan, &serial_only).assert_identical("generic serial");
    // Other types reach `field_lz` (SKIP(field_lz) until that graph lands).
    let mixed = [
        In::tagged(words(scale(300, 20), 1, 50, 2), 10),
        In::tagged(serial(scale(900, 80), 2), -3),
        In::tagged(Data::Struct(support::noise(6 * 40, 3), 6), 10),
        In::tagged(strings(&["alpha", "beta", "", "gamma"]), 0),
    ];
    let tally = check_clustering("generic mixed", &plan, &mixed);
    assert_eq!(tally.failed, 0, "{tally:?}");
}

#[test]
fn type_defaults_pick_the_successor_of_unconfigured_inputs() {
    let plan = Plan {
        clusters: vec![(DataType::Numeric, 8, 1, 2, vec![7])],
        defaults: vec![
            (DataType::Numeric, 4, 1, 2),
            (DataType::Serial, 1, 0, 0),
            (DataType::Struct, 6, 1, 1),
            (DataType::String, 0, 1, 3),
        ],
        successors: vec![ZSTD, STORE],
    };
    let inputs = [
        In::tagged(words(scale(400, 20), 1, 1 << 40, 8), 7),
        In::tagged(words(scale(400, 20), 2, 100, 4), 8),
        In::tagged(serial(scale(1500, 80), 3), 8),
        In::tagged(Data::Struct(support::noise(6 * 50, 4), 6), 8),
        In::tagged(strings(&["one", "two", "three"]), 8),
    ];
    check_clustering("defaults", &plan, &inputs).assert_identical("defaults");
}

#[test]
fn clusters_with_several_inputs_concatenate_before_the_successor() {
    // Needs the `field_lz` graph for the concat sizes.
    let plan = Plan {
        clusters: vec![
            (DataType::Numeric, 4, 0, 2, vec![1, 2, 3]),
            (DataType::Serial, 1, 1, 0, vec![4]),
            (DataType::Struct, 6, 0, 1, vec![9]),
            (DataType::String, 0, 1, 3, vec![11]),
            (DataType::Numeric, 8, 0, 2, vec![20]),
        ],
        defaults: vec![(DataType::Numeric, 2, 1, 2)],
        successors: vec![ZSTD, STORE],
    };
    let inputs = [
        In::tagged(words(scale(300, 20), 1, 1000, 4), 1),
        In::tagged(words(scale(200, 20), 2, 1 << 30, 4), 2),
        In::tagged(serial(scale(800, 60), 3), 4),
        In::tagged(words(scale(100, 20), 4, 1 << 40, 8), 20),
        In::tagged(words(scale(300, 20), 5, 77, 4), 3),
        In::tagged(Data::Struct(support::noise(6 * 30, 6), 6), 9),
        In::tagged(serial(scale(700, 60), 7), 4),
        In::tagged(Data::Struct(support::noise(6 * 25, 8), 6), 9),
        In::tagged(strings(&["a", "bb", "ccc"]), 11),
        In::tagged(strings(&["dddd", "", "e"]), 11),
        In::tagged(words(scale(150, 20), 9, 9, 2), 30),
        In::tagged(words(scale(150, 20), 10, 9, 2), 30),
    ];
    // While `field_lz` is not ported every comparison skips.
    let tally = check_clustering("concatenated", &plan, &inputs);
    assert_eq!(tally.failed, 0, "{tally:?}");
}

#[test]
fn input_and_cluster_order_decide_the_stream_order() {
    // The same inputs in another order must still match the reference.
    let plan = Plan {
        clusters: vec![
            (DataType::Numeric, 4, 0, 2, vec![1]),
            (DataType::Numeric, 4, 1, 2, vec![2]),
        ],
        defaults: vec![(DataType::Numeric, 4, 0, 2)],
        successors: vec![ZSTD, STORE],
    };
    let a = In::tagged(words(scale(300, 20), 1, 5000, 4), 2);
    let b = In::tagged(words(scale(300, 20), 2, 5000, 4), 1);
    let c = In::tagged(words(scale(300, 20), 3, 5000, 4), 99);
    for order in [[&a, &b, &c], [&c, &b, &a], [&b, &c, &a]] {
        let inputs: Vec<In> = order.iter().map(|input| (*input).clone()).collect();
        check_clustering("order", &plan, &inputs).assert_identical("order");
    }
}

#[test]
fn invalid_clustering_setups_fail_like_the_reference() {
    let inputs = [
        In::tagged(words(scale(200, 20), 1, 500, 4), 1),
        In::tagged(words(scale(200, 20), 2, 500, 4), 2),
    ];
    let valid = |clusters| Plan {
        clusters,
        defaults: vec![],
        successors: vec![ZSTD, STORE],
    };
    // A tag in two clusters of the same type and width.
    let duplicate = valid(vec![
        (DataType::Numeric, 4, 0, 0, vec![1]),
        (DataType::Numeric, 4, 1, 0, vec![1, 2]),
    ]);
    check_clustering("duplicate tag", &duplicate, &inputs).assert_failed("duplicate tag");
    // Successor index past the successors.
    let successor = valid(vec![(DataType::Numeric, 4, 5, 0, vec![1])]);
    check_clustering("bad successor", &successor, &inputs).assert_failed("bad successor");
    // Duplicate type defaults.
    let defaults = Plan {
        defaults: vec![(DataType::Numeric, 4, 0, 0), (DataType::Numeric, 4, 1, 0)],
        ..valid(vec![])
    };
    check_clustering("duplicate default", &defaults, &inputs).assert_failed("duplicate default");
    // An input without a tag.
    let untagged = [In::plain(words(scale(200, 20), 1, 500, 4))];
    let ok = valid(vec![(DataType::Numeric, 4, 0, 0, vec![1])]);
    check_clustering("no tag", &ok, &untagged).assert_failed("no tag");
}

#[test]
fn clustering_codec_indices_are_checked_when_the_graph_is_built() {
    let plan = Plan {
        clusters: vec![(DataType::Numeric, 4, 0, 4, vec![1])],
        defaults: vec![],
        successors: vec![STORE],
    };
    // The reference's registration returns the illegal graph.
    let reference = enc_ml::compress(
        &[In::tagged(words(100, 1, 9, 4), 1).c()],
        &plan.c_typed(),
        &[],
    );
    assert!(reference.is_err());
    assert!(plan.rust_graph(None).is_err());
    let mut builder = GraphBuilder::new();
    let custom: Vec<NodeId> = vec![StandardNode::ConcatNum.into()];
    let config = ClusteringConfig::default().with_cluster(DataType::Numeric, 4, 0, 0, [1]);
    assert!(
        builder
            .clustering_with_codecs(&config, &[StandardGraph::Store.into()], &custom)
            .is_ok()
    );
}

#[test]
fn clustering_configs_round_trip_through_cbor() {
    let config = ClusteringConfig::default()
        .with_cluster(DataType::Numeric, 4, 0, 1, [1, 2, i32::MAX])
        .with_cluster(DataType::String, 0, 1, 3, [])
        .with_type_default(DataType::Serial, 1, 0, 0)
        .with_type_default(DataType::Struct, 12, 1, 1);
    let blob: Vec<u8> = (&config).into();
    assert_eq!(ClusteringConfig::try_from(&blob[..]).unwrap(), config);
    assert_eq!(
        Vec::<u8>::from(&ClusteringConfig::default()),
        cbor(&V::Map(vec![
            ("clusters", V::Arr(vec![])),
            ("typeDefaults", V::Arr(vec![])),
        ]))
    );
    // Hand-written equivalent of one cluster and one default.
    let ts = |ty, width, s, c| {
        V::Map(vec![
            ("type", V::Int(ty)),
            ("eltWidth", V::Int(width)),
            ("successorIdx", V::Int(s)),
            ("clusteringCodecIdx", V::Int(c)),
        ])
    };
    let handwritten = cbor(&V::Map(vec![
        (
            "clusters",
            V::Arr(vec![V::Map(vec![
                ("typeSuccessor", ts(4, 4, 0, 1)),
                ("memberTags", V::Arr(vec![V::Int(1), V::Int(-2)])),
            ])]),
        ),
        ("typeDefaults", V::Arr(vec![ts(1, 1, 1, 0)])),
    ]));
    let parsed = ClusteringConfig::try_from(&handwritten[..]).unwrap();
    assert_eq!(
        parsed,
        ClusteringConfig::default()
            .with_cluster(DataType::Numeric, 4, 0, 1, [1, -2])
            .with_type_default(DataType::Serial, 1, 1, 0)
    );
    assert_eq!(Vec::<u8>::from(&parsed), handwritten);
    // Rejections.
    let tag = cbor(&V::Map(vec![
        (
            "clusters",
            V::Arr(vec![V::Map(vec![
                ("typeSuccessor", ts(4, 4, 0, 0)),
                ("memberTags", V::Arr(vec![V::Int(i64::from(i32::MAX) + 1)])),
            ])]),
        ),
        ("typeDefaults", V::Arr(vec![])),
    ]));
    assert!(ClusteringConfig::try_from(&tag[..]).is_err());
    let unknown_type = cbor(&V::Map(vec![
        ("clusters", V::Arr(vec![])),
        ("typeDefaults", V::Arr(vec![ts(3, 4, 0, 0)])),
    ]));
    assert!(ClusteringConfig::try_from(&unknown_type[..]).is_err());
    assert!(ClusteringConfig::try_from(&[0xFFu8][..]).is_err());
    assert!(ClusteringConfig::try_from(&cbor(&V::Map(vec![]))[..]).is_err());
}

#[test]
fn input_metadata_is_not_written_to_the_frame() {
    let builder = GraphBuilder::new();
    let graph = builder.build(StandardGraph::Store).unwrap();
    let data = [1u8, 0, 0, 0, 2, 0, 0, 0];
    let numbers = Numbers::try_from((&data[..], 4)).unwrap();
    let tags = [(0, 5), (7, -1)];
    let plain = Input::from(numbers);
    let tagged = Input::from(numbers).with_metadata(&tags);
    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()
        .unwrap();
    assert_eq!(
        compressor.compress_inputs(&[plain]).unwrap(),
        compressor.compress_inputs(&[tagged]).unwrap()
    );
}

#[test]
fn custom_clustering_codecs_are_indexed_by_the_config() {
    // Codec 0 is the numeric concat, codec 1 the serial one.
    let codecs = [34u32, 33];
    let config = ClusteringConfig::default()
        .with_cluster(DataType::Numeric, 4, 0, 0, [1])
        .with_cluster(DataType::Serial, 1, 1, 1, [2]);
    let c_config = enc_ml::ClusteringConfig {
        clusters: vec![
            Cluster {
                ty: 4,
                width: 4,
                successor: 0,
                codec: 0,
                tags: vec![1],
            },
            Cluster {
                ty: 1,
                width: 1,
                successor: 1,
                codec: 1,
                tags: vec![2],
            },
        ],
        defaults: vec![],
    };
    let inputs = [
        In::tagged(words(scale(200, 20), 1, 900, 4), 1),
        In::tagged(serial(scale(600, 60), 2), 2),
        In::tagged(words(scale(200, 20), 3, 90, 4), 1),
        In::tagged(serial(scale(600, 60), 4), 2),
    ];
    let c_graph = MlGraph::Clustering {
        config: c_config,
        successors: vec![MlGraph::Plain(Graph::Id(ZSTD)), MlGraph::STORE],
        nodes: codecs.to_vec(),
    };
    for version in VERSIONS {
        let cfg = config_for(version);
        let cinputs: Vec<MlInput<'_>> = inputs.iter().map(In::c).collect();
        let reference = enc_ml::compress(&cinputs, &c_graph, &c_params(&cfg));
        let mut builder = GraphBuilder::new();
        let nodes: Vec<NodeId> = codecs
            .iter()
            .map(|&id| StandardNode::try_from(id).unwrap().into())
            .collect();
        let graph = builder
            .clustering_with_codecs(
                &config,
                &[StandardGraph::Zstd.into(), StandardGraph::Store.into()],
                &nodes,
            )
            .unwrap();
        let rust = rust_compress(builder, graph, cfg, &inputs);
        compare(
            &format!("custom codecs v{version}"),
            reference,
            rust,
            &inputs,
        );
    }
}

fn config_for(version: u32) -> EncoderConfig {
    config(3, version, false)
}
