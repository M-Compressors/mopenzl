//! Uninitialized destinations: same bytes, counts and errors as the
//! initialized APIs. Buffers start as `MaybeUninit::uninit()` so Miri checks
//! that nothing reads before writing. Tests named `rust_only_*` never call
//! the C library and run under Miri.
#![cfg(all(feature = "compression", feature = "decompression"))]

mod support;

use std::mem::MaybeUninit;

use mopenzl::{
    ChunkSize, Chunking, Compressor, DecodeError, DecodeOperation, DecoderStatus, Decompressor,
    EncodeError, EncoderConfig, EncoderStatus, InputSize, Operation,
};

/// The first `len` bytes of `buffer`.
fn assume_init(buffer: &[MaybeUninit<u8>], len: usize) -> Vec<u8> {
    buffer[..len]
        .iter()
        // SAFETY: the API under test reported these bytes as written.
        .map(|byte| unsafe { byte.assume_init() })
        .collect()
}

fn uninit(len: usize) -> Vec<MaybeUninit<u8>> {
    vec![MaybeUninit::uninit(); len]
}

fn inputs() -> Vec<Vec<u8>> {
    let large = if cfg!(miri) { 3_000 } else { 100_000 };
    vec![
        Vec::new(),
        b"tiny".to_vec(),
        support::text(large, 1),
        support::noise(large / 3, 2),
    ]
}

#[test]
fn rust_only_one_shot_uninit_matches_the_initialized_apis() {
    let config = EncoderConfig::default().with_chunking(Chunking::Fixed(ChunkSize::MIN));
    let mut encoder = Compressor::new(config).unwrap();
    let mut decoder = Decompressor::default();
    for input in inputs() {
        let expected = encoder.compress(&input).unwrap();
        let mut out = uninit(Compressor::max_compressed_size(input.len()).unwrap());
        let written = encoder.compress_to_uninit(&input, &mut out).unwrap();
        assert_eq!(assume_init(&out, written), expected);

        let mut content = uninit(input.len() + 3);
        let decoded = decoder
            .decompress_to_uninit(&expected, &mut content)
            .unwrap();
        assert_eq!(assume_init(&content, decoded), input);
    }
}

#[test]
fn rust_only_small_uninit_destinations_report_the_same_errors() {
    let input = support::text(5_000, 3);
    let mut encoder = Compressor::default();
    let frame = encoder.compress(&input).unwrap();
    let mut out = uninit(10);
    assert_eq!(
        encoder.compress_to_uninit(&input, &mut out),
        Err(EncodeError::OutputTooSmall { provided: 10 })
    );
    let mut decoder = Decompressor::default();
    let mut content = uninit(100);
    assert!(matches!(
        decoder.decompress_to_uninit(&frame, &mut content),
        Err(DecodeError::OutputTooSmall { .. })
    ));
    let mut plain = vec![0u8; 100];
    assert_eq!(
        decoder.decompress_to_uninit(&frame, &mut content),
        decoder.decompress_to_slice(&frame, &mut plain)
    );
}

#[test]
fn rust_only_sessions_mix_uninit_and_initialized_calls() {
    let input = support::text(if cfg!(miri) { 4_000 } else { 120_000 }, 4);
    let config = EncoderConfig::default().with_chunking(Chunking::Fixed(ChunkSize::MIN));
    let mut encoder = Compressor::new(config).unwrap();
    let expected = encoder.compress(&input).unwrap();

    let mut session = encoder.start(InputSize::Exact(input.len() as u64).into());
    let (mut frame, mut rest, mut turn) = (Vec::new(), &input[..], 0usize);
    let mut plain = vec![0u8; 777];
    let mut spare = uninit(777);
    loop {
        let take = rest.len().min(1_000);
        let operation = if take == rest.len() {
            Operation::End
        } else {
            Operation::Continue
        };
        let progress = if turn % 2 == 0 {
            let progress = session
                .process_uninit(&rest[..take], &mut spare, operation)
                .unwrap();
            frame.extend(assume_init(&spare, progress.produced));
            progress
        } else {
            let progress = session
                .process(&rest[..take], &mut plain, operation)
                .unwrap();
            frame.extend_from_slice(&plain[..progress.produced]);
            progress
        };
        turn += 1;
        rest = &rest[progress.consumed..];
        if progress.status == EncoderStatus::Finished {
            break;
        }
    }
    assert_eq!(frame, expected);

    let mut decoder = Decompressor::default();
    let mut session = decoder.start();
    let (mut content, mut pos, mut turn) = (Vec::new(), 0usize, 0usize);
    loop {
        let end = (pos + 500).min(frame.len());
        let operation = if end == frame.len() {
            DecodeOperation::Finish
        } else {
            DecodeOperation::Process
        };
        let progress = if turn % 2 == 0 {
            let progress = session
                .process_uninit(&frame[pos..end], &mut spare, operation)
                .unwrap();
            content.extend(assume_init(&spare, progress.produced));
            progress
        } else {
            let progress = session
                .process(&frame[pos..end], &mut plain, operation)
                .unwrap();
            content.extend_from_slice(&plain[..progress.produced]);
            progress
        };
        turn += 1;
        pos += progress.consumed;
        if progress.status == DecoderStatus::Finished {
            break;
        }
    }
    assert_eq!(content, input);
}

#[cfg(feature = "zstd")]
#[test]
fn reference_frames_decode_into_uninit_memory() {
    let mut decoder = Decompressor::default();
    for input in inputs() {
        let frame = openzl_ffi::compress(&input, Default::default()).unwrap();
        let mut content = uninit(input.len());
        let written = decoder.decompress_to_uninit(&frame, &mut content).unwrap();
        assert_eq!(assume_init(&content, written), input);
    }
}

#[cfg(feature = "custom-codec")]
#[test]
fn rust_only_custom_codec_uses_uninitialized_destinations() {
    use mopenzl::DataType;
    use mopenzl::custom_codec::{
        CodecError, CodecInput, CustomCodec, DecoderContext, EncoderContext, Signature,
    };
    use mopenzl::graph::{GraphBuilder, StandardGraph};
    struct Identity;
    impl CustomCodec for Identity {
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
            cx.emit(0, 1, inputs[0].bytes.to_vec()).map(|_| ())
        }
        fn decode(
            &self,
            cx: &mut DecoderContext<'_>,
            inputs: &[CodecInput<'_>],
        ) -> Result<(), CodecError> {
            cx.emit(0, 1, inputs[0].bytes)
        }
    }
    let mut builder = GraphBuilder::new();
    let node = builder.custom_codec(22, Identity).unwrap();
    let graph = builder
        .static_graph(node, &[StandardGraph::Store.into()])
        .unwrap();
    let config = EncoderConfig::default().with_store_on_expansion(false);
    let mut encoder = Compressor::builder(config)
        .with_graph(builder.build(graph).unwrap())
        .build()
        .unwrap();
    let mut decoder = Decompressor::builder(Default::default())
        .with_custom_codec(22, Identity)
        .unwrap()
        .build();
    let input = b"custom output starts uninitialized";
    let mut output = uninit(1024);
    let written = encoder.compress_to_uninit(input, &mut output).unwrap();
    let frame = assume_init(&output, written);
    let mut content = uninit(input.len());
    let written = decoder.decompress_to_uninit(&frame, &mut content).unwrap();
    assert_eq!(assume_init(&content, written), input);
}
