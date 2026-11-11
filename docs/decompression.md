# Decompression and limits

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

## Set limits for untrusted input

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

## Frame metadata

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

## Where to go next

| Task | Read |
| --- | --- |
| Decode chunks as they arrive | [Sans I/O decompression](sans-io-decompression.md) |
| Decode from a `Read` source or into a `Write` sink | [Read / Write I/O](io.md) |
| Decode typed and multiple outputs | [Structured data](structured-data.md) |
| Decode a framed container | [Framing containers](frames.md) |
| Supply a dictionary bundle | [Dictionaries](dictionaries.md#decoding-with-a-bundlestore) |
