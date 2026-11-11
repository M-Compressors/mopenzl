# Getting started

Build a complete round trip, then choose the API that fits your application.
Requires **Rust 1.89+**.

## Install

```sh
cargo new openzl-demo
cd openzl-demo
cargo add mopenzl@0.2.3
```

Or add this to `Cargo.toml`:

```toml
[dependencies]
mopenzl = "0.2.3"
```

## Compress and decompress

Replace `src/main.rs` with this complete program:

```rust
use mopenzl::{Compressor, Decompressor, EncoderConfig, Level};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = "hello, smaller world! ".repeat(1000);
    let config = EncoderConfig::default().with_level(Level::try_from(5)?);
    let mut encoder = Compressor::new(config)?;
    let compressed = encoder.compress(input.as_bytes())?;

    let mut decoder = Decompressor::new(Default::default());
    let restored = decoder.decompress(&compressed)?;
    assert_eq!(restored, input.as_bytes());

    println!("{} input bytes → {} compressed bytes", input.len(), compressed.len());
    Ok(())
}
```

```sh
cargo run --release
```

Choose the level explicitly: the default is **6** and accepted values are
**-131072 through 22**. Graphs interpret the level according to their codecs;
numeric selection changes at level 7. Compare speed and output size on your
data; [the benchmark pages](benchmarks/README.md) show results by graph.

## Choose a graph or codec

OpenZL describes compression as a graph of codec nodes. The default serial
path uses Zstandard; typed data can use numeric, record and string graphs.
Install a standard graph with `Compressor::builder(config).with_graph(...)`,
or compose nodes through `GraphBuilder`. The decoder reads the codec layout
from the frame. Start with [graphs](graphs.md), [codecs](codecs.md), and
[typed inputs](structured-data.md) before tuning the compression level.

## Reuse the workspace and output

A codec retains working memory between calls. `compress_into` appends a stream
and returns its range; clear the destination when you want to reuse it for a
new payload.

```rust
use mopenzl::{Compressor, Decompressor, EncoderConfig, Level};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = Compressor::new(
        EncoderConfig::default().with_level(Level::try_from(5)?),
    )?;
    let mut decoder = Decompressor::new(Default::default());
    let mut output = Vec::new();

    for payload in [b"first message".as_slice(), b"second message".as_slice()] {
        output.clear();
        let stream = encoder.compress_into(payload, &mut output)?;
        assert_eq!(decoder.decompress(&output[stream])?, payload);
    }
    Ok(())
}
```

See [retention policies](README.md#reusing-memory) to control which workspace
allocations remain after an operation. Caller output capacity is separate.

## Stream through `Write`

Write plain input into the adapter, then call `finish` to obtain a complete
OpenZL stream. Dropping the writer does not finalize it.

```rust
use mopenzl::{Compressor, Decompressor, EncoderConfig, InputSize, Level};
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"a payload arriving in two writes";
    let mut encoder = Compressor::new(
        EncoderConfig::default().with_level(Level::try_from(5)?),
    )?;
    let stream = InputSize::Exact(input.len() as u64).into();
    let mut writer = encoder.writer(Vec::new(), stream);
    writer.write_all(&input[..10])?;
    writer.write_all(&input[10..])?;
    let compressed = writer.finish().map_err(mopenzl::io::FinishError::into_error)?;

    let mut decoder = Decompressor::new(Default::default());
    assert_eq!(decoder.decompress(&compressed)?, input);
    Ok(())
}
```

For a retryable sink failure, `FinishError::into_parts` returns the error and
writer. `into_error` discards the writer, as this in-memory example does.
The [streaming guide](README.md#streaming-and-completion) explains completion,
backpressure, reader read-ahead, and direct sessions.

## Choose features

| Configuration | Dependency |
| --- | --- |
| Default std encoder + decoder | `mopenzl = "0.2.3"` |
| Encoder only | `mopenzl = { version = "0.2.3", default-features = false, features = ["std", "compression"] }` |
| Decoder only | `mopenzl = { version = "0.2.3", default-features = false, features = ["std", "decompression"] }` |
| Indexed resources with independent graphs | `mopenzl = { version = "0.2.3", features = ["framed"] }` |
| Alloc-backed codecs without std | `mopenzl = { version = "0.2.3", default-features = false, features = ["no_std", "compression", "decompression"] }` |

`no_std` takes precedence when enabled and removes std-only adapters and parallel
APIs. See [codec features](../architecture/codec-features.md) and
[alloc-only support](../architecture/codec-features.md#directions). Add the codec
features you need, such as `zstd` or `all-codecs`, when disabling defaults.
The book tracks `main`; use a local or Git dependency for APIs not yet released.

## Next steps

- [Sans I/O compression](sans-io-compression.md) and [decompression](sans-io-decompression.md): caller buffers, progress counts, and completion.
- [Read / Write I/O](io.md): readers and writers for raw, parallel, and framed streams.
- [Parallel compression](parallel.md): scoped threads, Rayon, files, and streaming.
- [Framing containers](frames.md): multiple resources, metadata, and indexed reads.
- [API guide](README.md): buffers, limits, sessions, and error handling.
- [Examples](examples/README.md): pick a recipe and run the repository examples.
