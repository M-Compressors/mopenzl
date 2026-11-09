# Development

```sh
git clone --recurse-submodules https://github.com/M-Compressors/mopenzl.git
# or, in an existing clone (non-recursive: OpenZL's own test deps are not needed)
git submodule update --init
```

A C compiler is required to build `openzl-ffi`, the C oracle for tests and
benchmarks. The minimum supported Rust version is 1.89.

## Layout

| Path | Contents |
| --- | --- |
| `src/compressor`, `src/decompressor` | Public codec APIs; `core` submodules hold the implementation. |
| `src/shared` | Wire constants, varints, bit packing, XXH3. |
| `openzl-ffi` | Vendored OpenZL v0.3.0 + zstd 1.5.7 + lz4 1.10.0 and their bindings. |
| `tests` | Differential, streaming, I/O, backend and API tests. |
| `benches` | Criterion comparisons with the C library. |
| `fuzz/afl`, `fuzz/libfuzzer` | Fuzz targets (outside the workspace). |
| `architecture` | Always-current specifications. |

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo test --workspace --locked            # default features run the io tests
python3 scripts/check_codec_features.py
python3 scripts/check_simd_uninit.py --miri --asan
cargo llvm-cov --workspace --summary-only  # 100% function coverage
```

`--all-features` enables `no_std`, which removes the `std::io` adapters, so
run the default-feature suite too.
