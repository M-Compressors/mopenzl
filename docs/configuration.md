# Configuration and features

Four things are configured separately: the **graph** (which codecs run), the
**encoder** (global parameters shared by successive frames), the **stream**
(what you know about one input), and the **Cargo features** (which codecs and
adapters are compiled in). Graphs have their own guide: start with
[graphs](graphs.md). Decoder settings are covered in
[decompression and limits](decompression.md).

## Encoder settings

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
comment after each compression, it stays until you change it. Read the comment with `Decompressor::frame_info`; the decompressed payload
contains only the original input.

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

## Cargo features

The default feature set is `std`, `compression`, `decompression` and
`all-codecs`. `all-codecs` enables one Cargo feature per OpenZL codec (`zstd`,
`entropy`, `lz4`, `field-lz`, `delta`, `tokenize`, `custom-codec`, ...); the
[codec guide](codecs.md#features) lists them all.

| Configuration | Dependency |
| --- | --- |
| Default std encoder + decoder | `mopenzl = "0.2.4"` |
| Encoder only | `mopenzl = { version = "0.2.4", default-features = false, features = ["std", "compression"] }` |
| Decoder only | `mopenzl = { version = "0.2.4", default-features = false, features = ["std", "decompression"] }` |
| Decoder for default-graph frames only | `mopenzl = { version = "0.2.4", default-features = false, features = ["std", "decompression", "zstd"] }` |
| Indexed resources with independent graphs | `mopenzl = { version = "0.2.4", features = ["framed"] }` |
| Alloc-backed codecs without std | `mopenzl = { version = "0.2.4", default-features = false, features = ["no_std", "compression", "decompression"] }` |

Add the codec features you need, such as `zstd` or `all-codecs`, when disabling
defaults. Without a codec's feature the encoder never writes it (a chunk that
would use it is stored, so frames stop matching the reference's default graph)
and the decoder reports frames that use it as `DecodeError::Unsupported`. Frames
of the default graph need only `zstd`.

### `no_std`

`no_std` takes precedence when enabled. It requires a global allocator, removes
the `std::io` adapters, parallel APIs and runtime SIMD detection (the
compile-time baseline is used), and pulls in `libm` for the estimators the
numeric selectors rank candidates with.

**Caveat:** `libm`'s `expf` differs from the platform's by one ULP for some
inputs, so the numeric transformer selector's scores can differ slightly from
the reference's; decisions change only on exact ties (none were observed in
4000+ frames), but byte identity of encoder decisions that depend on floating
point is not guaranteed under `no_std`. Cargo features are additive: another
dependency can re-enable `std`.

See [codec features](../architecture/codec-features.md) for the gates and what
each build exposes.
