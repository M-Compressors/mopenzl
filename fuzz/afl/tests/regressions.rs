//! Replays every committed fuzz input through its target body.
//!
//! AFL finds an input, `cargo afl tmin` shrinks it, the minimised bytes land
//! in `regressions/<target>/`, and from then on `cargo afl test` re-checks
//! them without an instrumented binary.

use std::fs;
use std::path::{Path, PathBuf};

fn inputs_for(target: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("regressions")
        .join(target);
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .map(|entry| entry.expect("unreadable directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "bin"))
        .collect();
    paths.sort();
    paths
}

fn replay(target: &str, body: fn(&[u8])) {
    let inputs = inputs_for(target);
    assert!(!inputs.is_empty(), "no regression inputs for {target}");
    for path in inputs {
        let data = fs::read(&path).expect("readable regression input");
        body(&data);
    }
}

#[test]
fn custom_codec() {
    replay("custom_codec", mopenzl_afl::targets::custom_codec);
}

#[test]
fn framed_decode() {
    replay("framed_decode", mopenzl_afl::framed_targets::framed_decode);
}

#[test]
fn framed_roundtrip() {
    replay(
        "framed_roundtrip",
        mopenzl_afl::framed_targets::framed_roundtrip,
    );
}

#[test]
fn decompress() {
    replay("decompress", mopenzl_afl::targets::decompress);
}

#[test]
fn roundtrip() {
    replay("roundtrip", mopenzl_afl::targets::roundtrip);
}

#[test]
fn parallel() {
    replay("parallel", mopenzl_afl::targets::parallel);
}

#[test]
fn stream_compress() {
    replay("stream_compress", mopenzl_afl::targets::stream_compress);
}

#[test]
fn decompress_typed() {
    replay("decompress_typed", mopenzl_afl::targets::decompress_typed);
}

#[test]
fn typed_roundtrip() {
    replay("typed_roundtrip", mopenzl_afl::targets::typed_roundtrip);
}

#[test]
fn graph_configs() {
    replay("graph_configs", mopenzl_afl::targets::graph_configs);
}

#[test]
fn node_roundtrip() {
    replay("node_roundtrip", mopenzl_afl::targets::node_roundtrip);
}

#[test]
fn committed_encoder_inputs_decode_a_configuration() {
    for target in ["roundtrip", "stream_compress"] {
        for path in inputs_for(target) {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if name.starts_with("boundary-") {
                continue;
            }
            let data = fs::read(&path).expect("readable regression input");
            let decoded = mopenzl_afl::decode_config(&data)
                .unwrap_or_else(|| panic!("{target}/{name} decodes no configuration"));
            assert!(
                !decoded.rest.is_empty(),
                "{target}/{name} carries no payload"
            );
        }
    }
}

#[test]
fn every_binary_has_a_libfuzzer_adapter_and_regressions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for entry in fs::read_dir(root.join("src/bin")).expect("src/bin") {
        let path = entry.expect("unreadable directory entry").path();
        let name = path.file_stem().and_then(|name| name.to_str()).unwrap();
        assert!(
            root.join("../libfuzzer/fuzz_targets")
                .join(format!("{name}.rs"))
                .is_file(),
            "{name} has no libFuzzer adapter"
        );
        assert!(
            root.join("regressions").join(name).is_dir(),
            "{name} has no regressions directory"
        );
    }
}

/// Replays extra corpora named by `FUZZ_REPLAY`, a comma-separated list of
/// `target=directory`, through the target bodies: how the reach of a seed or
/// libFuzzer corpus is measured under `cargo llvm-cov` (see
/// `architecture/fuzzing.md`). Without the variable the test does nothing.
#[test]
fn replays_corpora_named_by_the_environment() {
    let Ok(list) = std::env::var("FUZZ_REPLAY") else {
        return;
    };
    for entry in list.split(',').filter(|entry| !entry.is_empty()) {
        let (target, directory) = entry
            .split_once('=')
            .unwrap_or_else(|| panic!("FUZZ_REPLAY entry {entry:?} is not target=directory"));
        let body: fn(&[u8]) = match target {
            "framed_decode" => mopenzl_afl::framed_targets::framed_decode,
            "framed_roundtrip" => mopenzl_afl::framed_targets::framed_roundtrip,
            "decompress" => mopenzl_afl::targets::decompress,
            "roundtrip" => mopenzl_afl::targets::roundtrip,
            "parallel" => mopenzl_afl::targets::parallel,
            "stream_compress" => mopenzl_afl::targets::stream_compress,
            "decompress_typed" => mopenzl_afl::targets::decompress_typed,
            "typed_roundtrip" => mopenzl_afl::targets::typed_roundtrip,
            "graph_configs" => mopenzl_afl::targets::graph_configs,
            "custom_codec" => mopenzl_afl::targets::custom_codec,
            "node_roundtrip" => mopenzl_afl::targets::node_roundtrip,
            other => panic!("unknown fuzz target {other}"),
        };
        let mut paths: Vec<PathBuf> = fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("{directory}: {error}"))
            .map(|entry| entry.expect("unreadable directory entry").path())
            .filter(|path| path.is_file())
            .collect();
        paths.sort();
        for path in paths {
            body(&fs::read(&path).expect("readable corpus input"));
        }
    }
}
