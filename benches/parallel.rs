#[cfg(not(feature = "no_std"))]
mod support;
#[cfg(not(feature = "no_std"))]
mod enabled {
    //! End-to-end caller-scheduled OpenZL compression versus the C serial segmenter.
    use super::support;
    use hotpath::criterion::{BenchmarkId, Criterion, Throughput, criterion_group};
    use mopenzl::compressor::parallel::{
        BatchConfig, ParallelCompressor, ParallelConfig, SegmentSize, TaskCount,
    };
    use mopenzl::{EncoderConfig, Level};
    use std::hint::black_box;

    const SEGMENT: usize = 256 << 10;
    fn run(c: &mut ParallelCompressor, input: &[u8], tasks: usize) -> Vec<u8> {
        let mut batch = c
            .prepare_slice(
                input,
                BatchConfig::auto(TaskCount::try_from(tasks).unwrap()),
            )
            .unwrap();
        let jobs = batch.take_tasks().unwrap();
        if tasks == 1 {
            for task in jobs {
                task.run();
            }
        } else {
            std::thread::scope(|scope| {
                for task in jobs {
                    scope.spawn(move || task.run());
                }
            });
        }
        batch.finish_to_writer(Vec::new()).unwrap().0
    }
    fn stream(c: &mut ParallelCompressor, input: &[u8]) -> Vec<u8> {
        use std::io::Write;
        std::thread::scope(|scope| {
            let (mut writer, queue) = c
                .writer(Vec::new(), TaskCount::try_from(4).unwrap())
                .unwrap();
            for _ in 0..4 {
                let queue = queue.clone();
                scope.spawn(move || queue.into_iter().for_each(|task| task.run()));
            }
            drop(queue);
            writer.write_all(input).unwrap();
            writer.finish().unwrap()
        })
    }
    fn parallel(c: &mut Criterion) {
        let mut cases = support::corpora();
        cases.push(("zeros-4MiB", vec![0; 4 << 20]));
        let mut group = c.benchmark_group("parallel");
        group.sample_size(10);
        group.warm_up_time(std::time::Duration::from_millis(200));
        group.measurement_time(std::time::Duration::from_secs(1));
        for (name, input) in cases {
            group.throughput(Throughput::Bytes(input.len() as u64));
            let encoder = EncoderConfig::from(Level::try_from(3).unwrap());
            let params = openzl_ffi::Params {
                level: 3,
                chunk_size: SEGMENT,
                ..Default::default()
            };
            let mut compressor = ParallelCompressor::new(
                encoder,
                ParallelConfig::from(SegmentSize::try_from(SEGMENT).unwrap())
                    .with_minimum_parallel_size(0)
                    .with_max_retained_workers(4),
            )
            .unwrap();
            let expected = run(&mut compressor, &input, 1);
            let reference = openzl_ffi::compress(&input, params).unwrap();
            assert_eq!(openzl_ffi::decompress(&expected).unwrap(), input);
            assert_eq!(openzl_ffi::decompress(&reference).unwrap(), input);
            println!(
                "{name}: input={} rust={} C={}",
                input.len(),
                expected.len(),
                reference.len()
            );
            for tasks in [1, 2, 4] {
                assert_eq!(run(&mut compressor, &input, tasks), expected);
                group.bench_function(BenchmarkId::new(format!("rust-{tasks}"), name), |b| {
                    b.iter(|| black_box(run(&mut compressor, black_box(&input), tasks)));
                });
            }
            assert_eq!(stream(&mut compressor, &input), expected);
            group.bench_function(BenchmarkId::new("rust-stream-4", name), |b| {
                b.iter(|| black_box(stream(&mut compressor, black_box(&input))));
            });
            group.bench_function(BenchmarkId::new("c-serial", name), |b| {
                b.iter(|| black_box(openzl_ffi::compress(black_box(&input), params).unwrap()));
            });
        }
        group.finish();
    }
    fn graphs(c: &mut Criterion) {
        use mopenzl::graph::{StandardGraph, params};
        use openzl_ffi::graph::{self, Graph, Input, Params, cparam};
        let mut cases = support::corpora();
        cases.push(("zeros-4MiB", vec![0; 4 << 20]));
        let mut group = c.benchmark_group("parallel-graphs");
        group.sample_size(10);
        group.warm_up_time(std::time::Duration::from_millis(200));
        group.measurement_time(std::time::Duration::from_secs(1));
        for selected in [
            StandardGraph::Store,
            StandardGraph::Zstd,
            StandardGraph::Lz4,
            StandardGraph::Huffman,
        ] {
            if match selected {
                StandardGraph::Zstd => !cfg!(feature = "zstd"),
                StandardGraph::Lz4 => !cfg!(feature = "lz4"),
                StandardGraph::Huffman => !cfg!(feature = "entropy"),
                _ => false,
            } {
                continue;
            }
            let c_graph = Graph::Parameterized {
                graph: StandardGraph::SegmentSerial as u32,
                params: Params {
                    ints: vec![(params::segmenters::SERIAL_CHUNK_BYTE_SIZE, SEGMENT as i32)],
                    copies: vec![],
                },
                custom: vec![Graph::Id(selected as u32)],
            };
            let globals = [
                (cparam::LEVEL, 3),
                (cparam::MIN_STREAM_SIZE, 1),
                (cparam::STORE_ON_EXPANSION, 2),
            ];
            let mut compressor = ParallelCompressor::builder(
                EncoderConfig::from(Level::try_from(3).unwrap())
                    .with_min_stream_size(1)
                    .with_store_on_expansion(false),
                ParallelConfig::from(SegmentSize::try_from(SEGMENT).unwrap())
                    .with_minimum_parallel_size(0)
                    .with_max_retained_workers(4),
            )
            .with_graph(selected.into())
            .build()
            .unwrap();
            for (name, input) in &cases {
                group.throughput(Throughput::Bytes(input.len() as u64));
                let id = format!("{selected:?}/{name}");
                let expected = run(&mut compressor, input, 4);
                let reference =
                    graph::compress(&[Input::Serial(input)], &c_graph, &globals).unwrap();
                assert_eq!(openzl_ffi::decompress(&expected).unwrap(), *input);
                assert_eq!(openzl_ffi::decompress(&reference).unwrap(), *input);
                println!(
                    "{id}: input={} rust={} C={}",
                    input.len(),
                    expected.len(),
                    reference.len()
                );
                group.bench_function(BenchmarkId::new("rust-4", &id), |b| {
                    b.iter(|| black_box(run(&mut compressor, black_box(input), 4)));
                });
                group.bench_function(BenchmarkId::new("c-serial", &id), |b| {
                    b.iter(|| {
                        black_box(
                            graph::compress(&[Input::Serial(black_box(input))], &c_graph, &globals)
                                .unwrap(),
                        )
                    });
                });
            }
        }
        group.finish();
    }
    criterion_group!(benches, parallel, graphs);

    pub fn main() {
        benches();
    }
}
#[cfg(feature = "no_std")]
fn main() {}
#[cfg(not(feature = "no_std"))]
fn main() {
    enabled::main();
}
