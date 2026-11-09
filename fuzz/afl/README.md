# AFL fuzz targets

This package fuzzes the mopenzl encoder and decoder against the C reference,
OpenZL v0.3.0, built by `openzl-ffi` from the vendored submodules. It is
excluded from the workspace because its binaries link AFL's runtime; the
target bodies also replay the committed regression corpus through
`cargo afl test`.

## Setup

AFL requires a C compiler and `make`; the C oracle needs the three vendored
submodules. From the repository root:

```sh
git submodule update --init openzl-ffi/vendor/openzl openzl-ffi/vendor/zstd openzl-ffi/vendor/lz4
cargo install cargo-afl --version 0.18.2 --locked
cargo afl config --build --force
cd fuzz/afl && cargo afl build --release
```

Do not pass `--recursive`: OpenZL's nested submodules are test and benchmark
dependencies the oracle does not compile.

Most hosts need shared memory or crash-reporting changes before the first
campaign (on macOS `shmget()` fails without them); `cargo afl system-config`
makes them (it runs `sudo`).

## Layout

| Path | Role |
| --- | --- |
| `src/lib.rs` | `decode_config`, the configuration header shared by the encoder targets, and the `Schedule` that picks split points |
| `src/targets.rs` | Target bodies, shared by AFL, libFuzzer and the regression replay |
| `src/bin/` | Thin `afl::fuzz!` adapters |
| `tests/regressions.rs` | Committed corpus replay |
| `examples/prepare_seeds.rs` | The seed generator `prepare-seeds.sh` runs |
| `regressions/<target>/` | Boundary cases, small seeds and minimised findings, `*.bin` |
| `prepare-seeds.sh` | Builds `seeds/params`, `seeds/stream` and `seeds/frames` |
| `campaign.sh` | Runs targets in parallel for a fixed time |
| `seeds/`, `findings/` | Local artifacts, never committed |

The same target bodies also run under libFuzzer for ClusterFuzzLite; see
[`fuzz/libfuzzer`](../libfuzzer/README.md).

## Input layout

`decode_config` reads two bytes. Byte 0: bits 0-2 pick the level from
`6, 1, 3, -1, -50, 9, 15, 19`; bits 3-5 the format version (`0..=6` are
21..=27, `7` is the default, 27); bit 6 turns the content checksum off and
bit 7 the compressed checksum. Byte 1: bits 0-1 pick the chunking (`0` the
whole input, then fixed chunks of 32 KiB, 40 000 and 100 000 bytes); its
other bits are ignored. The rest, capped at 256 KiB, is the payload.

## Targets

| Target | Input | Checks |
| --- | --- | --- |
| `decompress` | raw bytes, no header | Arbitrary bytes never panic. One-shot decoding, a session fed pieces and output windows of random size (seeded from the input) and `decompress_to_slice` agree, and the session keeps its progress contract. Whatever mopenzl decodes, `ZL_decompress` decodes to the same content; whatever `ZL_decompress` decodes, mopenzl decodes too unless it reports `Unsupported` (codecs not ported yet) or the 16 MiB output cap |
| `roundtrip` | header, payload | `Compressor::compress` equals `ZL_CCtx_compress` with the same parameters (default graph, or the serial segmenter for fixed chunks); the frame decodes back, one-shot and in pieces |
| `stream_compress` | header, 4 control bytes (output window `1 + 97 * n`; flags: bit 0 declares the size, bit 1 allows flushes, bit 2 drives an owned session; the seed of the schedule), payload | A session fed random pieces, flushes and output windows finishes, never over-reports progress and writes a frame that decodes to the payload; without a flush that can cut a chunk (no flush, or an undeclared size) the frame equals the one-shot frame |

Decoder targets cap output at 16 MiB in both implementations, so a frame
header cannot ask for gigabytes. The reference is only consulted where
mopenzl's verdict is in scope (not `Unsupported` and not over a limit), so the
fuzzer is not steered into C codecs the port does not implement. It is also
skipped when mopenzl reports `ContentSizeMismatch`: the reference sizes its
stream buffers from a chunk's claimed size before it rejects such a frame,
and that multi-gigabyte request is fatal under AddressSanitizer and
libFuzzer's malloc limit.

There is no `experimental` feature in mopenzl yet, so the package has none
either.

## Running

```sh
./prepare-seeds.sh
./campaign.sh 600              # every target, ten minutes each, in parallel
./campaign.sh 60 roundtrip     # one target
cargo afl test --release       # replay regressions/
```

A crash lands in `findings/<target>/default/crashes/`. Minimise it with
`cargo afl tmin`, commit it under `regressions/<target>/` as `*.bin`, and fix
the bug with the replay failing first. `campaign.sh` stops each instance at
the time limit and reports an instance that never started (usually missing
`cargo afl system-config`). Replay any timeout with
`cargo afl showmap -t 20000 -o /dev/null -- target/release/<target> < input`
before treating it as a hang.
