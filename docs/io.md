# Read / Write I/O

Use std adapters to connect the codecs to files, network streams, in-memory
buffers, and other implementations of `Read` or `Write`. Adapters drive the same
sessions as the [Sans I/O APIs](sans-io-compression.md), adding buffering
and synchronous I/O conventions.

## API map

| Operation | Pull bytes through a reader | Push bytes through a writer |
| --- | --- | --- |
| Raw compression | `Compressor::reader` | `Compressor::writer` |
| Raw decompression | `Decompressor::reader` | `Decompressor::writer` |
| Parallel compression | `ParallelCompressor::reader` + task queue | `ParallelCompressor::writer` + task queue |
| Decompress parallel output | `Decompressor::reader` | `Decompressor::writer` |
| Framed compression · `framed` | Drain a native `FramedEncoderSession` | `FramedCompressor::framed_writer` accepts complete resources |
| Framed decompression · `framed` | `framed_reader` or `framed_seek_reader` returns owned typed resources | Route native decoder resources to your sink |

The library has no dedicated framed decoder `Write` adapter. Its event API keeps
resource boundaries and metadata visible; forward each payload event to an
appropriate `Write` destination. It also has no parallel decoder: parallel
compression produces one ordinary OpenZL frame.

All adapters on this page require std. Enable `framed` for framing.
`no_std` removes the std surface even when std is also enabled.

## Raw compression and decompression through `Read`

An encoder reader consumes **plain** bytes and yields **compressed** bytes.
A decoder reader consumes compressed bytes and yields restored bytes.
`std::io::copy` drives each one to EOF. Use a `File` or socket in place of a slice.

```rust
use mopenzl::{Compressor, Decompressor, EncoderConfig, InputSize, Level};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"pull plain bytes through the encoder\n".repeat(100);
    let mut encoder = Compressor::new(
        EncoderConfig::default().with_level(Level::try_from(5)?),
    )?;
    let stream = InputSize::Exact(input.len() as u64).into();
    let mut reader = encoder.reader(input.as_slice(), stream);
    let mut compressed = Vec::new();
    std::io::copy(&mut reader, &mut compressed)?;
    drop(reader);

    let mut decoder = Decompressor::new(Default::default());
    let mut reader = decoder.reader(compressed.as_slice());
    let mut restored = Vec::new();
    std::io::copy(&mut reader, &mut restored)?;
    assert_eq!(restored, input);
    Ok(())
}
```

When the total input size is unknown, use the default stream configuration for
the encoder. Reading a decoder to EOF is necessary to observe final validation
errors. A raw decoder reader can read past a member boundary; recover its source
and buffered suffix with `into_parts` when another protocol message follows.

## Raw compression and decompression through `Write`

An encoder writer accepts plain bytes; a decoder writer accepts compressed bytes.
Each owns its sink and borrows its codec owner. Finalize **both** writers.

```rust
use mopenzl::{Compressor, Decompressor, EncoderConfig, Level};
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"push input into a writer\n".repeat(100);
    let mut encoder = Compressor::new(
        EncoderConfig::default().with_level(Level::try_from(5)?),
    )?;
    let mut writer = encoder.writer(Vec::new(), Default::default());
    for chunk in input.chunks(13) {
        writer.write_all(chunk)?;
    }
    let compressed = writer.finish().map_err(mopenzl::io::FinishError::into_error)?;

    let mut decoder = Decompressor::new(Default::default());
    let mut writer = decoder.writer(Vec::new());
    for chunk in compressed.chunks(3) {
        writer.write_all(chunk)?;
    }
    let restored = writer.finish().map_err(mopenzl::io::FinishError::into_error)?;
    assert_eq!(restored, input);
    Ok(())
}
```

A decoder writer's `finish` checks that input ends at a valid stream boundary;
`flush` only drains output and flushes the sink. With a known input size, encoder `flush` creates an intermediate chunk
boundary and can change compressed size; unknown-size frames wait until End. Dropping a writer
does not finalize or flush it.

## Parallel compression through `Read` and `Write`

Both constructors return an adapter **and a task queue**. Run that queue on
independent workers while using the adapter; reads, writes, and finish can block
waiting for tasks. A queue with no running executor cannot make progress.

This reader example compresses across scoped threads, then uses an ordinary
decoder reader to restore the assembled raw stream:

```rust
use mopenzl::compressor::parallel::{ParallelCompressor, ParallelConfig, TaskCount};
use mopenzl::{Decompressor, EncoderConfig, Level};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"parallel reader with caller-owned workers\n".repeat(220_000);
    let mut encoder = ParallelCompressor::new(
        EncoderConfig::default().with_level(Level::try_from(5)?),
        ParallelConfig::default(),
    )?;
    let mut compressed = Vec::new();
    std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
        let (mut reader, queue) = encoder.reader(input.as_slice(), TaskCount::try_from(4)?)?;
        for _ in 0..4 {
            let worker_queue = queue.clone();
            scope.spawn(move || worker_queue.into_iter().for_each(|task| task.run()));
        }
        drop(queue);
        std::io::copy(&mut reader, &mut compressed)?;
        Ok(())
    })?;

    let mut decoder = Decompressor::new(Default::default());
    let mut reader = decoder.reader(compressed.as_slice());
    let mut restored = Vec::new();
    std::io::copy(&mut reader, &mut restored)?;
    assert_eq!(restored, input);
    Ok(())
}
```

For the writer direction, the [complete parallel writer recipe](parallel.md#reader-and-writer-adapters)
copies a source into the returned writer, then finishes it while workers keep
running. The task window bounds outstanding segments; the parallel writer's
`flush` ends the current segment early. Decode its output with either raw adapter
shown above. See [parallel output and backpressure](../architecture/parallel-compression.md).

## Framed compression through readers and resource writers

Enable `framed`. A framed writer accepts complete resources as they arrive;
each `resource` command queues one OpenZL frame and optional metadata. It is
resource-oriented: there is no encoded `framed_reader` or byte-oriented resource
writer on `FramedCompressor`. Use its native session to drain a container into
caller-owned output buffers, or use `framed_writer` for a `Write` transport.

The [framed writer recipe](frames.md#write-resources-and-metadata) demonstrates
graph selection and metadata. Here is a complete writer paired with sequential
resource decoding:

```rust
use mopenzl::framing::{FramedCompressor, FramedDecompressor, FramedResource};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"a resource sent through an encoded writer";
    let mut encoder = FramedCompressor::new(Default::default())?;
    let mut writer = encoder.framed_writer(Vec::new())?;
    writer.resource(FramedResource::from(input.as_slice()))?;
    let container = writer.finish().map_err(|failure| failure.error)?;
    let mut decoder = FramedDecompressor::new(Default::default())?;
    let mut reader = decoder.framed_reader(container.as_slice());
    let mut restored = Vec::new();
    let mut resources = 0;
    while let Some(resource) = reader.read_resource()? {
        resources += 1;
        restored.extend_from_slice(resource.outputs[0].content());
    }
    assert_eq!(resources, 1);
    assert_eq!(restored, input);
    Ok(())
}
```

## Framed decompression into a `Write` sink

`FramedReader` accepts any `Read` source and returns owned `DecodedResource`
values. Route its outputs to a sink with `write_all`, using metadata to select
a destination when needed. Continue through None to validate the directory,
footer and EOF. Typed resources may have several outputs and string lengths;
the example below contains one serial output per resource.

```rust
use mopenzl::framing::{FramedCompressor, FramedDecompressor, FramedResource};
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let resources = [
        FramedResource::from(&b"first"[..]),
        FramedResource::from(&b"second"[..]),
    ];
    let container = FramedCompressor::new(Default::default())?
        .compress(resources.as_slice().into())?;
    let mut decoder = FramedDecompressor::new(Default::default())?;
    let mut reader = decoder.framed_reader(container.as_slice());
    let mut sinks: Vec<Vec<u8>> = Vec::new();
    while let Some(resource) = reader.read_resource()? {
        let mut sink = Vec::new();
        sink.write_all(resource.outputs[0].content())?;
        sinks.push(sink);
    }
    assert_eq!(sinks, vec![b"first".to_vec(), b"second".to_vec()]);
    Ok(())
}
```

An application can route resources to files or other writers. Retain any
unwritten suffix after a sink error; the owned resource remains available while
the transport retries. A resource already delivered remains tentative until the
container validates. This reader consumes through EOF; it does not expose a
buffered suffix for another container or protocol message.

For `Read + Seek`, [indexed reads](frames.md#indexed-resource-reads) use
`framed_seek_reader` and `read_resource(index)` to return a whole selected
resource. For push-driven compressed input use the
[native framed decoder](sans-io-decompression.md#decode-a-framing-container-as-events)
and route its owned resource events to sinks.

## Completion, retries, and ownership

| Adapter | Complete operation | Recover after a failure |
| --- | --- | --- |
| Ordinary encoder / decoder reader | Read to EOF | Decoder `into_parts` preserves read-ahead; codec errors are terminal |
| Ordinary encoder / decoder writer | `finish` or retryable `try_finish` | `FinishError::into_parts` returns error and writer |
| Parallel reader | Read to EOF with active workers | `into_parts` preserves read-ahead; keep queue execution alive |
| Parallel writer | `finish` with active workers | `FinishError::into_parts` returns error and adapter |
| Framed writer | Submit complete resources, then container `finish` | `FramedFinishError` exposes `writer` and `error` |
| Framed resource reader | `read_resource` until None | Transport failures preserve input; container/codec errors are sticky |

Short sink writes and errors must not replay accepted input. Ordinary and
parallel writers can accept input before delivery fails; respect returned byte
counts. A framed writer's resource command drains previous bytes before commit,
so a command error means that new resource was not accepted. WouldBlock is not
EOF. Use native sessions for explicit runtime readiness handling.
See [streaming completion](README.md#streaming-and-completion) and
[framed retry mechanics](../architecture/framing.md#encoder-lifecycle-and-retries).
