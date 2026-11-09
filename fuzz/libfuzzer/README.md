# libFuzzer targets

This package runs the AFL targets' bodies under libFuzzer, which is what
[ClusterFuzzLite](https://google.github.io/clusterfuzzlite/) builds for Rust.
It owns no target logic: each file in `fuzz_targets/` is a one line
`fuzz_target!` adapter around the body the AFL binary of the same name runs,
from the `mopenzl-afl` library. Inputs, oracles and corpora are described in
the [AFL guide](../afl/README.md).

Like `fuzz/afl`, the package is excluded from the root workspace, and it needs
the three vendored C submodules for the oracle.

## Targets

`decompress`, `roundtrip` and `stream_compress`: every AFL target.

## Local runs

cargo-fuzz needs nightly Rust for the sanitizer flags. From the repository root:

```sh
cargo install cargo-fuzz --locked
cargo +nightly fuzz build -O --debug-assertions --fuzz-dir fuzz/libfuzzer
ASAN_OPTIONS=allocator_may_return_null=1 \
    cargo +nightly fuzz run --fuzz-dir fuzz/libfuzzer -O --debug-assertions decompress -- -malloc_limit_mb=1048576
```

The two settings are what `.clusterfuzzlite/build.sh` writes to each target's
`.options` file: the C oracle sizes stream buffers from claims inside
malformed frames and treats a failed allocation as an error, so a huge
request must fail rather than be reported as a crash.

To reproduce exactly what ClusterFuzzLite builds, including the seed corpora,
run `.clusterfuzzlite/build.sh` with `SRC`, `OUT`, `WORK` and `SANITIZER` set.
`SRC` must contain the repository as `mopenzl`.

## Findings

ClusterFuzzLite attaches the crashing input to the failed workflow run.
Reproduce it with the binary of that target:

```sh
cargo +nightly fuzz run --fuzz-dir fuzz/libfuzzer -O --debug-assertions <target> <crash-file>
```

Then minimize it, commit it under `fuzz/afl/regressions/<target>/` as
`*.bin`, check that `cargo afl test` fails on it, and fix the bug. The
regression replay does not need a libFuzzer build.
