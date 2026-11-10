//! Explicit graph/codec selection across parallel scheduling and frame assembly.
#![cfg(all(
    feature = "compression",
    feature = "decompression",
    not(feature = "no_std")
))]
use mopenzl::compressor::parallel::{
    ArcBytesSource, BatchConfig, ParallelCompressor, ParallelConfig, ParallelEncodeError,
    ParallelEncoderStatus, SegmentSize, TaskCount,
};
use mopenzl::graph::{CompressorGraph, GraphBuilder, StandardGraph};
use mopenzl::{
    Backend, ChunkSize, Chunking, Compressor, Decompressor, EncoderConfig, FormatVersion, Level,
    Operation,
};
use std::{
    error::Error,
    io::{Read, Write},
    sync::Arc,
};

const SEGMENT: usize = 64 << 10;
fn parallel_config(minimum: u64) -> ParallelConfig {
    ParallelConfig::from(SegmentSize::try_from(SEGMENT).unwrap())
        .with_minimum_parallel_size(minimum)
        .with_max_retained_workers(4)
}
fn run(c: &mut ParallelCompressor, input: &[u8], tasks: usize) -> Vec<u8> {
    let mut b = c
        .prepare_slice(
            input,
            BatchConfig::auto(TaskCount::try_from(tasks).unwrap()),
        )
        .unwrap();
    std::thread::scope(|scope| {
        for task in b.take_tasks().unwrap().into_iter().rev() {
            scope.spawn(move || task.run());
        }
    });
    b.finish_to_writer(Vec::new()).unwrap().0
}
fn writer(c: &mut ParallelCompressor, input: &[u8]) -> Vec<u8> {
    std::thread::scope(|scope| {
        let (mut writer, queue) = c
            .writer(Vec::new(), TaskCount::try_from(2).unwrap())
            .unwrap();
        scope.spawn(move || queue.into_iter().for_each(|t| t.run()));
        for piece in input.chunks(4097) {
            writer.write_all(piece).unwrap();
        }
        writer.finish().unwrap()
    })
}
fn reader(c: &mut ParallelCompressor, input: &[u8]) -> Vec<u8> {
    std::thread::scope(|scope| {
        let (mut reader, queue) = c.reader(input, TaskCount::try_from(2).unwrap()).unwrap();
        scope.spawn(move || queue.into_iter().for_each(|t| t.run()));
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        bytes
    })
}
fn check(graph: CompressorGraph, input: &[u8], encoder: EncoderConfig) {
    let builder = ParallelCompressor::builder(encoder, parallel_config(0)).with_graph(graph);
    assert!(format!("{builder:?}").contains("ParallelCompressorBuilder"));
    for backend in Backend::available() {
        let mut c = builder.clone().with_backend(backend).build().unwrap();
        let expected = run(&mut c, input, 1);
        assert_eq!(openzl_ffi::decompress(&expected).unwrap(), input);
        assert_eq!(
            Decompressor::default().decompress(&expected).unwrap(),
            input
        );
        assert_eq!(run(&mut c, input, 4), expected);
        assert_eq!(writer(&mut c, input), expected);
        assert_eq!(reader(&mut c, input), expected);
        let mut b = c
            .prepare_source(
                ArcBytesSource::from(Arc::<[u8]>::from(input)),
                BatchConfig::auto(TaskCount::try_from(3).unwrap()),
            )
            .unwrap();
        b.run_inline().unwrap();
        assert_eq!(b.finish_to_writer(Vec::new()).unwrap().0, expected);
    }
}

#[test]
fn selected_standard_graphs_survive_every_scheduling_and_io_shape() {
    let input = b"explicit codec graphs: independent OpenZL chunks 0123456789\n".repeat(2700);
    let graphs = [
        StandardGraph::Store,
        StandardGraph::Zstd,
        StandardGraph::Lz4,
        StandardGraph::Fse,
        StandardGraph::Huffman,
        StandardGraph::Entropy,
    ];
    for graph in graphs {
        let enabled = match graph {
            StandardGraph::Zstd => cfg!(feature = "zstd"),
            StandardGraph::Lz4 => cfg!(feature = "lz4"),
            StandardGraph::Fse | StandardGraph::Huffman | StandardGraph::Entropy => {
                cfg!(feature = "entropy")
            }
            _ => true,
        };
        if !enabled {
            continue;
        }
        for version in [24, 27] {
            let config = EncoderConfig::default()
                .with_format_version(FormatVersion::try_from(version).unwrap())
                .with_min_stream_size(1)
                .with_store_on_expansion(false);
            check(graph.into(), &input, config);
        }
    }
}

#[test]
fn selected_graph_applies_to_empty_and_serial_fallback_and_survives_reconfigure() {
    let graph = CompressorGraph::standard(StandardGraph::Store);
    let initial = EncoderConfig::default();
    let mut c = ParallelCompressor::builder(initial, parallel_config(u64::MAX))
        .with_graph(graph.clone())
        .build()
        .unwrap();
    for encoder in [initial, initial.with_level(Level::try_from(1).unwrap())] {
        c.reconfigure(encoder).unwrap();
        let mut serial = Compressor::builder(encoder)
            .with_graph(graph.clone())
            .build()
            .unwrap();
        for input in [&b""[..], &b"stored fallback"[..], &vec![1; SEGMENT][..]] {
            let expected = serial.compress(input).unwrap();
            assert_eq!(run(&mut c, input, 4), expected);
            assert_eq!(writer(&mut c, input), expected);
        }
    }
    let input = vec![7; SEGMENT + 1];
    let output = run(&mut c, &input, 2);
    assert!(output.len() > input.len()); // Store stays selected after encoder reconfiguration.
    assert_eq!(openzl_ffi::decompress(&output).unwrap(), input);
}

#[test]
fn selected_segmenter_runs_inside_each_parallel_segment() {
    let mut builder = GraphBuilder::new();
    let start = builder
        .serial_segmenter(ChunkSize::MIN, StandardGraph::Store)
        .unwrap();
    check(
        builder.build(start).unwrap(),
        &vec![3; 2 * SEGMENT + 19],
        EncoderConfig::default(),
    );
}

#[test]
#[cfg(all(
    feature = "conversion",
    feature = "delta",
    feature = "zigzag",
    feature = "bitpack"
))]
fn a_pipeline_selects_individual_codecs() {
    use mopenzl::graph::StandardNode;
    let mut builder = GraphBuilder::new();
    let start = builder
        .pipeline(
            &[
                StandardNode::ConvertSerialToNumLe32.into(),
                StandardNode::DeltaInt.into(),
                StandardNode::Zigzag.into(),
                StandardNode::BitpackInt.into(),
            ],
            StandardGraph::Store,
        )
        .unwrap();
    let input: Vec<u8> = (0..40000u32).flat_map(|i| i.to_le_bytes()).collect();
    check(
        builder.build(start).unwrap(),
        &input,
        EncoderConfig::default()
            .with_min_stream_size(1)
            .with_store_on_expansion(false),
    );
}

#[test]
fn invalid_graph_settings_preserve_header_error_sources_before_any_write() {
    let config = EncoderConfig::default().with_chunking(Chunking::Fixed(ChunkSize::MIN));
    let mut builder = GraphBuilder::new();
    let start = builder
        .serial_segmenter(ChunkSize::MIN, StandardGraph::Store)
        .unwrap();
    let graph = builder.build(start).unwrap();
    let mut c = ParallelCompressor::builder(config, parallel_config(0))
        .with_graph(graph)
        .build()
        .unwrap();
    let error = c
        .prepare_slice(b"bad settings", BatchConfig::auto(TaskCount::ONE))
        .unwrap_err();
    assert!(matches!(error, ParallelEncodeError::FrameHeader(_)));
    assert!(matches!(
        error
            .source()
            .unwrap()
            .downcast_ref::<mopenzl::EncodeError>(),
        Some(mopenzl::EncodeError::Graph(_))
    ));
    assert!(error.to_string().contains("already a segmenter"));
    let mut sink = std::io::Cursor::new(Vec::new());
    let (mut session, queue) = c.start(TaskCount::ONE).unwrap();
    let p = session
        .process(b"bad settings", &mut [0; 128], Operation::End)
        .unwrap();
    assert_eq!(p.produced, 0);
    queue.try_recv().unwrap().run();
    assert!(session.finish(&mut [0; 128]).is_err());
    drop(session);
    let (mut writer, queue) = c.writer(&mut sink, TaskCount::ONE).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(move || queue.into_iter().for_each(|t| t.run()));
        writer.write_all(b"bad settings").unwrap();
        assert!(writer.finish().is_err());
    });
    assert!(sink.into_inner().is_empty());
}

#[test]
#[cfg(not(feature = "zstd"))]
fn an_explicit_disabled_codec_propagates_the_serial_graph_error() {
    let mut c = ParallelCompressor::builder(EncoderConfig::default(), parallel_config(0))
        .with_graph(StandardGraph::Zstd.into())
        .build()
        .unwrap();
    let mut batch = c
        .prepare_slice(&[0; 100], BatchConfig::auto(TaskCount::ONE))
        .unwrap();
    let error = batch.run_inline().unwrap_err();
    assert!(error.to_string().contains("disabled"));
    let mut dst = b"prefix".to_vec();
    assert!(batch.finish_into(&mut dst).is_err());
    assert_eq!(dst, b"prefix");
}

#[test]
fn rust_only_graph_sessions_write_uninitialized_output_and_close_their_queue() {
    use std::mem::MaybeUninit;
    let c = ParallelCompressor::builder(EncoderConfig::default(), parallel_config(0))
        .with_graph(StandardGraph::Store.into())
        .build()
        .unwrap();
    let (mut session, queue) = c.into_session(TaskCount::ONE).unwrap();
    let input = b"graph";
    let mut consumed = 0;
    let mut frame = Vec::new();
    loop {
        let mut output = [MaybeUninit::uninit(); 3];
        let p = session
            .process_uninit(&input[consumed..], &mut output, Operation::End)
            .unwrap();
        consumed += p.consumed;
        // SAFETY: the session initialized exactly the reported produced prefix.
        frame.extend(
            output[..p.produced]
                .iter()
                .map(|b| unsafe { b.assume_init() }),
        );
        while let Some(task) = queue.try_recv() {
            task.run();
        }
        if p.status == ParallelEncoderStatus::Finished {
            break;
        }
    }
    assert!(queue.recv().is_none());
    assert_eq!(Decompressor::default().decompress(&frame).unwrap(), input);
}

#[test]
#[cfg(feature = "zstd")]
fn local_codec_parameters_match_the_serial_graph_configuration() {
    use mopenzl::graph::{LocalParams, StandardNode, params};
    let input = b"parameterized zstd node 1234567890 ".repeat(7000);
    let mut builder = GraphBuilder::new();
    let node = builder
        .parameterize_node(
            StandardNode::Zstd,
            LocalParams::default().with_int(params::zstd::COMPRESSION_LEVEL, 1),
        )
        .unwrap();
    let start = builder
        .static_graph(node, &[StandardGraph::Store.into()])
        .unwrap();
    let graph = builder.build(start).unwrap();
    let encoder = EncoderConfig::default()
        .with_min_stream_size(1)
        .with_store_on_expansion(false);
    let mut serial = Compressor::builder(
        encoder.with_chunking(Chunking::Fixed(ChunkSize::try_from(SEGMENT).unwrap())),
    )
    .with_graph(graph.clone())
    .build()
    .unwrap();
    let mut c = ParallelCompressor::builder(encoder, parallel_config(0))
        .with_graph(graph)
        .build()
        .unwrap();
    assert_eq!(run(&mut c, &input, 3), serial.compress(&input).unwrap());
}

#[test]
#[cfg(feature = "zstd")]
fn dictionary_graphs_preserve_the_bundle_id_in_batch_and_stream_headers() {
    use mopenzl::dict::{BundleStore, Dict, DictBundle};
    use mopenzl::graph::{NodeParams, StandardNode};
    let raw = b"a raw content dictionary for independent zstd chunks ".repeat(30);
    let dict = Dict::zstd(&raw, 3).unwrap();
    let id = dict.id();
    let bundle = DictBundle::try_from(vec![dict]).unwrap();
    let mut builder = GraphBuilder::new();
    let node = builder
        .parameterize_node_with(
            StandardNode::Zstd,
            NodeParams::default().with_dictionary(id),
        )
        .unwrap();
    builder.load_dictionaries(&bundle).unwrap();
    let start = builder
        .static_graph(node, &[StandardGraph::Store.into()])
        .unwrap();
    let graph = builder.build(start).unwrap();
    let fat = Vec::from(&bundle);
    let input = b"a raw content dictionary for independent zstd chunks ".repeat(3000);
    for version in [25, 27] {
        for checksums in 0..4 {
            let config = EncoderConfig::default()
                .with_format_version(FormatVersion::try_from(version).unwrap())
                .with_min_stream_size(1)
                .with_store_on_expansion(false)
                .with_content_checksum(checksums & 1 != 0)
                .with_compressed_checksum(checksums & 2 != 0);
            let mut c = ParallelCompressor::builder(config, parallel_config(0))
                .with_graph(graph.clone())
                .build()
                .unwrap();
            let frame = run(&mut c, &input, 4);
            assert_eq!(writer(&mut c, &input), frame);
            let outputs = openzl_ffi::enc_dict::decompress(&frame, &[&fat]).unwrap();
            assert_eq!(outputs[0].bytes, input);
            let mut decoder = Decompressor::builder(Default::default())
                .with_dictionaries(BundleStore::try_from(&bundle).unwrap())
                .build();
            assert_eq!(decoder.decompress(&frame).unwrap(), input);
            c.reconfigure_parallel(parallel_config(u64::MAX));
            for small in [&input[..100], &input[..0]] {
                let fallback = run(&mut c, small, 1);
                assert_eq!(decoder.decompress(&fallback).unwrap(), small);
                assert_eq!(writer(&mut c, small), fallback);
            }
        }
    }
}

#[cfg(feature = "custom-codec")]
mod custom {
    use super::*;
    use mopenzl::DataType;
    use mopenzl::custom_codec::{
        CodecError, CodecInput, CustomCodec, DecoderContext, EncoderContext, Signature,
    };
    struct Xor;
    impl CustomCodec for Xor {
        fn signature(&self) -> Signature {
            Signature {
                inputs: &[DataType::Serial],
                variable_input: false,
                outputs: &[DataType::Serial],
                variable_outputs: &[],
            }
        }
        fn encode(
            &self,
            cx: &mut EncoderContext<'_>,
            inputs: &[CodecInput<'_>],
        ) -> Result<(), CodecError> {
            cx.send_header(&[0xa5])?;
            cx.emit(0, 1, inputs[0].bytes.iter().map(|b| b ^ 0xa5).collect())
                .map(|_| ())
        }
        fn decode(
            &self,
            cx: &mut DecoderContext<'_>,
            inputs: &[CodecInput<'_>],
        ) -> Result<(), CodecError> {
            let bytes: Vec<_> = inputs[0].bytes.iter().map(|b| b ^ 0xa5).collect();
            cx.emit(0, 1, &bytes)
        }
    }
    #[test]
    fn application_codec_graphs_are_shared_with_workers_and_decode_with_c() {
        let mut builder = GraphBuilder::new();
        let node = builder.custom_codec(1000, Xor).unwrap();
        let start = builder
            .static_graph(node, &[StandardGraph::Store.into()])
            .unwrap();
        let encoder = EncoderConfig::default()
            .with_min_stream_size(1)
            .with_store_on_expansion(false);
        let mut c = ParallelCompressor::builder(encoder, parallel_config(0))
            .with_graph(builder.build(start).unwrap())
            .build()
            .unwrap();
        let input = b"application nodes in parallel segments ".repeat(4000);
        let frame = run(&mut c, &input, 3);
        assert_eq!(writer(&mut c, &input), frame);
        assert_eq!(
            openzl_ffi::custom_codec::transform(true, 1000, 27, &frame, input.len()).unwrap(),
            input
        );
        let mut decoder = Decompressor::builder(Default::default())
            .with_custom_codec(1000, Xor)
            .unwrap()
            .build();
        assert_eq!(decoder.decompress(&frame).unwrap(), input);
    }
}

#[test]
#[cfg(feature = "lz4")]
fn unsupported_graph_codec_versions_report_the_serial_error_without_output() {
    let config = EncoderConfig::default()
        .with_format_version(FormatVersion::try_from(21).unwrap())
        .with_min_stream_size(1)
        .with_store_on_expansion(false);
    let graph = CompressorGraph::standard(StandardGraph::Lz4);
    let input = vec![7; SEGMENT + 1];
    let mut serial = Compressor::builder(config)
        .with_graph(graph.clone())
        .build()
        .unwrap();
    let serial_error = serial.compress(&input).unwrap_err().to_string();
    let mut c = ParallelCompressor::builder(config, parallel_config(0))
        .with_graph(graph)
        .build()
        .unwrap();
    let mut b = c
        .prepare_slice(&input, BatchConfig::auto(TaskCount::try_from(2).unwrap()))
        .unwrap();
    assert!(b.run_inline().is_err());
    let mut output = b"prefix".to_vec();
    let error = b.finish_into(&mut output).unwrap_err();
    assert!(error.to_string().contains(&serial_error));
    assert_eq!(output, b"prefix");
}
