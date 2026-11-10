#![cfg(feature = "compression")]
#![cfg(not(feature = "no_std"))]

//! One-stream interoperability, scheduler determinism, resource ownership and faults.
#[path = "parallel_support/mod.rs"]
mod support;
use mopenzl::compressor::parallel::*;
use mopenzl::{EncoderConfig, Level};
use std::{
    error::Error,
    hint::black_box,
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

fn config() -> ParallelConfig {
    // Keep the public API call observable to optimized coverage instrumentation.
    let with_retention = black_box(
        ParallelConfig::with_max_retained_workers as fn(ParallelConfig, usize) -> ParallelConfig,
    );
    with_retention(
        ParallelConfig::from(SegmentSize::try_from(64 << 10).unwrap())
            .with_minimum_parallel_size(0),
        4,
    )
}
fn compressor(q: u8) -> ParallelCompressor {
    ParallelCompressor::new(
        EncoderConfig::default().with_level(Level::try_from(i32::from(q)).unwrap()),
        config(),
    )
    .unwrap()
}
fn batch(n: usize) -> BatchConfig {
    BatchConfig::memory(TaskCount::try_from(n).unwrap(), 32 << 20)
}
fn data() -> Vec<u8> {
    let mut v =
        b"\xff\x00testing the compression dictionary: encyclopedia \xc3\xa9 abcdefghijklmnop "
            .repeat(3200);
    v.resize(3 * (64 << 10) + 1, 0);
    v
}

#[test]
fn generic_sources_accept_owned_shared_custom_conversion_and_erased_handles() {
    use std::io::Cursor;
    struct IntoSource(Vec<u8>);
    impl From<IntoSource> for Arc<SeekSource<Cursor<Vec<u8>>>> {
        fn from(value: IntoSource) -> Self {
            Arc::new(SeekSource::from(Cursor::new(value.0)))
        }
    }
    let input = data();
    let mut c = compressor(5);
    let expected = inline(&mut c, &input, 1);
    let source = SeekSource::from(Cursor::new(input.clone()));
    assert_eq!(source.len().unwrap(), input.len() as u64);
    assert!(source.identity().is_none());
    let mut b = c.prepare_source(source, batch(4)).unwrap();
    b.run_inline().unwrap();
    assert_eq!(b.finish_to_writer(Vec::new()).unwrap().0, expected);

    let mut b = c
        .prepare_source::<SeekSource<Cursor<Vec<u8>>>, _>(IntoSource(input.clone()), batch(4))
        .unwrap();
    b.run_inline().unwrap();
    assert_eq!(b.finish_to_writer(Vec::new()).unwrap().0, expected);

    let source = Arc::new(SeekSource::from(Cursor::new(input)));
    let mut b = c
        .prepare_source::<SeekSource<Cursor<Vec<u8>>>, _>(Arc::clone(&source), batch(4))
        .unwrap();
    let handles: Vec<_> = b
        .take_tasks()
        .unwrap()
        .into_iter()
        .rev()
        .map(|task| std::thread::spawn(move || task.run()))
        .collect();
    assert_eq!(b.finish_to_writer(Vec::new()).unwrap().0, expected);
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(Arc::strong_count(&source), 1);

    let erased: Arc<dyn RandomAccessSource> = source;
    let mut b = c
        .prepare_source::<dyn RandomAccessSource, _>(erased, batch(4))
        .unwrap();
    b.run_inline().unwrap();
    assert_eq!(b.finish_to_writer(Vec::new()).unwrap().0, expected);
    let empty = SeekSource::from(Cursor::new(Vec::<u8>::new()));
    assert!(empty.is_empty().unwrap());
    empty.read_exact_at(0, &mut []).unwrap();
    let mut b = c.prepare_source(empty, batch(2)).unwrap();
    b.run_inline().unwrap();
    assert_eq!(
        support::c_decompress(&b.finish_to_writer(Vec::new()).unwrap().0, 0).unwrap(),
        b""
    );
}

#[test]
fn seek_source_keeps_seek_and_short_reads_together_for_non_sync_readers() {
    use std::{
        cell::Cell,
        io::{Cursor, Read, Seek, SeekFrom},
    };
    struct Reader {
        cursor: Cursor<Vec<u8>>,
        reads: Cell<usize>,
    }
    impl Read for Reader {
        fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
            self.reads.set(self.reads.get() + 1);
            std::thread::yield_now();
            let len = dst.len().min(3);
            self.cursor.read(&mut dst[..len])
        }
    }
    impl Seek for Reader {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            let result = self.cursor.seek(pos);
            std::thread::yield_now();
            result
        }
    }
    let bytes: Vec<u8> = (0..4096).map(|n| (n % 251) as u8).collect();
    let mut cursor = Cursor::new(bytes.clone());
    cursor.set_position(13);
    let source = SeekSource::from(Reader {
        cursor,
        reads: Cell::new(0),
    });
    std::thread::scope(|scope| {
        for task in 0..8 {
            let source = &source;
            let bytes = &bytes;
            scope.spawn(move || {
                for i in 0..64 {
                    let offset = (task * 61 + i * 37) % (bytes.len() - 43);
                    assert_eq!(source.len().unwrap(), bytes.len() as u64);
                    let mut output = [0; 43];
                    source.read_exact_at(offset as u64, &mut output).unwrap();
                    assert_eq!(output, bytes[offset..offset + 43]);
                }
            });
        }
    });
}
fn inline(c: &mut ParallelCompressor, input: &[u8], n: usize) -> Vec<u8> {
    let mut b = c.prepare_slice(input, batch(n)).unwrap();
    b.run_inline().unwrap();
    let mut out = b"prefix".to_vec();
    let result = b.finish_into(&mut out).unwrap();
    assert_eq!(result.range, 6..out.len());
    assert_eq!(result.stats.output_bytes as usize, out.len() - 6);
    assert_eq!(result.stats.input_bytes as usize, input.len());
    out.split_off(6)
}
#[test]
fn empty_tiny_and_segment_edges_preserve_bytes_and_serial_fallback() {
    for q in 0..=11 {
        let mut c = compressor(q);
        for n in [0, 1, 2, 3, 4, 65535, 65536, 65537] {
            let input = vec![b'x'; n];
            let out = inline(&mut c, &input, 8);
            assert_eq!(support::c_decompress(&out, n).unwrap(), input);
        }
        c.reconfigure_parallel(ParallelConfig::default());
        let input = b"small serial fallback";
        let out = inline(&mut c, input, 8);
        let serial = mopenzl::Compressor::new(*c.encoder_config())
            .unwrap()
            .compress(input)
            .unwrap();
        assert_eq!(out, serial);
    }
}
#[test]
fn exact_staging_memory_bound_accepts_preparation_and_one_byte_less_fails() {
    for segment in [64 << 10, 256 << 10, 4 << 20] {
        let mut c = ParallelCompressor::new(
            EncoderConfig::default().with_level(Level::try_from(0).unwrap()),
            ParallelConfig::from(SegmentSize::try_from(segment).unwrap()),
        )
        .unwrap();
        for len in [0, 1, segment - 1, segment, segment + 1, 2 * segment + 1] {
            let input = vec![0; len];
            for count in [1, 3, 256] {
                let tasks = TaskCount::try_from(count).unwrap();
                let auto = black_box(BatchConfig::auto as fn(TaskCount) -> BatchConfig)(tasks);
                let estimate = c.estimate_source(len as u64, &auto).unwrap();
                let bound =
                    usize::try_from(estimate.maximum_staging_memory_bytes().unwrap()).unwrap();
                assert!(bound as u64 >= estimate.maximum_staged_bytes);
                assert!(
                    c.prepare_slice(&input, BatchConfig::memory(tasks, bound))
                        .is_ok()
                );
                assert!(matches!(
                    c.prepare_slice(&input, BatchConfig::memory(tasks, bound - 1)),
                    Err(ParallelEncodeError::MemoryStagingLimit)
                ));
                assert!(c.prepare_slice(&input, auto).is_ok());
                let directory = c
                    .estimate_source(len as u64, &BatchConfig::directory(tasks, "/tmp"))
                    .unwrap();
                assert_eq!(
                    directory.maximum_staging_memory_bytes().unwrap(),
                    bound as u64
                );
            }
        }
    }
}

#[test]
fn automatic_staging_matches_explicit_staging_for_slices_and_sources() {
    for input in [Vec::new(), b"serial fallback".to_vec(), data()] {
        let mut c =
            ParallelCompressor::new(EncoderConfig::default(), ParallelConfig::default()).unwrap();
        c.reconfigure_parallel(ParallelConfig::from(
            SegmentSize::try_from(64 << 10).unwrap(),
        ));
        let expected = inline(&mut c, &input, 1);
        let tasks = TaskCount::try_from(256).unwrap();
        let mut b = c.prepare_slice(&input, BatchConfig::auto(tasks)).unwrap();
        b.run_inline().unwrap();
        let (output, stats) = b.finish_to_writer(Vec::new()).unwrap();
        assert_eq!(output, expected);
        assert_eq!(stats.effective_tasks as u64, stats.segment_count.min(256));
        assert_eq!(stats.staging_kind, StagingKind::Memory);
        assert_eq!(support::c_decompress(&output, input.len()).unwrap(), input);
        let source = SeekSource::from(std::io::Cursor::new(input));
        let mut b = c.prepare_source(source, BatchConfig::auto(tasks)).unwrap();
        b.run_inline().unwrap();
        assert_eq!(b.finish_to_writer(Vec::new()).unwrap().0, expected);
    }
}

#[test]
fn automatic_staging_preserves_aggregate_limits_and_overflow_checks() {
    let mut c = compressor(0);
    let auto = BatchConfig::auto(TaskCount::ONE);
    let input = b"bounded auto";
    let estimate = c.estimate_source(input.len() as u64, &auto).unwrap();
    let aggregate = estimate.estimated_active_workspace_bytes
        + usize::try_from(estimate.maximum_staged_bytes).unwrap()
        + c.retained_bytes();
    c.reconfigure_parallel(config().with_aggregate_memory_limit(Some(aggregate)));
    assert!(c.prepare_slice(input, auto.clone()).is_ok());
    c.reconfigure_parallel(config().with_aggregate_memory_limit(Some(aggregate - 1)));
    assert!(matches!(
        c.prepare_slice(input, auto.clone()),
        Err(ParallelEncodeError::WorkerMemoryLimit)
    ));
    assert!(matches!(
        c.estimate_source(u64::MAX, &auto),
        Err(ParallelEncodeError::SizeOverflow)
    ));
    let mut malformed = estimate;
    malformed.maximum_staged_bytes = u64::MAX;
    assert!(matches!(
        malformed.maximum_staging_memory_bytes(),
        Err(ParallelEncodeError::SizeOverflow)
    ));
    malformed.maximum_staged_bytes = 0;
    malformed.segment_count = u64::MAX;
    assert!(matches!(
        malformed.maximum_staging_memory_bytes(),
        Err(ParallelEncodeError::SizeOverflow)
    ));
}

#[test]
fn reconfigure_updates_serial_fallback_and_planning() {
    let input = data();
    let mut c = compressor(0);
    c.reconfigure_parallel(config().with_minimum_parallel_size(u64::MAX));
    let old_estimate = c.estimate_source(input.len() as u64, &batch(2)).unwrap();
    let _ = inline(&mut c, &input[..100], 1);
    let encoder = EncoderConfig::default().with_level(Level::try_from(5).unwrap());
    c.reconfigure(encoder).unwrap();
    let estimate = c.estimate_source(input.len() as u64, &batch(2)).unwrap();
    assert!(
        estimate.estimated_active_workspace_bytes == old_estimate.estimated_active_workspace_bytes
    );
    let mut serial = mopenzl::Compressor::new(encoder).unwrap();
    for bytes in [&input[..100], &[]] {
        assert_eq!(inline(&mut c, bytes, 1), serial.compress(bytes).unwrap());
    }
}

#[test]
fn reconfigure_identical_settings_preserves_and_reuses_workers() {
    let input = data();
    let mut c = compressor(5);
    let original = inline(&mut c, &input, 3);
    let retained = c.retained_bytes();
    c.reconfigure(*c.encoder_config()).unwrap();
    assert_eq!(c.retained_worker_count(), 3);
    assert_eq!(c.retained_bytes(), retained);
    let mut b = c.prepare_slice(&input, batch(3)).unwrap();
    b.run_inline().unwrap();
    let mut output = Vec::new();
    let result = b.finish_into(&mut output).unwrap();
    assert_eq!(result.stats.workers_reused, 3);
    assert_eq!(result.stats.workers_created, 0);
    assert_eq!(output, original);
}

#[test]
fn configuration_bounds_and_retention_are_explicit() {
    assert!(SegmentSize::try_from(0).is_err());
    assert!(SegmentSize::try_from(65535).is_err());
    assert!(SegmentSize::try_from((16 << 20) + 1).is_err());
    assert_eq!(SegmentSize::default().get(), 4 << 20);
    assert_eq!(TaskCount::ONE.get(), 1);
    assert!(TaskCount::try_from(0).is_err());
    assert!(TaskCount::try_from(usize::MAX).is_err());
    assert!(TaskCount::available().unwrap().get() > 0);
    let mut c = compressor(5);
    assert_eq!(c.retained_worker_count(), 0);
    assert_eq!(c.retained_bytes(), 0);
    let e = c
        .estimate_source(
            (1u64 << 40) + 1,
            &BatchConfig::directory(TaskCount::ONE, "/tmp"),
        )
        .unwrap();
    assert!(e.segment_count > 1);
    assert!(e.maximum_final_bytes >= e.input_bytes);
    assert!(e.estimated_active_workspace_bytes < 2usize.pow(35));
    assert!(c.estimate_source(u64::MAX, &batch(1)).is_err());
    assert!(matches!(
        c.prepare_slice(&data(), BatchConfig::memory(TaskCount::ONE, 1)),
        Err(ParallelEncodeError::MemoryStagingLimit)
    ));
    c.reconfigure_parallel(config().with_aggregate_memory_limit(Some(1)));
    assert!(matches!(
        c.estimate_source(65536, &batch(1)),
        Err(ParallelEncodeError::WorkerMemoryLimit)
    ));
    c.reconfigure_parallel(config());
    let _ = inline(&mut c, &data(), 3);
    assert_eq!(c.retained_worker_count(), 3);
    assert!(c.retained_bytes() > 0);
    c.trim(ParallelRetentionPolicy::Aggressive);
    c.trim(ParallelRetentionPolicy::Bounded { max_bytes: 0 });
    assert_eq!(c.retained_bytes(), 0);
    let _ = inline(&mut c, &data(), 1);
    c.trim(ParallelRetentionPolicy::ReleaseAll);
    assert_eq!(c.retained_bytes(), 0);
    assert!(format!("{c:?}").contains("ParallelCompressor"));
    assert_eq!(c.parallel_config(), &config());
}
#[test]
fn files_directory_spools_and_file_writer_match_memory_and_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("input");
    let dest = dir.path().join("output");
    let input = data();
    std::fs::write(&source, &input).unwrap();
    let mut c = compressor(5);
    let expected = inline(&mut c, &input, 2);
    assert!(FileSource::open(dir.path()).is_err());
    let file = FileSource::try_from(std::fs::File::open(&source).unwrap()).unwrap();
    let mut b = c
        .prepare_source(
            file,
            BatchConfig::directory(TaskCount::try_from(3).unwrap(), dir.path()),
        )
        .unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
    for task in b.take_tasks().unwrap().into_iter().rev() {
        std::thread::spawn(move || task.run());
    }
    let (file, stats) = b
        .finish_to_writer(std::fs::File::create(&dest).unwrap())
        .unwrap();
    file.sync_all().unwrap();
    assert_eq!(stats.output_bytes as usize, expected.len());
    assert_eq!(std::fs::read(&dest).unwrap(), expected);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    let b = c
        .prepare_source(
            FileSource::open(&source).unwrap(),
            BatchConfig::directory(TaskCount::ONE, dir.path()),
        )
        .unwrap();
    drop(b);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    assert!(
        c.prepare_slice(
            &input,
            BatchConfig::directory(TaskCount::ONE, dir.path().join("missing"))
        )
        .is_err()
    );
    let mut b = c
        .prepare_source(FileSource::open(&source).unwrap(), batch(1))
        .unwrap();
    b.run_inline().unwrap();
    std::fs::write(&source, vec![1; input.len()]).unwrap();
    assert!(matches!(
        b.finish_into(&mut Vec::new()),
        Err(ParallelEncodeError::SourceChanged)
    ));
}

#[derive(Debug)]
struct Sink {
    bytes: Vec<u8>,
    fail_at: usize,
    interrupted: bool,
    zero: bool,
}
impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.interrupted {
            self.interrupted = true;
            return Err(io::ErrorKind::Interrupted.into());
        }
        if self.bytes.len() == self.fail_at {
            return if self.zero {
                Ok(0)
            } else {
                Err(io::ErrorKind::BrokenPipe.into())
            };
        }
        let n = bytes.len().min(7).min(self.fail_at - self.bytes.len());
        self.bytes.extend_from_slice(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("finish must not flush")
    }
}
#[test]
fn writer_failures_preserve_ownership_and_exact_accepted_prefix() {
    let input = vec![b'a'; 65537];
    let mut c = compressor(1);
    let expected = inline(&mut c, &input, 2);
    let positions: Vec<_> = if cfg!(feature = "zstd") {
        (0..expected.len()).collect()
    } else {
        vec![0, 1, 7, expected.len() / 2, expected.len() - 1]
    };
    for at in positions {
        let mut b = c.prepare_slice(&input, batch(2)).unwrap();
        b.run_inline().unwrap();
        let error = b
            .finish_to_writer(Sink {
                bytes: Vec::new(),
                fail_at: at,
                interrupted: false,
                zero: at % 2 == 0,
            })
            .unwrap_err();
        assert_eq!(error.bytes_written, at as u64);
        assert_eq!(error.writer.bytes, expected[..at]);
        assert!(error.source().is_some());
        assert!(error.to_string().contains("assembly"));
    }
    let mut b = c.prepare_slice(&input, batch(2)).unwrap();
    b.run_inline().unwrap();
    let (sink, _) = b
        .finish_to_writer(Sink {
            bytes: Vec::new(),
            fail_at: usize::MAX,
            interrupted: false,
            zero: false,
        })
        .unwrap();
    assert_eq!(sink.bytes, expected);
}

struct FaultSource {
    length: AtomicU64,
    panic: bool,
    fail: bool,
}
impl RandomAccessSource for FaultSource {
    fn len(&self) -> io::Result<u64> {
        Ok(self.length.load(Ordering::Relaxed))
    }
    fn read_exact_at(&self, _: u64, dst: &mut [u8]) -> io::Result<()> {
        assert!(!self.panic, "injected source panic");
        if self.fail {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        dst.fill(1);
        Ok(())
    }
}
#[test]
fn source_errors_panics_and_length_changes_never_mutate_destination() {
    let mut c = compressor(5);
    for panic in [false, true] {
        let source = Arc::new(FaultSource {
            length: AtomicU64::new(65537),
            panic,
            fail: !panic,
        });
        let mut b = c
            .prepare_source::<FaultSource, _>(source, batch(2))
            .unwrap();
        for t in b.take_tasks().unwrap() {
            t.run();
        }
        let error = b.finish_to_writer(Vec::new()).unwrap_err();
        assert!(error.writer.is_empty());
        assert_eq!(error.bytes_written, 0);
        assert!(
            error
                .to_string()
                .contains(if panic { "panicked" } else { "source read" })
        );
    }
    for policy in [
        SourceConsistency::VerifyLength,
        SourceConsistency::AssumeImmutable,
    ] {
        let with_consistency = black_box(
            ParallelConfig::with_source_consistency
                as fn(ParallelConfig, SourceConsistency) -> ParallelConfig,
        );
        c.reconfigure_parallel(with_consistency(config(), policy));
        let source = Arc::new(FaultSource {
            length: AtomicU64::new(65537),
            panic: false,
            fail: false,
        });
        let mut b = c
            .prepare_source::<FaultSource, _>(source.clone(), batch(2))
            .unwrap();
        b.run_inline().unwrap();
        source.length.store(1, Ordering::Relaxed);
        let result = b.finish_into(&mut Vec::new());
        assert_eq!(result.is_ok(), policy == SourceConsistency::AssumeImmutable);
    }
    let source = ArcBytesSource::from(Arc::<[u8]>::from(b"abc".as_slice()));
    assert_eq!(source.as_ref(), b"abc");
    assert_eq!(source.len().unwrap(), 3);
    assert!(!source.is_empty().unwrap());
    assert!(source.read_exact_at(u64::MAX, &mut [0; 1]).is_err());
    assert!(source.read_exact_at(2, &mut [0; 2]).is_err());
    assert_eq!(SourceIdentity::from(vec![1]), SourceIdentity::from(vec![1]));
}

#[test]
fn extraction_timeout_abandonment_cancellation_and_parent_reuse() {
    let mut c = compressor(5);
    let input = data();
    let expected = inline(&mut c, &input, 1);
    let mut b = c.prepare_slice(&input, batch(3)).unwrap();
    assert!(matches!(b.wait(), Err(ParallelEncodeError::NotReady)));
    assert!(matches!(
        b.poll().unwrap(),
        BatchPoll::Pending {
            completed: 0,
            total: 3
        }
    ));
    let jobs = b.take_tasks().unwrap();
    assert!(b.take_tasks().is_err());
    assert_eq!(
        b.wait_timeout(Duration::ZERO).unwrap(),
        WaitStatus::TimedOut
    );
    assert!(format!("{b:?} {:?}", jobs[0]).contains("ScopedParallelTask"));
    // Exercise the conversions at runtime even in optimized coverage builds.
    let task_number = black_box(u32::from as fn(TaskId) -> u32);
    let segment_number = black_box(u64::from as fn(SegmentId) -> u64);
    assert_eq!(task_number(jobs[0].id()), 0);
    assert_eq!(segment_number(jobs[0].segment_range().start), 0);
    drop(jobs);
    let error = b.wait().unwrap_err();
    assert!(error.to_string().contains("abandoned"));
    assert!(error.source().is_some());
    let mut dst = b"unchanged".to_vec();
    assert!(b.finish_into(&mut dst).is_err());
    assert_eq!(dst, b"unchanged");
    assert_eq!(inline(&mut c, &input, 3), expected);
    let mut b = c.prepare_slice(&input, batch(3)).unwrap();
    b.cancel();
    b.cancel();
    assert!(b.run_inline().is_err());
    drop(b);
    let mut b = c.prepare_slice(&input, batch(3)).unwrap();
    let jobs = b.take_tasks().unwrap();
    drop(b);
    assert_eq!(inline(&mut c, &input, 1), expected);
    for job in jobs {
        job.run();
    }
}

#[test]
fn task_order_versions_checksums_chunking_and_backends_are_openzl_compatible() {
    use mopenzl::{Backend, ChunkSize, Chunking, FormatVersion};
    let inputs = [data(), support::Rng::new(73).bytes(2 * (64 << 10) + 7, 256)];
    for backend in Backend::available() {
        for version in 21..=27 {
            for checks in 0..4 {
                let encoder = EncoderConfig::default()
                    .with_level(Level::try_from(if checks % 2 == 0 { -5 } else { 9 }).unwrap())
                    .with_format_version(FormatVersion::try_from(version).unwrap())
                    .with_chunking(Chunking::Fixed(ChunkSize::MIN))
                    .with_content_checksum(checks & 1 != 0)
                    .with_compressed_checksum(checks & 2 != 0);
                let mut c = ParallelCompressor::with_backend(encoder, config(), backend).unwrap();
                for input in &inputs {
                    let expected = inline(&mut c, input, 1);
                    let mut b = c.prepare_slice(input, batch(4)).unwrap();
                    std::thread::scope(|scope| {
                        for task in b.take_tasks().unwrap().into_iter().rev() {
                            scope.spawn(move || task.run());
                        }
                    });
                    let (actual, stats) = b.finish_to_writer(Vec::new()).unwrap();
                    assert_eq!(actual, expected);
                    assert_eq!(stats.context_prefix_bytes, 0);
                    assert_eq!(openzl_ffi::decompress(&actual).unwrap(), *input);
                }
            }
        }
    }
}

#[test]
fn nondefault_encoder_parameters_remain_openzl_compatible() {
    let input = data();
    for encoder in [
        EncoderConfig::default().with_decompression_level(1),
        EncoderConfig::default().with_min_stream_size(0),
        EncoderConfig::default().with_store_on_expansion(false),
        EncoderConfig::default().with_permissive(true),
    ] {
        let mut c = ParallelCompressor::new(encoder, config()).unwrap();
        let output = inline(&mut c, &input, 3);
        assert_eq!(openzl_ffi::decompress(&output).unwrap(), input);
    }
}
