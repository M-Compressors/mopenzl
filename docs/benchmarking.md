# Benchmarks and profiling

Compare throughput together with compressed size under a matched graph, typed
view, level, format version, checksum policy and API mode. Setup and validation
are separate from timed work unless the case explicitly measures an end-to-end
API. Every production optimization needs a measured hot path and a correct
baseline; see [performance methodology](../architecture/benchmark-comparison.md).

| Workload | Harness | Results |
| --- | --- | --- |
| Serial bytes and sessions | Criterion compress/decompress | Per-level IDs and printed frame sizes |
| Standard graphs and typed inputs | Isolated Silesia comparison | [Encoder matrix](benchmarks/encoders/README.md), [decoder matrix](benchmarks/decoders/README.md) |
| Parallel batches and streaming | Criterion parallel | [Recorded parallel results](../architecture/benchmark-comparison.md#parallel-compression) |
| Independent framed resources | Criterion framed (`framed`) | [Recorded container results](../architecture/framing.md#initial-measurements) |

## Encoder comparison

Build the isolated harness with the C oracle at -O3:

```sh
cd benchmarks/comparison
OPENZL_FFI_OPT_LEVEL=3 cargo build --release --locked
B=target/release/mopenzl-comparison
$B list
$B verify --corpus /path/to/silesia
$B run --corpus /path/to/silesia --min-time-ms 300 --rounds 5 --out after.csv
```

The harness lists each graph, input view and compatible files. It verifies
byte-identical Rust/C frames and round trips before timing alternating sides
and operations. Contexts and output buffers are prepared/reused; corpus loading
and validation are untimed. `--cases`, `--files` and `--limit BYTES` narrow a run.

Published comparisons use level 6, format 27, both checksums and full compatible
Silesia files. This varies the graph rather than sweeping quality numbers.
The [source configuration](../benchmarks/comparison/src/main.rs) owns the level
and global parameters; change both matched sides together for another study.
See [encoder interpretation](benchmarks/encoder-comparison.md).

## Decoder comparison

The same run times decoding on both sides of the exact same frames. The frame
corpus therefore records graph choice, level and typed layout even though the
decoder needs no compressor graph. Verify output types, widths and string lengths
as well as content before timing. See the
[decoder report](benchmarks/decoder-comparison.md).

Profile one untimed decoder workload rather than mixing instrumentation with
throughput measurements:

```sh
cd benchmarks/comparison
B=target/release/mopenzl-comparison
$B profile --corpus /path/to/silesia --case huffman --file dickens \
  --side rust --op decompress --iters 100 --limit 1000000
```

Use `--side c` for the matched C profile and `--op compress` for encoding.
`perf`, Callgrind, or the workspace's hotpath features can identify CPU and
allocation costs. Keep profiler runs separate from the timed baseline.

## Silesia comparison

The [full graph report](benchmarks/README.md) includes raw CSV, charts,
file-by-file throughput and compression ratios. The
[recorded environment](benchmarks/silesia-2026-11-10/README.md) identifies the
published host and what metadata the historical snapshot contains.

Build an older revision in a separate worktree and run the same corpus and
harness on the same host for a before/after comparison. Render new results with
`report.py`, which needs `matplotlib==3.10.8`:

```sh
cd benchmarks/comparison
python3 report.py --csv after.csv \
  --output ../../docs/benchmarks --subtitle "machine · toolchain · date"
```

Reports distinguish graph/input view rather than ranking different typed corpora
as one workload. Record bytes and ratio alongside speed. A changed output size
is a different encoding result, not an equivalent speed comparison.

## Sharded runs

For exploration, split cases or files into independent invocations of the same
built executable. Use separate output paths; keep validation enabled:

```sh
cd benchmarks/comparison
B=target/release/mopenzl-comparison
$B run --corpus /path/to/silesia --cases zstd,lz4 --out general.csv
$B run --corpus /path/to/silesia --cases huffman,fse --out entropy.csv
```

Concurrent lanes compete for caches, memory bandwidth and turbo budget. Record
all lanes, affinity and host load, and use a serial sweep when publishing matched
implementation comparisons. Do not merge measurements from different operating
conditions into one named baseline. `--files` and `--limit` also change the
workload, so report them with the corpus identity.

## API benchmarks

Criterion covers meaningful end-to-end and reused API modes:

```sh
cargo bench --bench compress --locked
cargo bench --bench decompress --locked
cargo bench --bench custom_codec --locked
cargo bench --bench parallel --locked
cargo bench --bench framed --features framed --locked
```

`compress`/`decompress` use identical serial bytes and parameters against C and
include sessions/uninitialized destinations. `custom_codec` compares application
callbacks. `parallel` covers task counts, graph selection, staging and a streaming
writer against the C serial segmenter; compressed sizes can differ. `framed`
compares whole containers and native sessions with independent C frames; C has
no implementation of the outer mopenzl container.

For a quick exploratory run:

```sh
cargo bench --bench compress -- --sample-size 10 --warm-up-time 1 --measurement-time 2
cargo bench --bench parallel --locked -- parallel-graphs
cargo bench --bench framed --features framed --locked -- rust-session
```

Short runs provide smoke evidence. Keep Criterion's configuration and confidence
intervals with any performance claim; there is no universal threshold across
graphs, APIs or platforms.

## Checks and profiling

The standalone comparison package has its own build/check boundary:

```sh
cargo fmt --manifest-path benchmarks/comparison/Cargo.toml --all -- --check
cargo clippy --manifest-path benchmarks/comparison/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path benchmarks/comparison/Cargo.toml --release --locked
```

Use representative optimized workloads with hotpath's optional modes:

```sh
cargo run --release --features hotpath-cpu --example profile_compressor
cargo run --release --features hotpath-alloc --example profile_compressor
cargo run --release --features hotpath-cpu,hotpath-alloc --example parallel
```

`profile_compressor` uses its built-in text payload and validates levels 1/3/6/12.
On Linux, keep native timing/allocation reports separate from Callgrind
instruction counts, then merge matching runs:

```sh
cargo build --release --example profile_compressor --features hotpath-alloc,hotpath-callgrind
HOTPATH_OUTPUT_FORMAT=json HOTPATH_OUTPUT_PATH=native.json \
  target/release/examples/profile_compressor
HOTPATH_OUTPUT_FORMAT=json HOTPATH_MERGE_REPORT=native.json HOTPATH_OUTPUT_PATH=merged.json \
  valgrind -q --tool=callgrind --instr-atstart=no target/release/examples/profile_compressor
```

Criterion adapters can run a fixed count for an exact benchmark ID:

```sh
HOTPATH_CRITERION_ITERATIONS=100 cargo bench --bench decompress --features hotpath-alloc \
  -- --test --exact decompress/rust/c-source-1MiB/L6
```

The [hotpath Criterion workflow](../.github/workflows/hotpath-criterion.yml)
records exact IDs, iteration counts, runner/toolchain and upload policy.
Its relay separates pull-request execution from authenticated report uploads.
See [CI profiling mechanics](../architecture/ci.md).

Record revision, lockfile, Rust/C compilers, CPU/target flags, affinity, corpus
identity, graph/parameters, input and output sizes, API mode, setup/allocation
policy and timing intervals. Generated Criterion, profiling and coverage artifacts
remain local. The [development guide](development.md) lists correctness checks.
