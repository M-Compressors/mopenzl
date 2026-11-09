//! Byte identity of the entropy encoders with the reference: the FSE,
//! Huffman, 16-bit Huffman, PivCo-Huffman, constant and `fse_ncount` nodes,
//! and the `fse`, `huffman`, `huffman_huf0`, `huffman_pivco`, `entropy` and
//! `constant` graphs.
//!
//! Every comparison encodes the same input in C (`openzl_ffi::graph`) and in
//! this crate, requires the same frame bytes (or that both fail), and decodes
//! the frame with this crate's decoder and the reference's.
//!
//! Every comparison is strict: a frame C produces must be produced here
//! byte for byte, whichever successor graph or node it reaches.
#![cfg(all(
    feature = "compression",
    feature = "decompression",
    feature = "conversion"
))]
#![cfg_attr(
    not(feature = "entropy"),
    expect(
        dead_code,
        reason = "helpers of the entropy tests, which the `entropy` feature gates"
    )
)]

mod support;

use mopenzl::graph::{CompressorGraph, GraphBuilder, StandardGraph, StandardNode};
#[cfg(feature = "entropy")]
use mopenzl::graph::{GraphParams, LocalParams};
use mopenzl::input::{Input, Numbers, Records};
use mopenzl::{Compressor, Decompressor, EncodeError, EncoderConfig, FormatVersion, Level};
use openzl_ffi::graph::{self, Graph, Input as CInput, cparam};

/// `ZL_ErrorCode_dstCapacity_tooSmall`.
const DST_CAPACITY_TOO_SMALL: i32 = 5;
const MIN_GAIN_BYTES_PID: i32 = 66;
const MIN_GAIN_PCT_PID: i32 = 80;

fn config(level: i32, version: u32) -> EncoderConfig {
    EncoderConfig::default()
        .with_level(Level::try_from(level).unwrap())
        .with_format_version(FormatVersion::try_from(version).unwrap())
}

/// A configuration that lets tiny streams reach the nodes (the minimum
/// stream size is 1) and keeps outputs that expand.
fn raw_config(version: u32) -> EncoderConfig {
    config(6, version)
        .with_min_stream_size(1)
        .with_store_on_expansion(false)
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

/// A typed test input.
#[derive(Clone)]
enum Data {
    Serial(Vec<u8>),
    Struct(Vec<u8>, usize),
    Numeric(Vec<u8>, usize),
}

impl Data {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Serial(bytes) | Self::Struct(bytes, _) | Self::Numeric(bytes, _) => bytes,
        }
    }

    fn input(&self) -> Input<'_> {
        match self {
            Self::Serial(bytes) => Input::from(&bytes[..]),
            Self::Struct(bytes, width) => {
                Input::from(Records::try_from((&bytes[..], *width)).unwrap())
            }
            Self::Numeric(bytes, width) => {
                Input::from(Numbers::try_from((&bytes[..], *width)).unwrap())
            }
        }
    }

    fn c_input(&self) -> CInput<'_> {
        match self {
            Self::Serial(bytes) => CInput::Serial(bytes),
            Self::Struct(bytes, width) => CInput::Struct {
                bytes,
                width: *width,
            },
            Self::Numeric(bytes, width) => CInput::Numeric {
                bytes,
                width: *width,
            },
        }
    }
}

/// How a comparison ended.
#[derive(Debug, Eq, PartialEq)]
enum Outcome {
    /// Both sides produced the same frame, which decodes to the input.
    Identical(usize),
    /// Both sides failed.
    BothFailed,
}

fn rust_compress(
    start: impl Into<CompressorGraph>,
    config: EncoderConfig,
    data: &Data,
) -> Result<Vec<u8>, EncodeError> {
    let mut compressor = Compressor::builder(config)
        .with_graph(start.into())
        .build()
        .unwrap();
    compressor.compress_inputs(&[data.input()])
}

/// Compresses `data` through `c_graph` in C and `rust_graph` here and
/// compares.
fn compare(
    label: &str,
    c_graph: &Graph,
    rust_graph: impl Into<CompressorGraph>,
    config: EncoderConfig,
    data: &Data,
) -> Outcome {
    let expected = graph::compress(&[data.c_input()], c_graph, &c_params(&config));
    let actual = rust_compress(rust_graph, config, data);
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => {
            assert_eq!(actual, expected, "{label}: frames differ");
            let decoded = Decompressor::default()
                .decompress_typed(&actual)
                .unwrap_or_else(|error| panic!("{label}: our decoder: {error}"));
            assert_eq!(decoded.len(), 1, "{label}");
            assert_eq!(decoded[0].content(), data.bytes(), "{label}: our decoder");
            let c = graph::decompress(&actual)
                .unwrap_or_else(|error| panic!("{label}: C decoder: {error:?}"));
            assert_eq!(c[0].bytes, data.bytes(), "{label}: C decoder");
            Outcome::Identical(actual.len())
        }
        (Err(_), Err(_)) => Outcome::BothFailed,
        (Ok(expected), Err(error)) => {
            panic!(
                "{label}: C produced {} bytes, we failed: {error}",
                expected.len()
            )
        }
        // The reference writes into a `ZL_compressBound` buffer and fails when an
        // expanding frame (store-on-expansion off) does not fit; this crate
        // returns the whole frame.
        (Err(error), Ok(actual))
            if error.0 == DST_CAPACITY_TOO_SMALL
                && actual.len() > openzl_ffi::compress_bound(data.bytes().len()) =>
        {
            Outcome::BothFailed
        }
        (Err(error), Ok(actual)) => {
            panic!(
                "{label}: C failed ({error:?}), we produced {} bytes",
                actual.len()
            )
        }
    }
}

/// Runs one standard node into `Store` successors.
fn node_graph(node: StandardNode, outputs: usize) -> CompressorGraph {
    let mut builder = GraphBuilder::new();
    let successors = vec![StandardGraph::Store.into(); outputs];
    let graph = builder.static_graph(node, &successors).unwrap();
    builder.build(graph).unwrap()
}

/// Compares one node into `Store` successors, once with `config` and once
/// with [`raw_config`] (tiny streams reach the node; expansions are kept).
fn compare_node(
    label: &str,
    id: u32,
    node: StandardNode,
    outputs: usize,
    config: EncoderConfig,
    data: &Data,
) -> Outcome {
    let successors = vec![Graph::STORE; outputs];
    let c_graph = Graph::node(id, successors);
    let first = compare(label, &c_graph, node_graph(node, outputs), config, data);
    let raw = raw_config(u32::from(config.format_version()));
    let second = compare(
        &format!("{label} (raw)"),
        &c_graph,
        node_graph(node, outputs),
        raw,
        data,
    );
    match (first, second) {
        (Outcome::Identical(size), _) | (_, Outcome::Identical(size)) => Outcome::Identical(size),
        (first, _) => first,
    }
}

fn compare_graph(
    label: &str,
    id: u32,
    graph: StandardGraph,
    config: EncoderConfig,
    data: &Data,
) -> Outcome {
    compare(label, &Graph::Id(id), graph, config, data)
}

// ---- Corpora ----

fn lcg(seed: u32) -> impl FnMut() -> u32 {
    let mut state = seed.wrapping_mul(2_654_435_761) | 1;
    move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    }
}

/// Bytes with a geometric distribution (each step continues with
/// probability 3/4) capped at `symbols` values.
fn skewed(len: usize, symbols: u32, seed: u32) -> Vec<u8> {
    let mut next = lcg(seed);
    (0..len)
        .map(|_| {
            let mut s = 0u32;
            while s + 1 < symbols && next() & 3 != 0 {
                s += 1;
            }
            s as u8
        })
        .collect()
}

/// Uniform bytes over `symbols` values.
fn uniform(len: usize, symbols: u32, seed: u32) -> Vec<u8> {
    let mut next = lcg(seed);
    (0..len).map(|_| (next() % symbols) as u8).collect()
}

/// 16-bit little-endian symbols over `symbols` values, spread over the range.
fn wide(len: usize, symbols: u32, seed: u32) -> Vec<u8> {
    let mut next = lcg(seed);
    (0..len)
        .flat_map(|_| {
            let r = next() % symbols;
            let value = (r.wrapping_mul(2_654_435_761) >> 16) as u16;
            value.to_le_bytes()
        })
        .collect()
}

/// Skewed 16-bit symbols.
fn wide_skewed(len: usize, seed: u32) -> Vec<u8> {
    let bytes = skewed(len, 50, seed);
    bytes
        .iter()
        .flat_map(|&b| (u16::from(b) * 101 + 7).to_le_bytes())
        .collect()
}

/// Symbol `i` repeated Fibonacci(i) times, shuffled: the deepest Huffman
/// trees, which need the code-length limiter. The tail is padded with the
/// most frequent symbol up to `n` elements.
fn fibonacci(n: usize, seed: u32) -> Vec<u16> {
    let mut counts = vec![1usize];
    let (mut a, mut b) = (1usize, 2usize);
    while counts.iter().sum::<usize>() + a <= n {
        counts.push(a);
        (a, b) = (b, a + b);
    }
    let mut out: Vec<u16> = Vec::with_capacity(n);
    for (symbol, &count) in counts.iter().enumerate() {
        out.extend(std::iter::repeat_n(symbol as u16, count));
    }
    let last = counts.len().saturating_sub(1) as u16;
    out.resize(n, last);
    let mut next = lcg(seed);
    for i in (1..out.len()).rev() {
        out.swap(i, (next() as usize) % (i + 1));
    }
    out
}

fn fibonacci_bytes(n: usize, seed: u32) -> Vec<u8> {
    fibonacci(n, seed).into_iter().map(|s| s as u8).collect()
}

fn fibonacci_wide(n: usize, seed: u32) -> Vec<u8> {
    fibonacci(n, seed)
        .into_iter()
        .flat_map(|s| (s * 41 + 5).to_le_bytes())
        .collect()
}

fn byte_corpus(sizes: &[usize]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for &n in sizes {
        if n >= 30 {
            out.push((format!("fibonacci-{n}"), fibonacci_bytes(n, 9)));
        }
        if n >= 100 {
            out.push((format!("uniform255-{n}"), uniform(n, 255, 21)));
            out.push((format!("uniform100-{n}"), uniform(n, 100, 22)));
        }
        out.push((format!("text-{n}"), support::text(n, 1)));
        out.push((format!("noise-{n}"), support::noise(n, 2)));
        out.push((format!("skewed-{n}"), skewed(n, 40, 3)));
        out.push((format!("skewed-wide-{n}"), skewed(n, 200, 31)));
        out.push((format!("uniform2-{n}"), uniform(n, 2, 4)));
        out.push((format!("uniform17-{n}"), uniform(n, 17, 5)));
        out.push((format!("constant-{n}"), vec![7u8; n]));
    }
    out
}

fn sizes_small() -> Vec<usize> {
    vec![
        0, 1, 2, 3, 4, 5, 7, 16, 63, 64, 65, 100, 255, 256, 257, 999, 1000, 1001, 4096,
    ]
}

fn sizes_large() -> Vec<usize> {
    if cfg!(miri) {
        vec![5000]
    } else {
        vec![40_000, 100_000]
    }
}

// ---- Nodes ----

#[cfg(feature = "entropy")]
#[test]
fn fse_v2_node_is_byte_identical() {
    let mut identical = 0;
    for version in [21, 24, 27] {
        for (name, bytes) in byte_corpus(&sizes_small()) {
            let data = Data::Serial(bytes);
            let outcome = compare_node(
                &format!("fse_v2 v{version} {name}"),
                58,
                StandardNode::FseV2,
                2,
                config(6, version),
                &data,
            );
            identical += usize::from(matches!(outcome, Outcome::Identical(_)));
        }
    }
    assert!(identical > 100, "{identical}");
}

#[cfg(feature = "entropy")]
#[test]
fn fse_v2_node_handles_large_inputs_with_four_states() {
    for (name, bytes) in byte_corpus(&sizes_large()) {
        let data = Data::Serial(bytes);
        compare_node(
            &format!("fse_v2 {name}"),
            58,
            StandardNode::FseV2,
            2,
            config(6, 27),
            &data,
        );
    }
}

#[cfg(feature = "entropy")]
#[test]
fn huffman_v2_node_is_byte_identical() {
    let mut identical = 0;
    for version in [21, 24, 27] {
        for (name, bytes) in byte_corpus(&sizes_small()) {
            let data = Data::Serial(bytes);
            let outcome = compare_node(
                &format!("huffman_v2 v{version} {name}"),
                59,
                StandardNode::HuffmanV2,
                2,
                config(6, version),
                &data,
            );
            identical += usize::from(matches!(outcome, Outcome::Identical(_)));
        }
    }
    assert!(identical > 100, "{identical}");
}

#[cfg(feature = "entropy")]
#[test]
fn huffman_v2_node_handles_four_streams_and_large_inputs() {
    for (name, bytes) in byte_corpus(&sizes_large()) {
        let data = Data::Serial(bytes);
        compare_node(
            &format!("huffman_v2 {name}"),
            59,
            StandardNode::HuffmanV2,
            2,
            config(6, 27),
            &data,
        );
    }
}

#[cfg(feature = "entropy")]
#[test]
fn huffman_struct_v2_node_is_byte_identical() {
    let sizes = [
        0usize, 1, 2, 3, 10, 100, 999, 1000, 1001, 1500, 5000, 20_000,
    ];
    let mut identical = 0;
    for version in [21, 27] {
        for &n in &sizes {
            let cases = [
                ("wide-2", wide(n, 2, 1)),
                ("wide-30", wide(n, 30, 2)),
                ("wide-500", wide(n, 500, 3)),
                ("wide-60000", wide(n, 60_000, 4)),
                ("skewed", wide_skewed(n, 5)),
                ("fibonacci", fibonacci_wide(n, 6)),
                ("constant", vec![0x34u8; 2 * n]),
            ];
            for (name, bytes) in cases {
                let data = Data::Struct(bytes, 2);
                let outcome = compare_node(
                    &format!("huffman_struct_v2 v{version} {name}-{n}"),
                    60,
                    StandardNode::HuffmanStructV2,
                    2,
                    config(6, version),
                    &data,
                );
                identical += usize::from(matches!(outcome, Outcome::Identical(_)));
            }
        }
    }
    assert!(identical > 50, "{identical}");
}

#[cfg(feature = "entropy")]
#[test]
fn huffman_struct_v2_node_rejects_other_widths() {
    for width in [1usize, 3, 4, 8] {
        let data = Data::Struct(support::noise(width * 400, 9), width);
        let outcome = compare_node(
            &format!("huffman_struct_v2 width {width}"),
            60,
            StandardNode::HuffmanStructV2,
            2,
            config(6, 27),
            &data,
        );
        assert_eq!(outcome, Outcome::BothFailed, "width {width}");
    }
}

#[cfg(feature = "entropy")]
#[test]
fn huffman_struct_v2_node_handles_large_inputs() {
    let n = if cfg!(miri) { 2000 } else { 60_000 };
    for (name, bytes) in [
        ("wide-5000", wide(n, 5000, 6)),
        ("skewed", wide_skewed(n, 7)),
    ] {
        let data = Data::Struct(bytes, 2);
        compare_node(
            &format!("huffman_struct_v2 {name}"),
            60,
            StandardNode::HuffmanStructV2,
            2,
            config(6, 27),
            &data,
        );
    }
}

/// Valid and invalid normalised-count tables as 16-bit numbers.
fn ncount_tables() -> Vec<(String, Vec<i16>)> {
    let mut out: Vec<(String, Vec<i16>)> = vec![
        ("two".into(), vec![16, 16]),
        ("three".into(), vec![16, 8, 8]),
        ("lowprob".into(), vec![-1, -1, 30]),
        ("zeros-inside".into(), vec![20, 0, 0, 0, 12]),
        ("long-zero-run".into(), {
            let mut v = vec![0i16; 60];
            v[0] = 16;
            v[59] = 16;
            v
        }),
        ("zero-run-24".into(), {
            let mut v = vec![0i16; 26];
            v[0] = 16;
            v[25] = 16;
            v
        }),
        ("zero-run-3".into(), vec![16, 0, 0, 0, 16]),
        (
            "log12".into(),
            vec![2048, 1024, 512, 256, 128, 64, 32, 16, 8, 4, 2, 1, 1],
        ),
        ("log12-ones".into(), {
            let mut v = vec![1i16; 4096];
            v.truncate(256);
            v
        }),
        // Invalid tables.
        ("empty".into(), vec![]),
        ("last-zero".into(), vec![16, 16, 0]),
        ("sum-not-pow2".into(), vec![16, 15]),
        ("below-minus-one".into(), vec![-2, 34]),
        ("log-too-small".into(), vec![8, 8]),
        ("log-too-large".into(), vec![4096, 4096]),
        ("zero-sum".into(), vec![0, 0, 1, -1]),
        ("over-256".into(), vec![1i16; 257]),
        ("single".into(), vec![32]),
    ];
    // Random distributions of 2^log over m symbols.
    let mut next = lcg(77);
    for log in 5..=12u32 {
        for round in 0..3 {
            let total = 1u32 << log;
            let m = 2 + (next() % 40) as usize;
            let mut counts = vec![1i16; m];
            let mut left = total as i32 - m as i32;
            while left > 0 {
                let i = (next() as usize) % m;
                let add = 1 + (next() % (left as u32).min(1 + total / 8)) as i32;
                counts[i] += add as i16;
                left -= add;
            }
            for c in counts.iter_mut() {
                if *c == 1 && next().is_multiple_of(3) {
                    *c = -1;
                }
            }
            // Interleave zeros.
            let mut with_zeros = Vec::new();
            for c in counts {
                let zeros = (next() % 4) / 3 * (next() % 30);
                with_zeros.extend(std::iter::repeat_n(0, zeros as usize));
                with_zeros.push(c);
            }
            out.push((format!("random-{log}-{round}"), with_zeros));
        }
    }
    out
}

#[cfg(feature = "entropy")]
#[test]
fn fse_ncount_node_is_byte_identical_and_fails_where_the_reference_fails() {
    let mut identical = 0;
    for version in [21, 27] {
        for (name, counts) in ncount_tables() {
            let bytes: Vec<u8> = counts.iter().flat_map(|c| c.to_le_bytes()).collect();
            let data = Data::Numeric(bytes, 2);
            let outcome = compare_node(
                &format!("fse_ncount v{version} {name}"),
                61,
                StandardNode::FseNcount,
                1,
                config(6, version),
                &data,
            );
            identical += usize::from(matches!(outcome, Outcome::Identical(_)));
        }
    }
    assert!(identical > 20, "{identical}");
    // Other widths fail.
    for width in [1usize, 4, 8] {
        let data = Data::Numeric(vec![16u8; width * 100], width);
        assert_eq!(
            compare_node(
                "fse_ncount width",
                61,
                StandardNode::FseNcount,
                1,
                config(6, 27),
                &data
            ),
            Outcome::BothFailed
        );
    }
}

#[cfg(feature = "constant")]
#[test]
fn constant_nodes_are_byte_identical() {
    for version in [21, 24, 27] {
        for n in [0usize, 1, 2, 5, 11, 100, 70_000] {
            let cases = [
                ("same", vec![9u8; n]),
                ("zeros", vec![0u8; n]),
                ("noise", support::noise(n, 4)),
                ("almost", {
                    let mut v = vec![5u8; n];
                    if n > 3 {
                        v[n - 1] = 6;
                    }
                    v
                }),
            ];
            for (name, bytes) in cases {
                compare_node(
                    &format!("constant_serial v{version} {name}-{n}"),
                    70,
                    StandardNode::ConstantSerial,
                    1,
                    config(6, version),
                    &Data::Serial(bytes),
                );
            }
        }
        for width in [1usize, 2, 3, 4, 7, 8, 12] {
            for n in [0usize, 1, 2, 9, 300] {
                let element: Vec<u8> = (0..width as u8).map(|b| b * 3 + 1).collect();
                let same: Vec<u8> = element.iter().cycle().take(width * n).copied().collect();
                let mut differ = same.clone();
                if n > 1 {
                    let last = differ.len() - 1;
                    differ[last] ^= 1;
                }
                for (name, bytes) in [("same", same), ("differ", differ)] {
                    compare_node(
                        &format!("constant_fixed v{version} w{width} {name}-{n}"),
                        71,
                        StandardNode::ConstantFixed,
                        1,
                        config(6, version),
                        &Data::Struct(bytes, width),
                    );
                }
            }
        }
    }
}

#[cfg(feature = "pivco-huffman")]
#[test]
fn pivco_huffman_node_is_byte_identical() {
    let mut sizes = sizes_small();
    sizes.extend([8191, 32767, 32768, 32769, 65536, 70_000]);
    let mut identical = 0;
    for (name, bytes) in byte_corpus(&sizes) {
        let data = Data::Serial(bytes);
        let outcome = compare_node(
            &format!("pivco_huffman {name}"),
            87,
            StandardNode::PivcoHuffman,
            2,
            config(6, 27),
            &data,
        );
        identical += usize::from(matches!(outcome, Outcome::Identical(_)));
    }
    assert!(identical > 80, "{identical}");
}

#[cfg(feature = "pivco-huffman")]
#[test]
fn pivco_huffman_node_exists_only_at_format_version_27() {
    for version in [21, 24, 26] {
        let data = Data::Serial(skewed(2000, 40, 8));
        assert_eq!(
            compare_node(
                &format!("pivco v{version}"),
                87,
                StandardNode::PivcoHuffman,
                2,
                config(6, version),
                &data,
            ),
            Outcome::BothFailed
        );
    }
}

#[cfg(feature = "pivco-huffman")]
#[test]
fn pivco_huffman_handles_wide_alphabets_and_flat_leaves() {
    // Alphabets that exercise flat leaves of every depth and a full 256 symbols.
    for symbols in [
        3u32, 4, 5, 6, 7, 8, 9, 15, 16, 17, 31, 33, 64, 100, 128, 129, 255, 256,
    ] {
        let bytes = uniform(20_000, symbols, symbols);
        compare_node(
            &format!("pivco uniform {symbols}"),
            87,
            StandardNode::PivcoHuffman,
            2,
            config(6, 27),
            &Data::Serial(bytes),
        );
        let bytes = skewed(20_000, symbols.min(250), symbols + 100);
        compare_node(
            &format!("pivco skewed {symbols}"),
            87,
            StandardNode::PivcoHuffman,
            2,
            config(6, 27),
            &Data::Serial(bytes),
        );
    }
}

// ---- Graphs ----

fn graph_inputs() -> Vec<(String, Data)> {
    let mut out = Vec::new();
    let sizes: Vec<usize> = if cfg!(miri) {
        vec![0, 1, 40, 300]
    } else {
        vec![0, 1, 2, 10, 64, 65, 300, 1000, 5000, 40_000]
    };
    for n in sizes {
        for (name, bytes) in [
            ("text", support::text(n, 1)),
            ("noise", support::noise(n, 2)),
            ("skewed", skewed(n, 40, 3)),
            ("skewed-wide", skewed(n, 200, 31)),
            ("uniform2", uniform(n, 2, 4)),
            ("uniform17", uniform(n, 17, 5)),
            ("uniform200", uniform(n, 200, 6)),
            ("constant", vec![9u8; n]),
        ] {
            out.push((format!("serial-{name}-{n}"), Data::Serial(bytes)));
        }
        out.push((format!("struct2-wide-{n}"), Data::Struct(wide(n, 30, 7), 2)));
        out.push((
            format!("struct2-wide-many-{n}"),
            Data::Struct(wide(n, 3000, 8), 2),
        ));
        out.push((
            format!("struct2-skewed-{n}"),
            Data::Struct(wide_skewed(n, 9), 2),
        ));
        out.push((
            format!("struct2-const-{n}"),
            Data::Struct(vec![3u8; 2 * n], 2),
        ));
        out.push((
            format!("struct2-small-alphabet-{n}"),
            Data::Struct(wide(n, 300, 12), 2),
        ));
        out.push((format!("num1-{n}"), Data::Numeric(skewed(n, 30, 10), 1)));
        out.push((format!("num2-{n}"), Data::Numeric(wide_skewed(n, 11), 2)));
    }
    out
}

fn run_graph_matrix(id: u32, graph: StandardGraph, levels: &[i32], versions: &[u32]) {
    let mut identical = 0;
    for (name, data) in graph_inputs() {
        for &version in versions {
            for &level in levels {
                let outcome = compare_graph(
                    &format!("graph {id} v{version} l{level} {name}"),
                    id,
                    graph,
                    config(level, version),
                    &data,
                );
                match outcome {
                    Outcome::Identical(_) => identical += 1,
                    Outcome::BothFailed => {}
                }
            }
        }
    }
    eprintln!("graph {id}: {identical} identical");
}

#[cfg(feature = "entropy")]
#[test]
fn fse_graph_is_byte_identical() {
    run_graph_matrix(3, StandardGraph::Fse, &[1, 6], &[21, 27]);
}

#[cfg(feature = "entropy")]
#[test]
fn huffman_graph_is_byte_identical() {
    run_graph_matrix(4, StandardGraph::Huffman, &[1, 6], &[21, 27]);
}

#[cfg(feature = "entropy")]
#[test]
fn huffman_huf0_graph_is_byte_identical() {
    run_graph_matrix(5, StandardGraph::HuffmanHuf0, &[6], &[21, 27]);
}

#[cfg(all(feature = "entropy", feature = "pivco-huffman"))]
#[test]
fn huffman_pivco_graph_is_byte_identical() {
    run_graph_matrix(6, StandardGraph::HuffmanPivco, &[6], &[21, 24, 27]);
}

#[cfg(feature = "entropy")]
#[test]
fn entropy_graph_is_byte_identical_at_every_level_class() {
    run_graph_matrix(
        7,
        StandardGraph::Entropy,
        &[1, 3, 6, 7, 10, 19],
        &[21, 24, 27],
    );
}

#[cfg(feature = "entropy")]
#[test]
fn entropy_graph_honours_the_minimum_gain_parameters() {
    for (bytes, pct) in [(0, 0), (1000, 1), (0, 50), (200, 100), (-5, 200)] {
        let c_graph = Graph::Parameterized {
            graph: 7,
            params: graph::Params {
                ints: vec![(MIN_GAIN_BYTES_PID, bytes), (MIN_GAIN_PCT_PID, pct)],
                copies: vec![],
            },
            custom: vec![],
        };
        let mut builder = GraphBuilder::new();
        let tuned = builder
            .parameterize_graph(
                StandardGraph::Entropy,
                GraphParams::default().with_params(
                    LocalParams::default()
                        .with_int(MIN_GAIN_BYTES_PID, bytes)
                        .with_int(MIN_GAIN_PCT_PID, pct),
                ),
            )
            .unwrap();
        let rust_graph = builder.build(tuned).unwrap();
        for (name, data) in graph_inputs() {
            if !name.contains("4000") && !name.contains("-300") && !name.contains("-1000") {
                continue;
            }
            compare(
                &format!("entropy gain {bytes}/{pct} {name}"),
                &c_graph,
                rust_graph.clone(),
                config(6, 27),
                &data,
            );
        }
    }
}

#[test]
fn constant_graph_is_byte_identical() {
    for version in [21, 27] {
        for (name, data) in graph_inputs() {
            compare_graph(
                &format!("constant v{version} {name}"),
                8,
                StandardGraph::Constant,
                config(6, version),
                &data,
            );
        }
    }
    // Multi-byte elements whose bytes all agree take the serial route.
    for (width, byte) in [(2usize, 0xffu8), (4, 0), (8, 0x55), (3, 1)] {
        for n in [1usize, 5, 200] {
            compare_graph(
                &format!("constant struct w{width}"),
                8,
                StandardGraph::Constant,
                config(6, 27),
                &Data::Struct(vec![byte; width * n], width),
            );
        }
    }
    for (width, bytes) in [(2usize, [1u8, 2]), (4, [9, 9]), (8, [0, 1])] {
        let element: Vec<u8> = bytes.iter().cycle().take(width).copied().collect();
        let data: Vec<u8> = element.iter().cycle().take(width * 50).copied().collect();
        compare_graph(
            &format!("constant numeric w{width}"),
            8,
            StandardGraph::Constant,
            config(6, 27),
            &Data::Numeric(data, width),
        );
    }
}

#[cfg(feature = "entropy")]
#[test]
fn entropy_graphs_split_inputs_of_100000_elements_or_more() {
    // `chunkInputStream` cuts 32 KiB chunks through the splitN nodes.
    let n = if cfg!(miri) { 2000 } else { 150_000 };
    let even = if cfg!(miri) { 2048 } else { 131_072 };
    for (name, data) in [
        ("serial-skewed", Data::Serial(skewed(n, 40, 1))),
        ("serial-even", Data::Serial(skewed(even, 40, 2))),
        ("serial-text", Data::Serial(support::text(n, 3))),
        ("struct2-skewed", Data::Struct(wide_skewed(n, 4), 2)),
        ("num2-skewed", Data::Numeric(wide_skewed(n, 5), 2)),
    ] {
        for (id, graph) in [
            (3, StandardGraph::Fse),
            (4, StandardGraph::Huffman),
            (7, StandardGraph::Entropy),
        ] {
            compare_graph(
                &format!("graph {id} {name}"),
                id,
                graph,
                config(6, 27),
                &data,
            );
        }
    }
}

#[cfg(feature = "entropy")]
#[test]
// The `fse` graph takes serial input, so wider elements reach it converted to
// bytes and it accepts them.
fn entropy_graphs_reject_element_widths_other_than_one_and_two() {
    for width in [3usize, 4, 8] {
        for (name, data) in [
            (
                "struct",
                Data::Struct(support::noise(width * 500, 1), width),
            ),
            (
                "numeric",
                Data::Numeric(
                    support::noise(width.next_power_of_two() * 500, 2),
                    width.next_power_of_two(),
                ),
            ),
        ] {
            for (id, graph) in [(4, StandardGraph::Huffman), (7, StandardGraph::Entropy)] {
                let outcome = compare_graph(
                    &format!("graph {id} {name} width {width}"),
                    id,
                    graph,
                    config(6, 27),
                    &data,
                );
                assert_eq!(outcome, Outcome::BothFailed, "{id} {name} {width}");
            }
        }
    }
}
