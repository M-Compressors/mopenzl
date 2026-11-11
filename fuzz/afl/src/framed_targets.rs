//! Bounded, engine-neutral framed-container fuzz targets.

use crate::Schedule;
use mopenzl::framing::*;
use mopenzl::graph::{CompressorGraph, StandardGraph};
use mopenzl::{DecodeLimits, DecodeOperation, DecoderConfig};

fn decoder() -> FramedDecompressor {
    FramedDecompressor::new(FramedDecodeConfig {
        framing: FramingConfig {
            max_resources: 128,
            max_frame_bytes: 1 << 20,
            max_metadata_bytes: 4096,
            max_output_bytes: 1 << 20,
            max_container_bytes: 2 << 20,
        },
        decoder: DecoderConfig::default()
            .with_limits(DecodeLimits::default().with_max_output_bytes(Some(1 << 20))),
    })
    .expect("valid config")
}

/// Arbitrary wire bytes: whole-buffer and split-input acceptance must agree.
pub fn framed_decode(data: &[u8]) {
    let data = &data[..data.len().min(2 << 20)];
    let expected = decoder().decompress(data);
    let mut owner = decoder();
    let mut session = owner.start();
    let mut schedule = Schedule::from_bytes(data);
    let mut at = 0usize;
    let mut outputs = Vec::new();
    let actual = loop {
        let end = at
            .saturating_add(schedule.length(data.len()))
            .min(data.len());
        let op = if end == data.len() {
            DecodeOperation::Finish
        } else {
            DecodeOperation::Process
        };
        match session.process(&data[at..end], op) {
            Ok(p) => {
                assert!(p.consumed <= end - at);
                at += p.consumed;
                if let Some(resource) = p.resource {
                    outputs.push(resource);
                }
                if p.status == FramedDecoderStatus::Finished {
                    break Ok(outputs);
                }
                assert!(p.consumed != 0 || p.status == FramedDecoderStatus::Resource);
            }
            Err(failure) => {
                assert!(failure.consumed <= end - at);
                break Err(failure.error);
            }
        }
    };
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => assert_eq!(expected.resources, actual),
        (Err(_), Err(_)) => {}
        other => panic!("one-shot/session disagreement: {other:?}"),
    }
}

/// Random resource boundaries, graph overrides and output backpressure. Every
/// inner frame is also decoded by the independent C OpenZL implementation.
pub fn framed_roundtrip(data: &[u8]) {
    let data = &data[..data.len().min(64 << 10)];
    let store = CompressorGraph::standard(StandardGraph::Store);
    let mut schedule = Schedule::from_bytes(data);
    let width = schedule.length(8192);
    let mut resources: Vec<_> = data
        .chunks(width)
        .take(64)
        .map(|bytes| FramedResource {
            graph: (bytes.first().copied().unwrap_or(0) & 1 != 0).then_some(&store),
            ..FramedResource::from(bytes)
        })
        .collect();
    if data.is_empty() {
        resources.push(FramedResource::from(data));
    }
    let mut owner = FramedCompressor::new(Default::default()).expect("encoder");
    let expected = owner.compress(resources.as_slice().into()).expect("encode");
    let mut session = owner.start().expect("session");
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    for &resource in &resources {
        loop {
            let n = schedule.length(buffer.len());
            let p = session
                .process(&mut buffer[..n], FramedEncodeOperation::Process)
                .expect("drain");
            bytes.extend_from_slice(&buffer[..p.produced]);
            if p.status == FramedEncoderStatus::NeedsInput {
                break;
            }
        }
        session.resource(resource).expect("resource");
    }
    loop {
        let n = schedule.length(buffer.len());
        let p = session
            .process(&mut buffer[..n], FramedEncodeOperation::Finish)
            .expect("finish");
        bytes.extend_from_slice(&buffer[..p.produced]);
        if p.status == FramedEncoderStatus::Finished {
            break;
        }
    }
    assert_eq!(bytes, expected);
    let decoded = decoder().decompress(&bytes).expect("decode");
    assert_eq!(decoded.resources.len(), resources.len());
    for (actual, original) in decoded.resources.iter().zip(&resources) {
        let ResourceInput::Serial(original) = original.input else {
            unreachable!()
        };
        assert_eq!(actual.outputs[0].content(), original);
        let at = actual.info.offset as usize;
        let end = at + actual.info.length as usize;
        assert_eq!(
            openzl_ffi::decompress(&bytes[at + 28..end]).expect("C decode"),
            original
        );
    }
    framed_decode(&bytes);
}
