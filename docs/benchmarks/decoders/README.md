# Decoder results

Published measurements compare mopenzl with the pinned OpenZL v0.3.0 C
implementation on identical complete Silesia files. The matrix varies graphs
and typed views at default level 6, format 27 and both checksums. Every inner
frame was checked byte-identical before either implementation was timed.

**AMD Ryzen 9 7950X (KVM, 2 vCPUs) · rustc 1.99.1 · C -O3 · 2026-11-10.**

| Graph / input view | Files | Input bytes | Compressed bytes | Ratio | C MB/s | mopenzl MB/s | / C |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| [zstd](../README.md#zstd) | 12 | 211,938,580 | 61,240,193 | 3.461 | 1,303.9 | 1,230.7 | 0.94× |
| [lz4](../README.md#lz4) | 12 | 211,938,580 | 78,385,876 | 2.704 | 2,551.8 | 2,393.6 | 0.94× |
| [lz](../README.md#lz) | 12 | 211,938,580 | 64,385,308 | 3.292 | 676.5 | 1,036.2 | 1.53× |
| [huffman](../README.md#huffman) | 12 | 211,938,580 | 130,419,862 | 1.625 | 1,591.3 | 1,234.5 | 0.78× |
| [fse](../README.md#fse) | 12 | 211,938,580 | 130,057,228 | 1.630 | 1,089.7 | 955.0 | 0.88× |
| [entropy](../README.md#entropy) | 12 | 211,938,580 | 130,379,952 | 1.626 | 1,507.1 | 1,197.8 | 0.79× |
| [huffman_pivco](../README.md#huffman_pivco) | 12 | 211,938,580 | 130,590,079 | 1.623 | 2,325.3 | 2,422.0 | 1.04× |
| [flatpack](../README.md#flatpack) | 12 | 211,938,580 | 196,426,372 | 1.079 | 2,733.3 | 4,072.7 | 1.49× |
| [generic](../README.md#generic) | 12 | 211,938,580 | 61,240,193 | 3.461 | 1,541.5 | 1,486.7 | 0.96× |
| [field_lz_struct4](../README.md#field_lz_struct4) | 3 | 50,891,072 | 10,944,133 | 4.650 | 1,946.2 | 1,398.1 | 0.72× |
| [bitpack_num16](../README.md#bitpack_num16) | 2 | 18,444,804 | 18,444,872 | 1.000 | 7,665.6 | 17,191.8 | 2.24× |
| [numeric16](../README.md#numeric16) | 2 | 18,444,804 | 8,318,978 | 2.217 | 1,019.6 | 809.7 | 0.79× |
| [numeric32](../README.md#numeric32) | 3 | 50,891,072 | 10,944,138 | 4.650 | 1,840.4 | 1,241.1 | 0.67× |
| [generic_num16](../README.md#generic_num16) | 2 | 18,444,804 | 8,318,978 | 2.217 | 963.9 | 799.4 | 0.83× |
| [generic_num32](../README.md#generic_num32) | 3 | 50,891,072 | 10,944,138 | 4.650 | 1,809.9 | 1,188.4 | 0.66× |

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
