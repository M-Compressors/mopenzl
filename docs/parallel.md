# Parallel compression

`mopenzl::compressor::parallel` follows mbrotli's caller-scheduled API. The
library creates no threads and has no executor dependency. Tasks own reusable
codec workspaces; run them with scoped threads, a thread pool, or inline.

```rust
use mopenzl::EncoderConfig;
use mopenzl::compressor::parallel::{
    BatchConfig, ParallelCompressor, ParallelConfig, TaskCount,
};

let input = b"parallel OpenZL ".repeat(1_000_000);
let mut compressor = ParallelCompressor::new(
    EncoderConfig::default(), ParallelConfig::default())?;
let mut batch = compressor.prepare_slice(&input,
    BatchConfig::auto(TaskCount::available()?))?;
let tasks = batch.take_tasks()?;
std::thread::scope(|scope| {
    for task in tasks {
        scope.spawn(move || task.run());
    }
});
let mut frame = Vec::new();
let result = batch.finish_into(&mut frame)?;
assert_eq!(result.stats.input_bytes, input.len() as u64);
# Ok::<(), Box<dyn std::error::Error>>(())
```

The result is one ordinary OpenZL frame, readable by the reference decoder.
Byte identity with the serial reference encoder is not guaranteed. Fixed input
and configuration produce the same bytes regardless of task count, execution
order, source type or staging backend. Segments have no cross-segment history.

## Choose a graph or individual codecs

Use the same `CompressorGraph` and `GraphBuilder` as for the serial compressor:

```rust
use mopenzl::compressor::parallel::{ParallelCompressor, ParallelConfig};
use mopenzl::graph::{CompressorGraph, StandardGraph};

let compressor = ParallelCompressor::builder(Default::default(), ParallelConfig::default())
    .with_graph(CompressorGraph::standard(StandardGraph::Store))
    .build()?;
# let _ = compressor;
# Ok::<(), mopenzl::compressor::parallel::ParallelConfigError>(())
```

For individual codecs, build nodes with `GraphBuilder::static_graph`, `pipeline`
and `parameterize_node`, then pass the built graph to `with_graph`. Standard and
application codecs, selectors, segmenters and loaded Zstandard dictionaries all
use the existing serial graph API. Pin a backend with the builder's
`with_backend` method. `new` keeps its default-graph behavior.

Every segment is a fresh serial input to that graph, including the tail and
flush-cut segments; choose boundaries that satisfy alignment/record requirements.
Graph selection applies to serial fallback and empty input and survives
`reconfigure`. Callbacks may run concurrently and must be deterministic if byte
invariance across scheduling matters. Decoder-side custom codec/dictionary
registration remains the caller's responsibility, just as with serial frames.
Disabled/unsupported explicit codecs report serial graph errors; permissive
settings can select the graph engine's normal fallbacks. Graph expansion remains
subject to the planned staging bound. Typed/multiple top-level inputs and header
comments are not currently exposed by the parallel API.

`ParallelConfig` selects fixed segment size (64 KiB–16 MiB, default 4 MiB), serial
fallback threshold, retained workers, optional aggregate memory estimate limit,
and source validation. A default compressor retains no workers between batches;
use `with_max_retained_workers` for repeated operations. `BatchConfig::auto`
selects memory staging without an explicit staging ceiling. For explicit limits,
use `estimate_source`, `maximum_staging_memory_bytes` and `BatchConfig::memory`.
`BatchConfig::directory` stages compressed payload in private temporary files.

`prepare_source` accepts an owned or shared `RandomAccessSource`:
`ArcBytesSource`, `FileSource`, `SeekSource<R>`, or a custom immutable source.
These tasks are `Send + 'static`; slice tasks borrow input and fit scoped
executors. Keep the source unchanged until completion. Validation checks source
length and optional identity before writing the destination; metadata checks do
not replace immutability. Detached tasks can outlive an abandoned coordinator;
its drop cancels without waiting. An unrun task's drop reports abandonment.

For incremental input, `start(window)` returns a borrowed session and task queue;
`into_session(window)` returns an owning session. Clone queue handles for
executors and run `for task in queue { task.run(); }`. Drive `process` with
`Operation::Continue`, `Flush`, and `End`, or use `flush`/`finish`. `Pending`
means tasks must progress; `NeedsOutput` means more destination room is needed.
`process_uninit` accepts uninitialized destination storage with the same progress
contract. `reader`/`writer` wrap the session in blocking standard I/O adapters.

OpenZL requires the total input size in its header. These size-free streaming
interfaces therefore **buffer compressed chunks until finish**. `Flush` encodes
the partial segment but emits no decodable prefix; a reader consumes to EOF
before its first output. The task window bounds active work, while buffered
compressed output grows with the input, subject to the optional aggregate limit.
Use random-access batches with directory staging for large files and bounded
payload memory.

Keep executors runnable while waiting or using blocking adapters. Finishing,
failure and session drop close the queue, ending executor loops. Writer
finalization failures preserve the adapter through `mopenzl::io::FinishError`;
batch `finish_to_writer` instead returns the writer, typed cause and accepted
byte count. See [the example](../examples/parallel.rs),
[architecture](../architecture/parallel-compression.md) and
[benchmark measurements](../architecture/benchmark-comparison.md#parallel-compression).
