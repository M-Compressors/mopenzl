# Changelog

## Unreleased

## [v0.2.1](https://github.com/M-Compressors/mopenzl/releases/tag/v0.2.1) - 2026-11-10

### Added

- Add hotpath CI profiles for the compressor and Criterion compression and
  decompression cases, merging native timing and allocations with Callgrind
  instruction counts. Pushes upload to hotpath-cloud using OIDC; pull requests
  publish artifacts for a separate relay workflow. Add `hotpath-callgrind`
  and `hotpath-cloud` features and a repeatable `profile_compressor` example.
- `custom-codec` (enabled by default): `CustomCodec`, typed callback inputs,
  validated signatures and encoder/decoder contexts; graph-node registration
  and custom decoder registration by wire ID, including private headers,
  variable ports, parameterization and streaming. Unregistered custom IDs
  continue to return `Unsupported`. The [user guide](docs/codecs.md#custom-codecs)
  documents registration, supported stream types and feature selection.


Performance work across the codec families, measured on the whole Silesia
corpus against OpenZL v0.3.0 C (`-O3`). Frames are unchanged: every one of the
123 benchmarked graph/file pairs is byte-identical to C. Figures are medians of
per-file C time / Rust time, before → after
([results](docs/benchmarks/README.md)).

### Performance

- **PivCo-Huffman:**
  - Table-driven 16-byte merge and partition kernels, plus AVX-512
    expand/compress kernels.
  - SIMD flat leaves and packing, allocation-free trees.
  - Compress 0.09× → 1.05×, decompress 0.05× → 1.03×.
  - `lz` decoding, which uses it, went from 0.44× to 1.46×.
- **Huffman and FSE:**
  - Huffman decodes through a pair table with sentinel refills, four lanes and
    several symbols per refill. FSE decodes four symbols per refill.
  - The Huffman encoder uses a top-aligned two-container writer like
    `HUF_addBits`. FSE and the Huffman weights use a 64-bit word writer.
  - Fixed-size tables and 8-byte histogram loads.
  - The four Huffman lanes decode in lockstep over output quarters with a
    stack-resident pair table filled in runs; 16-bit Huffman decodes its four
    blocks in lockstep.
  - FSE decodes through a shift-only loop when no cell reads zero bits, builds
    tables with the reference's fast spread, and encodes with states in
    locals into a fixed-size word buffer.
  - The Huffman encoder looks up scale-8 entry tables and adds codes in groups.
  - The Huffman lane loop looks up all four lanes before storing, writes
    through shrinking per-lane window slices and indexes a pair table of
    `2^log` cells by the top bits; it executes fewer instructions than C.
  - Huffman: compress 0.40× → 0.94×, decompress 0.20× → 0.78×. FSE: 0.44× →
    0.95×, 0.29× → 0.88×.
- **LZ4:**
  - Short- and long-sequence fast paths with chunked copies in the decoder.
  - A const-generic HC chain walk with hoisted candidate reads and 8-byte
    `count`/`count_back`.
  - Fixed-size HC tables, an inlined HC match search, and single-compare
    bounds on its reads.
  - The decoder's fast loop checks one 288-byte input and 640-byte output
    window per sequence and keeps long-match copies out of line. Past the
    first 64 KiB of output, sequences run in one fixed-size window where
    every offset is valid, with C's 8+8+2-byte short-match copy.
  - Compress 0.78× → 0.95×, decompress 0.31× → 0.92×.
- **`lz` and `field_lz`:**
  - 128-byte row-table rows with a `fearless_simd` tag compare and fused
    candidate scoring.
  - Parsers monomorphized per SIMD level and field width.
  - Chunked match copies in the decoders, and table-driven quantize.
  - The field_lz parse warms the rows it will probe with stores a few
    positions ahead, keeps rows on two cache lines, and fixes the hash length
    at compile time.
  - The field_lz decoder runs four tokens per group, resolves their repeat
    offsets from a 256-entry table (C's AVX2 shuffle, in scalar form) and
    copies short runs as fixed 16-byte blocks; it is at C's instruction count.
  - `lz` compress 0.49× → 0.83×, decompress 0.44× → 1.46×.
  - `field_lz` on 4-byte records: compress 0.60× → 0.83×, decompress 0.32× →
    0.76×.
- **bitpack, flatpack, transpose, tokenize, partition:**
  - SIMD bit counting and grouped packing in bitpack.
  - 8-per-word packing and table-driven decode in flatpack.
  - A SIMD byte transpose (`shared::byte_transpose`).
  - Hashed tokenize and specialized gathers.
  - A 64-bit partition bit reader.
  - bitpack compress 0.08× → 1.77×. flatpack 0.73× → 1.47× (compress) and
    1.43× → 1.49× (decompress).
- **Engine:**
  - The decoder hands the last regenerated stream to an empty `Vec`
    destination or `TypedOutput` instead of copying it.
  - A single typed output is checksummed in place.
  - `compress` and `compress_inputs` start their frame `Vec` at the compression
    bound instead of growing it.
- **Numeric graphs** (16/32-bit numbers through the numeric selector): compress
  0.48–0.62× → 0.83–0.94×, decompress 0.27–0.37× → 0.71–0.83×.

### Tooling

- `benchmarks/comparison`: a harness that runs every standard graph on Silesia
  against C. It has `verify`, `run` and `profile` (callgrind/perf/hotpath)
  commands. `report.py` renders charts and tables into `docs/benchmarks/`.
- `openzl-ffi` reads `OPENZL_FFI_OPT_LEVEL` to build the C library at `-O3`
  for benchmarks (tests keep `-O2`).
- The SIMD level is plumbed to every codec: `NodeCx::simd` for encoders and
  `State::simd` for decoders.

### Fixes

- `lz_common::count`'s padded fallback path is bounds-safe for runs that end
  past the data.

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
