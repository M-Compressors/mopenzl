# mopenzl

**OpenZL compression and decompression in safe Rust.**

A port of Meta's [OpenZL] v0.3.0, the format-aware compression framework, with a
Rust-native API. Compress serial bytes with the default graph or typed, multiple
inputs with any standard or custom graph; register application-defined codecs for
compression and decompression; decode every standard codec with one universal
decoder; reuse working memory between payloads; integrate with
`std::io::Read` and `std::io::Write`; and drive either direction incrementally.

[![Crates.io](https://img.shields.io/crates/v/mopenzl.svg)][crate]
[![docs.rs](https://docs.rs/mopenzl/badge.svg)][api]
[![Tests](https://github.com/M-Compressors/mopenzl/actions/workflows/ci.yml/badge.svg?branch=main)][ci]
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

[Quick start](#quick-start) · [Benchmarks](#performance) · [Structured data](#compress-structured-data) · [`Read` / `Write`](#read-and-write-streaming) · [User guide][guide] · [Graphs][graphs] · [Compatibility](#compatibility) · [Changelog][changelog]

## Why mopenzl?

- **Safe implementation, not a C wrapper.** Production code in this crate forbids
  `unsafe`. Zstandard comes from the safe-Rust [`mzstd`]; every other codec of the
  OpenZL graph is ported here. SIMD kernels use [`fearless_simd`].
- **Byte-identical serial encoder.** For the same input, graph and global parameters the
  frames are exactly those of OpenZL v0.3.0, at format versions **21 to 27**. See
  [compatibility](#compatibility) for the conditions.
- **Parallel compression with your executor.** The mbrotli-style
  `compressor::parallel` API offers tasks, sources, disk staging, sessions and
  I/O adapters. Output is OpenZL-compatible; byte identity is not guaranteed.
  See [parallel compression](docs/parallel.md).
- **Universal decoder.** One decoder reads the frame of every standard codec,
  including frames with typed and multiple outputs
  ([`Decompressor::decompress_typed`]).
- **Indexed resources with independent graphs.** Opt-in `framed` containers
  preserve typed outputs, resource metadata and whole-resource seek. Each
  resource is one ordinary OpenZL frame. See [framed containers](docs/framing.md).
- **Graphs, typed inputs and dictionaries.** Every standard node and graph is
  ported (`compress_generic`, entropy coders, LZ families, the numeric
  transformer selector, `ml_selector`, clustering, SDDL and SDDL2, segmenters,
  `brute_force`, ...). Build your own with `GraphBuilder`, including Rust
  selectors, function graphs and segmenters. Zstandard dictionary bundles work in
  both directions.
- **Custom codecs.** Implement `CustomCodec` and register encoder nodes with
  `GraphBuilder::custom_codec` and matching decoders with
  `DecompressorBuilder::with_custom_codec`. Typed streams, multiple and variable
  ports, private headers and streaming are supported. The `custom-codec` feature
  is enabled by default; see the [custom codec guide](docs/codecs.md#custom-codecs).
- **Sans-I/O sessions.** `EncoderSession` and `DecoderSession` work only with the
  buffers you pass in and never perform I/O, so you can drive them from blocking
  code, async runtimes or your own event loop.
- **Reusable state.** Keep compressors, decompressors and output buffers across
  requests instead of rebuilding them for every payload.
- **Select only the codecs you need.** One Cargo feature per codec, collected in
  `all-codecs`; `no_std` with `alloc` is supported.

## Performance

### Silesia, every codec family

Each standard graph compresses each [Silesia](http://sun.aei.polsl.pl/~sdeor/index.php?page=silesia)
file whole and is timed against OpenZL v0.3.0 C (built at `-O3`) in the same
process. Every frame is byte-identical to C's, so compressed sizes are equal by
construction. The chart and table give each implementation's throughput in
MB/s: mopenzl matches or beats C on PivCo-Huffman, flatpack, bitpack and `lz`
decoding, and is within about 10% of C on Zstandard, LZ4, FSE and Huffman
compression. Huffman decoding and the numeric and field_lz graphs remain
behind C.

<details>
<summary>Silesia benchmark results and methodology</summary>

![Throughput of OpenZL C and mopenzl per graph on Silesia, in MB/s][silesia-chart]

**Recorded 2026-11-10 · AMD Ryzen 9 7950X (KVM guest, 2 vCPUs, host shared) ·
rustc 1.99.1 · release profile · C at `-O3`.** Library defaults (level 6, format
version 27, both checksums). MB/s is 10⁶ uncompressed bytes per second; a
graph's figure is its total bytes over its total median time, and "/ C" is
mopenzl's speed relative to C.

| Graph | Files | C compress | mopenzl compress | / C | C decompress | mopenzl decompress | / C |
| --- | --- | --- | --- | --- | --- | --- | --- |
| zstd | 12 | 142 MB/s | 142 MB/s | 0.99× | 1,304 MB/s | 1,231 MB/s | 0.94× |
| lz4 | 12 | 80.2 MB/s | 75.7 MB/s | 0.94× | 2,552 MB/s | 2,394 MB/s | 0.94× |
| lz | 12 | 114 MB/s | 96.1 MB/s | 0.85× | 677 MB/s | 1,036 MB/s | 1.53× |
| huffman | 12 | 1,125 MB/s | 1,052 MB/s | 0.94× | 1,591 MB/s | 1,235 MB/s | 0.78× |
| fse | 12 | 619 MB/s | 597 MB/s | 0.96× | 1,090 MB/s | 955 MB/s | 0.88× |
| entropy | 12 | 1,075 MB/s | 989 MB/s | 0.92× | 1,507 MB/s | 1,198 MB/s | 0.79× |
| huffman_pivco | 12 | 967 MB/s | 1,054 MB/s | 1.09× | 2,325 MB/s | 2,422 MB/s | 1.04× |
| flatpack | 12 | 985 MB/s | 1,425 MB/s | 1.45× | 2,733 MB/s | 4,073 MB/s | 1.49× |
| generic | 12 | 145 MB/s | 141 MB/s | 0.97× | 1,541 MB/s | 1,487 MB/s | 0.96× |
| field_lz_struct4 | 3 | 89.5 MB/s | 73.2 MB/s | 0.82× | 1,946 MB/s | 1,398 MB/s | 0.72× |
| bitpack_num16 | 2 | 4,721 MB/s | 8,438 MB/s | 1.79× | 7,666 MB/s | 17,192 MB/s | 2.24× |
| numeric16 | 2 | 28.3 MB/s | 26.7 MB/s | 0.95× | 1,020 MB/s | 810 MB/s | 0.79× |
| numeric32 | 3 | 88.8 MB/s | 75.9 MB/s | 0.85× | 1,840 MB/s | 1,241 MB/s | 0.67× |
| generic_num16 | 2 | 28.6 MB/s | 25.2 MB/s | 0.88× | 964 MB/s | 799 MB/s | 0.83× |
| generic_num32 | 3 | 80.9 MB/s | 71.5 MB/s | 0.88× | 1,810 MB/s | 1,188 MB/s | 0.66× |

Single runs of five alternating rounds on a shared host: individual cells are
good to about ±10%.

[Per-graph and per-file results][silesia] · [Raw measurements][silesia-csv] · [Reproduce][benchmarking]

</details>

### Default graph (Criterion)

The [implementation comparison](architecture/benchmark-comparison.md) also
records the serial default graph against OpenZL v0.3.0 on small corpora
(Apple M5 Pro, initial port), where Zstandard decides: those rows mirror
`mzstd` against zstd, and incompressible inputs are stored, where the SIMD
checksum and the absence of per-call setup make mopenzl faster.

[silesia]: ./docs/benchmarks/README.md
[silesia-chart]: ./docs/benchmarks/charts/overview.svg
[silesia-csv]: ./docs/benchmarks/silesia-2026-11-10/

## Quick start

Requires **Rust 1.89 or later**. Add it to your project:

```toml
[dependencies]
mopenzl = "0.2"
```

```rust
use mopenzl::{Compressor, Decompressor, EncoderConfig, Level};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = "openzl ".repeat(1000);
    let config = EncoderConfig::default().with_level(Level::try_from(6)?);
    let mut encoder = Compressor::new(config)?;

    let frame = encoder.compress(input.as_bytes())?;
    println!("{} -> {} bytes", input.len(), frame.len());

    let restored = Decompressor::default().decompress(&frame)?;
    assert_eq!(restored, input.as_bytes());
    Ok(())
}
```

`EncoderConfig::default()` is the reference's default: level 6, the newest format
version (27) and both checksums on. Levels run from `-131072` to `22`.

## Choose your API

Compression and decompression expose the same core I/O shapes. Pick the shape that
matches how your application already moves bytes.

| I/O shape | Compression | Decompression |
| --- | --- | --- |
| Return a new `Vec<u8>` | `Compressor::compress` | `Decompressor::decompress` |
| Append to an existing Vec | `compress_into` | `decompress_into` |
| Write into a caller-owned slice | `compress_to_slice` | `decompress_to_slice` |
| Write into uninitialized memory | `compress_to_uninit` | `decompress_to_uninit` |
| Typed and multiple inputs / outputs | `compress_inputs`, `compress_inputs_into` | `decompress_typed` → `Vec<TypedOutput>` |
| Choose the graph / codec nodes | `Compressor::builder(..).with_graph(..)` / `ParallelCompressor::builder(..).with_graph(..)` | — (the frame carries its own) |
| Pull output through `std::io::Read` | `Compressor::reader` | `Decompressor::reader` |
| Push input through `std::io::Write` | `Compressor::writer` | `Decompressor::writer` |
| Drive input/output incrementally | `start` → `EncoderSession` | `start` → `DecoderSession` |
| Same, into uninitialized memory | `EncoderSession::process_uninit` | `DecoderSession::process_uninit` |
| Same, with the session owning the codec | `into_session` → `EncoderSessionOwned` | `into_session` → `DecoderSessionOwned` |
| Split one known input across your threads | `ParallelCompressor::prepare_slice` / `prepare_source` → `ScopedParallelBatch` | — |
| Stream chunked input across your threads | `ParallelCompressor::start` → `ParallelEncoderSession` + `ParallelTaskQueue` | — |
| Same, into uninitialized memory | `ParallelEncoderSession::process_uninit` | — |
| Same, with the session owning the compressor | `ParallelCompressor::into_session` → `ParallelEncoderSessionOwned` | — |
| Pull parallel output through `std::io::Read` | `ParallelCompressor::reader` → `ParallelEncoderReader` + `ParallelTaskQueue` | — |
| Push input through `std::io::Write` across your threads | `ParallelCompressor::writer` → `ParallelEncoderWriter` + `ParallelTaskQueue` | — |

`ParallelCompressor` lives in `mopenzl::compressor::parallel` and is unavailable
with `no_std`. The caller runs its tasks; streaming buffers compressed chunks
until finish, when the total input size is known. See [parallel compression](docs/parallel.md).

### Reuse memory between payloads

`Compressor` and `Decompressor` own reusable working state. Keep the codec and your
output buffer alive across operations when allocation reuse matters.

<details>
<summary>Example: reuse the compressor and output buffer</summary>

```rust
use mopenzl::{Compressor, Decompressor, EncoderConfig, Level};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = Compressor::new(EncoderConfig::default().with_level(Level::try_from(3)?))?;
    let mut decoder = Decompressor::default();
    let mut frame = Vec::new();

    for input in [b"first payload".as_slice(), b"second payload".as_slice()] {
        frame.clear(); // Keep the allocation; compress_into appends.
        let written = encoder.compress_into(input, &mut frame)?;
        assert_eq!(written, 0..frame.len());
        assert_eq!(decoder.decompress(&frame)?, input);
    }
    Ok(())
}
```

</details>

`RetentionPolicy` controls what a codec keeps between operations; see the
[user guide](docs/README.md#reusing-memory).

## `Read` and `Write` streaming

Both codecs provide synchronous adapters for the standard Rust I/O traits:

- `reader(...)` wraps an input `Read` and exposes transformed bytes through `Read`.
- `writer(...)` wraps an output `Write` and accepts source bytes through `Write`.

A frame header records the frame's total size, so declare it with
`InputSize::Exact` when you know it: a session then writes the header at once and,
with `Chunking::Fixed`, holds at most one chunk of input. With `InputSize::Unknown`
it holds the whole input until the frame ends. Encoder writers must be explicitly
finished; dropping one abandons the frame.

<details>
<summary>Compression and decompression with <code>Read</code> and <code>Write</code></summary>

<!-- cfg: not(feature = "no_std") -->
```rust
use mopenzl::{ChunkSize, Chunking, Compressor, Decompressor, EncoderConfig, InputSize};
use std::io::{Read, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let payload = b"streamed payload ".repeat(10_000);
    let config = EncoderConfig::default().with_chunking(Chunking::Fixed(ChunkSize::MIN));

    // Push model: write plain bytes into a compressed sink.
    let mut encoder = Compressor::new(config)?;
    let mut writer = encoder.writer(Vec::new(), InputSize::Exact(payload.len() as u64).into());
    writer.write_all(&payload)?;
    let frame = writer
        .finish()
        .map_err(mopenzl::io::FinishError::into_error)?;

    // Pull model: read compressed bytes from a plain source.
    let mut reader = encoder.reader(&payload[..], InputSize::Exact(payload.len() as u64).into());
    let mut from_reader = Vec::new();
    reader.read_to_end(&mut from_reader)?;
    assert_eq!(from_reader, frame);

    // Decompression, pulling restored bytes from a compressed source.
    let mut decoder = Decompressor::default();
    let mut restored = Vec::new();
    decoder.reader(&frame[..]).read_to_end(&mut restored)?;
    assert_eq!(restored, payload);

    // ... or pushing compressed bytes into a restored sink.
    let mut writer = decoder.writer(Vec::new());
    writer.write_all(&frame)?;
    let restored = writer
        .finish()
        .map_err(mopenzl::io::FinishError::into_error)?;
    assert_eq!(restored, payload);
    Ok(())
}
```

</details>

The decoder adapters use bounded internal buffering, and decoder-writer
finalization reports truncated or invalid input instead of silently accepting an
incomplete frame. See [streaming and completion](docs/README.md#streaming-and-completion).

## Compress structured data

OpenZL's strength is typed input: integers, fixed-width records and strings are
transformed by codecs that know their shape before an entropy coder sees them. Wrap
borrowed data in `mopenzl::input` types, pick a standard graph, and decode with
`decompress_typed`.

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::{CompressorGraph, StandardGraph};
use mopenzl::input::{Input, Numbers};
use mopenzl::{Compressor, DataType, Decompressor, EncoderConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 10 000 slowly growing 32-bit timestamps, little-endian.
    let values: Vec<u8> = (0..10_000u32)
        .flat_map(|i| (1_700_000_000 + i * 3).to_le_bytes())
        .collect();
    let numbers = Numbers::try_from((&values[..], 4))?;

    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(CompressorGraph::standard(StandardGraph::SelectNumeric))
        .build()?;
    let frame = compressor.compress_inputs(&[Input::from(numbers)])?;
    println!("{} -> {} bytes", values.len(), frame.len());

    let outputs = Decompressor::default().decompress_typed(&frame)?;
    assert_eq!(outputs[0].data_type(), DataType::Numeric);
    assert_eq!(outputs[0].width(), 4);
    assert_eq!(outputs[0].content(), values);
    Ok(())
}
```

`Compressor::compress` and the byte-oriented decoder methods handle one serial
input and one serial output, like `ZL_compress` and `ZL_decompress`; frames with
typed or several outputs go through `decompress_typed`. The
[structured data guide](docs/structured-data.md) covers numeric, record and string
inputs, clustering, ML selectors and SDDL; the [graph guide][graphs] lists every
standard graph.

## Build your own graph

A frame records the codecs that produced it, not the graph, so any graph decodes
with the same universal decoder. `GraphBuilder` derives new graphs from the
standard ones: pipelines of nodes, parameterized copies, and selectors, function
graphs and segmenters written in Rust.

Application-defined nodes are supported too: implement `CustomCodec`, register
the node with `GraphBuilder::custom_codec`, and register its decoder under the
same custom wire ID. See [custom codecs](docs/codecs.md#custom-codecs).

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::{GraphBuilder, StandardGraph, StandardNode};
use mopenzl::{Compressor, Decompressor, EncoderConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Read serial bytes as 32-bit integers, take their deltas, then field-LZ them.
    let mut builder = GraphBuilder::new();
    let start = builder.pipeline(
        &[
            StandardNode::ConvertSerialToNumLe32.into(),
            StandardNode::DeltaInt.into(),
        ],
        StandardGraph::FieldLz,
    )?;
    let graph = builder.build(start)?;

    let input: Vec<u8> = (0..4096u32).flat_map(|i| (i * 7).to_le_bytes()).collect();
    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()?;
    let frame = compressor.compress(&input)?;
    assert_eq!(Decompressor::default().decompress(&frame)?, input);
    Ok(())
}
```

Selectors and segmenters, local parameters, chunking and permissive fallback are in
the [graph guide][graphs].

## Dictionaries

A Zstandard dictionary travels in a `DictBundle`; the frame records the bundle's ID
and the decoder resolves it through a `BundleStore`. Parameterize the Zstandard node
with the dictionary, load the bundle, then build the graph.

<!-- cfg: feature = "zstd" -->
```rust
use mopenzl::dict::{BundleStore, Dict, DictBundle};
use mopenzl::graph::{GraphBuilder, NodeParams, StandardGraph, StandardNode};
use mopenzl::{Compressor, Decompressor, EncoderConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Training data: 40 records of the shape we will compress.
    let record = |i: usize| {
        let users = ["alice", "bob", "carol", "dave", "erin"];
        let cities = ["Oslo", "Lima", "Kyiv", "Pune"];
        format!(
            r#"{{"user":"{}","city":"{}","plan":"{}","seen":{},"flags":["beta","email"],"locale":"en-GB"}}"#,
            users[i % 5],
            cities[i % 4],
            if i.is_multiple_of(2) { "pro" } else { "free" },
            i * 7
        )
    };
    let history = (0..40).map(record).collect::<Vec<_>>().join("\n");
    let dict = Dict::zstd(history.as_bytes(), 3)?;
    let bundle = DictBundle::try_from(vec![dict.clone()])?;

    let mut builder = GraphBuilder::new();
    let node = builder.parameterize_node_with(
        StandardNode::Zstd,
        NodeParams::default().with_dictionary(dict.id()),
    )?;
    builder.load_dictionaries(&bundle)?;
    let start = builder.static_graph(node, &[StandardGraph::Store.into()])?;
    let graph = builder.build(start)?;

    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()?;
    let payload = record(1234).into_bytes();
    let frame = compressor.compress(&payload)?;
    let plain = Compressor::default().compress(&payload)?;
    println!(
        "{} bytes -> {} with the dictionary, {} without",
        payload.len(),
        frame.len(),
        plain.len()
    );

    let mut store = BundleStore::default();
    store.insert(&bundle)?;
    let mut decoder = Decompressor::builder(Default::default())
        .with_dictionaries(store)
        .build();
    assert_eq!(decoder.decompress(&frame)?, payload);
    Ok(())
}
```

Dictionaries need format version 25 or newer. See the
[dictionary guide](docs/dictionaries.md).

## Select only the codecs you need

The default feature set is `std`, `compression`, `decompression` and `all-codecs`.
`all-codecs` enables one Cargo feature per OpenZL codec (`zstd`, `entropy`, `lz4`,
`field-lz`, `delta`, `tokenize`, `custom-codec`, ...; the
[codec guide](docs/codecs.md) lists them all). Disable default features to select
directions and codecs yourself:

```toml
[dependencies]
mopenzl = { version = "0.2", default-features = false, features = ["std", "decompression", "zstd"] }
```

Without a codec's feature the encoder never writes it (a chunk that would use it is
stored, so frames stop matching the reference's default graph) and the decoder
reports frames that use it as `DecodeError::Unsupported`. Frames of the default
graph need only `zstd`.

For `no_std` with `alloc`, disable default features and enable `no_std`:

```toml
[dependencies]
mopenzl = { version = "0.2", default-features = false, features = ["no_std", "decompression", "all-codecs"] }
```

`no_std` requires a global allocator, removes the `std::io` adapters and runtime
SIMD detection (the compile-time baseline is used), and pulls in `libm` for the
estimators the numeric selectors rank candidates with. **Caveat:** `libm`'s `expf`
differs from the platform's by one ULP for some inputs, so the numeric transformer
selector's scores can differ slightly from the reference's; decisions change only on
exact ties (none were observed in 4000+ frames), but byte identity of encoder
decisions that depend on floating point is not guaranteed under `no_std`. Cargo
features are additive: another dependency can re-enable `std`.

## Compatibility

The reference in `openzl-ffi/vendor` is pinned to **OpenZL v0.3.0** with
**zstd 1.5.7** and **lz4 1.10.0**.

- **Encoder.** For the same input, graph (or the serial segmenter, with
  `Chunking::Fixed`), level, format version and checksum settings, `Compressor`
  writes the frame `ZL_CCtx_compress` writes, at format versions **21 to 27** and
  levels `-131072..=22`. User-written selectors, function graphs and segmenters are
  byte-identical when they make the same decisions as the reference's counterpart.
  The `no_std` caveat above applies.
- **Decoder.** Reads every standard codec and registered custom codec valid at
  format versions 21 to 27, with both checksums verified. On *malformed* data it
  deviates from the reference only where the reference reads uninitialized or
  out-of-bounds memory or wraps silently
  (offset 0 in LZ codecs, out-of-range partition bucket IDs or flatpack indices,
  over-reads in the struct Huffman decoder, reads past the extra bits of quantize
  codes, overflowing sizes): this decoder reports an error. See
  [decoder codecs](architecture/decoder-codecs.md).
- **Custom codecs.** Supported when the application registers the matching
  decoder. An unregistered custom ID reports `Unsupported::CustomCodec`.
- **Not supported.** Frames of format versions below 21 and legacy codecs that only
  exist there (`rolz`, `fastlz`, `zstd_fixed`, the deprecated FSE and Huffman
  codecs), and dictionary training.
- **Dictionaries.** Only Zstandard dictionaries can be materialized.

Typed and multiple inputs, the graph API and dictionaries follow the reference's
semantics; mopenzl does not provide a C ABI, so it is not a drop-in replacement for
`libopenzl`.

## Validation

The repository includes differential tests against the pinned C library (frames
byte-compared for every format version 21 to 27 and for each standard node and
graph, decoder agreement on C-produced frames), cross-API and cross-backend checks,
AFL++ and libFuzzer targets, Miri and AddressSanitizer runs over the SIMD and
uninitialized-output paths, and a 100% function coverage gate. See
[compatibility](architecture/compatibility.md) and [fuzzing](architecture/fuzzing.md)
for what is covered and how to reproduce it.

The crate enforces `#![cfg_attr(not(test), forbid(unsafe_code))]`. This describes
mopenzl's production implementation, not every transitive dependency: SIMD
intrinsics are encapsulated by `fearless_simd`, and differential tests use a C FFI
reference.

## Documentation and contributing

[API reference][api] · [User guide][guide] · [Graphs][graphs] · [Codecs](docs/codecs.md) · [Structured data](docs/structured-data.md) · [Dictionaries](docs/dictionaries.md) · [Architecture][architecture]

Bug reports with a minimal input, graph, parameters and reproduction steps are
welcome. For performance reports, include the CPU, compiler, feature set and
workload. If compression output differs from OpenZL v0.3.0 for the same graph and
parameters, open an issue with both frames.

Read [CONTRIBUTING.md][contributing] for the checks a change must pass, and follow
the [Code of Conduct][conduct]. The [development guide][development] covers local
checks, coverage and fuzzing. Report security vulnerabilities privately as described
in [SECURITY.md][security], not as public issues.

## License

[MIT](LICENSE-MIT) or [APACHE 2.0](LICENSE-APACHE) at your option. mopenzl is
derived from [OpenZL], which is BSD-licensed; see [NOTICE](NOTICE) for its
attribution.

[OpenZL]: https://github.com/facebook/openzl
[crate]: https://crates.io/crates/mopenzl
[api]: https://docs.rs/mopenzl
[ci]: https://github.com/M-Compressors/mopenzl/actions/workflows/ci.yml
[changelog]: ./CHANGELOG.md
[guide]: ./docs/README.md
[graphs]: ./docs/graphs.md
[architecture]: ./architecture/README.md
[benchmarking]: ./docs/benchmarking.md
[development]: ./docs/development.md
[contributing]: ./CONTRIBUTING.md
[conduct]: ./CODE_OF_CONDUCT.md
[security]: ./SECURITY.md
[`mzstd`]: https://crates.io/crates/mzstd
[`fearless_simd`]: https://crates.io/crates/fearless_simd
[`Decompressor::decompress_typed`]: https://docs.rs/mopenzl/latest/mopenzl/struct.Decompressor.html#method.decompress_typed
