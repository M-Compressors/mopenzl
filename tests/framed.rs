#![cfg(all(feature = "framed", feature = "compression", feature = "decompression"))]

use mopenzl::framing::*;
use mopenzl::graph::{CompressorGraph, StandardGraph};
use mopenzl::input::{Input, Numbers, Strings};
use mopenzl::{Backend, Compressor, DecodeOperation, Decompressor, EncoderConfig};

fn encode(resources: &[FramedResource<'_>]) -> Vec<u8> {
    FramedCompressor::new(Default::default())
        .unwrap()
        .compress(resources.into())
        .unwrap()
}

fn frame<'a>(bytes: &'a [u8], info: &ResourceInfo) -> &'a [u8] {
    let start = info.offset as usize;
    let meta = u32::from_le_bytes(bytes[start + 4..start + 8].try_into().unwrap()) as usize;
    &bytes[start + 28 + meta..start + info.length as usize]
}

#[test]
fn each_resource_keeps_its_graph_and_ordinary_openzl_frame() {
    let store = CompressorGraph::standard(StandardGraph::Store);
    let content = b"different independent graph per resource ".repeat(100);
    let resources = [
        FramedResource::from(content.as_slice()),
        FramedResource {
            graph: Some(&store),
            ..FramedResource::from(content.as_slice())
        },
        FramedResource::from(content.as_slice()),
        FramedResource::from(&b""[..]),
    ];
    let bytes = encode(&resources);
    let output = FramedDecompressor::new(Default::default())
        .unwrap()
        .decompress(&bytes)
        .unwrap();
    assert_eq!(output.resources.len(), 4);
    let expected = Compressor::default().compress(&content).unwrap();
    assert_eq!(frame(&bytes, &output.resources[0].info), expected);
    assert_eq!(frame(&bytes, &output.resources[2].info), expected);
    let expected_store = Compressor::builder(EncoderConfig::default())
        .with_graph(store)
        .build()
        .unwrap()
        .compress(&content)
        .unwrap();
    assert_eq!(frame(&bytes, &output.resources[1].info), expected_store);
    for resource in &output.resources {
        let raw = frame(&bytes, &resource.info);
        let decoded = openzl_ffi::decompress(raw).unwrap();
        assert_eq!(decoded, resource.outputs[0].content());
    }
    #[cfg(feature = "zstd")]
    assert_eq!(
        frame(&bytes, &output.resources[0].info),
        openzl_ffi::compress(&content, Default::default()).unwrap()
    );
}

#[test]
fn metadata_and_typed_multi_input_resources_preserve_their_shape() {
    let store = CompressorGraph::standard(StandardGraph::Store);
    let integers: Vec<_> = (0..32u32).flat_map(u32::to_le_bytes).collect();
    let lengths = [2, 0, 4];
    let inputs = [
        Input::from(Numbers::try_from((integers.as_slice(), 4)).unwrap()),
        Input::from(Strings::try_from((&b"abcdef"[..], &lengths[..])).unwrap()),
    ];
    let fields = [
        MetadataField {
            code: *b"id",
            value: "числа".as_bytes(),
        },
        MetadataField {
            code: *b"mt",
            value: &(-1i64).to_le_bytes(),
        },
        MetadataField {
            code: *b"AB",
            value: b"one",
        },
        MetadataField {
            code: *b"AB",
            value: b"two",
        },
    ];
    let resources = [FramedResource {
        graph: Some(&store),
        metadata: &fields,
        ..FramedResource::from(&inputs[..])
    }];
    // Storing typed inputs requires the conversion codec.
    if !cfg!(feature = "conversion") {
        return;
    }
    let bytes = encode(&resources);
    let output = FramedDecompressor::new(Default::default())
        .unwrap()
        .decompress(&bytes)
        .unwrap();
    let resource = &output.resources[0];
    assert_eq!(resource.info.decoded_size, integers.len() as u64 + 6);
    assert_eq!(resource.outputs[0].content(), integers);
    assert_eq!(resource.outputs[0].width(), 4);
    assert_eq!(resource.outputs[1].content(), b"abcdef");
    assert_eq!(resource.outputs[1].string_lens(), lengths);
    for (actual, expected) in resource.metadata.iter().zip(fields) {
        assert_eq!(actual.code, expected.code);
        assert_eq!(actual.value, expected.value);
    }
}

#[test]
fn sessions_match_one_shot_at_every_short_input_and_output_split() {
    let resources = [
        FramedResource::from(&b"first payload"[..]),
        FramedResource::from(&b""[..]),
        FramedResource::from(&b"last"[..]),
    ];
    let expected = encode(&resources);
    for width in [1, 2, 7, 31, 8192] {
        let mut owner = FramedCompressor::from(Compressor::default());
        let mut session = owner.start().unwrap();
        let mut bytes = Vec::new();
        let mut out = vec![0; width];
        let zero = session
            .process(&mut [], FramedEncodeOperation::Process)
            .unwrap();
        assert_eq!(zero.status, FramedEncoderStatus::NeedsOutput);
        for resource in resources {
            loop {
                let p = session
                    .process(&mut out, FramedEncodeOperation::Process)
                    .unwrap();
                bytes.extend_from_slice(&out[..p.produced]);
                if p.status == FramedEncoderStatus::NeedsInput {
                    break;
                }
            }
            session.resource(resource).unwrap();
            assert!(matches!(
                session.resource(resource),
                Err(FramedError::OutputPending)
            ));
        }
        loop {
            let p = session
                .process(&mut out, FramedEncodeOperation::Finish)
                .unwrap();
            bytes.extend_from_slice(&out[..p.produced]);
            if p.status == FramedEncoderStatus::Finished {
                break;
            }
        }
        assert!(matches!(
            session.resource(resources[0]),
            Err(FramedError::InvalidState)
        ));
        assert_eq!(bytes, expected);
    }
    let expected_decoded = FramedDecompressor::new(Default::default())
        .unwrap()
        .decompress(&expected)
        .unwrap();
    for width in [1, 2, 7, 31, expected.len()] {
        let mut owner = FramedDecompressor::from(Decompressor::default());
        let mut session = owner.start();
        assert_eq!(
            session
                .process(&[], DecodeOperation::Process)
                .unwrap()
                .consumed,
            0
        );
        let mut found = Vec::new();
        for piece in expected.chunks(width) {
            let mut at = 0;
            while at < piece.len() {
                let p = session
                    .process(&piece[at..], DecodeOperation::Process)
                    .unwrap();
                at += p.consumed;
                if let Some(resource) = p.resource {
                    found.push(resource);
                }
            }
        }
        assert_eq!(
            session
                .process(&[], DecodeOperation::Finish)
                .unwrap()
                .status,
            FramedDecoderStatus::Finished
        );
        assert_eq!(
            session
                .process(&[], DecodeOperation::Process)
                .unwrap()
                .status,
            FramedDecoderStatus::Finished
        );
        let failure = session
            .process(b"late input", DecodeOperation::Process)
            .unwrap_err();
        assert_eq!(failure.consumed, 0);
        assert!(matches!(failure.error, FramedError::InvalidState));
        assert_eq!(found, expected_decoded.resources);
    }
}

#[test]
fn empty_container_reuse_append_rollback_and_graph_error_recovery() {
    let mut owner = FramedCompressor::new(Default::default()).unwrap();
    let empty = owner.compress([].as_slice().into()).unwrap();
    assert!(
        FramedDecompressor::new(Default::default())
            .unwrap()
            .decompress(&empty)
            .unwrap()
            .resources
            .is_empty()
    );
    let fields = [MetadataField {
        code: *b"id",
        value: &[0xff],
    }];
    let invalid = FramedResource {
        metadata: &fields,
        ..FramedResource::from(&b"data"[..])
    };
    let mut dst = b"prefix".to_vec();
    assert!(
        owner
            .compress_into([invalid].as_slice().into(), &mut dst)
            .is_err()
    );
    assert_eq!(dst, b"prefix");
    let range = owner.compress_into([].as_slice().into(), &mut dst).unwrap();
    assert_eq!(&dst[range], empty);
    let constant = CompressorGraph::standard(StandardGraph::Constant);
    let bad_graph = FramedResource {
        graph: Some(&constant),
        ..FramedResource::from(&b"nonconstant input that cannot be constant"[..])
    };
    let mut session = owner.start().unwrap();
    session
        .process(&mut [0; 8], FramedEncodeOperation::Process)
        .unwrap();
    assert!(session.resource(bad_graph).is_err());
    assert!(session.resource(invalid).is_err());
    session
        .resource(FramedResource::from(&b"now valid"[..]))
        .unwrap();
    drop(session);
    assert_eq!(owner.compress([].as_slice().into()).unwrap(), empty);
}

#[test]
fn malformed_and_truncated_containers_fail_and_poison_only_the_session() {
    let valid = encode(&[FramedResource::from(&b"payload"[..])]);
    for end in 0..valid.len() {
        assert!(
            FramedDecompressor::new(Default::default())
                .unwrap()
                .decompress(&valid[..end])
                .is_err(),
            "prefix {end}"
        );
    }
    for at in 0..valid.len() {
        let mut changed = valid.clone();
        changed[at] ^= 0x80;
        assert!(
            FramedDecompressor::new(Default::default())
                .unwrap()
                .decompress(&changed)
                .is_err(),
            "mutated byte {at}"
        );
    }
    let mut trailing = valid.clone();
    trailing.push(0);
    assert!(
        FramedDecompressor::new(Default::default())
            .unwrap()
            .decompress(&trailing)
            .is_err()
    );
    let mut decoder = FramedDecompressor::new(Default::default()).unwrap();
    let mut session = decoder.start();
    let failure = session
        .process(b"invalid!", DecodeOperation::Finish)
        .unwrap_err();
    assert_eq!(failure.consumed, 8);
    assert!(matches!(
        session
            .process(&valid, DecodeOperation::Finish)
            .unwrap_err()
            .error,
        FramedError::InvalidState
    ));
    drop(session);
    assert!(decoder.decompress(&valid).is_ok());
}

#[test]
fn container_limits_apply_to_aggregate_output_counts_metadata_and_wire_lengths() {
    let resource = FramedResource::from(&b"abc"[..]);
    let resources = [resource, resource];
    let valid = encode(&resources);
    let cases = [
        FramingConfig {
            max_resources: 1,
            ..Default::default()
        },
        FramingConfig {
            max_frame_bytes: 1,
            ..Default::default()
        },
        FramingConfig {
            max_output_bytes: 5,
            ..Default::default()
        },
        FramingConfig {
            max_container_bytes: 60,
            ..Default::default()
        },
    ];
    for config in cases {
        let mut encoder = FramedCompressor::new(Default::default()).unwrap();
        encoder.reconfigure(config).unwrap();
        assert!(matches!(
            encoder.compress(resources.as_slice().into()),
            Err(FramedError::Limit(_))
        ));
        let mut decoder = FramedDecompressor::new(Default::default()).unwrap();
        decoder.reconfigure(config).unwrap();
        assert!(matches!(
            decoder.decompress(&valid),
            Err(FramedError::Limit(_))
        ));
    }
    let invalid = FramingConfig {
        max_container_bytes: 0,
        ..Default::default()
    };
    assert!(
        FramedCompressor::new(FramedEncodeConfig {
            framing: invalid,
            ..Default::default()
        })
        .is_err()
    );
    assert!(
        FramedDecompressor::new(FramedDecodeConfig {
            framing: invalid,
            ..Default::default()
        })
        .is_err()
    );
    let mut encoder = FramedCompressor::new(Default::default()).unwrap();
    let mut decoder = FramedDecompressor::new(Default::default()).unwrap();
    assert!(encoder.reconfigure(invalid).is_err());
    assert!(decoder.reconfigure(invalid).is_err());
    assert!(
        decoder
            .decompress(&encoder.compress([].as_slice().into()).unwrap())
            .is_ok()
    );
}

#[test]
fn metadata_reserved_codes_are_validated_before_commit() {
    let invalid_fields = [
        vec![
            MetadataField {
                code: *b"id",
                value: b"one",
            },
            MetadataField {
                code: *b"id",
                value: b"two",
            },
        ],
        vec![MetadataField {
            code: *b"mt",
            value: b"short",
        }],
        vec![MetadataField {
            code: *b"ab",
            value: b"bad code",
        }],
        vec![MetadataField {
            code: *b"id",
            value: &[0xff],
        }],
    ];
    for metadata in invalid_fields {
        let resource = FramedResource {
            metadata: &metadata,
            ..FramedResource::from(&b""[..])
        };
        assert!(
            FramedCompressor::new(Default::default())
                .unwrap()
                .compress([resource].as_slice().into())
                .is_err()
        );
    }
    let config = FramingConfig {
        max_metadata_bytes: 6,
        ..Default::default()
    };
    let metadata = [MetadataField {
        code: *b"AB",
        value: b"one",
    }];
    let resource = FramedResource {
        metadata: &metadata,
        ..FramedResource::from(&b""[..])
    };
    assert!(
        FramedCompressor::new(FramedEncodeConfig {
            framing: config,
            ..Default::default()
        })
        .unwrap()
        .compress([resource].as_slice().into())
        .is_err()
    );
    let valid = encode(&[resource]);
    assert!(
        FramedDecompressor::new(FramedDecodeConfig {
            framing: config,
            ..Default::default()
        })
        .unwrap()
        .decompress(&valid)
        .is_err()
    );
}

#[test]
fn framed_bytes_are_identical_on_every_available_backend() {
    let content = b"backend identity across independent frames ".repeat(100);
    let resources = [FramedResource::from(content.as_slice())];
    let baseline = encode(&resources);
    for backend in Backend::available() {
        let raw = Compressor::builder(EncoderConfig::default())
            .with_backend(backend)
            .build()
            .unwrap();
        assert_eq!(
            FramedCompressor::from(raw)
                .compress(resources.as_slice().into())
                .unwrap(),
            baseline
        );
        let raw = Decompressor::builder(Default::default())
            .with_backend(backend)
            .build();
        assert_eq!(
            FramedDecompressor::from(raw)
                .decompress(&baseline)
                .unwrap()
                .resources[0]
                .outputs[0]
                .content(),
            content
        );
    }
}

#[cfg(feature = "zstd")]
#[test]
fn dictionary_graphs_do_not_leak_into_following_resources_and_keep_decoder_stores() {
    use mopenzl::dict::{BundleStore, Dict, DictBundle};
    use mopenzl::graph::{GraphBuilder, NodeParams, StandardNode};
    let dictionary = Dict::zstd(
        b"reusable dictionary words for independent resource frames ",
        6,
    )
    .unwrap();
    let id = dictionary.id();
    let bundle = DictBundle::try_from(vec![dictionary]).unwrap();
    let mut builder = GraphBuilder::new();
    let node = builder
        .parameterize_node_with(
            StandardNode::Zstd,
            NodeParams::default().with_dictionary(id),
        )
        .unwrap();
    let start = builder
        .static_graph(node, &[StandardGraph::Store.into()])
        .unwrap();
    builder.load_dictionaries(&bundle).unwrap();
    let graph = builder.build(start).unwrap();
    let content = b"reusable dictionary words for independent resource frames ".repeat(20);
    let resources = [
        FramedResource {
            graph: Some(&graph),
            ..FramedResource::from(content.as_slice())
        },
        FramedResource::from(content.as_slice()),
    ];
    let bytes = encode(&resources);
    assert!(
        FramedDecompressor::new(Default::default())
            .unwrap()
            .decompress(&bytes)
            .is_err()
    );
    let store = BundleStore::try_from(&bundle).unwrap();
    let raw = Decompressor::builder(Default::default())
        .with_dictionaries(store)
        .build();
    let output = FramedDecompressor::from(raw).decompress(&bytes).unwrap();
    for resource in &output.resources {
        assert_eq!(resource.outputs[0].content(), content);
    }
    assert!(
        mopenzl::FrameInfo::try_from(frame(&bytes, &output.resources[0].info))
            .unwrap()
            .bundle_id()
            .is_some()
    );
    assert!(
        mopenzl::FrameInfo::try_from(frame(&bytes, &output.resources[1].info))
            .unwrap()
            .bundle_id()
            .is_none()
    );
}

#[cfg(not(feature = "no_std"))]
mod transport {
    use super::*;
    use std::io::{Cursor, ErrorKind, Read, Seek, SeekFrom, Write};

    #[derive(Debug, Default)]
    struct Stubborn {
        bytes: Vec<u8>,
        calls: usize,
    }
    impl Write for Stubborn {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            match self.calls % 5 {
                1 => Err(ErrorKind::Interrupted.into()),
                2 => Err(ErrorKind::WouldBlock.into()),
                3 => Ok(0),
                _ => {
                    let n = bytes.len().min(3);
                    self.bytes.extend_from_slice(&bytes[..n]);
                    Ok(n)
                }
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn resource_writer_retries_without_duplicates_and_matches_native_bytes() {
        let resources = [
            FramedResource::from(&b"one"[..]),
            FramedResource::from(&b"two"[..]),
        ];
        let expected = encode(&resources);
        let mut owner = FramedCompressor::new(Default::default()).unwrap();
        let mut writer = owner.framed_writer(Stubborn::default()).unwrap();
        for resource in resources {
            while let Err(error) = writer.resource(resource) {
                assert!(matches!(error, FramedError::Io(_)));
            }
        }
        while let Err(error) = writer.flush() {
            assert!(matches!(error, FramedError::Io(_)));
        }
        let result = loop {
            match writer.finish() {
                Ok(sink) => break sink,
                Err(failure) => {
                    assert!(matches!(failure.error, FramedError::Io(_)));
                    assert!(format!("{failure:?}").contains("FramedFinishError"));
                    assert!(format!("{failure}").contains("could not be finished"));
                    assert!(std::error::Error::source(&*failure).is_some());
                    writer = failure.writer;
                }
            }
        };
        assert_eq!(result.bytes, expected);
    }

    #[test]
    fn a_large_directory_and_resource_cross_transport_buffer_boundaries() {
        let payload = vec![0x5a; 20_000];
        let graph = CompressorGraph::standard(StandardGraph::Store);
        let resource = FramedResource {
            graph: Some(&graph),
            ..FramedResource::from(payload.as_slice())
        };
        let mut resources = vec![FramedResource::from(&b""[..]); 400];
        resources.push(resource);
        let expected = encode(&resources);
        let mut owner = FramedCompressor::new(Default::default()).unwrap();
        let mut writer = owner.framed_writer(Vec::new()).unwrap();
        for &resource in &resources {
            writer.resource(resource).unwrap();
        }
        writer.try_finish().unwrap();
        assert!(matches!(
            writer.resource(resource),
            Err(FramedError::InvalidState)
        ));
        assert_eq!(writer.finish().unwrap(), expected);
    }

    #[test]
    fn sequential_and_seek_readers_preserve_metadata_and_embedded_offsets() {
        let resources = [
            FramedResource::from(&b"first"[..]),
            FramedResource::from(&b""[..]),
            FramedResource::from(&b"last"[..]),
        ];
        let bytes = encode(&resources);
        let expected = FramedDecompressor::new(Default::default())
            .unwrap()
            .decompress(&bytes)
            .unwrap();
        let mut owner = FramedDecompressor::new(Default::default()).unwrap();
        let mut reader = owner.framed_reader(Cursor::new(&bytes));
        for resource in &expected.resources {
            assert_eq!(reader.read_resource().unwrap().as_ref(), Some(resource));
        }
        assert!(reader.read_resource().unwrap().is_none());
        assert!(reader.read_resource().unwrap().is_none());
        drop(reader);
        let mut embedded = Cursor::new([b"prefix".as_slice(), &bytes].concat());
        embedded.seek(SeekFrom::Start(6)).unwrap();
        let mut seek = owner.framed_seek_reader(embedded).unwrap();
        assert_eq!(seek.resources().len(), 3);
        for i in [2, 0, 1, 2] {
            assert_eq!(seek.read_resource(i).unwrap(), expected.resources[i]);
        }
        assert!(seek.read_resource(3).is_err());
    }

    #[derive(Debug)]
    struct ShortReader {
        bytes: Cursor<Vec<u8>>,
        calls: usize,
    }
    impl Read for ShortReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls % 3 == 1 {
                return Err(ErrorKind::Interrupted.into());
            }
            if self.calls % 3 == 2 {
                return Err(ErrorKind::WouldBlock.into());
            }
            let n = out.len().min(1);
            self.bytes.read(&mut out[..n])
        }
    }

    #[test]
    fn sequential_reader_retries_transports_and_keeps_codec_failure_sticky() {
        let bytes = encode(&[FramedResource::from(&b"content"[..])]);
        let source = ShortReader {
            bytes: Cursor::new(bytes),
            calls: 0,
        };
        let mut owner = FramedDecompressor::new(Default::default()).unwrap();
        let mut reader = owner.framed_reader(source);
        let mut outputs = Vec::new();
        loop {
            match reader.read_resource() {
                Ok(Some(resource)) => outputs.push(resource),
                Ok(None) => break,
                Err(error) => assert!(matches!(error, FramedError::Io(_))),
            }
        }
        assert_eq!(outputs.len(), 1);
        drop(reader);
        let mut reader = owner.framed_reader(&b"bad data"[..]);
        assert!(reader.read_resource().is_err());
        assert!(matches!(
            reader.read_resource(),
            Err(FramedError::InvalidState)
        ));
    }

    #[test]
    fn seek_index_and_selected_payloads_are_validated_independently() {
        let valid = encode(&[FramedResource::from(&b"payload"[..])]);
        for at in 0..valid.len() {
            let mut changed = valid.clone();
            changed[at] ^= 0x80;
            let mut owner = FramedDecompressor::new(Default::default()).unwrap();
            if let Ok(mut reader) = owner.framed_seek_reader(Cursor::new(changed)) {
                assert!(reader.read_resource(0).is_err(), "mutated byte {at}");
            }
        }
        let mut owner = FramedDecompressor::new(Default::default()).unwrap();
        assert!(owner.framed_seek_reader(Cursor::new([])).is_err());
    }
}
