#!/usr/bin/env python3
"""Check isolated codec consumers, including imports that must be unavailable.

Every combination of a standard-library mode (none, `std`, `no_std`), the
two codecs and the codec features (`zstd`, ...) builds a throwaway consumer crate that depends on mopenzl with only
those features, then tries to re-export each public item. An item that must
exist has to compile; an item that must not exist has to fail with an
unresolved import, so a gate that leaks an item, or hides one, fails the run.
"""
from itertools import product
from pathlib import Path
import json
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
consumer = root / "target" / "codec-feature-consumer"
(consumer / "src").mkdir(parents=True, exist_ok=True)
# `cargo metadata` rather than `tomllib`, which needs Python 3.11.
metadata = json.loads(subprocess.run(
    ["cargo", "metadata", "--no-deps", "--offline", "--format-version", "1"],
    cwd=root, text=True, capture_output=True, check=True,
).stdout)
features_table = next(
    package for package in metadata["packages"] if package["name"] == "mopenzl"
)["features"]
defaults = features_table["default"]
assert {"std", "compression", "decompression", "all-codecs"} <= set(defaults), defaults
assert "experimental" not in features_table, "add the experimental probes to this script"
# Every OpenZL codec has its own feature, enabled by default. A new codec
# feature must be listed here so its absence is checked too.
CODECS = sorted(features_table["all-codecs"])
assert {"zstd", "custom-codec"} <= set(CODECS), CODECS
known = {"default", "std", "no_std", "compression", "decompression", "all-codecs", *CODECS}
unknown = {
    name for name in features_table
    if name not in known and not name.startswith("hotpath")
}
assert not unknown, f"classify these features in check_codec_features.py: {unknown}"

# A consumer resolves dependencies on its own, so it inherits the root's
# `[patch]` table (an unreleased dependency checked out locally).
root_manifest = (root / "Cargo.toml").read_text()
patch_table = ""
if "[patch.crates-io]" in root_manifest:
    patch_lines = []
    for line in root_manifest.split("[patch.crates-io]", 1)[1].splitlines()[1:]:
        if line.startswith("["):
            break
        patch_lines.append(line)
    patch_table = "[patch.crates-io]\n" + "\n".join(patch_lines) + "\n"

for mode, compression, decompression, codecs in product(
    ["", "std", "no_std"], [False, True], [False, True], [False, True]
):
    if not mode and (compression or decompression):
        continue  # Active codecs need a math provider; empty builds need neither.
    if codecs and not (compression or decompression):
        continue  # Codec features add nothing without a codec direction.
    features = [mode] if mode else []
    features += [name for name, enabled in [
        ("compression", compression), ("decompression", decompression),
    ] if enabled]
    if codecs:
        features += CODECS
    (consumer / "Cargo.toml").write_text(f'''[package]
name = "codec-feature-consumer"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies.mopenzl]
path = {json.dumps(str(root))}
default-features = false
features = {json.dumps(features)}
{patch_table}''')
    either = compression or decompression
    with_std = mode == "std"
    probes = {
        "": True,
        "custom_codec": either and codecs,
        "custom_codec::CustomCodec": either and codecs,
        "custom_codec::Signature": either and codecs,
        "custom_codec::CodecInput": either and codecs,
        "custom_codec::CodecError": either and codecs,
        "custom_codec::EncoderContext": compression and codecs,
        "custom_codec::DecoderContext": decompression and codecs,
        # Shared by both codecs.
        "Backend": either,
        "RetentionPolicy": either,
        "FormatVersion": either,
        "DataType": either,
        "UnsupportedFormatVersion": either,
        # Compression.
        "compressor": compression,
        "Compressor": compression,
        "CompressorBuilder": compression,
        "EncoderConfig": compression,
        "EncoderSession": compression,
        "EncoderSessionOwned": compression,
        "EncoderStatus": compression,
        "EncodeError": compression,
        "ConfigError": compression,
        "Parameter": compression,
        "SizeOverflow": compression,
        "ChunkSize": compression,
        "Chunking": compression,
        "Level": compression,
        "InputSize": compression,
        "Operation": compression,
        "Progress": compression,
        "StreamConfig": compression,
        "graph": compression,
        "graph::params": compression,
        "graph::params::zstd": compression,
        # Decompression.
        "decompressor": decompression,
        "Decompressor": decompression,
        "DecompressorBuilder": decompression,
        "DecoderConfig": decompression,
        "DecoderSession": decompression,
        "DecoderSessionOwned": decompression,
        "DecoderStatus": decompression,
        "DecodeError": decompression,
        "DecodeFailure": decompression,
        "DecodeLimits": decompression,
        "DecodeOperation": decompression,
        "DecodeProgress": decompression,
        "FrameMode": decompression,
        "InvalidDataKind": decompression,
        "Unsupported": decompression,
        "TypedOutput": decompression,
        "FrameInfo": decompression,
        "decompressor::FrameInfo": decompression,
        "decompressor::OutputInfo": decompression,
        # `std::io` adapters.
        "io": with_std and either,
        "io::FinishError": with_std and either,
        "io::EncoderWriter": with_std and compression,
        "io::EncoderReader": with_std and compression,
        "io::EncoderReaderParts": with_std and compression,
        "io::DecoderWriter": with_std and decompression,
        "io::DecoderReader": with_std and decompression,
        "io::DecoderReaderParts": with_std and decompression,
        "compressor::io": with_std and compression,
        "decompressor::io": with_std and decompression,
    }
    if (compression or decompression) and not codecs:
        # Codec features change behaviour, not the API: check a handful.
        probes = {item: probes[item] for item in ["", "Compressor", "Decompressor", "Backend"]}
    # The parallel API follows the std/compression boundary independently of
    # codec features; it must disappear when no_std is selected.
    probes.update({
        "compressor::parallel": with_std and compression,
        "compressor::parallel::ParallelCompressor": with_std and compression,
        "compressor::parallel::ParallelCompressorBuilder": with_std and compression,
        "compressor::parallel::ParallelEncoderSession": with_std and compression,
        "compressor::parallel::RandomAccessSource": with_std and compression,
    })
    for item, expected in probes.items():
        source = "#![no_std]\n" if mode == "no_std" else ""
        if item:
            source += f"pub use mopenzl::{item};\n"
        (consumer / "src" / "lib.rs").write_text(source)
        result = subprocess.run(
            ["cargo", "check", "--offline", "--lib", "--manifest-path", str(consumer / "Cargo.toml")],
            cwd=root, text=True, capture_output=True,
        )
        if (result.returncode == 0) != expected:
            sys.exit(f"{features}: {item or 'empty consumer'}, expected={expected}\n{result.stderr}")
        if not expected and "error[E0432]" not in result.stderr and "error[E0433]" not in result.stderr:
            sys.exit(f"{features}: {item} failed for another reason\n{result.stderr}")
    print(f"{','.join(features) or 'no features'}: codec API gates OK", flush=True)
