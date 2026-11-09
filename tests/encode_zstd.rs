//! Byte identity of the Zstandard node (`EI_zstd`) and of the default graph's
//! Zstandard step with the reference, and round trips of the frames.
//!
//! Node level: `Graph::node(62, [STORE])` built by C against the same node
//! built with `GraphBuilder`, for local parameter lists, global levels,
//! decompression level 1, data shapes and format versions. Inputs and
//! parameters C rejects must be rejected on both sides.
#![cfg(all(feature = "compression", feature = "decompression", feature = "zstd"))]

mod support;

use mopenzl::graph::{GraphBuilder, LocalParams, StandardGraph, StandardNode};
use mopenzl::input::{Input, Numbers, Records, Strings};
use mopenzl::{Compressor, Decompressor, EncodeError, EncoderConfig, FormatVersion, Level};
use openzl_ffi::graph::{self, Graph, Input as CInput, cparam};

/// `ZL_CParam_decompressionLevel`.
const DECOMPRESSION_LEVEL: i32 = 3;

fn config(level: i32, version: u32, decompression_level: u8) -> EncoderConfig {
    EncoderConfig::default()
        .with_level(Level::try_from(level).unwrap())
        .with_format_version(FormatVersion::try_from(version).unwrap())
        .with_decompression_level(decompression_level)
}

fn c_params(config: &EncoderConfig) -> Vec<(i32, i32)> {
    vec![
        (cparam::LEVEL, config.level().get()),
        (DECOMPRESSION_LEVEL, i32::from(config.decompression_level())),
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

/// The frame of the zstd node with `ints` feeding STORE, by the reference.
fn c_node(
    inputs: &[CInput<'_>],
    ints: &[(i32, i32)],
    config: &EncoderConfig,
) -> Result<Vec<u8>, openzl_ffi::Error> {
    graph::compress(
        inputs,
        &Graph::node_with(62, ints.iter().copied(), [Graph::STORE]),
        &c_params(config),
    )
}

/// The same frame by this crate.
fn rust_node(
    inputs: &[Input<'_>],
    ints: &[(i32, i32)],
    config: EncoderConfig,
) -> Result<Vec<u8>, EncodeError> {
    let mut builder = GraphBuilder::new();
    let start = if ints.is_empty() {
        builder
            .static_graph(StandardNode::Zstd, &[StandardGraph::Store.into()])
            .unwrap()
    } else {
        let params = ints
            .iter()
            .fold(LocalParams::default(), |params, &(id, value)| {
                params.with_int(id, value)
            });
        let node = builder
            .parameterize_node(StandardNode::Zstd, params)
            .unwrap();
        builder
            .static_graph(node, &[StandardGraph::Store.into()])
            .unwrap()
    };
    let mut compressor = Compressor::builder(config)
        .with_graph(builder.build(start).unwrap())
        .build()
        .unwrap();
    compressor.compress_inputs(inputs)
}

/// Both sides write the same frame, or both fail. Returns the frame.
fn check_serial(data: &[u8], ints: &[(i32, i32)], config: EncoderConfig) -> Option<Vec<u8>> {
    let label = format!(
        "{} bytes, params {ints:?}, level {}, decompression level {}, version {}",
        data.len(),
        config.level().get(),
        config.decompression_level(),
        u32::from(config.format_version()),
    );
    let expected = c_node(&[CInput::Serial(data)], ints, &config);
    let actual = rust_node(&[Input::from(data)], ints, config);
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => {
            assert_eq!(actual, expected, "{label}");
            // Both decoders read the frame back.
            assert_eq!(Decompressor::default().decompress(&actual).unwrap(), data);
            let outputs = graph::decompress(&actual).expect("reference decode");
            assert_eq!(outputs[0].bytes, data, "{label}");
            Some(actual)
        }
        (Err(_), Err(_)) => None,
        (expected, actual) => panic!("{label}: C {expected:?}, Rust {actual:?}"),
    }
}

/// Mostly repetitive bytes with a noisy tail, so literals and sequences both
/// matter.
fn structured(len: usize) -> Vec<u8> {
    let mut data = support::text(len * 3 / 4, 3);
    data.extend(support::noise(len - data.len(), 4));
    data
}

fn shapes() -> Vec<(&'static str, Vec<u8>)> {
    let big = if cfg!(miri) { 4096 } else { 300_000 };
    vec![
        ("empty", vec![]),
        ("tiny", b"0123456789".to_vec()),
        ("below-min-stream", b"abc".to_vec()),
        ("constant", vec![7u8; 5000]),
        ("text", support::text(20_000, 5)),
        ("noise", support::noise(5000, 6)),
        ("structured", structured(50_000)),
        ("large", structured(big)),
    ]
}

#[test]
fn the_node_matches_the_reference_at_every_level() {
    let levels: Vec<i32> = if cfg!(miri) {
        vec![-5, 3, 22]
    } else {
        (-5..=22).collect()
    };
    let data = structured(30_000);
    for level in levels {
        let frame = check_serial(&data, &[], config(level, 27, 3)).expect("compresses");
        assert!(frame.len() < data.len());
    }
}

#[test]
fn the_node_matches_the_reference_on_every_shape_and_format_version() {
    for (name, data) in shapes() {
        for version in [21, 24, 27] {
            for level in [1, 6, 19] {
                let _ = name;
                check_serial(&data, &[], config(level, version, 3));
            }
        }
    }
}

#[test]
fn a_decompression_level_of_one_stores_literals_raw() {
    for (_, data) in shapes() {
        for level in [-3, 1, 3, 9, 19] {
            check_serial(&data, &[], config(level, 27, 1));
        }
    }
    // Raw literals change the frame of text.
    let data = support::text(40_000, 8);
    assert_ne!(
        check_serial(&data, &[], config(3, 27, 1)),
        check_serial(&data, &[], config(3, 27, 3))
    );
    // With local parameters too: they apply after the literal mode, so
    // `literalCompressionMode` (1002) set locally wins.
    let data = structured(40_000);
    for ints in [
        &[(1002, 0)][..],
        &[(1002, 1)][..],
        &[(105, 4), (100, 7)][..],
    ] {
        check_serial(&data, ints, config(3, 27, 1));
    }
}

#[test]
fn the_default_graph_writes_raw_literals_at_decompression_level_one() {
    let data = structured(40_000);
    for level in [1, 3, 7, 19] {
        let config = config(level, 27, 1);
        let expected = graph::compress(
            &[CInput::Serial(&data)],
            &Graph::Id(StandardGraph::CompressGeneric as u32),
            &c_params(&config),
        )
        .unwrap();
        let frame = Compressor::new(config).unwrap().compress(&data).unwrap();
        assert_eq!(frame, expected, "level {level}");
        assert_eq!(Decompressor::default().decompress(&frame).unwrap(), data);
    }
}

#[test]
fn local_parameters_match_the_reference() {
    let data = structured(60_000);
    let lists: Vec<Vec<(i32, i32)>> = vec![
        // The match finder.
        vec![(100, 9)],
        vec![(100, -3)],
        vec![(100, 0)],
        vec![(100, 100)],
        vec![(101, 15)],
        vec![(101, 10)],
        vec![(101, 28)],
        vec![(102, 12)],
        vec![(102, 27)],
        vec![(103, 12)],
        vec![(103, 27)],
        vec![(104, 3)],
        vec![(105, 3)],
        vec![(105, 7)],
        vec![(106, 64)],
        vec![(107, 1)],
        vec![(107, 9)],
        vec![(107, 0)],
        vec![
            (101, 18),
            (102, 14),
            (103, 16),
            (104, 4),
            (105, 5),
            (106, 24),
            (107, 6),
        ],
        // Long-distance matching.
        vec![(160, 1)],
        vec![(160, 2)],
        vec![(160, 1), (161, 16), (162, 32), (163, 4), (164, 6)],
        vec![(161, 20)],
        vec![(164, 5)],
        // Flags and modes.
        vec![(201, 1)],
        vec![(202, 0)],
        vec![(202, 1)],
        vec![(1000, 1)],
        vec![(1002, 0)],
        vec![(1002, 1)],
        vec![(1002, 2)],
        vec![(1004, 1000)],
        vec![(1010, 1)],
        vec![(1011, 1)],
        vec![(1011, 2)],
        vec![(1015, 4096)],
        vec![(1017, 3)],
        vec![(130, 0)],
        vec![(400, 0)],
        vec![(401, 0)],
        // Later values replace earlier ones.
        vec![(100, 3), (100, 12)],
        vec![(105, 7), (105, 0)],
        vec![(100, 19), (101, 0), (102, 0)],
        // The binding's refusals.
        vec![(10, 0)],
        vec![(10, 1)],
        vec![(200, 0)],
        vec![(200, 1)],
        vec![(100, 5), (10, 1)],
        vec![(101, 29)],
        vec![(101, 31)],
        vec![(102, 28)],
        vec![(103, 28)],
        vec![(161, 28)],
        vec![(101, -1)],
        // zstd's own refusals.
        vec![(101, 9)],
        vec![(102, 5)],
        vec![(104, 99)],
        vec![(105, 8)],
        vec![(105, 2)],
        vec![(107, 10)],
        vec![(107, -1)],
        vec![(1017, 7)],
        vec![(12345, 1)],
        vec![(-5, 1)],
        vec![(0, 0)],
        vec![(400, 1)],
        vec![(1004, -1)],
    ];
    let mut accepted = 0;
    let mut refused = 0;
    for ints in lists {
        for (level, dl) in [(3, 3), (19, 1)] {
            match check_serial(&data, &ints, config(level, 27, dl)) {
                Some(_) => accepted += 1,
                None => refused += 1,
            }
        }
    }
    assert!(accepted > 60 && refused > 20, "{accepted} / {refused}");
}

#[test]
fn local_parameters_match_the_reference_on_small_and_empty_inputs() {
    for (_, data) in shapes().into_iter().take(5) {
        for ints in [
            vec![(100, 9), (101, 12)],
            vec![(1002, 1)],
            vec![(201, 1)],
            vec![(101, 29)],
        ] {
            check_serial(&data, &ints, config(5, 27, 3));
        }
    }
}

#[test]
fn typed_inputs_convert_to_serial_or_are_refused_like_the_reference() {
    // Struct and numeric streams reach the serial-only node through an
    // implicit conversion, which leaves the node a serial stream (width 1,
    // so no block splitting); strings have no conversion and fail.
    let bytes = structured(8192);
    let lens = vec![2048u32; 4];
    let cases: [(&str, Input<'_>, CInput<'_>); 4] = [
        (
            "struct-2048",
            Records::try_from((&bytes[..], 2048)).unwrap().into(),
            CInput::Struct {
                bytes: &bytes,
                width: 2048,
            },
        ),
        (
            "struct-8",
            Records::try_from((&bytes[..], 8)).unwrap().into(),
            CInput::Struct {
                bytes: &bytes,
                width: 8,
            },
        ),
        (
            "numeric-4",
            Numbers::try_from((&bytes[..], 4)).unwrap().into(),
            CInput::Numeric {
                bytes: &bytes,
                width: 4,
            },
        ),
        (
            "string",
            Strings::try_from((&bytes[..], &lens[..])).unwrap().into(),
            CInput::String {
                bytes: &bytes,
                lens: &lens,
            },
        ),
    ];
    for (name, input, c_input) in cases {
        for (level, dl) in [(3, 3), (3, 1), (12, 3)] {
            let cfg = config(level, 27, dl);
            let expected = c_node(std::slice::from_ref(&c_input), &[], &cfg);
            let actual = rust_node(std::slice::from_ref(&input), &[], cfg);
            match (expected, actual) {
                (Ok(expected), Ok(actual)) => {
                    assert_eq!(actual, expected, "{name}");
                    let outputs = graph::decompress(&actual).expect("reference decode");
                    assert_eq!(outputs[0].bytes, bytes, "{name}");
                }
                (Err(_), Err(_)) => assert_eq!(name, "string"),
                (expected, actual) => panic!("{name}: C {expected:?}, Rust {actual:?}"),
            }
        }
    }
}

#[test]
fn target_block_sizes_match_the_reference() {
    // `ZSTD_c_targetCBlockSize`: values below 1340 are raised to it, values
    // above 131072 are refused, 0 restores the default.
    let target = mopenzl::graph::params::zstd::TARGET_C_BLOCK_SIZE;
    let sizes: &[usize] = if cfg!(miri) {
        &[5000]
    } else {
        &[0, 900, 60_000, 300_000]
    };
    let targets = [0, 1, 1339, 1340, 2000, 16_384, 131_072, 131_073, -5];
    for &len in sizes {
        let data = structured(len);
        for &value in &targets {
            for (level, dlevel) in [(1, 3), (3, 3), (19, 1)] {
                check_serial(&data, &[(target, value)], config(level, 27, dlevel));
            }
        }
    }
}

#[test]
fn the_default_graph_matches_the_reference_at_every_level() {
    let data = structured(40_000);
    let levels: Vec<i32> = if cfg!(miri) {
        vec![1, 22]
    } else {
        (-5..=22).collect()
    };
    for level in levels {
        let cfg = config(level, 27, 3);
        let expected = graph::compress(
            &[CInput::Serial(&data)],
            &Graph::Id(StandardGraph::CompressGeneric as u32),
            &c_params(&cfg),
        )
        .unwrap();
        let frame = Compressor::new(cfg).unwrap().compress(&data).unwrap();
        assert_eq!(frame, expected, "level {level}");
    }
}
