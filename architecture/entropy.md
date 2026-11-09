# Entropy coders

OpenZL carries its own FSE and Huffman libraries, forked from Zstandard 1.5.7
(configurable number of FSE states, table-log aware histogram, different
workspace layout), so `mzstd` cannot stand in for them. The decoder side lives
in `decompressor::core::codecs::entropy` and `pivco_huffman` (see the
[decoder catalogue](decoder-codecs.md)); this page covers the encoder side: the
shared FSE/HUF library, the five nodes and the entropy graph's plan.

## Module map

```mermaid
graph TD
    G["graphs/entropy.rs: graphs 3 to 7, set_entropy_destination"] --> N
    subgraph N ["codecs (features entropy, pivco-huffman)"]
        F["entropy.rs: fse_v2 58, fse_ncount 61, huffman_v2 59, huffman_struct_v2 60"]
        P["pivco_huffman.rs: node 87, v27 only"]
    end
    F --> LIB
    P --> LIB
    subgraph LIB ["shared::entropy (OpenZL fork)"]
        BITS["bits: forward BitWriter"]
        FSE["fse: normalize_count, CTable, write NCount, compress_using_ctable"]
        HUF["huf: build ctable, table log, write ctable, estimate, 1x and 4x"]
        H16["huf16: 16-bit Huffman for huffman_struct_v2"]
    end
    G --> STATS
    F --> STATS
    subgraph STATS ["shared::stats"]
        HIST[histogram]
        DS["data_stats: DataStatsU8, integer entropy with log table"]
        CNT[count]
    end
```

`shared::entropy` is compiled with `compression` and either `entropy` or
`pivco-huffman` (pivco only builds Huffman tables); `shared::stats` likewise.
All encoders write the reference's exact bytes. The reference's unrolled and
"fast" loop variants differ only in speed, so one straightforward bit writer
replaces them; the buffer-capacity failure rules are kept
(`bits::BitWriter::close`). Deviations: stable sort for the 16-bit Huffman,
scalar PivCo.

## Nodes

| Node | Codec | Inputs | Outputs | Header |
| --- | --- | --- | --- | --- |
| `fse_v2` 58 | 49 | Serial | Numeric counts (w2) with metadata 0 = table log, Serial bits | `[states 2 or 4][size]`, 2 states below 1000 symbols |
| `fse_ncount` 61 | 52 | Numeric (w2) | Serial `FSE_writeNCount` header | none |
| `huffman_v2` 59 | 50 | Serial | Numeric weights (w1) with metadata 0 = table log, Serial bits | `[x4][size]`, four streams from 256 bytes |
| `huffman_struct_v2` 60 | 51 | Struct (w2) | Numeric weights (up to 65536 symbols, log up to 20), Serial blocks | blocks `[count u32][size u32][payload]`, one or four streams |
| `pivco_huffman` 87 | 67 | Serial | Numeric weights (w1), Serial bit stream | varint decoded size, plus the block size when the input exceeds 32 KiB |

The size in the `[flag][size]` header is little endian in
`nextPow2(size + 1)` bits rounded up to bytes. Every coding node requires at
least two elements and refuses a single repeated value (`constant` handles it).
The graph passes precomputed state as reference parameters, `HISTOGRAM_PID` 246
(`Arc<Histogram>`) and `HUF_CTABLE_PID` 247 (`Arc<HufCTable>`), so the node
skips recomputing it; a node run on its own builds both itself (histograms exist
for widths 1 and 2).

PivCo-Huffman codes the input through the Huffman tree level by level: at each
tree node the ranks of the symbols below it are partitioned by the next code bit
into a bitmap (plus a one-count for internal nodes), then the halves recurse;
runs of equal-length codes become flat leaves coded as fixed-width indices. Max
256 symbols, table log 12, 32 KiB blocks. Only the portable kernel is ported (the
SSE/AVX2/AVX-512/NEON kernels are byte-equivalent by definition).

## The entropy graph

Graphs `fse` (3), `huffman` (4), `huffman_huf0` (5), `huffman_pivco` (6) and
`entropy` (7) are one function graph, `entropyDynamicGraph`, with a backend mode
(FSE, Huffman, or whichever is estimated smaller) and a Huffman mode (`huffman_v2`
or `pivco_huffman`). Graph 6 selects PivCo at format 27 and newer; graphs 4 and
7 always use `huffman_v2`; below v27 PivCo falls back to `huffman_v2`. The
deprecated pre-15 codecs are not supported (the minimum format is 21).

```mermaid
flowchart TD
    A[input stream] --> C["convert: width 1 to serial, width 2 to struct; other widths error"]
    C --> S{"1 element or fewer?"}
    S -- yes --> ST[store]
    S -- no --> M["minGain = max(param 66 or 32, size/100 * param 80 or 1)"]
    M --> K{"100000 elements or more?"}
    K -- yes --> SP["splitN into 32 KiB chunks (ref param 323); minGain scaled by chunk share"]
    K -- no --> CH
    SP --> CH[per chunk]
    CH --> W{"serial?"}
    W -- yes --> B[byte plan]
    W -- no --> D[wide plan]
```

Byte plan (`plan_byte_chunk`), for serial chunks:

```mermaid
flowchart TD
    H[histogram and DataStatsU8] --> I{"Huffman mode and largest count at most total/128 + 4?"}
    I -- yes --> MS[mode = Store]
    I -- no --> CD{"cardinality 1?"}
    MS --> CD
    CD -- yes --> CO[constant 8]
    CD -- no --> CT["build Huffman ctable when mode is Huf or Any"]
    CT --> RM["resolve_mode: n at most 64 means Store; Any compares FSE and Huffman estimates (FSE wins when fse + n/100 <= huf); the winner needs entropy + minGain <= n, else Store"]
    RM --> FP{"flatpacked < bitpacked?"}
    FP -- yes --> F2{"flatpacked < entropy and + minGain <= n"}
    F2 -- yes --> FLAT[flatpack 11]
    FP -- no --> BP{"bitpacked <= entropy and + minGain <= n"}
    BP -- yes --> BIT[bitpack 10]
    F2 -- no --> OUT
    BP -- no --> OUT
    OUT{"mode"} --> HU["Huffman node: weights to bitpack_int 41, bits to store"]
    OUT --> FS["fse_v2 node: counts to fse_ncount 44, bits to store"]
    OUT --> STO[store 2]
```

Wide plan (`plan_wide_chunk`), for 16-bit struct chunks: one value ->
`constant`; bit-packing when it beats the Huffman estimate and store by `minGain`
-> `bitpack`; store when the entropy exceeds 15 bits or the estimate does not
pay -> `store`; fewer than 256 distinct values -> `tokenize`, alphabet bit-packed
(when the maximum value needs fewer than 16 bits, else stored) and indices to `huffman`; else
`huffman_struct_v2` with its weights to `fse` and bits to `store`.

`set_entropy_destination(cx, edge, graph, min_gain_bytes, min_gain_pct)`
(`ZL_Edge_setEntropyDestination`) sends an edge to an entropy graph with
optional minimum-gain parameters; the LZ graphs use it and fall back to the
default graph when an override index is invalid (see [LZ](lz.md)).

## Known gaps

- No SIMD: the reference's X2 and assembly Huffman decoders and AVX2/BMI2
  kernels are not ported (output is the same).
- Per-call tables: a struct Huffman table at log 20 costs 4 MiB per call.
- The 16-bit Huffman uses a stable sort, the PivCo kernel is the portable one,
  and a single bit writer replaces the reference's unrolled variants.
