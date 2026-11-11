# Sans I/O compression

Use a session when your application owns the input/output buffers and decides
when more bytes are available. Sessions perform no I/O. This works in a blocking
loop, an async task, or a custom event loop without an I/O adapter.

| Output format | Session entry point | Scheduling |
| --- | --- | --- |
| OpenZL frame | `Compressor::start(stream)` | Call `process` yourself |
| One raw stream encoded across workers | `ParallelCompressor::start(window)` | Drive `process` and run its task queue |
| Framed resources · `framed` | `FramedCompressor::start()` | Drain container output, submit complete resources, finish container |

For transport integration, see [Read / Write I/O](io.md). For the inverse
operation, see [Sans I/O decompression](sans-io-decompression.md).

## Drive a raw encoder

Supply chunks with `Operation::Continue`. After the last chunk, call `Finish`
until `EncoderStatus::Finished`. An empty input passed with `Process` is not EOF.
The deliberately small output buffer below forces repeated output drains.

```rust
use mopenzl::{
    Compressor, Decompressor, EncoderConfig, EncoderStatus, InputSize, Operation, Level,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"incremental encoding with tiny buffers\n".repeat(100);
    let config = EncoderConfig::default().with_level(Level::try_from(5)?);
    let mut encoder = Compressor::new(config)?;
    let mut compressed = Vec::new();
    let mut output = [0; 11];
    {
        let mut session = encoder.start(InputSize::Exact(input.len() as u64).into());
        for chunk in input.chunks(7) {
            let mut remaining = chunk;
            loop {
                let progress = session.process(remaining, &mut output, Operation::Continue)?;
                remaining = &remaining[progress.consumed..];
                compressed.extend_from_slice(&output[..progress.produced]);
                if progress.status == EncoderStatus::NeedsInput {
                    assert!(remaining.is_empty());
                    break;
                }
            }
        }
        loop {
            let progress = session.process(&[], &mut output, Operation::End)?;
            compressed.extend_from_slice(&output[..progress.produced]);
            if progress.status == EncoderStatus::Finished {
                break;
            }
        }
    }
    // Exact input size, offset zero, and no flushes preserve one-shot bytes.
    assert_eq!(compressed, encoder.compress(&input)?);
    let mut decoder = Decompressor::new(Default::default());
    assert_eq!(decoder.decompress(&compressed)?, input);
    Ok(())
}
```

Always advance input by `consumed` and publish only `output[..produced]` before
handling status. `NeedsOutput` requires output space and the unconsumed input
suffix; reaching the end of the current input slice alone does not finish the
stream.

## Flush and finish are different

`Operation::Flush` ends the current chunk without ending the frame, provided
its total input size is known. With unknown size it has no effect.
Drain it to `NeedsInput` before changing operation or supplying new input.
Flush boundaries can change output size and bytes. `Operation::End` ends the frame;
while retrying it, keep the same operation and exact unconsumed input suffix.

Use `InputSize::Exact` when the total input size is known. Otherwise use the
default unknown size; the encoder buffers input until End because the frame
header needs the total size.
See [stream scheduling and identity](README.md#output-compatibility).

## Drive parallel input

`ParallelCompressor::start(window)` returns `(session, queue)`. Keep workers
running the queue while you feed input and drain output. The session's `process`
does not block: `ParallelEncoderStatus::Pending` means progress depends on tasks.
Call `session.wait()` with independently runnable workers, or execute queued tasks
yourself before waiting on a single thread.

The [complete Sans I/O parallel example](parallel.md#streaming-input) handles
all statuses and source chunks. Its window bounds active tasks; OpenZL streaming holds compressed results until
the total size is known at End. Without flushes, output matches a batch over the same
input and segment configuration. See [parallel session mechanics](../architecture/parallel-compression.md).

## Drive a framed encoder

Enable `framed`. Container `process` takes an output buffer and a
`FramedEncodeOperation`. Submit each complete `FramedResource` with its optional
graph, drain the queued frame, then finish the container. Resource inputs can
be serial, typed or multiple; this API does not offer byte-oriented resource guards.

```rust
use mopenzl::framing::{
    FramedCompressor, FramedDecompressor, FramedEncodeOperation,
    FramedEncoderStatus, FramedResource,
};
use mopenzl::graph::{CompressorGraph, StandardGraph};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"framed incremental payload\n".repeat(20);
    let store = CompressorGraph::standard(StandardGraph::Store);
    let mut encoder = FramedCompressor::new(Default::default())?;
    let mut session = encoder.start()?;
    let mut buffer = [0; 13];
    let mut container = Vec::new();
    // Drain the main header before accepting a resource command.
    loop {
        let progress = session.process(&mut buffer, FramedEncodeOperation::Process)?;
        container.extend_from_slice(&buffer[..progress.produced]);
        if progress.status == FramedEncoderStatus::NeedsInput { break; }
    }
    session.resource(FramedResource {
        graph: Some(&store),
        ..FramedResource::from(input.as_slice())
    })?;
    loop {
        let progress = session.process(&mut buffer, FramedEncodeOperation::Finish)?;
        container.extend_from_slice(&buffer[..progress.produced]);
        if progress.status == FramedEncoderStatus::Finished { break; }
    }
    drop(session);
    let mut decoder = FramedDecompressor::new(Default::default())?;
    assert_eq!(decoder.decompress(&container)?.resources[0].outputs[0].content(), input);
    Ok(())
}
```

An empty resource is an ordinary `FramedResource::from(&b""[..])` command and
still produces one complete frame. A new resource requires previous container
output to be drained; otherwise `OutputPending` rejects the command before
commit. After a failed resource command the same session can accept a corrected
resource. See [resource lifecycles and metadata](frames.md).

## Ownership and cancellation

Borrowed sessions exclusively borrow their codec owner. Dropping an unfinished
session cancels its frame/container and releases the owner; Drop performs no
transport I/O. Raw codec `into_session` and parallel `into_session` provide owned
sessions when a lifetime-free holder is useful. Framed native sessions are
borrowed only. See [encoder sessions](../architecture/compressor.md#sessions),
[decoder sessions](../architecture/decompressor.md#sessions) and
[framed lifecycles](../architecture/framing.md).
