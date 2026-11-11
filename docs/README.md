# Choose your API

Compression and decompression expose the same I/O shapes. Pick the row that
matches how your application already moves bytes, then follow its guide.
New to the crate? Start with [getting started](getting-started.md); to choose
what runs inside a frame, read [graphs](graphs.md).

## Both codecs

| I/O shape | Compression | Decompression |
| --- | --- | --- |
| Return a new `Vec<u8>` | `Compressor::compress` | `Decompressor::decompress` |
| Append to an existing Vec | `compress_into` | `decompress_into` |
| Write into a caller-owned slice | `compress_to_slice` | `decompress_to_slice` |
| Write into uninitialized memory | `compress_to_uninit` | `decompress_to_uninit` |
| Typed and multiple inputs / outputs | `compress_inputs`, `compress_inputs_into` | `decompress_typed` → `Vec<TypedOutput>` |
| Choose the graph / codec nodes | `Compressor::builder(..).with_graph(..)` / `ParallelCompressor::builder(..).with_graph(..)` | — (the frame carries its own) |
| Pull output through `std::io::Read` | `Compressor::reader` | `Decompressor::reader` |
| Push input through `std::io::Write` | `Compressor::writer` | `Decompressor::writer` |
| Drive input/output incrementally | `start` → `EncoderSession` | `start` → `DecoderSession` |
| Same, into uninitialized memory | `EncoderSession::process_uninit` | `DecoderSession::process_uninit` |
| Same, with the session owning the codec | `into_session` → `EncoderSessionOwned` | `into_session` → `DecoderSessionOwned` |

The decoder needs no graph: a frame records the codecs that produced it.
`Compressor` and `Decompressor` own reusable working state. Keep one alive
across payloads when allocation reuse matters.

## Parallel compression

`ParallelCompressor` lives in `mopenzl::compressor::parallel` and is
unavailable with `no_std`. It splits one input into segments and hands them to
your executor as tasks; the library does not create a thread pool. Streaming
buffers compressed chunks until finish, when the total input size is known.

| I/O shape | Entry point |
| --- | --- |
| Split one known input across your threads | `prepare_slice` / `prepare_source` → `ScopedParallelBatch` |
| Stream chunked input across your threads | `start` → `ParallelEncoderSession` + `ParallelTaskQueue` |
| Same, into uninitialized memory | `ParallelEncoderSession::process_uninit` |
| Same, with the session owning the compressor | `into_session` → `ParallelEncoderSessionOwned` |
| Pull parallel output through `std::io::Read` | `reader` → `ParallelEncoderReader` + `ParallelTaskQueue` |
| Push input through `std::io::Write` across your threads | `writer` → `ParallelEncoderWriter` + `ParallelTaskQueue` |

See [parallel compression](parallel.md) for scoped threads, Rayon, sources,
staging, and streaming.

## Guides by task

| Task | Read |
| --- | --- |
| Choose graphs, selectors, segmenters or codec pipelines | [Graphs](graphs.md) |
| See what each codec does and which feature enables it | [Codecs](codecs.md) |
| Compress integers, records or strings | [Structured data](structured-data.md) |
| Implement and register an application-defined codec | [Custom codecs](codecs.md#custom-codecs) |
| Use a Zstandard dictionary | [Dictionaries](dictionaries.md) |
| Set the level, format version, checksums or Cargo features | [Configuration and features](configuration.md) |
| Control allocations, or write into uninitialized memory | [Buffers and memory reuse](memory.md) |
| Decode untrusted input with resource limits | [Decompression and limits](decompression.md) |
| Read a frame's comment, version or output types without decoding | [Frame metadata](decompression.md#frame-metadata) |
| Understand readers, writers, sessions, and `finish` | [Streaming and completion](streaming.md) |
| Connect synchronous I/O | [Read / Write I/O](io.md) |
| Drive caller-owned buffers directly | Sans I/O [compression](sans-io-compression.md) and [decompression](sans-io-decompression.md) |
| Schedule compression on your own threads | [Parallel compression](parallel.md) |
| Use a different graph for each indexed resource | [Framing containers](frames.md) and [graph recipes](framing.md) (`framed` feature) |
| Copy a complete program | [Examples](examples/README.md) |

## Errors

`ConfigError` covers configuration, `EncodeError` covers compression (including
`EncodeError::Graph(GraphError)` for graph failures), `DecodeError` covers
decompression, `graph::GraphError` covers graph construction, and `dict::DictError`
covers dictionaries. I/O adapters return `std::io::Error`; writer finalization
preserves the writer in `FinishError`.

## Reference

- [API reference on docs.rs](https://docs.rs/mopenzl)
- [Compatibility and validation](correctness.md): what is byte-identical to
  OpenZL v0.3.0, and how it is checked
- [Benchmark results](benchmarks/README.md) per graph on Silesia, with raw data
- [Architecture](../architecture/README.md): how the implementation works
- [Development](development.md) and [benchmarking](benchmarking.md) for
  contributors
