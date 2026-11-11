# Examples

The recipes below include complete programs and round-trip assertions. Examples
use the default `std`, compression, and decompression features unless stated
otherwise. The documentation workflow compiles and runs their Rust code against
the current checkout.

| Task | Recipe | Features / extra dependency |
| --- | --- | --- |
| Choose graphs and compose codec nodes | [Graphs](../graphs.md) · [Codecs](../codecs.md) | Default; per-codec features |
| Preserve typed and multiple inputs | [Structured data](../structured-data.md) | Default |
| Set the level, format version and checksums | [Encoder settings](../configuration.md#encoder-settings) | Default |
| Write into a slice or uninitialized memory | [Output buffers](../memory.md#output-buffers) | Default |
| Decode untrusted input with limits | [Set limits for untrusted input](../decompression.md#set-limits-for-untrusted-input) | Default |
| Read a frame header without decoding | [Frame metadata](../decompression.md#frame-metadata) | Default |
| Compress and decompress bytes | [First round trip](../getting-started.md#compress-and-decompress) | Default |
| Reuse codec and destination memory | [Reuse the workspace](../getting-started.md#reuse-the-workspace-and-output) | Default |
| Write a raw stream incrementally | [Stream through Write](../getting-started.md#stream-through-write) | Default |
| Drive a raw or framed encoder with tiny buffers | [Sans I/O compression](../sans-io-compression.md) | Default; `framed` for frames |
| Decode arriving chunks and resource events | [Sans I/O decompression](../sans-io-decompression.md) | Default; `framed` for frames |
| Read or write raw, parallel, and framed streams | [Read / Write I/O](../io.md) | Default; `framed` for frames |
| Use scoped CPU workers | [Compressing a slice](../parallel.md#compressing-a-slice) | Default |
| Reuse a Rayon thread pool | [Running tasks with Rayon](../parallel.md#running-tasks-with-rayon) | Default + `rayon` |
| Stream through parallel workers | [Reader and writer adapters](../parallel.md#reader-and-writer-adapters) | Default |
| Package compressed and stored resources | [One container, two resources](../frames.md#one-container-two-resources) | `framed` |
| Add resource names and write incrementally | [Write resources and metadata](../frames.md#write-resources-and-metadata) | `framed` |
| Read just the selected resource | [Indexed resource reads](../frames.md#indexed-resource-reads) | `framed` |

## Run from the repository

Initialize the C reference submodule used by development builds:

```sh
git submodule update --init --recursive
cargo run --release --example compress
```

The [parallel example](../../examples/parallel.rs) uses caller-owned scoped
threads, reuses its compressor and verifies every assembled frame:

```sh
cargo run --release --example parallel
```

For a parallel batch, all taken tasks must run or be dropped before finalization.
For a streaming adapter, keep workers running while the adapter blocks.
Finish ordinary/parallel writers and the enclosing framed container explicitly.
Framed resource commands accept complete input rather than resource writer guards.

## Verify the documented recipes

```sh
python3 scripts/docs.py test-examples
```

This creates a temporary Cargo consumer under `target/`, depends on the local
crate with `framed` enabled and on Rayon, and runs each fenced Rust program
from all user-guide chapters, including graphs, codecs, typed inputs, dictionaries,
parallel, framing and Sans I/O. It does not
exercise the codec with `--all-features`, because that would also enable `no_std`
and remove the std APIs these recipes demonstrate.
