# Encoder results

Published measurements compare mopenzl with the pinned OpenZL v0.3.0 C
implementation on identical complete Silesia files. The matrix varies graphs
and typed views at default level 6, format 27 and both checksums. Every inner
frame was checked byte-identical before either implementation was timed.

**AMD Ryzen 9 7950X (KVM, 2 vCPUs) · rustc 1.99.1 · C -O3 · 2026-11-10.**

| Graph / input view | Files | Input bytes | Compressed bytes | Ratio | C MB/s | mopenzl MB/s | / C |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| [zstd](../README.md#zstd) | 12 | 211,938,580 | 61,240,193 | 3.461 | 142.4 | 141.6 | 0.99× |
| [lz4](../README.md#lz4) | 12 | 211,938,580 | 78,385,876 | 2.704 | 80.2 | 75.7 | 0.94× |
| [lz](../README.md#lz) | 12 | 211,938,580 | 64,385,308 | 3.292 | 113.7 | 96.1 | 0.85× |
| [huffman](../README.md#huffman) | 12 | 211,938,580 | 130,419,862 | 1.625 | 1,124.6 | 1,051.6 | 0.94× |
| [fse](../README.md#fse) | 12 | 211,938,580 | 130,057,228 | 1.630 | 618.6 | 596.7 | 0.96× |
| [entropy](../README.md#entropy) | 12 | 211,938,580 | 130,379,952 | 1.626 | 1,075.3 | 989.2 | 0.92× |
| [huffman_pivco](../README.md#huffman_pivco) | 12 | 211,938,580 | 130,590,079 | 1.623 | 967.1 | 1,053.9 | 1.09× |
| [flatpack](../README.md#flatpack) | 12 | 211,938,580 | 196,426,372 | 1.079 | 985.3 | 1,425.5 | 1.45× |
| [generic](../README.md#generic) | 12 | 211,938,580 | 61,240,193 | 3.461 | 145.2 | 141.1 | 0.97× |
| [field_lz_struct4](../README.md#field_lz_struct4) | 3 | 50,891,072 | 10,944,133 | 4.650 | 89.5 | 73.2 | 0.82× |
| [bitpack_num16](../README.md#bitpack_num16) | 2 | 18,444,804 | 18,444,872 | 1.000 | 4,721.3 | 8,437.7 | 1.79× |
| [numeric16](../README.md#numeric16) | 2 | 18,444,804 | 8,318,978 | 2.217 | 28.3 | 26.7 | 0.95× |
| [numeric32](../README.md#numeric32) | 3 | 50,891,072 | 10,944,138 | 4.650 | 88.8 | 75.9 | 0.85× |
| [generic_num16](../README.md#generic_num16) | 2 | 18,444,804 | 8,318,978 | 2.217 | 28.6 | 25.2 | 0.88× |
| [generic_num32](../README.md#generic_num32) | 3 | 50,891,072 | 10,944,138 | 4.650 | 80.9 | 71.5 | 0.88× |

Throughput is uncompressed MB/s (10⁶ bytes/s), aggregated as total bytes over
the sum of per-file median times. Compressed bytes are equal for both sides.
The linked graph pages contain file-by-file numbers and charts. This dataset
is a graph sweep, not a per-level benchmark; no unmeasured levels are implied.

## Read the measurements

Use the [full comparison](../README.md) for per-file charts and ratios,
[raw CSV](../silesia-2026-11-10/after.csv) for analysis, and
[commands](../../benchmarking.md) to reproduce the API mode and corpus.
Numeric/record views use only aligned compatible files, so compare each graph
within its own corpus rather than ranking unrelated rows as one workload.

## Other API modes

The [parallel API](../../../architecture/benchmark-comparison.md#parallel-compression)
records segment/task scheduling separately. The [framed measurements](../../../architecture/framing.md#initial-measurements)
record container overhead and independent-frame C timings. End-to-end results
include different setup/allocation costs from reused per-graph benchmarks.
