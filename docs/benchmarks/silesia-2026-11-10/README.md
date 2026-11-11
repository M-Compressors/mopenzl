# Silesia record — 2026-11-10

**AMD Ryzen 9 7950X (KVM, 2 vCPUs) · rustc 1.99.1 · C at -O3.**
Full Silesia files; level 6, format 27, both checksums. Every compared frame was
verified byte-identical and both decoders restored the input before timing.

- [After measurements](after.csv)
- [Before measurements](before.csv)
- [Rendered comparison and charts](../README.md)
- [Reproduction commands](../../benchmarking.md#silesia-comparison)

CSV columns are graph case, file, input bytes, compressed bytes, implementation
side, operation, median/minimum time in nanoseconds, rounds and iterations.
The dataset contains per-side/per-operation records. Numeric and record views
select aligned files, as the harness's list command reports.

The committed historical CSV contains the stated environment and measurement
values; it does not embed full compiler flags, corpus digests or revision IDs.
Record those alongside any new published run rather than inferring them from
this snapshot. See the [benchmark specification](../../../architecture/benchmark-comparison.md).
