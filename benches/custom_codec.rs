//! End-to-end custom XOR callback overhead against the same C transform.
mod support;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mopenzl::custom_codec::{
    CodecError, CodecInput, CustomCodec, DecoderContext, EncoderContext, Signature,
};
use mopenzl::graph::{GraphBuilder, StandardGraph};
use mopenzl::{Compressor, DataType, Decompressor, EncoderConfig};
use std::hint::black_box;

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
        let [key] = cx.header() else {
            return Err(CodecError::Invalid("XOR header"));
        };
        let bytes: Vec<_> = inputs[0].bytes.iter().map(|b| b ^ key).collect();
        cx.emit(0, 1, &bytes)
    }
}

fn custom_codec(c: &mut Criterion) {
    let mut group = c.benchmark_group("custom-codec-end-to-end");
    let mut corpora = support::corpora();
    corpora.push(("zeros-1MiB", vec![0; 1 << 20]));
    for (name, input) in corpora {
        let mut builder = GraphBuilder::new();
        let node = builder.custom_codec(1000, Xor).unwrap();
        let start = builder
            .static_graph(node, &[StandardGraph::Store.into()])
            .unwrap();
        let graph = builder.build(start).unwrap();
        let config = EncoderConfig::default()
            .with_min_stream_size(1)
            .with_store_on_expansion(false);
        let mut encoder = Compressor::builder(config)
            .with_graph(graph.clone())
            .build()
            .unwrap();
        let decoder_builder = Decompressor::builder(Default::default())
            .with_custom_codec(1000, Xor)
            .unwrap();
        let frame = encoder.compress(&input).unwrap();
        let capacity = input.len() + 1024;
        assert_eq!(
            frame,
            openzl_ffi::custom_codec::transform(false, 1000, 27, &input, capacity).unwrap()
        );
        assert_eq!(
            decoder_builder.clone().build().decompress(&frame).unwrap(),
            input
        );
        assert_eq!(
            openzl_ffi::custom_codec::transform(true, 1000, 27, &frame, input.len()).unwrap(),
            input
        );
        eprintln!(
            "custom XOR {name}: {} -> {} bytes",
            input.len(),
            frame.len()
        );
        group.throughput(Throughput::Bytes(input.len() as u64));
        // Both sides deliberately include context construction and output allocation.
        group.bench_function(BenchmarkId::new("rust-encode", name), |b| {
            b.iter(|| {
                Compressor::builder(config)
                    .with_graph(graph.clone())
                    .build()
                    .unwrap()
                    .compress(black_box(&input))
                    .unwrap()
            })
        });
        group.bench_function(BenchmarkId::new("c-encode", name), |b| {
            b.iter(|| {
                openzl_ffi::custom_codec::transform(false, 1000, 27, black_box(&input), capacity)
                    .unwrap()
            })
        });
        group.bench_function(BenchmarkId::new("rust-decode", name), |b| {
            b.iter(|| {
                decoder_builder
                    .clone()
                    .build()
                    .decompress(black_box(&frame))
                    .unwrap()
            })
        });
        group.bench_function(BenchmarkId::new("c-decode", name), |b| {
            b.iter(|| {
                openzl_ffi::custom_codec::transform(true, 1000, 27, black_box(&frame), input.len())
                    .unwrap()
            })
        });
    }
    group.finish();
}
criterion_group!(benches, custom_codec);
criterion_main!(benches);
