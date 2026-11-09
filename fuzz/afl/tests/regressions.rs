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
fn decompress() {
    replay("decompress", mopenzl_afl::targets::decompress);
}

#[test]
fn roundtrip() {
    replay("roundtrip", mopenzl_afl::targets::roundtrip);
}

#[test]
fn stream_compress() {
    replay("stream_compress", mopenzl_afl::targets::stream_compress);
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
