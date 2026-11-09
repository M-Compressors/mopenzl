# Codec features

```mermaid
flowchart TD
    D[default = std + compression + decompression + every codec] --> C[compression]
    D --> R[decompression]
    D --> S[std]
    D --> Z[zstd: Zstandard via mzstd]
    N[no_std: alloc only] --> X[fearless_simd libm, mzstd no_std]
    C --> M1[mzstd?/compression]
    R --> M2[mzstd?/decompression]
    Z --> MZ[dep:mzstd]
    S --> IO[io adapters, std::error::Error, runtime SIMD detection]
```

## Directions

- `compression` exposes `compressor` and its root re-exports; `decompression`
  exposes `decompressor` and its re-exports. `Backend`, `RetentionPolicy` and
  `FormatVersion` exist with either direction; with neither the crate is
  empty.
- `io` exists only with a direction and without `no_std`. `no_std` selects the
  compile-time baseline SIMD level and wins over `std` when both are set
  (as `--all-features` does); `hotpath*` anchors are compiled out with it.

## Codecs

Each OpenZL codec has its own feature, and every codec feature is in
`default`. Codec features change behaviour, not the public API:

```mermaid
flowchart LR
    I[chunk input] --> F{codec feature on?}
    F -- yes --> E[codec chunk when it shrinks the input]
    F -- no --> S[stored chunk]
    H[chunk header naming the codec] --> G{codec feature on?}
    G -- yes --> R[run the codec]
    G -- no --> U["DecodeError::Unsupported { Codec { id } }"]
```

| Feature | Codec | Dependency | Gated items |
| --- | --- | --- | --- |
| `zstd` | `ZL_StandardTransformID_zstd` (22) | `mzstd` | `compressor::core::zstd`, `Engine::zstd`, the zstd branch of `write_chunk`, `CoreError::Internal`; `decompressor::core::zstd`, `Codec::Zstd`, `Engine::run_zstd`, `Out::zstd`; `wire::CODEC_ZSTD`, `MIN_STREAM_SIZE` |

Without `zstd` the encoder stores every chunk (still a valid frame the
reference decodes, but no longer byte-identical to its default graph), and the
decoder rejects the reference's Zstandard chunks. The chunk layout is parsed
in full either way, so an unsupported codec is reported before any stream is
wired.

## Checks

Shared helpers carry per-direction `cfg`s so single-direction builds have no
dead code. `scripts/check_codec_features.py` builds an isolated consumer for
every mode × direction × codec-set combination and probes which public items
exist; it fails on a codec feature missing from `default` or a feature it does
not classify. CI runs clippy over the same matrix and the test suite without
codec features.
