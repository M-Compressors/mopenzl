# mopenzl

[OpenZL](https://github.com/facebook/openzl) compression and decompression in
safe Rust.

- **Byte-identical encoder.** For the same input and global parameters, frames
  are exactly those of OpenZL v0.3.0's `ZL_CCtx_compress` with its default
  graph, or of its serial segmenter: format versions 21 to 27, levels
  `-131072..=22`, content and compressed checksums.
- **Decoder** for frames built from the default graph's codecs (stored and
  Zstandard chunks), with both checksums verified, single or concatenated
  frames, input and output limits, and explicit errors for codecs not ported
  yet.
- **No `unsafe`.** The checksum kernel uses [`fearless_simd`] with the
  instruction set detected once per codec; every backend writes the same
  bytes. Zstandard coding comes from [`mzstd`], byte-identical to the zstd
  1.5.7 OpenZL vendors.
- **One feature per codec**, all on by default (`zstd` today).
- **Sans-I/O sessions** for both directions, plus `std::io` adapters, and
  `no_std` + `alloc` support.

```rust
use mopenzl::{Compressor, Decompressor, EncoderConfig, Level};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = Compressor::new(EncoderConfig::from(Level::try_from(6)?))?;
    let frame = encoder.compress(b"format-aware compression")?;
    assert_eq!(Decompressor::default().decompress(&frame)?, b"format-aware compression");
    Ok(())
}
```

## Status

This is the first phase of a port of the OpenZL framework: the serial
compression path and the universal decoder's frame, chunk and stream-graph
machinery. Other OpenZL codecs (entropy coders, field LZ, transforms),
typed and multiple inputs, custom graphs, training and dictionaries are not
ported yet; frames using them report `DecodeError::Unsupported`. See
[architecture](architecture/README.md) for the mechanics and known gaps and
[the comparison](architecture/benchmark-comparison.md) for performance against
the C library.

## License

MIT OR Apache-2.0. Derived from OpenZL (BSD); see [NOTICE](NOTICE).

[`fearless_simd`]: https://crates.io/crates/fearless_simd
[`mzstd`]: https://crates.io/crates/mzstd
