# Changelog

## Unreleased

- **Initial port of OpenZL v0.3.0's serial compression path.**
  `Compressor` writes frames byte-identical to `ZL_CCtx_compress` with the
  default graph (stored chunks below ten bytes, Zstandard through `mzstd`,
  store on expansion) for format versions 21 to 27, levels `-131072..=22`,
  either checksum switch and `Chunking::Fixed` (the serial segmenter). The
  encoder sessions write the one-shot frame for any split of input and output,
  hold one chunk of input when the size is declared, and cut a chunk on flush.
- **Decoder.** `Decompressor` decodes frames of versions 21 to 27 built from
  stored and Zstandard chunks, verifying both checksums, with single or
  concatenated frame modes, input and output limits, sans-I/O sessions that
  never consume bytes past the frame, and precise `Unsupported` errors for
  codecs not ported yet.
- **Uninitialized output.** `Compressor::compress_to_uninit`,
  `Decompressor::decompress_to_uninit` and `process_uninit` on both sessions
  write into `MaybeUninit` memory without `unsafe` or zeroing (stored bytes
  and compressed frames) and match their initialized counterparts byte for
  byte; `Vec` destinations of the decoder are no longer zero-filled before
  Zstandard decodes into them.
- **Per-codec features.** Every OpenZL codec has its own Cargo feature, all
  enabled by default; `zstd` is the first. Without it the encoder stores
  every chunk and the decoder reports Zstandard chunks as `Unsupported`, and
  `mzstd` is not compiled.
- **Fixed** the decoder accepting frames the reference rejects: frames
  without chunks (now `InvalidData { kind: EmptyFrame }`) and single-output
  frames with type bits for a second output. `ContentSizeMismatch` no longer
  reports a codec's claimed size as decoded bytes. Found by the `decompress`
  fuzz target.
- **SIMD XXH3.** The checksum's stripe accumulator runs on `fearless_simd` at
  the backend selected once per codec, checked against a scalar baseline and
  the C hash.
- **Tooling.** `openzl-ffi` builds the pinned C library (with zstd 1.5.7 and
  lz4 1.10.0) as test and benchmark oracle; differential, streaming, I/O and
  backend tests; Criterion benchmarks against C; architecture specifications.
- **Fixed** `EncoderWriter` losing track of accepted input when the sink
  failed after the session took it; the input is now reported as written and
  its compressed bytes stay pending.
