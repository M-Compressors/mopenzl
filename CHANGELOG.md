# Changelog

## [v0.2.0](https://github.com/M-Compressors/mopenzl/releases/tag/v0.2.0) - 2026-11-10

Every non-legacy part of OpenZL v0.3.0 (format versions 21 to 27) is now
ported, with frames byte-identical to the reference for the same graph and
parameters.

### Breaking changes

- `CompressorBuilder` and `DecompressorBuilder` are no longer `Copy` (they can
  hold a compressor graph and a dictionary store).
- `Unsupported::Dictionary` is removed: dictionary frames decode now, and a
  missing bundle is `DecodeError::MissingBundle`.
- The default features are `std`, `compression`, `decompression` and the new
  `all-codecs`, which enables one feature per codec.
- `mzstd` 0.2.12 is required (magicless frames, `CParameter`, dictionaries
  with `refCDict` semantics, `ZSTD_c_targetCBlockSize`).

### Compression

- **Graph engine.** A private port of OpenZL's runtime (`cctx.c`): typed
  streams with zero-copy references, runtime graph numbering, function graphs
  with the reference's edge-handle successor order, selectors, segmenters,
  implicit conversions, store graphs, the min-stream-size rule, store on
  expansion, permissive fallback, limits and trial compression (`try_graph`).
  The registry lists every standard node and graph.
- **Every standard node encoder:** conversions and string separation, zstd
  (every `ZSTD_c_*` local parameter including `targetCBlockSize`,
  uncompressed literals at decompression
  level 1, block splitting, dictionaries), LZ4 (fast, HC and the optimal
  parser, ported from lz4 1.10.0), FSE, Huffman (byte and 16-bit) and pivco
  Huffman from OpenZL's own entropy fork, constant, bitpack, bitunpack,
  flatpack, delta, zigzag, range_pack, divide_by, merge_sorted, partition,
  sparse_num, sentinel, bitSplit, float deconstruction, tokenize, splitN,
  split_by_struct, dispatchN_byTag, dispatch_string, concat, dedup,
  interleave, prefix, parse_int, transpose_split, field_lz, lz, mux_lengths
  and quantize.
- **Every standard graph:** store, compress_generic and its typed routers,
  fse, huffman, huf0, pivco, entropy, constant, zstd, lz4, bitpack, flatpack,
  field_lz with its literals graph and channel selector, lz,
  compress_small_lengths, partition_bitpack, merge_sorted, try_parse_int, the
  split graphs, select_numeric and the whole transformer family (scorer tables
  generated from the reference by `scripts/gen_transformer_scorers.py`),
  ml_selector (GBT models), clustering, sddl, sddl2, brute_force and the
  serial, numeric and num-from-serial segmenters.
- **Typed and multiple inputs** (`mopenzl::input`: `Input`, `Numbers`,
  `Records`, `Strings`, int metadata), `Compressor::compress_inputs` and
  `compress_inputs_into`.
- **Graph API** (`mopenzl::graph`): `StandardGraph` and `StandardNode` with
  the reference IDs, `GraphBuilder` (static graphs, pipelines, parameterized
  nodes and graphs, user `Selector`, `FunctionGraph` and `Segmenter`
  implementations, typed builders for the serial and num-from-serial
  segmenters, brute force, ml_selector, clustering, sddl and sddl2),
  `CompressorGraph`, `LocalParams`, `NodeParams`, `GraphParams`, the graph,
  selector and segmenter contexts (including `run_split_node`,
  `run_dispatch_node`, `run_dispatch_string_node` and `try_graph`),
  `CompressorBuilder::with_graph`.
- **Header comments** (`Comment`, `Compressor::set_comment`, v22+) and the
  `EncoderConfig` settings `decompression_level`, `min_stream_size`,
  `store_on_expansion` and `permissive`.

- **Named parameter IDs.** `graph::params` names the reference's local
  parameter and metadata IDs, one module per codec or graph (`zstd`, `lz`,
  `lz4`, `field_lz`, `entropy`, `divide_by`, `bitunpack`, `partition`,
  `sparse_num`, `sentinel`, `tokenize`, `split`, `dispatch`, `mux_lengths`,
  `segmenters`, `sddl`, `sddl2`, `clustering`, ...), each citing its C macro.
  The encoders read the same constants.
- **Documented successors.** Every `StandardGraph` variant that needs custom
  graphs or nodes says which, and links its builder helper;
  `GraphBuilder::num_from_serial_segmenter` documents that its successor
  receives each chunk as serial data, with an example.

### Decompression

- **Universal decoder.** Every standard codec valid at v21+ decodes; the
  engine runs any node graph with typed streams, variable inputs and outputs
  and a per-stream allocation budget, tested against C-made frames including
  damaged ones.
- **Typed and multiple outputs.** `Decompressor::decompress_typed` returns
  one `TypedOutput` (`DataType`, width, content, string lengths) per frame
  output.
- Zstandard frames decode natively in the magicless format, without the
  staged copy; slice and uninit destinations are up to ~9% faster on text.
- **Frame metadata.** `Decompressor::frame_info` and
  `FrameInfo::try_from(&[u8])` read a frame header without decoding a chunk:
  `FrameInfo` (format version, outputs, checksum flags, comment, bundle ID)
  and `OutputInfo` (type, size, element count), mirroring `ZL_FrameInfo_*`
  and tested against C. `DecodeOperation` and `Operation` cross-reference each
  other, and the user guide explains why the adapters return no `Result`.

### Dictionaries

- Bundles for v25+ in both directions: `mopenzl::dict` (`DictId`, `BundleId`,
  `Dict`, `DictBundle`, `BundleStore`), `GraphBuilder::load_dictionaries`,
  `NodeParams::with_dictionary`, the frame bundle ID and per-chunk dictionary
  lane, and `DecompressorBuilder::with_dictionaries`. Zstandard dictionaries
  (raw or trained) are byte-identical to `ZSTD_createCDict_advanced` +
  `ZSTD_CCtx_refCDict`.

### Fixed

- The decoder rejected frames whose codec stream counts (variable inputs,
  regenerated streams) wrap at 32 bits; the reference reads them as
  `(uint32_t)varint + 1` and `+ 2`, and so does mopenzl now. Found by the
  `decompress` fuzz target.
- The `partition` encoder panicked in debug builds on a custom partition
  larger than 2^63; its bit widths now follow `ZL_nextPow2`. Found by the
  `node_roundtrip` fuzz target.

### Platform and tooling

- `libm` is an optional dependency enabled by `no_std`; the estimators and
  scorers use the platform libm with `std`. Under `no_std` a transformer or
  estimator decision may differ from the reference at an exact score tie
  (`libm`'s `expf` can differ by one ULP).
- `openzl-ffi` builds C with `-ffp-contract=off`, and gained graph, typed
  input/output, comment, dictionary and per-family oracles.

### Fuzzing

- Four new AFL targets, each with a libFuzzer adapter for ClusterFuzzLite, seeds and
  replayed regressions: `decompress_typed` (arbitrary bytes into
  `Decompressor::decompress_typed` and `frame_info` under input and output
  limits; `frame_info` agrees with the decoded outputs, and the C reference
  agrees on the typed outputs), `typed_roundtrip` (one to four typed inputs
  through a fixed list of standard graphs and `compress_inputs`, decoded back
  and compared with C), `graph_configs` (dictionary bundles, ML selector and
  clustering CBOR, SDDL descriptions and SDDL2 bytecode, then a compressor built
  from whatever parses) and `node_roundtrip` (any standard node with integer
  and copy parameters, byte-identical to the reference frame, decoded back).
- `examples/codec_seeds.rs` asks the C reference for one frame per standard node
  and graph, so `decompress` and `decompress_typed` start with every decoder on
  their path; `prepare-seeds.sh` runs it.
- Findings left open (regression inputs fail `cargo afl test`): `ZL_decompress`
  decodes a 43-byte frame that `decompress` rejects with `InvalidData { kind:
  Graph }`, and `partition` with a custom size above 2^63 overflows in
  `Partition::bits`.
- `cargo afl test` replays any corpus named by `FUZZ_REPLAY` for coverage runs.

## [v0.1.0](https://github.com/M-Compressors/mopenzl/releases/tag/v0.1.0) - 2026-11-09

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
