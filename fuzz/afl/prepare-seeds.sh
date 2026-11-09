#!/bin/sh
# Builds the AFL seed corpora.
#
# examples/prepare_seeds.rs writes three corpora under seeds/, one per input
# layout: seeds/params (a configuration header and a payload, for roundtrip),
# seeds/stream (a header, stream_compress's control bytes and a payload) and
# seeds/frames (frames mopenzl writes, byte-identical to the reference's, with
# truncated, corrupted, concatenated and padded copies, for decompress).
#
# Payloads are synthetic plus the start of a few text files from the vendored
# submodules, when they are checked out; the generator cuts each to 64 KiB.
#
# examples/codec_seeds.rs adds seeds/codec_frames (a frame of every standard
# node and graph, written by the C reference, for decompress and
# decompress_typed) and seeds/node_params (for node_roundtrip).
#
# Usage: fuzz/afl/prepare-seeds.sh

set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
vendor="$here/../../openzl-ffi/vendor"
cd "$here"

files=""
for path in "$vendor/openzl/README.md" "$vendor/openzl/include/openzl/zl_compress.h" \
    "$vendor/zstd/lib/zstd.h" "$vendor/lz4/lib/lz4.c"; do
    [ -f "$path" ] && files="$files $path"
done
if [ -z "$files" ]; then
    echo "the vendored submodules are not checked out; using synthetic payloads only" >&2
    echo "run: git submodule update --init openzl-ffi/vendor/openzl openzl-ffi/vendor/zstd openzl-ffi/vendor/lz4" >&2
fi

# shellcheck disable=SC2086 # the file list is split on purpose
cargo run --quiet --release --example prepare_seeds -- "$here/seeds" $files
# One reference frame per standard node and graph, and node_roundtrip inputs.
cargo run --quiet --release --example codec_seeds -- "$here/seeds"
