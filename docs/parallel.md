# Parallel compression

`mopenzl::compressor::parallel` divides a known input into fixed independent
segments and assembles them into one ordinary OpenZL frame. It uses the
same encoder configuration, graphs and codec registry as serial compression.
Typed/multiple top-level inputs and frame comments are not exposed here.

The caller runs the tasks using scoped threads, Rayon, or another executor.
The library does not create a thread pool.

## Compressing a slice

```rust
use mopenzl::compressor::parallel::{
    BatchConfig, ParallelCompressor, ParallelConfig, TaskCount,
};
use mopenzl::{Decompressor, EncoderConfig, Level};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = vec![b'a'; 8 << 20];
    let mut encoder = ParallelCompressor::new(
        EncoderConfig::default().with_level(Level::try_from(5)?),
        ParallelConfig::default(),
    )?;
    let mut batch = encoder.prepare_slice(
        &input,
        BatchConfig::auto(TaskCount::try_from(4)?),
    )?;
    let tasks = batch.take_tasks()?;
    std::thread::scope(|scope| {
        for task in tasks {
            scope.spawn(move || task.run());
        }
    });
    let mut output = Vec::new();
    batch.finish_into(&mut output)?;
    let mut decoder = Decompressor::new(Default::default());
    assert_eq!(decoder.decompress(&output)?, input);
    Ok(())
}
```

`take_tasks` transfers each task once. Every taken task must run or be dropped
so its completion can be reported. Finalization collects task results, validates
artifacts, and assembles them in input order. A blocking wait requires the
caller's executor to remain runnable.

## Running tasks with Rayon

Add `rayon = "1.12"` alongside `mopenzl` in your dependencies. Create a pool
once and reuse it across requests. Task completion order does not affect assembly
order or the resulting bytes for fixed segment settings.

```rust
use mopenzl::compressor::parallel::{
    BatchConfig, ParallelCompressor, ParallelConfig, TaskCount,
};
use mopenzl::{Decompressor, EncoderConfig, Level};
use rayon::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let mut encoder = ParallelCompressor::new(
        EncoderConfig::default().with_level(Level::try_from(5)?),
        ParallelConfig::default(),
    )?;
    let input = b"parallel compression with a reusable pool\n".repeat(220_000);
    let mut batch = encoder.prepare_slice(
        &input,
        BatchConfig::auto(TaskCount::try_from(4)?),
    )?;
    let tasks = batch.take_tasks()?;
    pool.install(|| tasks.into_par_iter().for_each(|task| task.run()));

    let mut compressed = Vec::new();
    batch.finish_into(&mut compressed)?;
    let mut decoder = Decompressor::new(Default::default());
    assert_eq!(decoder.decompress(&compressed)?, input);
    Ok(())
}
```

For many unrelated small inputs, another option is one ordinary `Compressor` per
worker. That preserves serial compression decisions for each input. The batch
API above splits **one** input into a single assembled stream.

## Streaming input

When input arrives in chunks, `start(window)` returns a session and a task
queue. Each full segment the session accepts becomes a task on that queue;
clone the queue for each executor and run `for task in queue { task.run() }`.
OpenZL requires the total size in its frame header, so the session stages
compressed segments until End establishes that size. It then assembles results
in input order. The queue closes when the session finishes, fails, or is dropped.

```rust
use mopenzl::compressor::parallel::{
    ParallelCompressor, ParallelConfig, ParallelEncoderStatus, TaskCount,
};
use mopenzl::{EncoderConfig, Operation, Level};
use std::io::{Read, Write};

fn compress(
    mut reader: impl Read,
    mut writer: impl Write,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = ParallelCompressor::new(
        EncoderConfig::default().with_level(Level::try_from(5)?),
        ParallelConfig::default(),
    )?;
    std::thread::scope(|scope| {
        let (mut session, queue) = encoder.start(TaskCount::try_from(4)?)?;
        for _ in 0..4 {
            let queue = queue.clone();
            scope.spawn(move || queue.into_iter().for_each(|task| task.run()));
        }
        drop(queue);
        let mut chunk = vec![0; 1 << 16];
        let mut output = vec![0; 1 << 16];
        loop {
            let read = reader.read(&mut chunk)?;
            let operation = if read == 0 { Operation::End } else { Operation::Continue };
            let mut input = &chunk[..read];
            loop {
                let progress = session.process(input, &mut output, operation)?;
                input = &input[progress.consumed..];
                writer.write_all(&output[..progress.produced])?;
                match progress.status {
                    ParallelEncoderStatus::Finished => return Ok(()),
                    ParallelEncoderStatus::Pending => session.wait()?,
                    ParallelEncoderStatus::NeedsInput if input.is_empty() => break,
                    _ => {}
                }
            }
        }
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"incremental parallel input\n".repeat(350_000);
    let mut compressed = Vec::new();
    compress(input.as_slice(), &mut compressed)?;
    let mut decoder = mopenzl::Decompressor::new(Default::default());
    assert_eq!(decoder.decompress(&compressed)?, input);
    Ok(())
}
```

- `process` never blocks. `Pending` means the window is full or a flush or
  finish waits for queued tasks; call `wait`, which blocks until one completes.
- `window` bounds the segments queued, running or awaiting delivery, and therefore active work. Compressed results accumulate until End; the
  optional aggregate memory estimate also accounts for buffered results.
- Without a flush, the output is byte-identical to `prepare_slice` over the same
  bytes, however the input was chunked. `Operation::Flush` ends the current
  segment early, which moves later segment boundaries.
- On a single thread, run `queue.try_recv()` tasks before `wait`; nothing else
  will. Dropping every queue handle fails the stream instead of hanging.
- `into_session(window)` returns an owned session without a lifetime;
  `into_compressor` returns the compressor.

## Reader and writer adapters

`writer(sink, window)` and `reader(source, window)` wrap the same session in
`std::io::Write` and `std::io::Read` and return the task queue beside the
adapter, so the loop above becomes `std::io::copy`:

```rust
use mopenzl::compressor::parallel::{ParallelCompressor, ParallelConfig, TaskCount};
use mopenzl::io::FinishError;
use mopenzl::{EncoderConfig, Level};
use std::io::{Read, Write};

fn compress<W: Write>(
    mut reader: impl Read,
    writer: W,
) -> Result<W, Box<dyn std::error::Error>> {
    let mut encoder = ParallelCompressor::new(
        EncoderConfig::default().with_level(Level::try_from(5)?),
        ParallelConfig::default(),
    )?;
    std::thread::scope(|scope| {
        let (mut sink, queue) = encoder.writer(writer, TaskCount::try_from(4)?)?;
        for _ in 0..4 {
            let queue = queue.clone();
            scope.spawn(move || queue.into_iter().for_each(|task| task.run()));
        }
        drop(queue);
        std::io::copy(&mut reader, &mut sink)?;
        Ok(sink.finish().map_err(FinishError::into_error)?)
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"stream through caller-owned workers\n".repeat(250_000);
    let compressed = compress(input.as_slice(), Vec::new())?;
    let mut decoder = mopenzl::Decompressor::new(Default::default());
    assert_eq!(decoder.decompress(&compressed)?, input);
    Ok(())
}
```

- Unlike `process`, the adapters block: a write waits while the window is
  full, a read waits for the next part, and `finish` waits for every part. Run
  the queue on threads other than the one using the adapter.
- Output is byte-identical to `prepare_slice` over the same bytes. The writer's
  `flush` ends the partial segment early and moves later boundaries; it
  emits no decodable prefix until End establishes the total size.
- The writer keeps every compressed byte until the sink takes it, so short
  writes, `WouldBlock` and sink errors lose nothing, and `finish` hands the
  adapter back inside `FinishError` for a retry, as `EncoderWriter` does.
  `reader.into_parts()` returns source bytes read ahead of the session.
- Session failures arrive as `std::io::Error` with the `ParallelEncodeError`
  as their source. Dropping an adapter abandons the stream and closes the
  queue.

## Segments and compressed size

Segment size defaults to 4 MiB and accepts 64 KiB–16 MiB. Effective task count
is at most the number of segments. For fixed input and segment settings, task
grouping and completion order do not change output bytes.

Segments are encoded independently, without cross-segment history. The frame is
OpenZL-compatible, but byte identity with the serial reference is not promised.
For fixed input, graph and segment settings, task grouping and completion order
do not change bytes. Compare compressed size and throughput with serial output.

Empty input uses serial encoding without tasks. A nonempty input below the
default 8 MiB parallel threshold uses a single serial task only when it also
fits in one segment.

## Sources and staging

| Input | Entry point |
| --- | --- |
| Borrowed slice | `prepare_slice` |
| Regular file | `prepare_source(FileSource::open(path)?, config)` |
| Owned seekable reader | `prepare_source(SeekSource::from(reader), config)` |
| Custom random-access source | `prepare_source(source, config)` with `RandomAccessSource` |

`FileSource` uses positional reads on Unix and Windows. `SeekSource` accepts
`Read + Seek + Send`, serializes seek/read operations, and lets compression run
concurrently. Sources must remain immutable for the batch lifetime; metadata
checks cannot detect every in-place mutation.

Owned sources and shared handles use the same generic entry point. For an
existing `Arc<FileSource>`, type inference can require
`prepare_source::<FileSource, _>(shared_file, config)`.

`BatchConfig::auto(tasks)` stages in memory using bounds derived from source
length and configured segment size. It caps effective tasks at the segment
count and still enforces any configured aggregate memory limit. It does not
query available RAM or switch to files.

`BatchConfig::memory(tasks, max_bytes)` sets an explicit payload-and-descriptor
budget. To obtain its exact required preflight allowance, call `estimate_source`
with `BatchConfig::auto(tasks)`, then convert
`estimate.maximum_staging_memory_bytes()?` using `usize::try_from`.
`maximum_staged_bytes` remains the payload-only bound. Planning also accounts
for worker and assembly storage. These estimates are not a process
RSS measurement. `BatchConfig::directory(tasks, directory)` stages output in
exclusive temporary files in an existing directory, keeping payload RAM
proportional to active workers and segment size. File cleanup is best effort
on drop.

## Output and cancellation

`finish_into` appends to a vector and restores its length on failure.
`finish_to_writer` consumes the batch and returns the destination with
statistics on success. On failure it preserves destination ownership and the
number of accepted bytes; the consumed batch cannot retry assembly. The caller
controls flushing, file durability, and publication.

Cancellation is cooperative around source reads and segment encoding. A running
codec call or blocking source read can delay it. Dropping a batch cancels work
without waiting for detached tasks. Worker retention is configurable and defaults
to retaining no workers.

The repository example runs scoped batches, reuses the compressor and checks
each resulting frame:

```sh
cargo run --release --example parallel
```

See [parallel mechanics](../architecture/parallel-compression.md) for lifecycle,
resource accounting, source checks, and error propagation.

## Choose a graph or individual codecs

`ParallelCompressor::builder(config, parallel).with_graph(graph).build()`
accepts the same `CompressorGraph` used by the serial API. Standard graphs,
parameterized nodes, selectors, segmenters and loaded Zstandard dictionaries
run for every segment, including tails and flush-cut segments. Choose aligned
boundaries for a graph interpreting bytes as records or numbers. Callbacks may
run concurrently and must be deterministic for schedule-independent output.

```rust
use mopenzl::compressor::parallel::{BatchConfig, ParallelCompressor, ParallelConfig, TaskCount};
use mopenzl::graph::{CompressorGraph, StandardGraph};
use mopenzl::Decompressor;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = ParallelCompressor::builder(Default::default(), ParallelConfig::default())
        .with_graph(CompressorGraph::standard(StandardGraph::Store))
        .build()?;
    let input = b"graph selected for every segment";
    let mut batch = encoder.prepare_slice(input, BatchConfig::auto(TaskCount::ONE))?;
    for task in batch.take_tasks()? { task.run(); }
    let mut frame = Vec::new();
    batch.finish_into(&mut frame)?;
    assert_eq!(Decompressor::default().decompress(&frame)?, input);
    Ok(())
}
```

For individual codecs, use `GraphBuilder::pipeline`, `static_graph` or
`parameterize_node`, then build the selected root graph and pass it to
`with_graph`. Graph selection also applies to serial fallback and survives
reconfiguration. See [graphs](graphs.md) and [codec nodes](codecs.md).
