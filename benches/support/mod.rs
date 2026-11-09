//! Corpora shared by the benchmarks.

#![allow(dead_code, reason = "each benchmark uses a different subset")]

use std::path::Path;

fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2_654_435_761) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 24) as u8
        })
        .collect()
}

/// The vendored OpenZL C sources concatenated: real-world text.
fn source_text(limit: usize) -> Vec<u8> {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("openzl-ffi/vendor/openzl/src/openzl/compress");
    let mut paths: Vec<_> = std::fs::read_dir(&root)
        .unwrap_or_else(|error| {
            panic!(
                "{}: {error} (run `git submodule update --init`)",
                root.display()
            )
        })
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "c"))
        .collect();
    paths.sort();
    let mut text = Vec::new();
    for path in paths {
        text.extend(std::fs::read(path).expect("source file"));
        if text.len() >= limit {
            break;
        }
    }
    text.truncate(limit);
    text
}

/// Structured little-endian records with slowly varying fields.
fn records(len: usize) -> Vec<u8> {
    (0..len / 16)
        .flat_map(|i: usize| {
            let id = i as u64;
            let value = (i as u32).wrapping_mul(7) % 1000;
            [
                id.to_le_bytes().as_slice(),
                &value.to_le_bytes(),
                &[1, 0, 0, 0],
            ]
            .concat()
        })
        .collect()
}

/// Named inputs: per-call cost, text, structured binary and random bytes.
pub fn corpora() -> Vec<(&'static str, Vec<u8>)> {
    let text = source_text(1 << 20);
    vec![
        ("tiny-64B", text[..64].to_vec()),
        ("c-source-1MiB", text),
        ("records-1MiB", records(1 << 20)),
        ("random-256KiB", noise(256 << 10, 3)),
    ]
}
