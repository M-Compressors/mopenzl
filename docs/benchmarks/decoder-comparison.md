# Decoder comparison

The [decoder matrix](decoders/README.md) decodes byte-identical OpenZL frames on
both implementations. Each result preserves the same content type, width and
string boundaries as its input. The [full report](README.md) includes per-file
throughput, ratio and charts.

## Implementations and API mode

Reuse prepared decoder contexts and caller output storage. Frame generation,
corpus loading, validation and output checks remain outside timing. The
published cases use the same level-6, format-27 frames as the encoder matrix;
a decoder comparison must record how the compressed corpus was generated.

## Interpret the result

Use total uncompressed bytes divided by summed median time for aggregate
throughput. Distinguish serial, numeric and record views and their selected
files. Backend availability and compiler flags belong to the recorded host,
not a universal guarantee. Stored frames, typed node execution and compressed
formats have different costs; a short smoke run is not a regression threshold.

## Sources

- [Per-graph and per-file report](README.md)
- [Recorded environment and CSV](silesia-2026-11-10/README.md)
- [Reproduction and profiling](../benchmarking.md#decoder-comparison)
- [Decoder implementation](../../architecture/decompressor.md)
