# Benchmarking

```sh
cargo bench --bench compress
cargo bench --bench decompress
# a quick pass
cargo bench --bench compress -- --warm-up-time 1 --measurement-time 2
```

Every case verifies identical output before timing. Record the command,
machine, toolchain and revision with any result; see
[the comparison](../architecture/benchmark-comparison.md) for the current
figures.
