//! mopenzl's encoder against OpenZL v0.3.0's `ZL_CCtx_compress`.
//!
//! Each case compresses the same bytes with the same global parameters,
//! after checking that both write identical frames, so every speed figure is
//! read against an identical ratio (printed before timing). The C side
//! includes its context creation, as `ZL_compress`-style callers pay it; the
//! Rust side reuses its compressor, as a long-lived caller would. A
//! streaming group drives an `EncoderSession` with 64 KiB pieces.

mod support;

use hotpath::criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mopenzl::{
    ChunkSize, Chunking, Compressor, EncoderConfig, EncoderStatus, InputSize, Level, Operation,
};
use std::hint::black_box;

const LEVELS: [i32; 4] = [1, 3, 6, 12];

fn config(level: i32) -> EncoderConfig {
    EncoderConfig::from(Level::try_from(level).expect("level"))
}

fn params(level: i32) -> openzl_ffi::Params {
    openzl_ffi::Params {
        level,
        ..Default::default()
    }
}

fn compress(c: &mut Criterion) {
    let mut group = c.benchmark_group("compress");
    for (name, input) in support::corpora() {
        group.throughput(Throughput::Bytes(input.len() as u64));
        for level in LEVELS {
            let mut encoder = Compressor::new(config(level)).expect("config");
            let mut out = vec![0u8; Compressor::max_compressed_size(input.len()).expect("bound")];
            let rust = encoder.compress(&input).expect("rust");
            let c_frame = openzl_ffi::compress(&input, params(level)).expect("c");
            assert_eq!(rust, c_frame, "{name} level {level}");
            println!(
                "{name} level {level}: {} -> {} bytes",
                input.len(),
                rust.len()
            );
            let id = format!("{name}/L{level}");
            group.bench_function(BenchmarkId::new("rust", &id), |b| {
                b.iter(|| {
                    encoder
                        .compress_to_slice(black_box(&input), &mut out)
                        .expect("rust")
                });
            });
            let mut spare: Vec<u8> = Vec::with_capacity(out.len());
            group.bench_function(BenchmarkId::new("rust-uninit", &id), |b| {
                b.iter(|| {
                    encoder
                        .compress_to_uninit(black_box(&input), spare.spare_capacity_mut())
                        .expect("rust")
                });
            });
            group.bench_function(BenchmarkId::new("c", &id), |b| {
                b.iter(|| openzl_ffi::compress(black_box(&input), params(level)).expect("c"));
            });
        }
    }
    group.finish();
}

fn stream(c: &mut Criterion) {
    let mut group = c.benchmark_group("compress-stream");
    let config = EncoderConfig::default().with_chunking(Chunking::Fixed(ChunkSize::MIN));
    for (name, input) in support::corpora() {
        group.throughput(Throughput::Bytes(input.len() as u64));
        let mut encoder = Compressor::new(config).expect("config");
        let expected = encoder.compress(&input).expect("one-shot");
        let mut buffer = vec![0u8; 1 << 17];
        let mut run = |encoder: &mut Compressor| {
            let mut frame = Vec::with_capacity(expected.len());
            let mut session = encoder.start(InputSize::Exact(input.len() as u64).into());
            for piece in input.chunks(64 << 10) {
                let mut piece = piece;
                while !piece.is_empty() {
                    let progress = session
                        .process(piece, &mut buffer, Operation::Continue)
                        .expect("process");
                    piece = &piece[progress.consumed..];
                    frame.extend_from_slice(&buffer[..progress.produced]);
                }
            }
            loop {
                let progress = session.finish(&mut buffer).expect("finish");
                frame.extend_from_slice(&buffer[..progress.produced]);
                if progress.status == EncoderStatus::Finished {
                    return frame;
                }
            }
        };
        assert_eq!(run(&mut encoder), expected);
        group.bench_function(BenchmarkId::new("rust-session", name), |b| {
            b.iter(|| run(&mut encoder));
        });
    }
    group.finish();
}

criterion_group!(benches, compress, stream);
criterion_main!(benches);
