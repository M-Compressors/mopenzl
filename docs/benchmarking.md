# Benchmarking

## Criterion (default graph, small corpora)

```sh
cargo bench --bench compress
cargo bench --bench decompress
# a quick pass
cargo bench --bench compress -- --warm-up-time 1 --measurement-time 2
```

Every case verifies identical output before timing.

## Silesia comparison (every codec family)

`benchmarks/comparison` is a separate workspace that runs one standard graph
at a time over the [Silesia corpus](http://sun.aei.polsl.pl/~sdeor/index.php?page=silesia)
against OpenZL v0.3.0 C through `openzl-ffi`. Each pair is compressed on both
sides and must give byte-identical frames that both decoders restore before
anything is timed. `OPENZL_FFI_OPT_LEVEL=3` builds the C library at `-O3`,
as OpenZL's CMake release build does (the tests use `-O2`).

```sh
cd benchmarks/comparison
OPENZL_FFI_OPT_LEVEL=3 cargo build --release
B=target/release/mopenzl-comparison
$B list                                   # cases: graph, typed view, files
$B verify --corpus /path/to/silesia       # identity and round trips only
$B run --corpus /path/to/silesia --min-time-ms 300 --rounds 5 --out after.csv
# one side and operation, untimed, for callgrind/perf/hotpath
$B profile --corpus /path/to/silesia --case huffman --file dickens \
  --side rust --op decompress --iters 100 --limit 1000000
```

`--cases`, `--files` and `--limit BYTES` narrow a run. To compare against an
older revision, build the same harness in a worktree of that revision and run
it on the same host right after. The report and charts come from
`report.py` (needs `matplotlib==3.10.8`):

```sh
python3 report.py --csv after.csv --before before.csv \
  --output ../../docs/benchmarks --subtitle "machine · toolchain · date"
```

Record the command, machine, toolchain and revision with any result; see
[the Silesia results](benchmarks/README.md) and
[the comparison](../architecture/benchmark-comparison.md).
