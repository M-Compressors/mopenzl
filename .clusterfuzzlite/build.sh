#!/bin/bash -eu
# Builds the libFuzzer targets in fuzz/libfuzzer for ClusterFuzzLite.
#
# Each target is a three line adapter around a body in the AFL package's
# library, so a finding here replays through `cargo afl test` once its input is
# committed under fuzz/afl/regressions/<target>/.

cd "$SRC/mopenzl"

# The C oracle builds OpenZL v0.3.0 and the zstd and lz4 it vendors from three
# submodules, which ClusterFuzzLite's own checkout leaves empty. The pins come
# from the checked out commit. Not recursive: openzl's own nested submodules
# are test and benchmark dependencies the oracle does not compile.
if [ ! -f openzl-ffi/vendor/openzl/include/openzl/openzl.h ] ||
    [ ! -f openzl-ffi/vendor/zstd/lib/zstd.h ] ||
    [ ! -f openzl-ffi/vendor/lz4/lib/lz4.h ]; then
    git config --global --add safe.directory '*'
    git submodule update --init --depth 1 \
        openzl-ffi/vendor/openzl openzl-ffi/vendor/zstd openzl-ffi/vendor/lz4
fi

host=$(rustc -vV | sed -n 's/^host: //p')

# The image's CFLAGS define FUZZING_BUILD_MODE_UNSAFE_FOR_PRODUCTION. Under it
# the vendored OpenZL skips frame checksums, accepts trailing bytes and caps
# LZ match lengths at 1024 (zstd and lz4 change too), so the C oracle stops
# being the reference: it decodes corrupted frames mopenzl rightly rejects and
# writes frames mopenzl rightly does not. Keep the sanitizer and coverage
# flags, which instrument the C code too, and drop only the define;
# openzl-ffi's build script refuses to compile while it is present.
without_fuzzing_mode() {
    printf ' %s ' "${1:-}" |
        sed -E -e ':strip' \
            -e 's/ -DFUZZING_BUILD_MODE_UNSAFE_FOR_PRODUCTION(=[^ ]*)? / /' \
            -e 't strip'
}
CFLAGS=$(without_fuzzing_mode "${CFLAGS:-}")
CXXFLAGS=$(without_fuzzing_mode "${CXXFLAGS:-}")
export CFLAGS CXXFLAGS

# Debug assertions also turn on overflow checks, which AFL's builds have too.
#
# Under the coverage sanitizer the image's cargo wrapper turns
# `cargo fuzz build` into `cargo build --bins` run from ./fuzz, dropping
# --fuzz-dir and --target-dir; here that builds the root crate, which has no
# binaries. The coverage build therefore calls cargo build itself, with the
# image's RUSTFLAGS (instrument-coverage, --cfg fuzzing) and the same release
# profile and output directory as cargo fuzz. The remap gives the adapters
# absolute paths in the report, as the wrapper does.
build() {
    if [ "$SANITIZER" = coverage ]; then
        (
            cd fuzz/libfuzzer
            RUSTFLAGS="$RUSTFLAGS -Cdebug-assertions --remap-path-prefix fuzz_targets=$PWD/fuzz_targets" \
                cargo build --release --target "$host" --bins "$@"
        )
    else
        cargo fuzz build -O --debug-assertions --fuzz-dir fuzz/libfuzzer "$@"
    fi
}

build --target-dir "$WORK/libfuzzer"
release="$WORK/libfuzzer/$host/release"
for source in fuzz/libfuzzer/fuzz_targets/*.rs; do
    target=$(basename "$source" .rs)
    cp "$release/$target" "$OUT/"
    # The C oracle sizes buffers from claims inside malformed frames and
    # handles a failed allocation as an error. Let such a request fail
    # instead of reporting it: ASan returns null, and libFuzzer's malloc
    # limit sits far above anything the bounded Rust targets allocate, while
    # rss_limit_mb still catches real growth.
    cat > "$OUT/$target.options" <<'OPTIONS'
[libfuzzer]
malloc_limit_mb = 1048576
[asan]
allocator_may_return_null = 1
OPTIONS
done

# Seed corpora: the generated AFL corpus each target's input layout reads, as
# campaign.sh maps them, plus its committed regressions. The generator cuts
# payloads to 64 KiB. The generator is an ordinary program: it is built
# without the image's sanitizer and coverage flags, which would leave it, and
# the C library it links, referring to runtimes nothing links in.
env -u RUSTFLAGS -u RUSTDOCFLAGS -u CFLAGS -u CXXFLAGS \
    CARGO_TARGET_DIR="$WORK/seeds-target" fuzz/afl/prepare-seeds.sh

seeds_for() {
    case "$1" in
        decompress) echo fuzz/afl/seeds/frames fuzz/afl/seeds/codec_frames ;;
        framed_decode) echo fuzz/afl/seeds/framed ;;
        roundtrip|parallel|framed_roundtrip) echo fuzz/afl/seeds/params ;;
        stream_compress) echo fuzz/afl/seeds/stream ;;
        decompress_typed) echo fuzz/afl/seeds/typed_frames fuzz/afl/seeds/codec_frames ;;
        typed_roundtrip) echo fuzz/afl/seeds/typed_params ;;
        graph_configs) echo fuzz/afl/seeds/configs ;;
        node_roundtrip) echo fuzz/afl/seeds/node_params ;;
    esac
}

for source in fuzz/libfuzzer/fuzz_targets/*.rs; do
    target=$(basename "$source" .rs)
    corpus="$WORK/corpus/$target"
    rm -rf "$corpus"
    mkdir -p "$corpus"
    # Prefixes keep a seed and a regression with the same name apart.
    for directory in $(seeds_for "$target") "fuzz/afl/regressions/$target"; do
        [ -d "$directory" ] || continue
        prefix=$(basename "$(dirname "$directory")")
        find "$directory" -maxdepth 1 -type f -print0 |
            while IFS= read -r -d '' path; do
                cp "$path" "$corpus/$prefix-$(basename "$path")"
            done
    done
    if [ -n "$(ls -A "$corpus")" ]; then
        (cd "$corpus" && zip -q "$OUT/${target}_seed_corpus.zip" ./*)
    fi
done
