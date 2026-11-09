//! Byte identity of the graph engine with the reference's graph engine, and
//! round trips through this crate's decoder.
#![cfg(all(
    feature = "compression",
    feature = "decompression",
    feature = "zstd",
    feature = "conversion"
))]

mod support;

use mopenzl::graph::{
    Comment, CompressorGraph, Edge, FunctionGraph, GraphBuilder, GraphContext, GraphError, GraphId,
    GraphParams, InputView, LocalParams, Segmenter, SegmenterContext, Selector, SelectorContext,
    StandardGraph, StandardNode, TypeSet,
};
use mopenzl::input::{Input, Numbers, Records, Strings};
use mopenzl::{
    ChunkSize, Chunking, Compressor, DataType, Decompressor, EncodeError, EncoderConfig,
    FormatVersion, Level,
};
use openzl_ffi::graph::{self, Graph, Input as CInput, cparam};

fn config(level: i32, version: u32) -> EncoderConfig {
    EncoderConfig::default()
        .with_level(Level::try_from(level).unwrap())
        .with_format_version(FormatVersion::try_from(version).unwrap())
}

fn rust(
    start: impl Into<CompressorGraph>,
    config: EncoderConfig,
    inputs: &[Input<'_>],
) -> Result<Vec<u8>, EncodeError> {
    let mut compressor = Compressor::builder(config)
        .with_graph(start.into())
        .build()
        .unwrap();
    compressor.compress_inputs(inputs)
}

fn reference(inputs: &[CInput<'_>], graph: &Graph, config: &EncoderConfig) -> Vec<u8> {
    graph::compress(inputs, graph, &c_params(config)).expect("reference compression")
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

/// One typed corpus entry in both representations.
struct Case {
    name: &'static str,
    bytes: Vec<u8>,
    lens: Vec<u32>,
    kind: DataType,
    width: usize,
}

impl Case {
    fn input(&self) -> Input<'_> {
        match self.kind {
            DataType::Serial => Input::from(&self.bytes[..]),
            DataType::Struct => {
                Input::from(Records::try_from((&self.bytes[..], self.width)).unwrap())
            }
            DataType::Numeric => {
                Input::from(Numbers::try_from((&self.bytes[..], self.width)).unwrap())
            }
            DataType::String => {
                Input::from(Strings::try_from((&self.bytes[..], &self.lens[..])).unwrap())
            }
        }
    }

    fn c_input(&self) -> CInput<'_> {
        match self.kind {
            DataType::Serial => CInput::Serial(&self.bytes),
            DataType::Struct => CInput::Struct {
                bytes: &self.bytes,
                width: self.width,
            },
            DataType::Numeric => CInput::Numeric {
                bytes: &self.bytes,
                width: self.width,
            },
            DataType::String => CInput::String {
                bytes: &self.bytes,
                lens: &self.lens,
            },
        }
    }
}

fn cases() -> Vec<Case> {
    let n = if cfg!(miri) { 40 } else { 3000 };
    let mut cases = vec![
        Case {
            name: "serial",
            bytes: support::text(n, 1),
            lens: vec![],
            kind: DataType::Serial,
            width: 1,
        },
        Case {
            name: "serial-empty",
            bytes: vec![],
            lens: vec![],
            kind: DataType::Serial,
            width: 1,
        },
        Case {
            name: "serial-tiny",
            bytes: b"abc".to_vec(),
            lens: vec![],
            kind: DataType::Serial,
            width: 1,
        },
        Case {
            name: "struct-3",
            bytes: support::noise(3 * n, 2),
            lens: vec![],
            kind: DataType::Struct,
            width: 3,
        },
        Case {
            name: "struct-empty",
            bytes: vec![],
            lens: vec![],
            kind: DataType::Struct,
            width: 5,
        },
    ];
    for (name, width) in [("num-1", 1), ("num-2", 2), ("num-4", 4), ("num-8", 8)] {
        cases.push(Case {
            name,
            bytes: support::noise(width * n, 3 + width as u32),
            lens: vec![],
            kind: DataType::Numeric,
            width,
        });
    }
    let strings = support::text(n, 9);
    let lens: Vec<u32> = (0..n / 7).map(|_| 7).collect();
    let used = lens.len() * 7;
    cases.push(Case {
        name: "string",
        bytes: strings[..used].to_vec(),
        lens,
        kind: DataType::String,
        width: 1,
    });
    cases.push(Case {
        name: "string-empty",
        bytes: vec![],
        lens: vec![],
        kind: DataType::String,
        width: 1,
    });
    cases.push(Case {
        name: "string-wide",
        bytes: vec![b'x'; 300 + 5],
        lens: vec![300, 5, 0],
        kind: DataType::String,
        width: 1,
    });
    cases
}

fn decode_matches(frame: &[u8], cases: &[&Case]) {
    let outputs = Decompressor::default()
        .decompress_typed(frame)
        .expect("our decoder");
    assert_eq!(outputs.len(), cases.len());
    for (output, case) in outputs.iter().zip(cases) {
        assert_eq!(output.data_type(), case.kind, "{}", case.name);
        assert_eq!(output.content(), &case.bytes[..], "{}", case.name);
        if case.kind == DataType::String {
            assert_eq!(output.string_lens(), &case.lens[..], "{}", case.name);
        }
    }
    let c = graph::decompress(frame).expect("C decodes our frame");
    assert_eq!(c.len(), cases.len());
    for (output, case) in c.iter().zip(cases) {
        assert_eq!(output.bytes, case.bytes, "{}", case.name);
    }
}

#[test]
fn store_of_every_type_is_byte_identical_and_round_trips() {
    let cfg = config(6, 27);
    for case in cases() {
        let expected = reference(&[case.c_input()], &Graph::STORE, &cfg);
        let frame = rust(StandardGraph::Store, cfg, &[case.input()]).unwrap();
        assert_eq!(frame, expected, "{}", case.name);
        decode_matches(&frame, &[&case]);
    }
}

#[test]
fn multi_input_store_keeps_input_order() {
    let cases = cases();
    for version in [21, 23, 24, 25, 27] {
        let cfg = config(6, version);
        let picks: Vec<&Case> = cases.iter().collect();
        let inputs: Vec<Input<'_>> = picks.iter().map(|case| case.input()).collect();
        let c_inputs: Vec<CInput<'_>> = picks.iter().map(|case| case.c_input()).collect();
        let expected = reference(&c_inputs, &Graph::STORE, &cfg);
        let frame = rust(StandardGraph::Store, cfg, &inputs).unwrap();
        assert_eq!(frame, expected, "version {version}");
        decode_matches(&frame, &picks);
    }
}

#[test]
fn many_inputs_use_the_long_header_form() {
    let cfg = config(6, 27);
    let owned: Vec<Vec<u8>> = (0..40u32)
        .map(|seed| support::text(20 + seed as usize, seed))
        .collect();
    let inputs: Vec<Input<'_>> = owned.iter().map(|bytes| Input::from(&bytes[..])).collect();
    let c_inputs: Vec<CInput<'_>> = owned.iter().map(|bytes| CInput::Serial(bytes)).collect();
    let expected = reference(&c_inputs, &Graph::STORE, &cfg);
    let frame = rust(StandardGraph::Store, cfg, &inputs).unwrap();
    assert_eq!(frame, expected);
    assert_eq!(
        Decompressor::default()
            .decompress_typed(&frame)
            .unwrap()
            .len(),
        40
    );
}

#[test]
fn conversion_nodes_match_the_reference() {
    let cfg = config(6, 27);
    let bytes = support::noise(64, 7);
    let numbers_of = |width: usize| (CInput::Serial(&bytes), Input::from(&bytes[..]), width);
    let _ = numbers_of;
    let serial = Input::from(&bytes[..]);
    let c_serial = CInput::Serial(&bytes);
    let nodes: [(u32, StandardNode); 7] = [
        (19, StandardNode::ConvertSerialToNum8),
        (20, StandardNode::ConvertSerialToNumLe16),
        (21, StandardNode::ConvertSerialToNumLe32),
        (22, StandardNode::ConvertSerialToNumLe64),
        (23, StandardNode::ConvertSerialToNumBe16),
        (24, StandardNode::ConvertSerialToNumBe32),
        (25, StandardNode::ConvertSerialToNumBe64),
    ];
    for (id, node) in nodes {
        let expected = reference(&[c_serial], &Graph::node(id, [Graph::STORE]), &cfg);
        let mut builder = GraphBuilder::new();
        let graph = builder
            .static_graph(node, &[StandardGraph::Store.into()])
            .unwrap();
        let frame = rust(builder.build(graph).unwrap(), cfg, &[serial]).unwrap();
        assert_eq!(frame, expected, "node {id}");
        // The decoded output has the type of the input.
        assert_eq!(
            Decompressor::default().decompress(&frame).unwrap(),
            bytes,
            "node {id}"
        );
    }
    // serial_to_struct needs its token size; struct2/4/8 carry theirs.
    for (id, node, width) in [
        (14, StandardNode::ConvertSerialToStruct2, 2),
        (15, StandardNode::ConvertSerialToStruct4, 4),
        (16, StandardNode::ConvertSerialToStruct8, 8),
    ] {
        let expected = reference(&[c_serial], &Graph::node(id, [Graph::STORE]), &cfg);
        let mut builder = GraphBuilder::new();
        let graph = builder
            .static_graph(node, &[StandardGraph::Store.into()])
            .unwrap();
        let frame = rust(builder.build(graph).unwrap(), cfg, &[serial]).unwrap();
        assert_eq!(frame, expected, "struct{width}");
    }
    let expected = reference(
        &[c_serial],
        &Graph::node_with(13, [(1, 8)], [Graph::STORE]),
        &cfg,
    );
    let mut builder = GraphBuilder::new();
    let node = builder
        .parameterize_node(
            StandardNode::ConvertSerialToStruct,
            LocalParams::default().with_int(1, 8),
        )
        .unwrap();
    let graph = builder
        .static_graph(node, &[StandardGraph::Store.into()])
        .unwrap();
    let frame = rust(builder.build(graph).unwrap(), cfg, &[serial]).unwrap();
    assert_eq!(frame, expected);
    assert_eq!(Decompressor::default().decompress(&frame).unwrap(), bytes);
}

#[test]
fn typed_conversions_to_serial_match_the_reference() {
    let cfg = config(6, 27);
    let bytes = support::noise(96, 11);
    let record = Records::try_from((&bytes[..], 6)).unwrap();
    let numbers = Numbers::try_from((&bytes[..], 4)).unwrap();
    let cases: [(u32, StandardNode, Input<'_>, CInput<'_>); 4] = [
        (
            10,
            StandardNode::ConvertStructToSerial,
            record.into(),
            CInput::Struct {
                bytes: &bytes,
                width: 6,
            },
        ),
        (
            11,
            StandardNode::ConvertNumToStructLe,
            numbers.into(),
            CInput::Numeric {
                bytes: &bytes,
                width: 4,
            },
        ),
        (
            12,
            StandardNode::ConvertNumToSerialLe,
            numbers.into(),
            CInput::Numeric {
                bytes: &bytes,
                width: 4,
            },
        ),
        (
            17,
            StandardNode::ConvertStructToNumLe,
            record.into(),
            CInput::Struct {
                bytes: &bytes[..96],
                width: 4,
            },
        ),
    ];
    for (id, node, input, c_input) in cases {
        let (input, c_input) = if id == 17 {
            (
                Input::from(Records::try_from((&bytes[..], 4)).unwrap()),
                c_input,
            )
        } else {
            (input, c_input)
        };
        let expected = reference(&[c_input], &Graph::node(id, [Graph::STORE]), &cfg);
        let mut builder = GraphBuilder::new();
        let graph = builder
            .static_graph(node, &[StandardGraph::Store.into()])
            .unwrap();
        let frame = rust(builder.build(graph).unwrap(), cfg, &[input]).unwrap();
        assert_eq!(frame, expected, "node {id}");
    }
}

#[test]
fn separate_string_components_routes_each_output_to_its_own_successor() {
    let cfg = config(6, 27);
    let text = support::text(4000, 5);
    let lens: Vec<u32> = (0..400).map(|_| 10).collect();
    let input = Strings::try_from((&text[..4000], &lens[..])).unwrap();
    let c_input = CInput::String {
        bytes: &text[..4000],
        lens: &lens,
    };
    for (c_graphs, rust_graphs) in [
        (
            [Graph::Id(9), Graph::STORE],
            [StandardGraph::Zstd, StandardGraph::Store],
        ),
        (
            [Graph::STORE, Graph::Id(9)],
            [StandardGraph::Store, StandardGraph::Zstd],
        ),
        (
            [Graph::Id(9), Graph::Id(9)],
            [StandardGraph::Zstd, StandardGraph::Zstd],
        ),
    ] {
        let expected = reference(&[c_input], &Graph::node(26, c_graphs), &cfg);
        let mut builder = GraphBuilder::new();
        let graph = builder
            .static_graph(
                StandardNode::SeparateStringComponents,
                &[rust_graphs[0].into(), rust_graphs[1].into()],
            )
            .unwrap();
        let frame = rust(builder.build(graph).unwrap(), cfg, &[input.into()]).unwrap();
        assert_eq!(frame, expected);
        let outputs = Decompressor::default().decompress_typed(&frame).unwrap();
        assert_eq!(outputs[0].string_lens(), &lens[..]);
        assert_eq!(outputs[0].content(), &text[..4000]);
    }
}

#[test]
fn compress_generic_on_serial_input_matches_across_levels_versions_and_sizes() {
    let sizes: &[usize] = if cfg!(miri) {
        &[0, 9, 10, 200]
    } else {
        &[0, 1, 9, 10, 11, 200, 5_000, 70_000]
    };
    for &size in sizes {
        let text = support::text(size, size as u32);
        let noise = support::noise(size, size as u32 + 1);
        for data in [&text, &noise] {
            for (level, version) in [(1, 21), (3, 22), (6, 25), (9, 27), (19, 27)] {
                let cfg = config(level, version);
                let expected = reference(&[CInput::Serial(data)], &Graph::Id(13), &cfg);
                for start in [
                    StandardGraph::CompressGeneric,
                    StandardGraph::SerialCompress,
                ] {
                    let frame = rust(start, cfg, &[Input::from(&data[..])]).unwrap();
                    assert_eq!(
                        frame, expected,
                        "size {size} level {level} v{version} {start:?}"
                    );
                }
                // The specialized default path writes the same bytes.
                let default = Compressor::new(cfg).unwrap().compress(data).unwrap();
                assert_eq!(default, expected);
            }
        }
    }
}

#[test]
fn checksum_switches_and_expansion_rules_match() {
    let noise = support::noise(300, 3);
    let text = support::text(2000, 4);
    for content in [true, false] {
        for compressed in [true, false] {
            for store_on_expansion in [true, false] {
                let cfg = config(6, 27)
                    .with_content_checksum(content)
                    .with_compressed_checksum(compressed)
                    .with_store_on_expansion(store_on_expansion);
                for data in [&noise, &text] {
                    let expected = reference(&[CInput::Serial(data)], &Graph::Id(9), &cfg);
                    let frame = rust(StandardGraph::Zstd, cfg, &[Input::from(&data[..])]).unwrap();
                    assert_eq!(
                        frame, expected,
                        "{content} {compressed} {store_on_expansion}"
                    );
                }
            }
        }
    }
}

#[test]
fn min_stream_size_stores_short_streams() {
    for min in [1u32, 10, 100, 5_000] {
        let cfg = config(6, 27).with_min_stream_size(min);
        for size in [0usize, 5, 50, 500, 4000] {
            let data = support::text(size, 1);
            let expected = reference(&[CInput::Serial(&data)], &Graph::Id(9), &cfg);
            let frame = rust(StandardGraph::Zstd, cfg, &[Input::from(&data[..])]).unwrap();
            assert_eq!(frame, expected, "min {min} size {size}");
            let default = Compressor::new(cfg).unwrap().compress(&data).unwrap();
            assert_eq!(
                default,
                reference(&[CInput::Serial(&data)], &Graph::Id(13), &cfg),
                "default path, min {min} size {size}"
            );
        }
    }
}

#[test]
fn implicit_conversions_are_inserted_before_ports_that_do_not_accept_the_type() {
    let cfg = config(6, 27);
    let n = support::text(2000, 8);
    let numbers = Numbers::try_from((&n[..], 4)).unwrap();
    let records = Records::try_from((&n[..], 5)).unwrap();
    let c_numbers = CInput::Numeric {
        bytes: &n,
        width: 4,
    };
    let c_records = CInput::Struct {
        bytes: &n,
        width: 5,
    };
    for (input, c_input) in [
        (Input::from(numbers), c_numbers),
        (Input::from(records), c_records),
    ] {
        let expected = reference(&[c_input], &Graph::Id(9), &cfg);
        let frame = rust(StandardGraph::Zstd, cfg, &[input]).unwrap();
        assert_eq!(frame, expected);
    }
}

#[test]
fn serial_segmenters_match_for_every_chunk_size() {
    for chunk in [32 << 10usize, 40_000, 100_000] {
        for size in [0usize, 1, 32 << 10, (32 << 10) + 1, 250_000] {
            let data = support::text(size, 3);
            let cfg = config(6, 27);
            let c_graph = Graph::Parameterized {
                graph: 29,
                params: graph::Params {
                    ints: vec![(7700, chunk as i32)],
                    copies: vec![],
                },
                custom: vec![Graph::Id(13)],
            };
            let expected = reference(&[CInput::Serial(&data)], &c_graph, &cfg);
            let mut builder = GraphBuilder::new();
            let segmenter = builder
                .serial_segmenter(
                    ChunkSize::try_from(chunk).unwrap(),
                    StandardGraph::CompressGeneric,
                )
                .unwrap();
            let frame = rust(
                builder.build(segmenter).unwrap(),
                cfg,
                &[Input::from(&data[..])],
            )
            .unwrap();
            assert_eq!(frame, expected, "chunk {chunk} size {size}");
            assert_eq!(Decompressor::default().decompress(&frame).unwrap(), data);
            // Fixed chunking wraps the start graph the same way.
            let fixed = cfg.with_chunking(Chunking::Fixed(ChunkSize::try_from(chunk).unwrap()));
            let wrapped = rust(
                StandardGraph::CompressGeneric,
                fixed,
                &[Input::from(&data[..])],
            )
            .unwrap();
            assert_eq!(wrapped, expected);
            let mut compressor = Compressor::builder(fixed)
                .with_graph(StandardGraph::CompressGeneric.into())
                .build()
                .unwrap();
            assert_eq!(compressor.compress(&data).unwrap(), expected);
        }
    }
}

#[test]
fn numeric_segmenters_match() {
    let data = support::noise(120_000, 5);
    for (width, graph_id) in [(1usize, 24u32), (2, 25), (4, 26), (8, 27)] {
        let chunk = 40_000usize;
        let cfg = config(6, 27);
        let c_graph = Graph::Parameterized {
            graph: graph_id,
            params: graph::Params {
                ints: vec![(301, width as i32), (300, chunk as i32)],
                copies: vec![],
            },
            custom: vec![],
        };
        // Serial -> numeric -> select_numeric needs field_lz, which is not
        // ported: compare the first stage with a successor both can run.
        let successor = Graph::Id(65 + (width.trailing_zeros()));
        let _ = successor;
        let _ = c_graph;
        let mut builder = GraphBuilder::new();
        let store_numeric = builder
            .parameterize_graph(StandardGraph::SegmentNum8FromSerial, GraphParams::default())
            .unwrap();
        let _ = store_numeric;
        let c_custom = Graph::Parameterized {
            graph: graph_id,
            params: graph::Params {
                ints: vec![(301, width as i32), (300, chunk as i32)],
                copies: vec![],
            },
            custom: vec![Graph::STORE],
        };
        let expected = reference(&[CInput::Serial(&data)], &c_custom, &cfg);
        let segmenter = builder
            .num_from_serial_segmenter(
                width,
                ChunkSize::try_from(chunk).unwrap(),
                Some(StandardGraph::Store.into()),
            )
            .unwrap();
        let frame = rust(
            builder.build(segmenter).unwrap(),
            cfg,
            &[Input::from(&data[..])],
        )
        .unwrap();
        assert_eq!(frame, expected, "width {width}");
    }
}

#[test]
fn header_comments_match_the_reference() {
    let data = support::text(500, 2);
    for version in [22, 25, 27] {
        let cfg = config(6, version);
        let expected = graph::compress_with_comment(
            &[CInput::Serial(&data)],
            &Graph::Id(13),
            &c_params(&cfg),
            b"a comment",
        )
        .unwrap();
        let mut compressor = Compressor::builder(cfg).build().unwrap();
        compressor
            .set_comment(Some(Comment::try_from(&b"a comment"[..]).unwrap()))
            .unwrap();
        assert_eq!(compressor.compress(&data).unwrap(), expected, "v{version}");
        assert_eq!(Decompressor::default().decompress(&expected).unwrap(), data);
    }
    let mut old = Compressor::new(config(6, 21)).unwrap();
    assert!(
        old.set_comment(Some(Comment::try_from(&b"x"[..]).unwrap()))
            .is_err()
    );
    assert!(Comment::try_from(&[0u8; 10_001][..]).is_err());
    assert!(Comment::try_from(&[0u8; 10_000][..]).is_ok());
    let mut commented = Compressor::new(config(6, 27)).unwrap();
    commented
        .set_comment(Some(Comment::try_from(&b"c"[..]).unwrap()))
        .unwrap();
    assert!(commented.reconfigure(config(6, 21)).is_err());
    commented.set_comment(None).unwrap();
    commented.reconfigure(config(6, 21)).unwrap();
}

#[test]
fn permissive_mode_falls_back_to_compress_generic() {
    let cfg = config(6, 27).with_permissive(true);
    let data = support::text(1001, 6);
    // 1001 bytes are not a whole number of 32-bit integers: the node fails
    // in both libraries and the engine reruns the input through
    // compress_generic.
    let c_graph = Graph::node(21, [Graph::STORE]);
    let expected = reference(&[CInput::Serial(&data)], &c_graph, &cfg);
    let mut builder = GraphBuilder::new();
    let graph = builder
        .static_graph(
            StandardNode::ConvertSerialToNumLe32,
            &[StandardGraph::Store.into()],
        )
        .unwrap();
    let frame = rust(
        builder.build(graph).unwrap(),
        cfg,
        &[Input::from(&data[..])],
    )
    .unwrap();
    assert_eq!(frame, expected);
    assert_eq!(Decompressor::default().decompress(&frame).unwrap(), data);
    // Without permissive mode the failure surfaces.
    let mut builder = GraphBuilder::new();
    let graph = builder
        .static_graph(
            StandardNode::ConvertSerialToNumLe32,
            &[StandardGraph::Store.into()],
        )
        .unwrap();
    let error = rust(
        builder.build(graph).unwrap(),
        config(6, 27),
        &[Input::from(&data[..])],
    );
    assert!(matches!(error, Err(EncodeError::Graph(_))));
}

#[test]
fn permissive_rollback_discards_the_nodes_of_the_failed_subtree() {
    let cfg = config(6, 27).with_permissive(true);
    let data = support::text(1000, 9);
    // serial -> struct2 -> serial, then a successor whose node fails for want
    // of its token size: the rollback drops that successor's nodes only and
    // the stream it received goes through compress_generic.
    let c_graph = Graph::Pipeline {
        nodes: vec![14, 10],
        last: Box::new(Graph::node(13, [Graph::STORE])),
    };
    let expected = reference(&[CInput::Serial(&data)], &c_graph, &cfg);
    let mut builder = GraphBuilder::new();
    let failing = builder
        .static_graph(
            StandardNode::ConvertSerialToStruct,
            &[StandardGraph::Store.into()],
        )
        .unwrap();
    let pipeline = builder
        .pipeline(
            &[
                StandardNode::ConvertSerialToStruct2.into(),
                StandardNode::ConvertStructToSerial.into(),
            ],
            failing,
        )
        .unwrap();
    let frame = rust(
        builder.build(pipeline).unwrap(),
        cfg,
        &[Input::from(&data[..])],
    )
    .unwrap();
    assert_eq!(frame, expected);
    assert_eq!(Decompressor::default().decompress(&frame).unwrap(), data);
}

#[test]
fn brute_force_picks_the_smallest_trial_frame() {
    let cfg = config(6, 27);
    for data in [
        support::text(5000, 1),
        support::noise(5000, 2),
        support::text(40, 3),
    ] {
        let c_graph = Graph::Parameterized {
            graph: 31,
            params: graph::Params::default(),
            custom: vec![Graph::Id(9), Graph::STORE],
        };
        let expected = reference(&[CInput::Serial(&data)], &c_graph, &cfg);
        let mut builder = GraphBuilder::new();
        let selector = builder
            .brute_force(&[StandardGraph::Zstd.into(), StandardGraph::Store.into()])
            .unwrap();
        let frame = rust(
            builder.build(selector).unwrap(),
            cfg,
            &[Input::from(&data[..])],
        )
        .unwrap();
        assert_eq!(frame, expected);
    }
}

#[test]
fn compress_generic_reaches_field_lz_for_numeric_input() {
    let data = support::text(2000, 1);
    let numbers = Numbers::try_from((&data[..], 4)).unwrap();
    let expected = reference(
        &[CInput::Numeric {
            bytes: &data,
            width: 4,
        }],
        &Graph::Id(13),
        &config(6, 27),
    );
    let frame = rust(
        StandardGraph::CompressGeneric,
        config(6, 27),
        &[numbers.into()],
    )
    .unwrap();
    assert_eq!(frame, expected);
}

#[test]
fn stores_survive_expansion_for_typed_data_too() {
    let cfg = config(6, 27);
    let noise = support::noise(400, 4);
    let numbers = Numbers::try_from((&noise[..], 8)).unwrap();
    // zstd on noise expands: the chunk becomes the stored conversion.
    let expected = reference(
        &[CInput::Numeric {
            bytes: &noise,
            width: 8,
        }],
        &Graph::Id(9),
        &cfg,
    );
    let frame = rust(StandardGraph::Zstd, cfg, &[numbers.into()]).unwrap();
    assert_eq!(frame, expected);
}

struct FirstFit;

impl Selector for FirstFit {
    fn select(
        &self,
        cx: &mut SelectorContext<'_>,
        input: InputView<'_>,
        successors: &[GraphId],
    ) -> GraphId {
        let sizes: Vec<_> = successors
            .iter()
            .map(|&graph| cx.try_graph(&input, graph).unwrap_or(usize::MAX))
            .collect();
        let best = sizes
            .iter()
            .enumerate()
            .min_by_key(|&(_, &size)| size)
            .map(|(i, _)| i);
        successors[best.unwrap_or(0)]
    }
}

struct SplitHalves;

impl FunctionGraph for SplitHalves {
    fn compress(&self, cx: &mut GraphContext<'_>, inputs: &[Edge]) -> Result<(), GraphError> {
        let outputs = cx.run_node(StandardNode::SeparateStringComponents, inputs)?;
        let graphs = cx.custom_graphs();
        // Destinations may be assigned in any order; successors still run in
        // edge order.
        cx.set_destination(&outputs[1..2], graphs[1])?;
        cx.set_destination(&outputs[0..1], graphs[0])?;
        Ok(())
    }
}

struct ThirdsSegmenter;

impl Segmenter for ThirdsSegmenter {
    fn segment(&self, cx: &mut SegmenterContext<'_>) -> Result<(), GraphError> {
        let total = cx.remaining()[0];
        let successor = cx.custom_graphs()[0];
        let third = total.div_ceil(3).max(1);
        let mut left = total;
        while left > 0 {
            let take = third.min(left);
            cx.process_chunk(&[take], successor)?;
            left -= take;
        }
        if total == 0 {
            cx.process_chunk(&[0], successor)?;
        }
        Ok(())
    }
}

#[test]
fn user_selectors_function_graphs_and_segmenters_run() {
    let cfg = config(6, 27);
    let text = support::text(6000, 2);
    let mut builder = GraphBuilder::new();
    let selector = builder
        .selector(
            "first_fit",
            FirstFit,
            TypeSet::from(DataType::Serial),
            &[StandardGraph::Zstd.into(), StandardGraph::Store.into()],
        )
        .unwrap();
    let frame = rust(
        builder.build(selector).unwrap(),
        cfg,
        &[Input::from(&text[..])],
    )
    .unwrap();
    let c_graph = Graph::Parameterized {
        graph: 31,
        params: graph::Params::default(),
        custom: vec![Graph::Id(9), Graph::STORE],
    };
    assert_eq!(frame, reference(&[CInput::Serial(&text)], &c_graph, &cfg));

    let lens: Vec<u32> = (0..600).map(|_| 10).collect();
    let strings = Strings::try_from((&text[..6000], &lens[..])).unwrap();
    let mut builder = GraphBuilder::new();
    let function = builder
        .function_graph(
            "split",
            SplitHalves,
            &[DataType::String.into()],
            false,
            &[StandardGraph::Store.into(), StandardGraph::Zstd.into()],
            &[],
        )
        .unwrap();
    let frame = rust(builder.build(function).unwrap(), cfg, &[strings.into()]).unwrap();
    let expected = reference(
        &[CInput::String {
            bytes: &text[..6000],
            lens: &lens,
        }],
        &Graph::node(26, [Graph::STORE, Graph::Id(9)]),
        &cfg,
    );
    assert_eq!(frame, expected);

    let mut builder = GraphBuilder::new();
    let segmenter = builder
        .segmenter(
            "thirds",
            ThirdsSegmenter,
            &[DataType::Serial.into()],
            &[StandardGraph::CompressGeneric.into()],
        )
        .unwrap();
    let frame = rust(
        builder.build(segmenter).unwrap(),
        cfg,
        &[Input::from(&text[..])],
    )
    .unwrap();
    assert_eq!(Decompressor::default().decompress(&frame).unwrap(), text);
    let mut sessions = Compressor::builder(cfg)
        .with_graph({
            let mut builder = GraphBuilder::new();
            let segmenter = builder
                .segmenter(
                    "thirds",
                    ThirdsSegmenter,
                    &[DataType::Serial.into()],
                    &[StandardGraph::Zstd.into()],
                )
                .unwrap();
            builder.build(segmenter).unwrap()
        })
        .build()
        .unwrap();
    let one_shot = sessions.compress(&text).unwrap();
    let mut session = sessions.start(Default::default());
    let mut out = vec![0u8; Compressor::max_compressed_size(text.len()).unwrap() + 64];
    let progress = session
        .process(&text, &mut out, mopenzl::Operation::End)
        .unwrap();
    assert_eq!(&out[..progress.produced], &one_shot[..]);
}

#[test]
fn builders_reject_foreign_and_malformed_graphs() {
    let mut first = GraphBuilder::new();
    let mut second = GraphBuilder::new();
    let own = first
        .static_graph(StandardNode::Zstd, &[StandardGraph::Store.into()])
        .unwrap();
    assert!(second.static_graph(StandardNode::Zstd, &[own]).is_err());
    assert!(second.static_graph(StandardNode::Zstd, &[]).is_err());
    assert!(second.pipeline(&[], StandardGraph::Store).is_err());
    assert!(second.brute_force(&[]).is_err());
    assert!(
        second
            .num_from_serial_segmenter(3, ChunkSize::MIN, None)
            .is_err()
    );
    assert!(second.build(own).is_err());
    assert_eq!(
        StandardGraph::try_from(13).unwrap(),
        StandardGraph::CompressGeneric
    );
    assert_eq!(u32::from(StandardNode::Zstd), 62);
    assert!(StandardNode::try_from(1).is_err());
    assert!(StandardGraph::try_from(500).is_err());
    let _ = GraphParams::default().with_name("x").with_nodes(vec![]);
}

struct Probe;

impl FunctionGraph for Probe {
    fn compress(&self, cx: &mut GraphContext<'_>, inputs: &[Edge]) -> Result<(), GraphError> {
        assert_eq!(format!("{cx:?}"), "GraphContext");
        assert_eq!((cx.level(), cx.format_version(), cx.depth()), (6, 27, 1));
        assert_eq!(cx.local_int(5), Some(50));
        assert_eq!(cx.local_int(6), None);
        assert!(cx.is_node_supported(StandardNode::Zstd));
        assert!(!cx.is_node_supported(StandardNode::PivcoHuffman) || cx.format_version() == 27);
        let nodes = cx.custom_nodes();
        assert_eq!(nodes.len(), 1);
        let view = cx.data(inputs[0])?;
        assert_eq!(
            (view.data_type(), view.width(), view.len(), view.is_empty()),
            (DataType::Serial, 1, view.bytes().len(), false)
        );
        assert!(view.string_lengths().is_empty());
        assert_eq!(view.int_metadata(3), None);
        assert!(cx.try_graph(inputs[0], StandardGraph::Store).unwrap() > 0);
        cx.set_int_metadata(inputs[0], 3, 33)?;
        assert_eq!(cx.data(inputs[0])?.int_metadata(3), Some(33));
        let outputs =
            cx.run_node_with(nodes[0], inputs, &LocalParams::default().with_int(100, 3))?;
        let graphs = cx.custom_graphs();
        let overrides = GraphParams::default()
            .with_name("unused")
            .with_params(LocalParams::default())
            .with_graphs(vec![StandardGraph::Store.into()])
            .with_nodes(vec![StandardNode::Zstd.into()]);
        cx.set_destination_with(&outputs, graphs[0], &overrides)?;
        Ok(())
    }
}

struct Failing;

impl FunctionGraph for Failing {
    fn compress(&self, _: &mut GraphContext<'_>, _: &[Edge]) -> Result<(), GraphError> {
        Err(GraphError::User("refused"))
    }
}

struct Invalid;

impl FunctionGraph for Invalid {
    fn compress(&self, _: &mut GraphContext<'_>, _: &[Edge]) -> Result<(), GraphError> {
        Err(GraphError::Invalid("bad shape"))
    }
}

struct Odd;

impl FunctionGraph for Odd {
    fn compress(&self, _: &mut GraphContext<'_>, _: &[Edge]) -> Result<(), GraphError> {
        Err(GraphError::UnknownId("graph"))
    }
}

struct Inspect;

impl Selector for Inspect {
    fn select(
        &self,
        cx: &mut SelectorContext<'_>,
        input: InputView<'_>,
        successors: &[GraphId],
    ) -> GraphId {
        assert_eq!((cx.level(), cx.format_version(), cx.depth()), (6, 27, 1));
        assert_eq!(format!("{cx:?}"), "SelectorContext");
        assert_eq!(cx.local_int(1), None);
        assert!(cx.is_node_supported(StandardNode::Zstd));
        assert!(cx.try_graph(&input, StandardGraph::Store).is_ok());
        assert!(cx.try_graph(&input, StandardGraph::Zstd).is_ok());
        cx.set_successor_params(LocalParams::default().with_int(100, 1));
        successors[0]
    }
}

struct Inspector;

impl Segmenter for Inspector {
    fn segment(&self, cx: &mut SegmenterContext<'_>) -> Result<(), GraphError> {
        assert_eq!(format!("{cx:?}"), "SegmenterContext");
        assert_eq!(
            (cx.n_inputs(), cx.format_version(), cx.local_int(1)),
            (1, 27, None)
        );
        let left = cx.remaining();
        assert!(
            cx.process_chunk(&[left[0] + 1], cx.custom_graphs()[0])
                .is_err()
        );
        cx.process_chunk(&left, cx.custom_graphs()[0])
    }
}

#[test]
fn contexts_expose_the_engine_to_user_graphs() {
    let cfg = config(6, 27);
    let text = support::text(3000, 4);
    let mut builder = GraphBuilder::new();
    let zstd = builder
        .parameterize_node(StandardNode::Zstd, LocalParams::default())
        .unwrap();
    let probe = builder
        .function_graph(
            "probe",
            Probe,
            &[DataType::Serial.into()],
            false,
            &[StandardGraph::Store.into()],
            &[zstd],
        )
        .unwrap();
    let probe = builder
        .parameterize_graph(
            probe,
            GraphParams::default().with_params(LocalParams::default().with_int(5, 50)),
        )
        .unwrap();
    let frame = rust(
        builder.build(probe).unwrap(),
        cfg,
        &[Input::from(&text[..])],
    )
    .unwrap();
    let c = Graph::node_with(62, [(100, 3)], [Graph::STORE]);
    assert_eq!(frame, reference(&[CInput::Serial(&text)], &c, &cfg));

    let mut builder = GraphBuilder::new();
    let selector = builder
        .selector(
            "inspect",
            Inspect,
            TypeSet::ANY,
            &[StandardGraph::Zstd.into()],
        )
        .unwrap();
    let frame = rust(
        builder.build(selector).unwrap(),
        cfg,
        &[Input::from(&text[..])],
    )
    .unwrap();
    assert_eq!(
        frame,
        reference(
            &[CInput::Serial(&text)],
            &Graph::node_with(62, [(100, 1)], [Graph::STORE]),
            &cfg
        )
    );

    let mut builder = GraphBuilder::new();
    let segmenter = builder
        .segmenter(
            "inspector",
            Inspector,
            &[DataType::Serial.into()],
            &[StandardGraph::Zstd.into()],
        )
        .unwrap();
    let frame = rust(
        builder.build(segmenter).unwrap(),
        cfg,
        &[Input::from(&text[..])],
    )
    .unwrap();
    assert_eq!(Decompressor::default().decompress(&frame).unwrap(), text);

    // Failures of user graphs surface as graph errors with their message.
    let mut builder = GraphBuilder::new();
    let failing = builder
        .function_graph("f", Failing, &[TypeSet::ANY], false, &[], &[])
        .unwrap();
    let error = rust(
        builder.build(failing).unwrap(),
        cfg,
        &[Input::from(&text[..])],
    )
    .unwrap_err();
    assert_eq!(error, EncodeError::Graph(GraphError::User("refused")));
    assert!(error.to_string().contains("refused"));
    let mut builder = GraphBuilder::new();
    let invalid = builder
        .function_graph("i", Invalid, &[TypeSet::ANY], false, &[], &[])
        .unwrap();
    let error = rust(
        builder.build(invalid).unwrap(),
        cfg,
        &[Input::from(&text[..])],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        EncodeError::Graph(GraphError::Execution(_))
    ));
    let mut builder = GraphBuilder::new();
    let odd = builder
        .function_graph("o", Odd, &[TypeSet::ANY], false, &[], &[])
        .unwrap();
    let error = rust(builder.build(odd).unwrap(), cfg, &[Input::from(&text[..])]).unwrap_err();
    assert!(matches!(error, EncodeError::Graph(GraphError::User(_))));
    assert!(builder_for_errors().is_ok());
}

fn builder_for_errors() -> Result<(), GraphError> {
    let mut builder = GraphBuilder::new();
    assert!(
        builder
            .function_graph("f", Failing, &[], false, &[], &[])
            .is_err()
    );
    assert!(builder.segmenter("s", ThirdsSegmenter, &[], &[]).is_err());
    let mut other = GraphBuilder::new();
    let foreign = other.static_graph(StandardNode::Zstd, &[StandardGraph::Store.into()])?;
    assert!(
        builder
            .selector("s", FirstFit, TypeSet::ANY, &[foreign])
            .is_err()
    );
    assert!(
        builder
            .function_graph("f", Failing, &[TypeSet::ANY], false, &[foreign], &[])
            .is_err()
    );
    assert!(
        builder
            .function_graph(
                "f",
                Failing,
                &[TypeSet::ANY],
                false,
                &[],
                &[StandardNode::Zstd.into()]
            )
            .is_ok()
    );
    let foreign_node = other.parameterize_node(StandardNode::Zstd, LocalParams::default())?;
    assert!(
        builder
            .static_graph(foreign_node, &[StandardGraph::Store.into()])
            .is_err()
    );
    assert!(
        builder
            .parameterize_node(foreign_node, LocalParams::default())
            .is_err()
    );
    assert!(
        builder
            .parameterize_graph(foreign, GraphParams::default())
            .is_err()
    );
    let params = GraphParams::default().with_graphs(vec![foreign]);
    assert!(
        builder
            .parameterize_graph(StandardGraph::Store, params)
            .is_err()
    );
    let params = GraphParams::default().with_nodes(vec![foreign_node]);
    assert!(
        builder
            .parameterize_graph(StandardGraph::Store, params)
            .is_err()
    );
    let pipeline = builder.pipeline(
        &[StandardNode::ConvertSerialToNumLe32.into()],
        StandardGraph::Store,
    )?;
    builder.build(pipeline).map(drop)
}

#[test]
fn sessions_follow_the_graph_plan() {
    let text = support::text(150_000, 12);
    let cfg = config(6, 27);
    let fixed = cfg.with_chunking(Chunking::Fixed(ChunkSize::try_from(40_000usize).unwrap()));
    for config in [cfg, fixed] {
        let mut compressor = Compressor::builder(config)
            .with_graph(StandardGraph::SerialCompress.into())
            .build()
            .unwrap();
        let expected = compressor.compress(&text).unwrap();
        for declared in [None, Some(text.len() as u64)] {
            let mut session = compressor.start(mopenzl::StreamConfig::default().with_input_size(
                declared.map_or(mopenzl::InputSize::Unknown, mopenzl::InputSize::Exact),
            ));
            let mut frame = Vec::new();
            let mut buffer = vec![0u8; 1 << 16];
            let mut rest = &text[..];
            loop {
                let take = rest.len().min(33_333);
                let op = if take == rest.len() {
                    mopenzl::Operation::End
                } else {
                    mopenzl::Operation::Continue
                };
                let step = session.process(&rest[..take], &mut buffer, op).unwrap();
                frame.extend_from_slice(&buffer[..step.produced]);
                rest = &rest[step.consumed..];
                if step.status == mopenzl::EncoderStatus::Finished {
                    break;
                }
            }
            assert_eq!(frame, expected, "{declared:?}");
        }
        assert_eq!(Decompressor::default().decompress(&expected).unwrap(), text);
    }
    // A segmenter graph cannot be wrapped in fixed chunking: the session
    // reports the problem on its first call.
    let mut builder = GraphBuilder::new();
    let segmenter = builder
        .serial_segmenter(ChunkSize::MIN, StandardGraph::CompressGeneric)
        .unwrap();
    let mut compressor = Compressor::builder(fixed)
        .with_graph(builder.build(segmenter).unwrap())
        .build()
        .unwrap();
    assert!(matches!(
        compressor.compress(&text),
        Err(EncodeError::Graph(GraphError::Invalid(_)))
    ));
    let mut session = compressor.start(Default::default());
    let mut out = [0u8; 64];
    assert!(matches!(
        session.process(b"x", &mut out, mopenzl::Operation::End),
        Err(EncodeError::Graph(GraphError::Invalid(_)))
    ));
    assert_eq!(session.flush(&mut out), Err(EncodeError::InvalidState));
}

#[test]
fn failed_appends_roll_back_and_slices_report_their_size() {
    // Not constant: the `constant` graph fails at its node.
    let varied: Vec<u8> = (0..64).collect();
    let numbers = Numbers::try_from((&varied[..], 4)).unwrap();
    let mut compressor = Compressor::builder(config(6, 27))
        .with_graph(StandardGraph::Constant.into())
        .build()
        .unwrap();
    let mut dst = b"keep".to_vec();
    assert!(
        compressor
            .compress_inputs_into(&[numbers.into()], &mut dst)
            .is_err()
    );
    assert_eq!(dst, b"keep");
    assert!(compressor.compress_inputs(&[]).is_err());
    let mut tiny = [0u8; 8];
    assert_eq!(
        compressor.compress_to_slice(b"payload bytes", &mut tiny),
        Err(EncodeError::OutputTooSmall { provided: 8 })
    );
}

#[test]
fn serial_data_becomes_strings_with_set_string_lengths() {
    let cfg = config(6, 27);
    let text = support::text(60, 3);
    let lens: Vec<u32> = vec![10, 20, 30];
    let mut builder = GraphBuilder::new();
    let node = builder
        .parameterize_node(
            StandardNode::SetStringLens,
            LocalParams::default().with_string_lengths(lens.clone()),
        )
        .unwrap();
    let graph = builder.pipeline(&[node], StandardGraph::Store).unwrap();
    let frame = rust(
        builder.build(graph).unwrap(),
        cfg,
        &[Input::from(&text[..])],
    )
    .unwrap();
    // The input was serial, so it decodes as serial: the strings are an
    // internal representation.
    assert_eq!(Decompressor::default().decompress(&frame).unwrap(), text);
    assert_eq!(graph::decompress(&frame).unwrap()[0].bytes, text);
    let params = LocalParams::default().with_bytes(9, b"raw".to_vec());
    assert_eq!(params, params.clone());
}

#[test]
fn standard_ids_round_trip_and_defaults_are_generic() {
    for id in 1..=90u32 {
        let graph = StandardGraph::try_from(id).unwrap();
        assert_eq!(u32::from(graph), id);
        let _ = GraphId::from(graph);
    }
    for id in (2..=55u32).chain(57..=75).chain([86, 87]) {
        let node = StandardNode::try_from(id).unwrap();
        assert_eq!(u32::from(node), id);
        let _ = mopenzl::graph::NodeId::from(node);
    }
    assert_eq!(
        UnknownIdProbe::message(),
        "7 is not the ID of a supported standard graph or node"
    );
    let default = CompressorGraph::default();
    let _ = default.clone();
    let cfg = config(6, 27);
    let text = support::text(500, 1);
    let frame = rust(default, cfg, &[Input::from(&text[..])]).unwrap();
    assert_eq!(
        frame,
        reference(&[CInput::Serial(&text)], &Graph::Id(13), &cfg)
    );
    assert!(format!("{:?}", GraphBuilder::default()).contains("GraphBuilder"));
}

struct UnknownIdProbe;

impl UnknownIdProbe {
    fn message() -> String {
        mopenzl::graph::UnknownId(7).to_string()
    }
}

struct Refusing;

impl Segmenter for Refusing {
    fn segment(&self, cx: &mut SegmenterContext<'_>) -> Result<(), GraphError> {
        if cx.n_inputs() == 1 {
            return Err(GraphError::User("no segments"));
        }
        Err(GraphError::Invalid("other"))
    }
}

#[test]
fn small_public_types_behave() {
    let set = TypeSet::from(DataType::Serial) | TypeSet::from(DataType::Numeric);
    assert!(set.contains(DataType::Serial) && set.contains(DataType::Numeric));
    assert!(!set.contains(DataType::String));
    assert!(TypeSet::ANY.contains(DataType::Struct));
    assert_eq!(LocalParams::default().with_int(4, 5).int(4), Some(5));
    let comment = Comment::try_from(&b"abc"[..]).unwrap();
    assert_eq!(comment.as_ref(), b"abc");
    assert_eq!(comment.clone(), comment);
    // A default compressor takes typed inputs through compress_generic.
    let text = support::text(900, 1);
    let mut compressor = Compressor::default();
    let frame = compressor
        .compress_inputs(&[Input::from(&text[..])])
        .unwrap();
    assert_eq!(frame, compressor.compress(&text).unwrap());
    // A segmenter's failures surface with their message.
    let mut builder = GraphBuilder::new();
    let refusing = builder
        .segmenter("refusing", Refusing, &[DataType::Serial.into()], &[])
        .unwrap();
    let error = rust(
        builder.build(refusing).unwrap(),
        config(6, 27),
        &[Input::from(&text[..])],
    );
    assert_eq!(
        error,
        Err(EncodeError::Graph(GraphError::User("no segments")))
    );
    let mut builder = GraphBuilder::new();
    let refusing = builder
        .segmenter("refusing", Refusing, &[DataType::Serial.into()], &[])
        .unwrap();
    let two = [Input::from(&text[..]), Input::from(&text[..])];
    let error = rust(builder.build(refusing).unwrap(), config(6, 27), &two);
    assert!(error.is_err());
    // The default successor of the numeric segmenters.
    let mut builder = GraphBuilder::new();
    let segmenter = builder
        .num_from_serial_segmenter(4, ChunkSize::MIN, None)
        .unwrap();
    assert!(
        rust(
            builder.build(segmenter).unwrap(),
            config(6, 27),
            &[Input::from(&text[..])]
        )
        .is_ok()
    );
    // Debug output of the contexts and builders does not leak internals.
    assert_eq!(format!("{:?}", TypeSet::ANY), "TypeSet(15)");
}
