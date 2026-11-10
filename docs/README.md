# User guide

[Quick start](../README.md#quick-start) · [API reference](https://docs.rs/mopenzl)

| Task | Read |
| --- | --- |
| Decode and set resource limits | [Decompression](#decompression) |
| Read a frame's comment, version or output types without decoding | [Frame metadata](#frame-metadata) |
| Choose compression settings | [Configuration](#configuration) |
| Control allocations | [Output buffers](#output-buffers) and [reuse](#reusing-memory) |
| Write into uninitialized memory | [Output buffers](#output-buffers) |
| Schedule compression on your own threads | [Parallel compression](parallel.md) |
| Connect synchronous I/O or drive a session | [Streaming](#streaming-and-completion) |
| Compress integers, records or strings | [Structured data](structured-data.md) |
| Pick or build a compression graph | [Graphs](graphs.md) |
| See what each codec does and which feature enables it | [Codecs](codecs.md) |
| Implement and register an application-defined codec | [Custom codecs](codecs.md#custom-codecs) |
| Use a Zstandard dictionary | [Dictionaries](dictionaries.md) |
| Compare with the C library | [Benchmarking](benchmarking.md) |
| Build, test and fuzz the crate | [Development](development.md) |

## Decompression

`Decompressor` owns reusable state. `decompress` returns a Vec, `decompress_into`
appends, and `decompress_to_slice` writes into caller storage. Sessions report
consumed and produced counts; `reader` and `writer` adapt synchronous I/O. The
decoder needs no graph: a frame records the codecs that produced it, so one
decoder reads frames from the default graph, from any standard graph and from your
own.

The byte-oriented methods read frames with one serial output, as the reference's
`ZL_decompress` does. A frame built from typed or several inputs reports
`DecodeError::Unsupported` there (`TypedOutput` or `MultipleOutputs`); read it with
`decompress_typed`, which returns one `TypedOutput` per output with its
`data_type()`, `width()`, `content()` and, for strings, `string_lens()`.

By default one call decodes exactly one frame and rejects trailing bytes.
`FrameMode::Concatenated` decodes every frame until the input ends. Both checksums
are verified unless you switch them off with `with_content_checksum_check(false)`
and `with_compressed_checksum_check(false)`.

**Configure limits for untrusted input.** Limits are unlimited by default. A frame
declaring more output than allowed fails before any chunk is decoded.

```rust
use mopenzl::{DecodeError, DecodeLimits, DecoderConfig, Decompressor, FrameMode};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let limits = DecodeLimits::default()
        .with_max_input_bytes(Some(1 << 20))
        .with_max_output_bytes(Some(8 << 20));
    let config = DecoderConfig::default()
        .with_frame_mode(FrameMode::Concatenated)
        .with_limits(limits);
    let mut decoder = Decompressor::new(config);

    // Two frames back to back decode to their concatenation.
    let mut encoder = mopenzl::Compressor::default();
    let mut frames = encoder.compress(b"first frame, ")?;
    frames.extend(encoder.compress(b"second frame")?);
    assert_eq!(decoder.decompress(&frames)?, b"first frame, second frame");

    // Garbage and over-limit frames are errors, not panics.
    assert!(decoder.decompress(b"not a frame").is_err());
    let big = encoder.compress(&vec![7u8; 16 << 20])?;
    assert!(matches!(
        decoder.decompress(&big),
        Err(DecodeError::OutputLimitExceeded { .. })
    ));
    let _ = DecoderConfig::from(limits);
    Ok(())
}
```

A frame that needs a codec whose Cargo feature is off, an unregistered custom
codec, or a format version outside 21 to 27 reports `DecodeError::Unsupported`.
Custom codecs are supported through `CustomCodec`, `GraphBuilder::custom_codec`
and `DecompressorBuilder::with_custom_codec`; see [custom codecs](codecs.md#custom-codecs).
A frame compressed with a dictionary bundle the decoder does not hold reports
`DecodeError::MissingBundle`;
see [dictionaries](dictionaries.md). See [decoder mechanics](../architecture/decompressor.md)
and [the codec catalogue](../architecture/decoder-codecs.md).

### Frame metadata

`Decompressor::frame_info(&frame)` (or `FrameInfo::try_from(&frame[..])`) reads the
frame header without decoding any chunk: the format version, every output's type,
size and element count (`ZL_FrameInfo_getOutputType`, `getDecompressedSize`,
`getNumElts`; struct and numeric outputs have no element count, as in the
reference), the checksum flags, the header comment and the dictionary bundle ID.

```rust
use mopenzl::graph::Comment;
use mopenzl::{Compressor, DataType, Decompressor};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = Compressor::default();
    encoder.set_comment(Some(Comment::try_from(&b"nightly export"[..])?))?;
    let frame = encoder.compress(b"payload")?;

    let info = Decompressor::default().frame_info(&frame)?;
    assert_eq!(info.comment(), Some(&b"nightly export"[..]));
    assert_eq!(info.outputs()[0].data_type(), DataType::Serial);
    assert_eq!(info.decompressed_size(), 7);
    assert!(info.bundle_id().is_none());
    Ok(())
}
```

## Configuration

`EncoderConfig` holds the global parameters shared by successive frames. They are
the reference's global parameters and shape the bytes of a frame.

| Setting | Type | Behavior |
| --- | --- | --- |
| Level | `Level` | `-131072..=22`; `0` selects the default, 6. Standard graphs hand it to Zstandard and to level-sensitive codecs |
| Format version | `FormatVersion` | 21 to 27; default 27. Older versions disable newer codecs and features (comments need 22, dictionaries 25) |
| Content checksum | `bool` | An XXH3 checksum of each chunk's content; default on |
| Compressed checksum | `bool` | An XXH3 checksum of each chunk's compressed bytes plus a frame-header check byte; default on |
| Chunking | `Chunking` | `Whole` (default, as `ZL_compress`) or `Fixed(ChunkSize)`, the serial segmenter, `32 KiB..=i32::MAX` |
| Decompression level | `u8` | Trades ratio for decoder speed in some codecs; `0` selects the default, 3. At level 1 Zstandard literals are stored uncompressed |
| Minimum stream size | `u32` | A lone stream below it is stored; `0` selects the default, 10 |
| Store on expansion | `bool` | A chunk that would not shrink is stored instead; default on |
| Permissive | `bool` | A failing successor is replaced by `compress_generic`; default off |

`Compressor::set_comment(Some(Comment))` stores 1 to 10 000 bytes in the frame
header (format version 22 or newer). Unlike the reference, which forgets the
comment after each compression, it stays until you change it. The comment is
carried in the frame but not returned by the decoder.

`StreamConfig` carries the declared input size of a session; its default is
`InputSize::Unknown`. For a complete known input use
`StreamConfig::from(InputSize::Exact(len))`.

<!-- cfg: feature = "zstd" -->
```rust
use mopenzl::graph::Comment;
use mopenzl::{Compressor, Decompressor, EncoderConfig, FormatVersion, Level};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = EncoderConfig::default()
        .with_level(Level::try_from(19)?)
        .with_format_version(FormatVersion::try_from(24)?)
        .with_content_checksum(false)
        .with_min_stream_size(32)
        .with_store_on_expansion(true);
    let mut encoder = Compressor::new(config)?;
    encoder.set_comment(Some(Comment::try_from(&b"produced by the docs"[..])?))?;

    let frame = encoder.compress(&b"configured payload ".repeat(100))?;
    assert_eq!(
        Decompressor::default().decompress(&frame)?,
        b"configured payload ".repeat(100)
    );

    // A comment needs format version 22; version 21 refuses it.
    let mut old = Compressor::new(config.with_format_version(FormatVersion::try_from(21)?))?;
    assert!(
        old.set_comment(Some(Comment::try_from(&b"too old"[..])?))
            .is_err()
    );
    Ok(())
}
```

Validated values (`Level`, `FormatVersion`, `ChunkSize`, `Comment`) reject
out-of-range inputs when built. `Compressor::new` and `reconfigure` check
combinations.

## Output buffers

`compress` returns a new `Vec<u8>`. `compress_into` appends to the vector you pass
and returns the range that holds the new frame; existing bytes stay intact.
`compress_to_slice` returns the number of bytes written. Size the slice with
`Compressor::max_compressed_size`:

```rust
use mopenzl::{Compressor, Decompressor, EncodeError};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"data to compress";
    let mut encoder = Compressor::default();
    let bound = Compressor::max_compressed_size(input.len())?;
    let mut output = vec![0; bound];
    let written = encoder.compress_to_slice(input, &mut output)?;
    output.truncate(written);
    assert_eq!(output, encoder.compress(input)?);

    // A slice that is too small is an error, not a truncated frame.
    let mut cramped = [0u8; 4];
    assert!(matches!(
        encoder.compress_to_slice(input, &mut cramped),
        Err(EncodeError::OutputTooSmall { provided: 4 })
    ));

    // Decompression into a slice reports how much it wrote.
    let mut restored = [0u8; 64];
    let n = Decompressor::default().decompress_to_slice(&output, &mut restored)?;
    assert_eq!(&restored[..n], input);
    Ok(())
}
```

### Uninitialized memory

`compress_to_uninit`, `decompress_to_uninit` and the sessions' `process_uninit`
write into `&mut [MaybeUninit<u8>]`, such as `Vec::spare_capacity_mut`, without
zeroing it first. They produce the same bytes, counts and errors as the
initialized APIs; on success exactly the reported prefix is initialized and nothing
after it is written. Reading the prefix back is the caller's `unsafe` step. See
[uninitialized output](../architecture/uninit-output.md).

```rust
use mopenzl::{Compressor, Decompressor};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"uninitialized destination ".repeat(50);
    let mut encoder = Compressor::default();
    let mut frame: Vec<u8> = Vec::with_capacity(Compressor::max_compressed_size(input.len())?);
    let written = encoder.compress_to_uninit(&input, frame.spare_capacity_mut())?;
    // SAFETY: `compress_to_uninit` initialized the first `written` bytes.
    unsafe { frame.set_len(written) };
    assert_eq!(frame, encoder.compress(&input)?);

    let mut restored: Vec<u8> = Vec::with_capacity(input.len());
    let n = Decompressor::default().decompress_to_uninit(&frame, restored.spare_capacity_mut())?;
    // SAFETY: `decompress_to_uninit` initialized the first `n` bytes.
    unsafe { restored.set_len(n) };
    assert_eq!(restored, input);
    Ok(())
}
```

## Reusing memory

A compressor or decompressor retains working state between operations: hash
tables, stream buffers and the Zstandard context. Compatible calls reuse the
allocations. Destination capacity is yours to manage.

| Retention policy | Effect |
| --- | --- |
| `Aggressive` (default) | Keeps every buffer between operations |
| `Bounded { max_bytes }` | Releases retained storage when its accounting exceeds the ceiling |
| `ReleaseAll` | Releases storage after each operation |

Set the policy with `Compressor::builder(config).with_retention(policy).build()`
(`Decompressor::builder` likewise). Use `trim(policy)` for an immediate cleanup and
`retained_bytes()` for the owned heap size; it excludes caller buffers and
allocator overhead, and a ceiling is not a peak-memory limit. The policy never
changes the bytes produced.

```rust
use mopenzl::{Compressor, EncoderConfig, RetentionPolicy};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = Compressor::builder(EncoderConfig::default())
        .with_retention(RetentionPolicy::Bounded { max_bytes: 1 << 20 })
        .build()?;
    encoder.compress(&b"warm the tables up ".repeat(1000))?;
    println!("retained {} bytes", encoder.retained_bytes());
    encoder.trim(RetentionPolicy::ReleaseAll);
    assert_eq!(encoder.retained_bytes(), 0);
    Ok(())
}
```

For simultaneous independent streams use one codec per worker; a
`CompressorGraph` is cheap to clone and share between them. A compressor
configured with a graph keeps it; `Compressor::builder(config).with_graph(graph)`.

## Streaming and completion

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

## Errors and further reading

`ConfigError` covers configuration, `EncodeError` covers compression (including
`EncodeError::Graph(GraphError)` for graph failures), `DecodeError` covers
decompression, `graph::GraphError` covers graph construction, and `dict::DictError`
covers dictionaries. I/O adapters return `std::io::Error`; writer finalization
preserves the writer in `FinishError`.

- [Graphs](graphs.md), [codecs](codecs.md), [structured data](structured-data.md)
  and [dictionaries](dictionaries.md)
- [Compressor mechanics](../architecture/compressor.md) and
  [compression graphs](../architecture/compressor-graphs.md)
- [Decompressor mechanics](../architecture/decompressor.md)
- [Codec features](../architecture/codec-features.md)
- [Benchmarking](benchmarking.md) and [development checks](development.md)
