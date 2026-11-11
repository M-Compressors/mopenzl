# Encoder comparison

The [encoder matrix](encoders/README.md) compares safe Rust OpenZL against the
pinned C implementation, with matching graphs, parameters, typed views, frame
bytes and corpus. C and Rust compressed sizes are identical; throughput is read
against that fixed ratio. The [full report](README.md) includes every file.

## Implementations and API mode

The C side uses OpenZL v0.3.0, zstd 1.5.7 and lz4 1.10.0 from the pinned
submodules at -O3. The Rust side uses mopenzl and mzstd with the lockfile recorded
by the comparison workspace. Prepared contexts and caller output storage are
reused. Untimed validation precedes the alternating measurement rounds.

## Levels, graphs and corpora

The published graph sweep uses level 6 and format 27. Serial bytes, numeric
16/32-bit inputs and four-byte records are separate views, with only compatible
files in each view. Store-on-expansion and checksums follow defaults. There is
no published multi-level sweep; changing a node/graph or level requires a fresh
matched run. See [reproduction](../benchmarking.md#encoder-comparison).

## Sources

- [Per-graph and per-file report](README.md)
- [Recorded environment and CSV](silesia-2026-11-10/README.md)
- [Methodology and parallel results](../../architecture/benchmark-comparison.md)
