# Architecture

Specifications describe how the implementation works today: ownership, APIs,
data flow, state transitions, SIMD dispatch, errors and known gaps. To use the
crate, read the [guides](../docs/README.md) instead; for checks and repository
layout, see [development](../docs/development.md).

## Where to start

| You want to understand | Read, in order |
| --- | --- |
| What a frame looks like on the wire | [Frame format](frame-format.md) |
| How a `compress` call becomes a frame | [Compressor](compressor.md) → [Compression graphs](compressor-graphs.md) → [Standard graphs](standard-graphs.md) → [Encoder codecs](encoder-codecs.md) |
| How a frame is decoded | [Decompressor](decompressor.md) → [Decoder codecs](decoder-codecs.md) |
| How a codec family works | [Entropy coders](entropy.md), [LZ codecs and graphs](lz.md), [Transformer](transformer.md), [ML selector and clustering](ml-and-clustering.md), [SDDL and SDDL2](sddl.md) |
| How applications extend the codec set | [Custom codecs](custom-codecs.md) → [Dictionaries](dictionaries.md) |
| How one input is split across threads | [Parallel compression](parallel-compression.md) |
| How containers work | [Framed containers](framing.md) |
| How byte identity is established | [Compatibility](compatibility.md) → [Fuzzing](fuzzing.md) |
| Which features compile which code | [Codec features](codec-features.md) |

Specifications share an outline: module and ownership boundaries, public
surface, control and data flow with Mermaid diagrams, and error propagation.
What is missing or slower than the reference is listed under
[known gaps](#known-gaps-whole-crate).

## Module map

```mermaid
graph TD
    Root[mopenzl public facade] --> Encode[compressor: compression feature]
    Root --> Decode[decompressor: decompression feature]
    Root --> Framed["framing: opt-in framed resource containers"]
    Framed --> FCore["private framing::core: resource assembly, units, directory"]
    FCore --> Encode
    FCore --> Decode
    Root --> GraphApi["graph, input: graph builder, typed inputs (compression)"]
    Root --> Custom["custom_codec: application callbacks, signatures and contexts"]
    Custom --> ECore
    Custom --> DCore
    Root --> DictApi["dict: Dict, DictBundle, BundleStore"]
    Root --> Common[Backend, RetentionPolicy, FormatVersion, DataType, io]
    Encode --> Parallel["parallel: caller-scheduled tasks, batches, streams (std)"]
    GraphApi --> Parallel
    Parallel --> PCore[private parallel::core: workers, staging, ordered assembly]
    PCore --> ECore
    Encode --> Dest[shared::destination: initialized or MaybeUninit output]
    Decode --> Dest
    GraphApi --> Reg
    Encode --> ECore[private compressor::core]
    Decode --> DCore[private decompressor::core]
    ECore --> EStream[core::stream: session state machine]
    ECore --> EChunk[core::chunk + core::frame: specialized default-graph writer]
    ECore --> EZstd[core::zstd: Zstandard node]
    ECore --> EGraph[core::exec + layout + rtgraph: graph engine]
    EGraph --> Reg["core::registry: 85 nodes, 90 graphs, dict state"]
    Reg --> ECodecs["core::codecs: every node encoder, one module per feature"]
    Reg --> EGraphs["core::graphs: standard graphs, selectors, segmenters"]
    EGraphs --> Special["transformer, ml_selector, clustering, sddl, sddl2, entropy, lz"]
    DCore --> DStream[core::stream: unit assembly]
    DCore --> DChunk[core::chunk: chunk graph layout]
    DCore --> DZstd[core::zstd: magicless Zstandard decoder]
    DCore --> DCodecs[core::codecs: every standard codec, one module per feature]
    DCore --> DData[core::data: typed streams]
    EChunk --> Shared[private shared]
    ECodecs --> Shared
    EGraphs --> Shared
    DChunk --> Shared
    DCodecs --> Shared
    Shared --> Prims["wire, varint, bitpack, xxh3, sha256, dictwire"]
    Shared --> Algo["entropy, stats, estimate, numeric, bitpack_kernels, byte_transpose, cbor"]
    Prims --> SIMD[fearless_simd: selected backend]
    ECodecs --> SIMD
    DCodecs --> SIMD
    EZstd --> Mzstd[mzstd: safe-Rust zstd 1.5.7]
    DZstd --> Mzstd
    C[openzl-ffi: OpenZL v0.3.0 C] -. tests and benchmarks .-> Root
```

Public modules own ergonomic configuration and high-level errors. Private `core`
modules own algorithms and state machines; implementation errors, SIMD types and
`mzstd` types do not appear in public signatures. Each codec compiles
independently.

SIMD dispatch boundary: the `fearless_simd::Level` is chosen once per
`Compressor` / `Decompressor` (its `Backend`) and reaches every node encoder
as `NodeCx::simd` and every codec decoder as `State::simd`. Each codec
dispatches on it once per stream (PivCo-Huffman once per tree node) and runs
monomorphized kernels inside the dispatched function; every kernel keeps a
scalar baseline that tests compare at each level the host offers.

## Common boundaries

The wire format, feature gates and primitives both directions share.

| Specification | Scope |
| --- | --- |
| [Frame format](frame-format.md) | Magic, frame header, chunk header lanes, stream numbering, stored streams, checksums and limits as both codecs read and write them. |
| [Codec features](codec-features.md) | Direction and per-codec feature gates, `std`/`no_std`, libm and profiling features, and how disabled codecs behave. |
| [Shared primitives](shared-primitives.md) | Varints, bit packing, wire constants, XXH3 with its SIMD accumulator, and the encoder-side shared modules. |
| [Uninitialized output](uninit-output.md) | `compress_to_uninit`, `decompress_to_uninit` and `process_uninit`: `BorrowedBuf` sinks, zero-free Vec decoding and their Miri/ASan coverage. |

## Compression

From the public `Compressor` to the graph engine and node encoders.

| Specification | Scope |
| --- | --- |
| [Compressor](compressor.md) | Public API, the specialized default-graph path, chunking, sinks, the session state machine, graph selection, I/O and errors. |
| [Compression graphs](compressor-graphs.md) | The graph engine: typed streams, runtime graph, contexts, successor order, stores, conversions, trial compression and the registry tables. |
| [Standard graphs](standard-graphs.md) | The decision logic of every standard graph: routing, stores, segmenters and structure graphs. |
| [Encoder codecs](encoder-codecs.md) | Every node encoder: node ID to codec ID, feature, ports, wire summaries, deviations and gaps. |
| [Parallel compression](parallel-compression.md) | Caller-scheduled segments, graph/codec builder, source/staging ownership, dictionary-aware frame assembly and deferred streaming output. |

## Codec families

Selectors, interpreters and coders with enough machinery for a specification of their own.

| Specification | Scope |
| --- | --- |
| [Entropy coders](entropy.md) | OpenZL's FSE/Huffman fork, PivCo-Huffman, the five entropy nodes and the entropy graph's plan. |
| [LZ codecs and graphs](lz.md) | lz4 (fast, HC, optimal), lz, field_lz, mux_lengths, quantize, the shared lz_common infrastructure and the LZ graphs. |
| [Transformer](transformer.md) | The numeric selector: features, generated scorers, guards, depth limits, static fallbacks, float determinism and the no_std caveat. |
| [ML selector and clustering](ml-and-clustering.md) | Gradient-boosted-tree selection and tag-based clustering with their CBOR configurations. |
| [SDDL and SDDL2](sddl.md) | The description interpreter, the bytecode VM, chunking and replay. |
| [Custom codecs](custom-codecs.md) | Application codec registration, typed and variable ports, callback contexts, custom wire IDs, validation, errors and lifetime. |
| [Dictionaries](dictionaries.md) | Dict, DictBundle and BundleStore, wire formats, the encoder and decoder flows. |

## Decompression

The universal decoder and its codec catalogue.

| Specification | Scope |
| --- | --- |
| [Decompressor](decompressor.md) | Frame and unit parsing, chunk graph layout, the typed node executor, typed multi-output decoding, dictionaries, sessions, limits, I/O and errors. |
| [Decoder codecs](decoder-codecs.md) | Every standard codec decoder: signatures, wire formats, invariants and deviations from the reference on malformed data. |

## Containers

The opt-in `framed` container around ordinary frames.

| Specification | Scope |
| --- | --- |
| [Framed containers](framing.md) | Opt-in graph-per-resource containers, typed frames, metadata, bounded native sessions, transport retries and checked seek directories. |

## Development tools

How the repository measures, tests and publishes itself.

| Specification | Scope |
| --- | --- |
| [Compatibility](compatibility.md) | The C oracle and its shims per family, byte-identity and decode differential tests, and what each test file covers. |
| [Fuzzing](fuzzing.md) | AFL targets, oracles, regression replay and the libFuzzer adapters. |
| [Implementation comparison](benchmark-comparison.md) | Criterion and Silesia harness methodology (standard graphs and custom callbacks against C) and recorded results. |
| [Continuous integration](ci.md) | Workflows, feature probes, AFL smoke runs, Miri and AddressSanitizer selection, coverage gate, hotpath reports and releases. |
| [Documentation publishing](documentation.md) | Source staging, navigation, section pages, recipe execution, link validation and Pages deployment. |

## Known gaps (whole crate)

- Scope: every non-legacy OpenZL v0.3.0 node, graph, selector and segmenter is
  ported, on both sides. Deprecated nodes 76 to 85 and format versions below 21
  are not supported; unregistered custom codecs decode as `Unsupported`.
- Dictionaries: only Zstandard dictionaries materialize; no training
  ([dictionaries](dictionaries.md)).
- Performance: `fearless_simd` kernels cover XXH3, the LZ row-table tag match,
  PivCo-Huffman (128-bit, plus 512-bit expand/compress on AVX-512), bitpack bit
  counting, byte transposes and flatpack. FSE, Huffman, LZ4 and the LZ decoders
  are tuned scalar loops compiled inside the dispatched level. Huffman decoding
  and the numeric and field_lz graphs remain 0.7 to 0.9× of C on Silesia; LZ4,
  FSE and Zstandard are within about 10%
  ([comparison](benchmark-comparison.md)). The partition+bitpack decoder
  fusion is not ported.
- `no_std` builds use the `libm` crate for floating point; the Transformer and
  estimator decisions are byte-identical with the reference only with `std`
  ([transformer](transformer.md)).
- No C ABI crate. Parallel compression supports serial inputs; streaming holds
  compressed chunks until the total input size is known.
- Verification gaps: no AFL targets for the graph engine, SDDL or CBOR
  configurations; new SIMD kernels and uninit paths require explicit Miri
  and ASan selections ([fuzzing](fuzzing.md), [CI](ci.md)).
