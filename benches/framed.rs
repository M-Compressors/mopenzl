//! End-to-end resource containers versus the C encoder/decoder's independent
//! frames. The C API has no mopenzl container; its times exclude outer framing.
mod support;

use hotpath::criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mopenzl::DecodeOperation;
use mopenzl::framing::{FramedCompressor, FramedDecompressor, FramedResource};
use mopenzl::framing::{FramedDecoderStatus, FramedEncodeOperation, FramedEncoderStatus};
use std::hint::black_box;

fn framed(c: &mut Criterion) {
    let mut group = c.benchmark_group("framed");
    for (name, input) in support::corpora() {
        let resources: Vec<_> = input.chunks(64 << 10).map(FramedResource::from).collect();
        let mut encoder = FramedCompressor::new(Default::default()).expect("encoder");
        let mut decoder = FramedDecompressor::new(Default::default()).expect("decoder");
        let bytes = encoder
            .compress(resources.as_slice().into())
            .expect("encode");
        let frames: Vec<_> = input
            .chunks(64 << 10)
            .map(|chunk| openzl_ffi::compress(chunk, Default::default()).expect("C frame"))
            .collect();
        let decoded = decoder.decompress(&bytes).expect("decode");
        for (resource, reference) in decoded.resources.iter().zip(&frames) {
            let at = resource.info.offset as usize;
            let frame_start = at + 28; // no resource metadata in these cases
            assert_eq!(
                &bytes[frame_start..at + resource.info.length as usize],
                reference
            );
            assert_eq!(
                openzl_ffi::decompress(reference).expect("C decode"),
                resource.outputs[0].content()
            );
        }
        let frame_size: usize = frames.iter().map(Vec::len).sum();
        println!(
            "{name}: {} input, {frame_size} OpenZL, {} container bytes",
            input.len(),
            bytes.len()
        );
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_function(BenchmarkId::new("rust-encode-container", name), |b| {
            b.iter(|| {
                encoder
                    .compress(black_box(resources.as_slice()).into())
                    .expect("encode")
            });
        });
        let mut buffer = [0; 8192];
        let mut encode_session = || {
            let mut session = encoder.start().expect("session");
            let mut output = Vec::new();
            for &resource in &resources {
                loop {
                    let p = session
                        .process(&mut buffer, FramedEncodeOperation::Process)
                        .expect("drain");
                    output.extend_from_slice(&buffer[..p.produced]);
                    if p.status == FramedEncoderStatus::NeedsInput {
                        break;
                    }
                }
                session.resource(resource).expect("resource");
            }
            loop {
                let p = session
                    .process(&mut buffer, FramedEncodeOperation::Finish)
                    .expect("finish");
                output.extend_from_slice(&buffer[..p.produced]);
                if p.status == FramedEncoderStatus::Finished {
                    return output;
                }
            }
        };
        assert_eq!(encode_session(), bytes);
        group.bench_function(
            BenchmarkId::new("rust-session-encode-container", name),
            |b| {
                b.iter(&mut encode_session);
            },
        );
        group.bench_function(BenchmarkId::new("c-encode-frames", name), |b| {
            b.iter(|| {
                input
                    .chunks(64 << 10)
                    .map(|chunk| {
                        openzl_ffi::compress(black_box(chunk), Default::default()).expect("C")
                    })
                    .collect::<Vec<_>>()
            });
        });
        group.bench_function(BenchmarkId::new("rust-decode-container", name), |b| {
            b.iter(|| decoder.decompress(black_box(&bytes)).expect("decode"));
        });
        let mut decode_session = || {
            let mut session = decoder.start();
            let mut at = 0;
            let mut output = Vec::new();
            loop {
                let end = (at + 1024).min(bytes.len());
                let op = if end == bytes.len() {
                    DecodeOperation::Finish
                } else {
                    DecodeOperation::Process
                };
                let p = session.process(&bytes[at..end], op).expect("decode");
                at += p.consumed;
                if let Some(resource) = p.resource {
                    output.push(resource);
                }
                if p.status == FramedDecoderStatus::Finished {
                    return output;
                }
            }
        };
        assert_eq!(decode_session(), decoded.resources);
        group.bench_function(
            BenchmarkId::new("rust-session-decode-container", name),
            |b| {
                b.iter(&mut decode_session);
            },
        );
        group.bench_function(BenchmarkId::new("c-decode-frames", name), |b| {
            b.iter(|| {
                frames
                    .iter()
                    .map(|frame| openzl_ffi::decompress(black_box(frame)).expect("C"))
                    .collect::<Vec<_>>()
            });
        });
    }
    group.finish();
}

criterion_group!(benches, framed);
criterion_main!(benches);
