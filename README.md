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

**[Documentation][site]** · [Getting started][start] · [Graphs][graphs] · [Choose your API][guide] · [Examples][examples] · [Benchmarks](#performance) · [API reference][api] · [Changelog][changelog]

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

Before decoding input you do not control, set limits: they are unlimited by
default. See [decompression and limits][decoder-guide]. [Getting started][start]
continues with memory reuse, streaming and feature selection.

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
  See [parallel compression][parallel].
- **Universal decoder.** One decoder reads the frame of every standard codec,
  including frames with typed and multiple outputs
  ([`Decompressor::decompress_typed`]).
- **Indexed resources with independent graphs.** Opt-in `framed` containers
  preserve typed outputs, resource metadata and whole-resource seek. Each
  resource is one ordinary OpenZL frame. See [framing containers][frames].
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
  is enabled by default; see the [custom codec guide][custom-codecs].
- **Sans-I/O sessions.** `EncoderSession` and `DecoderSession` work only with the
  buffers you pass in and never perform I/O, so you can drive them from blocking
  code, async runtimes or your own event loop.
- **Reusable state.** Keep compressors, decompressors and output buffers across
  requests instead of rebuilding them for every payload.
- **Select only the codecs you need.** One Cargo feature per codec, collected in
  `all-codecs`; `no_std` with `alloc` is supported.

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
[structured data guide][structured] covers numeric, record and string
inputs, clustering, ML selectors and SDDL; the [graph guide][graphs] lists every
standard graph.

## Build your own graph

A frame records the codecs that produced it, not the graph, so any graph decodes
with the same universal decoder. `GraphBuilder` derives new graphs from the
standard ones: pipelines of nodes, parameterized copies, and selectors, function
graphs and segmenters written in Rust.

Application-defined nodes are supported too: implement `CustomCodec`, register
the node with `GraphBuilder::custom_codec`, and register its decoder under the
same custom wire ID. See [custom codecs][custom-codecs].

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

## Choose your API

Compression and decompression expose the same I/O shapes. Pick the one that matches
how your application already moves bytes.

| I/O shape | Compression | Decompression | Guide |
| --- | --- | --- | --- |
| Return a new `Vec<u8>` | `Compressor::compress` | `Decompressor::decompress` | [Getting started][start] |
| Append to a Vec, write into a slice or uninitialized memory | `compress_into`, `compress_to_slice`, `compress_to_uninit` | `decompress_into`, `decompress_to_slice`, `decompress_to_uninit` | [Buffers and memory reuse][memory] |
| Typed and multiple inputs / outputs | `compress_inputs`, `compress_inputs_into` | `decompress_typed` → `Vec<TypedOutput>` | [Structured data][structured] |
| Choose the graph / codec nodes | `Compressor::builder(..).with_graph(..)` | — (the frame carries its own) | [Graphs][graphs] |
| Pull through `std::io::Read` | `Compressor::reader` | `Decompressor::reader` | [Read / Write I/O][io] |
| Push through `std::io::Write` | `Compressor::writer` | `Decompressor::writer` | [Read / Write I/O][io] |
| Drive buffers yourself, no I/O | `start` → `EncoderSession` | `start` → `DecoderSession` | Sans I/O [compression][sans-io-enc], [decompression][sans-io-dec] |
| Use a Zstandard dictionary | `DictBundle` loaded into the graph | `BundleStore` on the decoder | [Dictionaries][dictionaries] |
| Split one input across your threads | `ParallelCompressor` | Ordinary `Decompressor` | [Parallel compression][parallel] |
| Indexed resources, a graph per resource | `framed` feature | `framed` feature | [Framing containers][frames] |

The [full API table][guide] also lists owned sessions, `process_uninit` and the
parallel streaming, reader and writer entry points.

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

## Cargo features

The default feature set is `std`, `compression`, `decompression` and `all-codecs`.
`all-codecs` enables one Cargo feature per OpenZL codec. Disable default features to
select directions and codecs yourself, or to build for `no_std` with `alloc`:

```toml
[dependencies]
mopenzl = { version = "0.2", default-features = false, features = ["std", "decompression", "zstd"] }
```

Frames of the default graph need only `zstd`. See [Cargo features][features] for
every combination, what a disabled codec does, and the [`no_std` caveat][no-std]
about floating-point byte identity; the [codec guide][codecs] lists each codec's
feature.

## Compatibility

The reference in `openzl-ffi/vendor` is pinned to **OpenZL v0.3.0** with
**zstd 1.5.7** and **lz4 1.10.0**.

- **Encoder.** For the same input, graph (or the serial segmenter, with
  `Chunking::Fixed`), level, format version and checksum settings, `Compressor`
  writes the frame `ZL_CCtx_compress` writes, at format versions **21 to 27** and
  levels `-131072..=22`. User-written selectors, function graphs and segmenters are
  byte-identical when they make the same decisions as the reference's counterpart.
  The [`no_std` caveat][no-std] applies.
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

## Documentation

| If you want to | Read |
| --- | --- |
| Build a first round trip | [Getting started][start] |
| Pick or build a compression graph | [Graphs][graphs] |
| See what each codec does | [Codecs][codecs] |
| Compress integers, records or strings | [Structured data][structured] |
| Use a Zstandard dictionary | [Dictionaries][dictionaries] |
| Find the right entry point | [Choose your API][guide] |
| Set the level, format version or features | [Configuration and features][configuration] |
| Control allocations | [Buffers and memory reuse][memory] |
| Decode untrusted input | [Decompression and limits][decoder-guide] |
| Stream with readers, writers, or sessions | [Streaming and completion][streaming] |
| Use several cores | [Parallel compression][parallel] |
| Package indexed resources | [Framing containers][frames] |
| Look up a type or method | [API reference on docs.rs][api] |
| Understand the implementation | [Architecture][architecture] |

The same pages live in [`docs/`](docs/README.md) and
[`architecture/`](architecture/README.md) in this repository.

## Contributing

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
[benchmarking]: https://m-compressors.github.io/mopenzl/docs/benchmarking.html
[development]: https://m-compressors.github.io/mopenzl/docs/development.html
[contributing]: ./CONTRIBUTING.md
[conduct]: ./CODE_OF_CONDUCT.md
[security]: ./SECURITY.md
[`mzstd`]: https://crates.io/crates/mzstd
[`fearless_simd`]: https://crates.io/crates/fearless_simd
[`Decompressor::decompress_typed`]: https://docs.rs/mopenzl/latest/mopenzl/struct.Decompressor.html#method.decompress_typed
[site]: https://m-compressors.github.io/mopenzl/
[start]: https://m-compressors.github.io/mopenzl/docs/getting-started.html
[guide]: https://m-compressors.github.io/mopenzl/docs/
[examples]: https://m-compressors.github.io/mopenzl/docs/examples/
[graphs]: https://m-compressors.github.io/mopenzl/docs/graphs.html
[codecs]: https://m-compressors.github.io/mopenzl/docs/codecs.html
[custom-codecs]: https://m-compressors.github.io/mopenzl/docs/codecs.html#custom-codecs
[structured]: https://m-compressors.github.io/mopenzl/docs/structured-data.html
[dictionaries]: https://m-compressors.github.io/mopenzl/docs/dictionaries.html
[configuration]: https://m-compressors.github.io/mopenzl/docs/configuration.html
[features]: https://m-compressors.github.io/mopenzl/docs/configuration.html#cargo-features
[no-std]: https://m-compressors.github.io/mopenzl/docs/configuration.html#no_std
[memory]: https://m-compressors.github.io/mopenzl/docs/memory.html
[decoder-guide]: https://m-compressors.github.io/mopenzl/docs/decompression.html
[streaming]: https://m-compressors.github.io/mopenzl/docs/streaming.html
[io]: https://m-compressors.github.io/mopenzl/docs/io.html
[sans-io-enc]: https://m-compressors.github.io/mopenzl/docs/sans-io-compression.html
[sans-io-dec]: https://m-compressors.github.io/mopenzl/docs/sans-io-decompression.html
[parallel]: https://m-compressors.github.io/mopenzl/docs/parallel.html
[frames]: https://m-compressors.github.io/mopenzl/docs/frames.html
[architecture]: https://m-compressors.github.io/mopenzl/architecture/
