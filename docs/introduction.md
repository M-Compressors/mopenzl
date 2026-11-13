# OpenZL compression and decompression in safe Rust

<div class="landing">
<p class="lede">mopenzl is a port of Meta's OpenZL v0.3.0, the format-aware compression framework, with a Rust-native API. Describe compression as a graph of codecs, feed it typed data, and decode any frame with one universal decoder. Serial frames are byte-identical to the C library's.</p>
<div class="actions">
<a class="primary" href="getting-started.md">Get started</a>
<a href="graphs.md">Choose a graph</a>
<a href="README.md">Choose your API</a>
<a href="https://docs.rs/mopenzl">API reference on docs.rs</a>
</div>
</div>

## A round trip

```toml
[dependencies]
mopenzl = "0.2.4"
```

```rust
use mopenzl::{Compressor, Decompressor, EncoderConfig, Level};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = "hello, smaller world! ".repeat(1000);
    let mut encoder = Compressor::new(EncoderConfig::default().with_level(Level::try_from(6)?))?;
    let frame = encoder.compress(input.as_bytes())?;

    let mut decoder = Decompressor::default();
    assert_eq!(decoder.decompress(&frame)?, input.as_bytes());
    Ok(())
}
```

Requires Rust **1.89 or later**. Without a graph the compressor runs the
default, `compress_generic`, which uses Zstandard for serial bytes.
[Getting started](getting-started.md) continues from here.

## Tell it the shape of your data

OpenZL's advantage over a general-purpose compressor is typed input. Say that
a buffer holds 32-bit integers, and a numeric graph transforms them before an
entropy coder sees them:

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

The decoder needs no graph: the frame records the codecs that produced it.
[Structured data](structured-data.md) covers numbers, records and strings.

## Pick a graph

| Your data | Start with |
| --- | --- |
| General bytes | `CompressGeneric`, the default |
| Fastest decode, or already-compressed data | `Store`, `Lz4` |
| Integers | `SelectNumeric` |
| Fixed-width records | `FieldLz` |
| Strings | `StringCompress` |
| A known binary layout | `SimpleDataDescriptionLanguage` (SDDL) |
| Not sure | `BruteForce` with your candidates; it keeps the smallest |

[Graphs](graphs.md#which-graph-when) has the full table, and
[codecs](codecs.md) lists every node and the Cargo feature that enables it.
You can also compose nodes yourself with `GraphBuilder` or register a
[custom codec](codecs.md#custom-codecs).

## Find your path

<div class="paths">
<a href="getting-started.md"><strong>First round trip</strong><span>Install, compress, decompress and reuse a codec.</span></a>
<a href="graphs.md"><strong>Graphs</strong><span>Standard graphs, pipelines, selectors and segmenters.</span></a>
<a href="structured-data.md"><strong>Structured data</strong><span>Numbers, records, strings, clustering and SDDL.</span></a>
<a href="README.md"><strong>Choose your API</strong><span>Every entry point for both directions in one table.</span></a>
<a href="configuration.md"><strong>Configuration and features</strong><span>Level, format version, checksums and per-codec features.</span></a>
<a href="decompression.md"><strong>Decode untrusted input</strong><span>Input and output limits, frame metadata.</span></a>
<a href="streaming.md"><strong>Streaming</strong><span>Readers, writers, sans-I/O sessions and <code>finish</code>.</span></a>
<a href="parallel.md"><strong>Parallel compression</strong><span>One input across scoped threads, Rayon or your executor.</span></a>
<a href="frames.md"><strong>Framing containers <span class="tag">opt-in</span></strong><span>Indexed resources, each with its own graph.</span></a>
<a href="benchmarks/README.md"><strong>Benchmarks</strong><span>Every graph against OpenZL C on Silesia, with raw data.</span></a>
<a href="../architecture/README.md"><strong>How it works</strong><span>Frame format, graph engine and codecs, with diagrams.</span></a>
</div>

## What you can rely on

- **Byte-identical serial encoder.** For the same input, graph and global
  parameters, frames are exactly those of OpenZL v0.3.0 at format versions 21
  to 27. See [what is claimed and how it is checked](correctness.md).
- **Safe implementation, not a C wrapper.** Production code forbids `unsafe`.
  Zstandard comes from the safe-Rust `mzstd` crate; every other codec is
  ported here. The C library is only a test and benchmark oracle.
- **No hidden I/O or threads.** Sessions touch only the buffers you pass in,
  and parallel compression hands tasks to your scheduler. The library does not
  create a thread pool.
- **Measured against C.** The [benchmarks](benchmarks/README.md) show where
  mopenzl is faster than C and where it is still behind.

Framing requires the opt-in `framed` feature. The book tracks `main`, including
unreleased changes; documented recipes are checked against the local checkout.
