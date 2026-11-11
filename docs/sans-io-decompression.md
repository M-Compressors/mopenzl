# Sans I/O decompression

Feed compressed input and supply writable output space as it becomes available.
Sessions own decoding state and retain no caller input/output slices between
calls. `DecodeOperation::Process` allows more input; `Finish` declares the final
input boundary and enables truncation checks.

| Input | Decoder |
| --- | --- |
| Ordinary OpenZL frame | `Decompressor::start` |
| Output of `ParallelCompressor` | The same `Decompressor::start` |
| Framing container · `framed` | `FramedDecompressor::start` |

There is no separate parallel decoder. Parallel compression assembles a standard
OpenZL frame, decoded through the ordinary decoder APIs. For transport adapters,
see [Read / Write I/O](io.md).

## Decode a raw stream in chunks

This example supplies three compressed bytes at a time, uses a tiny output
buffer, and declares `Finish` only for the last input chunk.

```rust
use mopenzl::{
    Compressor, DecodeOperation, DecoderStatus, Decompressor, EncoderConfig, Level,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"incremental decoding preserves every byte\n".repeat(100);
    let mut encoder = Compressor::new(
        EncoderConfig::default().with_level(Level::try_from(5)?),
    )?;
    let compressed = encoder.compress(&input)?;
    let mut decoder = Decompressor::new(Default::default());
    let mut session = decoder.start();
    let mut chunks = compressed.chunks(3).peekable();
    let mut output = [0; 11];
    let mut restored = Vec::new();
    let mut finished = false;
    while let Some(chunk) = chunks.next() {
        let operation = if chunks.peek().is_none() {
            DecodeOperation::Finish
        } else {
            DecodeOperation::Process
        };
        let mut remaining = chunk;
        loop {
            let progress = session.process(remaining, &mut output, operation)?;
            remaining = &remaining[progress.consumed..];
            restored.extend_from_slice(&output[..progress.produced]);
            match progress.status {
                DecoderStatus::NeedsInput => {
                    assert!(remaining.is_empty());
                    break;
                }
                DecoderStatus::NeedsOutput => {}
                DecoderStatus::Finished => {
                    assert!(remaining.is_empty());
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            break;
        }
    }
    assert!(finished);
    assert_eq!(restored, input);
    Ok(())
}
```

In an application where EOF is learned from a later read, offer chunks with
`Process` and then call `process(&[], output, Finish)` until `Finished`.
After the first `Finish`, continue with `Finish` and exactly the remaining
input suffix. Do not add new bytes or switch back to `Process`.

`NeedsOutput` can occur after every input byte has been consumed: deliver the
output and call again with an empty input slice and fresh output space.
For protocols with bytes after the OpenZL member, preserve unconsumed input
when `Finished` arrives. The example asserts an empty suffix because its input
contains exactly one complete member.

## Decode a framing container as events

With `framed`, a session yields one owned `DecodedResource` at each resource
boundary. Metadata and every typed output are attached to that value. There
are no borrowed payload-fragment events or caller output buffer in this API.

```rust
use mopenzl::framing::{
    FramedCompressor, FramedDecompressor, FramedDecoderStatus, FramedResource,
};
use mopenzl::DecodeOperation;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"resource decoded from tiny arriving chunks";
    let resources = [FramedResource::from(input.as_slice())];
    let container = FramedCompressor::new(Default::default())?
        .compress(resources.as_slice().into())?;
    let mut decoder = FramedDecompressor::new(Default::default())?;
    let mut session = decoder.start();
    let mut chunks = container.chunks(3).peekable();
    let mut restored = Vec::new();
    let mut resources = 0;
    let mut finished = false;
    while let Some(chunk) = chunks.next() {
        let operation = if chunks.peek().is_none() {
            DecodeOperation::Finish
        } else {
            DecodeOperation::Process
        };
        let mut remaining = chunk;
        loop {
            let progress = session.process(remaining, operation)?;
            remaining = &remaining[progress.consumed..];
            if let Some(resource) = progress.resource {
                resources += 1;
                restored.extend_from_slice(resource.outputs[0].content());
            }
            match progress.status {
                FramedDecoderStatus::NeedsInput => { assert!(remaining.is_empty()); break; }
                FramedDecoderStatus::Finished => { assert!(remaining.is_empty()); finished = true; break; }
                FramedDecoderStatus::Resource => {}
            }
        }
        if finished { break; }
    }
    assert!(finished);
    assert_eq!(resources, 1);
    assert_eq!(restored, input);
    Ok(())
}
```

A resource event does not validate the rest of the container. Continue through
Finished to verify its directory, footer and EOF. For multiple resources, route
each `DecodedResource` to a separate destination. Metadata does not select an
extraction path. See [framed event semantics](../architecture/framing.md#decoder-lifecycle-and-limits).

## Limits and errors

Set input and output budgets in `DecoderConfig` before starting an ordinary
OpenZL decoder. Framing adds `FramingConfig` caps for encoded frame bytes,
metadata, resource count, aggregate decoded content and total wire bytes.
See [resource-limit configuration](README.md#decompression).

Raw `DecodeFailure` carries consumed/produced counts for a failing call;
`FramedDecodeFailure` carries consumed bytes and no partial owned resource.
Previously returned resource events remain tentative until the container
finishes validation. Failures are sticky: drop and start a new session.
Finish reports truncation; an empty Process call does not declare EOF.
