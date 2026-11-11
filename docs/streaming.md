# Streaming and completion

Use streaming when the payload does not fit in memory, arrives in pieces, or
must leave before the input ends. There are three shapes, and they drive the
same state machine:

| Shape | You call | The library does | Recipes |
| --- | --- | --- | --- |
| Reader | `read` on the adapter | Pulls source bytes and yields transformed bytes | [Read / Write I/O](io.md) |
| Writer | `write_all`, then `finish` | Pushes transformed bytes into your sink | [Read / Write I/O](io.md) |
| Session | `process` with your own buffers | No I/O; reports consumed and produced counts | Sans I/O [compression](sans-io-compression.md) and [decompression](sans-io-decompression.md) |

Readers and writers require `std`. Sessions fit blocking code, async runtimes
and event loops.

## Declare the input size

A frame header records the frame's total size, so declare it with
`InputSize::Exact` when you know it: a session then writes the header at once
and, with `Chunking::Fixed`, holds at most one chunk of input. With
`InputSize::Unknown` it holds the whole input until the frame ends.

## Readers and writers

`reader(source, stream)` consumes source bytes and yields transformed bytes through
`Read`; `writer(sink, stream)` accepts source bytes through `Write` and delivers
transformed bytes to the sink. Compression takes plain input, decompression takes
compressed input (and no stream argument). Both adapter families require `std`.

A writer borrows the codec and owns its sink. Use `write_all` for input and
`finish` to end the frame: consuming `finish` returns the sink, and
`FinishError` retains the writer on failure (`try_finish` retries, `into_error`
discards the writer). Dropping a writer performs no finalization. A reader owns its
source; read to EOF for the complete output. `flush()` makes accepted input
decodable without ending the frame.

<!-- cfg: not(feature = "no_std") -->
```rust
use mopenzl::{ChunkSize, Chunking, Compressor, Decompressor, EncoderConfig, InputSize};
use std::io::{Read, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let payload = b"streamed payload ".repeat(10_000);
    let config = EncoderConfig::default().with_chunking(Chunking::Fixed(ChunkSize::MIN));

    // Push model: write plain bytes into a compressed sink.
    let mut encoder = Compressor::new(config)?;
    let mut writer = encoder.writer(Vec::new(), InputSize::Exact(payload.len() as u64).into());
    writer.write_all(&payload)?;
    let frame = writer
        .finish()
        .map_err(mopenzl::io::FinishError::into_error)?;

    // Pull model: read compressed bytes from a plain source.
    let mut reader = encoder.reader(&payload[..], InputSize::Exact(payload.len() as u64).into());
    let mut from_reader = Vec::new();
    reader.read_to_end(&mut from_reader)?;
    assert_eq!(from_reader, frame);

    // Decompression, pulling restored bytes from a compressed source.
    let mut decoder = Decompressor::default();
    let mut restored = Vec::new();
    decoder.reader(&frame[..]).read_to_end(&mut restored)?;
    assert_eq!(restored, payload);

    // ... or pushing compressed bytes into a restored sink.
    let mut writer = decoder.writer(Vec::new());
    writer.write_all(&frame)?;
    let restored = writer
        .finish()
        .map_err(mopenzl::io::FinishError::into_error)?;
    assert_eq!(restored, payload);
    Ok(())
}
```

The decoder adapters use bounded internal buffering, and decoder-writer
finalization reports truncated or invalid input instead of silently accepting
an incomplete frame.

## Sessions

For direct buffer control `start` creates an `EncoderSession`; call `process` with
input, output and an `Operation`:

| Operation / status | Meaning |
| --- | --- |
| `Operation::Continue` | Accept input without ending the frame |
| `Operation::Flush` | End the current chunk so everything accepted so far is decodable (costs ratio; no effect with `InputSize::Unknown`) |
| `Operation::End` | End the frame after the supplied input |
| `EncoderStatus::NeedsInput` | Supply more input |
| `EncoderStatus::NeedsOutput` | Supply output space and continue with the unconsumed input |
| `EncoderStatus::Finished` | All final output has been delivered |

`Progress` reports `consumed`, `produced` and `status`; advance by the counts.
Finishing can take several calls when output buffers are small. The decoder's
`DecoderSession::process` mirrors it with `DecodeOperation::{Process, Finish}`,
`DecoderStatus` and `DecodeProgress`, and `total_in`, `total_out` and
`frames_decoded` counters.

**Operation names.** The encoder and decoder name the same idea differently, and
both are kept for compatibility:

| Meaning | Encoder `Operation` | Decoder `DecodeOperation` |
| --- | --- | --- |
| More input may follow | `Continue` | `Process` |
| End the current chunk so far is decodable | `Flush` | none (the decoder emits what it can on every call) |
| This is all the input | `End` | `Finish` |

**Adapters return no `Result`.** `Compressor::reader`, `Compressor::writer` and
the decoder's `reader` and `writer` return the adapter directly. Unlike
`mbrotli`, whose adapter constructors return a `Result`, nothing can fail when
they are created; every error surfaces from the first `read`, `write` or
`finish` call as an `io::Error`. `FinishError` lives in `mopenzl::io`, not at
the crate root: use `writer.finish().map_err(mopenzl::io::FinishError::into_error)?`
to turn a failed finish into the plain error and drop the writer.

**Sessions and graphs.** A session holds the input a frame needs. With the default
graph and `Chunking::Fixed`, or a start graph that is a serial segmenter, it
compresses chunk by chunk and holds at most one chunk when you declare
`InputSize::Exact`. Any other graph buffers its input until the frame ends and then
runs the graph once; the declared size is checked at the end. Session output is
byte-identical to the one-shot call. See [graphs](graphs.md#chunking-and-segmenters).

```rust
use mopenzl::{
    ChunkSize, Chunking, Compressor, DecodeOperation, DecoderStatus, Decompressor, EncoderConfig,
    EncoderStatus, InputSize, Operation,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let payload = b"incremental payload ".repeat(5_000);
    let config = EncoderConfig::default().with_chunking(Chunking::Fixed(ChunkSize::MIN));
    let mut encoder = Compressor::new(config)?;
    let expected = encoder.compress(&payload)?;

    // Compress in 4 KiB pieces through a small 1 KiB output buffer.
    let mut session = encoder.start(InputSize::Exact(payload.len() as u64).into());
    let mut frame = Vec::new();
    let mut buffer = [0u8; 1024];
    let mut rest = &payload[..];
    loop {
        let take = rest.len().min(4096);
        let operation = if take == rest.len() {
            Operation::End
        } else {
            Operation::Continue
        };
        let progress = session.process(&rest[..take], &mut buffer, operation)?;
        frame.extend_from_slice(&buffer[..progress.produced]);
        rest = &rest[progress.consumed..];
        if progress.status == EncoderStatus::Finished {
            break;
        }
    }
    assert_eq!(frame, expected);

    // Decompress the same way.
    let mut decoder = Decompressor::default();
    let mut session = decoder.start();
    let mut restored = Vec::new();
    let mut position = 0;
    loop {
        let end = (position + 1000).min(frame.len());
        let operation = if end == frame.len() {
            DecodeOperation::Finish
        } else {
            DecodeOperation::Process
        };
        let progress = session.process(&frame[position..end], &mut buffer, operation)?;
        restored.extend_from_slice(&buffer[..progress.produced]);
        position += progress.consumed;
        if progress.status == DecoderStatus::Finished {
            break;
        }
    }
    assert_eq!(restored, payload);
    Ok(())
}
```

A dropped unfinished session abandons its frame; the codec is ready for the next
one.
