# mopenzl user guide

- **Compress**: `Compressor::new(EncoderConfig)` then `compress`,
  `compress_into` (appends to a `Vec`) or `compress_to_slice` (size the slice
  with `Compressor::max_compressed_size`). Frames are byte-identical to
  OpenZL v0.3.0's `ZL_CCtx_compress` with the same level, format version and
  checksum settings.
- **Decompress**: `Decompressor::new(DecoderConfig)` then `decompress`,
  `decompress_into` or `decompress_to_slice`. Frames of versions 21 to 27
  whose chunks are stored or Zstandard decode; others report
  `DecodeError::Unsupported`.
- **Uninitialized memory**: `compress_to_uninit`, `decompress_to_uninit` and
  the sessions' `process_uninit` write into `Vec::spare_capacity_mut` and
  similar without zeroing.
- **Stream**: `Compressor::start(StreamConfig)` / `Decompressor::start()` give
  sans-I/O sessions; `writer`/`reader` wrap them in `std::io`.
- **Bound memory**: declare `InputSize::Exact` and use
  `Chunking::Fixed(ChunkSize)` so the encoder holds one chunk and decoders
  deliver output chunk by chunk.
- **Untrusted input**: set `DecodeLimits::with_max_input_bytes` and
  `with_max_output_bytes`; frames declaring more output fail before decoding.
- **Reuse**: keep codecs alive across payloads; `RetentionPolicy` controls
  what they keep between operations.

See the crate documentation for examples and [architecture](../architecture/README.md)
for mechanics.
