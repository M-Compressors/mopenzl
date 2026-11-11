# Development

Requires Rust 1.89 or later. `openzl-ffi` builds the pinned OpenZL v0.3.0,
zstd 1.5.7 and lz4 1.10.0 sources as the C test/benchmark oracle. A C compiler
is required for development builds. Production uses safe Rust codecs.

```sh
git clone --recurse-submodules https://github.com/M-Compressors/mopenzl.git
# In an existing checkout, only the three pinned submodules are needed:
git submodule update --init
cargo run --example compress
cargo doc --workspace --all-features --no-deps --locked
```

## Workspace checks

The documentation website uses mdBook; see [local builds and publication](website.md).
Run these at the workspace root:

```sh
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
```

The differential suite is faster in release mode. All-features enables no_std,
which removes std transports and parallel APIs, so check std/framed separately:

```sh
cargo test --workspace --release --locked
cargo test --workspace --features framed --release --locked
cargo clippy --workspace --all-targets --features framed --locked -- -D warnings
python3 scripts/check_codec_features.py
```

## Coverage

Every changed Rust function must be exercised. The repository targets 100%
function coverage for owned library code; inspect a report rather than assuming
passing tests reached each function.

```sh
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --version 0.9.0 --locked
cargo llvm-cov clean --workspace
CARGO_INCREMENTAL=1 cargo llvm-cov --workspace --release --features framed --locked --no-report
cargo llvm-cov report --workspace --release --ignore-filename-regex '/openzl-ffi/|/vendor/' --fail-under-functions 100
cargo llvm-cov report --workspace --release --html
```

Incremental compilation preserves counters for small public functions copied
across crates in optimized builds. Reports remain local artifacts under target.
The [CI coverage workflow](../.github/workflows/ci-coverage.yml) records its
exact invocation and library exclusions.

## Fuzzing

AFL and libFuzzer are isolated workspaces. After changing target bodies or APIs
they call, run from `fuzz/afl`:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --no-default-features -- -D warnings
cargo afl test --no-default-features
```

`cargo afl test` links AFL's runtime and replays the committed corpus. The
[AFL guide](../fuzz/afl/README.md) covers installation, seeds, dictionaries,
campaigns and minimization. Avoid committing generated queues or findings.

Run the libFuzzer package's checks from `fuzz/libfuzzer`:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
```

The [libFuzzer guide](../fuzz/libfuzzer/README.md) describes local sanitizer
builds and ClusterFuzzLite. Both engines call the same target bodies.

## Formal verification

There is no Lean/Aeneas proof tree in mopenzl. Correctness evidence comes from
unit/integration tests, C differential checks, coverage, fuzzing and memory
sanitizers. See [compatibility and validation](correctness.md) for the scope and
limits of that evidence.

## Repository map

| Path | Contents |
| --- | --- |
| `src/compressor`, `src/decompressor` | Public codecs; private core implementations |
| `src/graph.rs`, `src/input.rs` | Graph construction and typed borrowed inputs |
| `src/framing` | Opt-in independent-resource container and seek index |
| `src/custom_codec`, `src/dict` | Application codecs and external dictionary bundles |
| `src/shared` | Wire parsing, checksums, shared scalar/SIMD kernels |
| `tests`, `examples`, `benches` | Public behavior, runnable APIs and Criterion comparisons |
| `benchmarks/comparison` | Isolated graph/typed-view Silesia C comparison |
| `openzl-ffi` | C oracle, shims and the three pinned upstream submodules |
| `fuzz/afl`, `fuzz/libfuzzer` | Isolated fuzz packages and regression replay |
| `.clusterfuzzlite` | ClusterFuzzLite image and build script |
| `docs`, `architecture` | Canonical guides, data and implementation specifications |
| `book.toml`, `book-theme` | mdBook renderer, local diagram assets and styling |
| `scripts/docs.py`, `scripts/test_docs.py` | Staging, navigation, link checks and runnable book recipes |

[Repository rules](../AGENTS.md) require public/private core boundaries,
focused error types, per-codec feature gating and updated architecture diagrams.
Vendored sources are maintained separately. The [CI specification](../architecture/ci.md)
describes feature matrices, packaging, fuzzing and profiling workflows.

SIMD/uninitialized-output checks use `python3 scripts/check_simd_uninit.py miri`
and `python3 scripts/check_simd_uninit.py asan`. x86-64 Miri uses
`RUSTFLAGS=-Ctarget-cpu=x86-64-v3`; unsupported Neon paths are covered by ASan.
See [Miri and AddressSanitizer](../CONTRIBUTING.md) for commands and limits.

The book workflow builds every canonical page, validates local files and anchors,
and runs all complete user-guide Rust programs in a temporary consumer with
framed and Rayon enabled. Core guide examples also remain in the workspace's
existing generated documentation tests; the external consumer supplies dependencies
used only by book recipes. See [documentation architecture](../architecture/documentation.md).
