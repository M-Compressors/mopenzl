//! mopenzl's decoder against OpenZL v0.3.0's `ZL_decompress`.
//!
//! Every case decodes the same reference frame with both implementations
//! after checking that both regenerate the input. The C side sizes its
//! output from the frame header, as `ZL_decompress` callers do; the Rust
//! side decodes into a reused slice. A streaming group drives a
//! `DecoderSession` with 64 KiB pieces of a segmented frame.

mod support;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mopenzl::{DecodeOperation, DecoderStatus, Decompressor};
use std::hint::black_box;

fn decompress(c: &mut Criterion) {
    let mut group = c.benchmark_group("decompress");
    for (name, input) in support::corpora() {
        for level in [1, 6] {
            let params = openzl_ffi::Params {
                level,
                ..Default::default()
            };
            let frame = openzl_ffi::compress(&input, params).expect("reference frame");
            let mut decoder = Decompressor::default();
            assert_eq!(decoder.decompress(&frame).expect("rust"), input);
            assert_eq!(openzl_ffi::decompress(&frame).expect("c"), input);
            group.throughput(Throughput::Bytes(input.len() as u64));
            let mut out = vec![0u8; input.len()];
            let id = format!("{name}/L{level}");
            group.bench_function(BenchmarkId::new("rust", &id), |b| {
                b.iter(|| {
                    decoder
                        .decompress_to_slice(black_box(&frame), &mut out)
                        .expect("rust")
                });
            });
            let mut spare: Vec<u8> = Vec::with_capacity(input.len());
            group.bench_function(BenchmarkId::new("rust-uninit", &id), |b| {
                b.iter(|| {
                    decoder
                        .decompress_to_uninit(black_box(&frame), spare.spare_capacity_mut())
                        .expect("rust")
                });
            });
            group.bench_function(BenchmarkId::new("rust-vec", &id), |b| {
                b.iter(|| decoder.decompress(black_box(&frame)).expect("rust"));
            });
            group.bench_function(BenchmarkId::new("c", &id), |b| {
                b.iter(|| openzl_ffi::decompress(black_box(&frame)).expect("c"));
            });
        }
    }
    group.finish();
}

fn stream(c: &mut Criterion) {
    let mut group = c.benchmark_group("decompress-stream");
    for (name, input) in support::corpora() {
        let params = openzl_ffi::Params {
            chunk_size: 64 << 10,
            ..Default::default()
        };
        let frame = openzl_ffi::compress(&input, params).expect("reference frame");
        let mut decoder = Decompressor::default();
        let mut buffer = vec![0u8; 1 << 17];
        let mut run = |decoder: &mut Decompressor| {
            let mut content = Vec::with_capacity(input.len());
            let mut session = decoder.start();
            let mut pieces = frame.chunks(64 << 10).peekable();
            let mut piece: &[u8] = pieces.next().unwrap_or_default();
            loop {
                let operation = if pieces.peek().is_none() {
                    DecodeOperation::Finish
                } else {
                    DecodeOperation::Process
                };
                let progress = session
                    .process(piece, &mut buffer, operation)
                    .expect("decode");
                piece = &piece[progress.consumed..];
                content.extend_from_slice(&buffer[..progress.produced]);
                match progress.status {
                    DecoderStatus::Finished => return content,
                    DecoderStatus::NeedsInput if piece.is_empty() => {
                        piece = pieces.next().unwrap_or_default();
                    }
                    _ => {}
                }
            }
        };
        assert_eq!(run(&mut decoder), input);
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_function(BenchmarkId::new("rust-session", name), |b| {
            b.iter(|| run(&mut decoder));
        });
    }
    group.finish();
}

criterion_group!(benches, decompress, stream);
criterion_main!(benches);
