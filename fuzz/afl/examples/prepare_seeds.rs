//! Writes the AFL seed corpora: `prepare-seeds.sh` runs this.
//!
//! Usage: `cargo run --release --example prepare_seeds -- <seeds dir> [payload file...]`
//!
//! Three corpora, one per input layout:
//!
//! - `params/`: a configuration header (see `decode_config`) and a payload,
//!   for `roundtrip`;
//! - `stream/`: a header, the four control bytes of `stream_compress`, and a
//!   payload;
//! - `frames/`: frames mopenzl writes (byte-identical to the reference's),
//!   plus truncated, corrupted, concatenated and padded copies, for
//!   `decompress`.
//!
//! Payloads are synthetic (text, noise, runs, the store/zstd threshold) plus
//! the first 64 KiB of every file named on the command line.

use std::fs;
use std::path::{Path, PathBuf};

use mopenzl::Compressor;
use mopenzl_afl::{Schedule, decode_config};

/// Large seeds slow every iteration far more than they add coverage.
const MAX_SEED_PAYLOAD: usize = 64 << 10;

/// Configuration headers: the default, every level, every format version,
/// the checksum switches and every chunking.
const HEADERS: &[[u8; 2]] = &[
    [0x38, 0],
    [0x39, 0],
    [0x3A, 0],
    [0x3B, 0],
    [0x3C, 0],
    [0x3D, 0],
    [0x3E, 0],
    [0x3F, 0],
    [0x00, 0],
    [0x08, 0],
    [0x10, 0],
    [0x18, 0],
    [0x20, 0],
    [0x28, 0],
    [0x30, 0],
    [0x78, 0],
    [0xB8, 0],
    [0xF8, 0],
    [0x38, 1],
    [0x38, 2],
    [0x38, 3],
    [0x01, 1],
];

/// `stream_compress` control bytes: output window, flags, seed.
const CONTROLS: &[[u8; 4]] = &[
    [0, 0, 1, 0],
    [255, 1, 7, 3],
    [3, 2, 9, 0],
    [40, 3, 200, 17],
    [1, 5, 77, 1],
    [128, 7, 13, 99],
];

fn text(len: usize, seed: u32) -> Vec<u8> {
    const WORDS: [&[u8]; 10] = [
        b"openzl ", b"format ", b"aware ", b"graph ", b"codec ", b"zstd ", b"chunk ", b"frame ",
        b"the ", b"of ",
    ];
    let mut schedule = Schedule::new(seed);
    let mut out = Vec::with_capacity(len + 16);
    while out.len() < len {
        out.extend_from_slice(WORDS[schedule.bits() as usize % WORDS.len()]);
    }
    out.truncate(len);
    out
}

fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut schedule = Schedule::new(seed);
    (0..len).map(|_| (schedule.bits() >> 24) as u8).collect()
}

fn payloads(files: &[PathBuf]) -> Vec<(String, Vec<u8>)> {
    let mut payloads = vec![
        ("empty".to_owned(), Vec::new()),
        ("one".to_owned(), b"x".to_vec()),
        ("nine".to_owned(), b"123456789".to_vec()),
        ("ten".to_owned(), b"1234567890".to_vec()),
        ("text-1k".to_owned(), text(1000, 1)),
        ("text-40k".to_owned(), text(40_000, 2)),
        ("noise-300".to_owned(), noise(300, 3)),
        ("noise-40k".to_owned(), noise(40_000, 4)),
        ("zeros-64k".to_owned(), vec![0; MAX_SEED_PAYLOAD]),
        ("mixed-64k".to_owned(), {
            let mut mixed = text(32 << 10, 5);
            mixed.extend(noise(32 << 10, 6));
            mixed
        }),
    ];
    for path in files {
        let Ok(mut bytes) = fs::read(path) else {
            eprintln!("skipping unreadable {}", path.display());
            continue;
        };
        bytes.truncate(MAX_SEED_PAYLOAD);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file")
            .replace('.', "-");
        payloads.push((name, bytes));
    }
    payloads
}

fn write(dir: &Path, name: &str, bytes: &[u8]) {
    fs::write(dir.join(name), bytes).unwrap_or_else(|error| panic!("{name}: {error}"));
}

fn main() {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let root = args
        .next()
        .expect("usage: prepare_seeds <seeds dir> [payload file...]");
    let files: Vec<PathBuf> = args.collect();
    let payloads = payloads(&files);

    let [params, stream, frames] = ["params", "stream", "frames"].map(|name| {
        let dir = root.join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("seed directory");
        dir
    });

    for (name, payload) in &payloads {
        for header in HEADERS {
            let mut seed = header.to_vec();
            seed.extend_from_slice(payload);
            write(
                &params,
                &format!("{:02x}{:02x}-{name}", header[0], header[1]),
                &seed,
            );
        }
        for (index, control) in CONTROLS.iter().enumerate() {
            let header = HEADERS[index * 3 % HEADERS.len()];
            let mut seed = header.to_vec();
            seed.extend_from_slice(control);
            seed.extend_from_slice(payload);
            write(&stream, &format!("c{index}-{name}"), &seed);
        }
        for header in [[0x38, 0], [0x01, 1], [0xF8, 0], [0x00, 0], [0x3C, 3]] {
            let decoded = decode_config(&header).expect("a valid header");
            let frame = Compressor::new(decoded.config)
                .expect("valid configuration")
                .compress(payload)
                .expect("compression");
            let tag = format!("{:02x}{:02x}-{name}", header[0], header[1]);
            write(&frames, &tag, &frame);
            write(&frames, &format!("{tag}-half"), &frame[..frame.len() / 2]);
            let mut flipped = frame.clone();
            let middle = flipped.len() / 2;
            flipped[middle] ^= 0x40;
            write(&frames, &format!("{tag}-flipped"), &flipped);
            let mut padded = frame.clone();
            padded.push(0);
            write(&frames, &format!("{tag}-padded"), &padded);
            let mut twice = frame.clone();
            twice.extend_from_slice(&frame);
            write(&frames, &format!("{tag}-twice"), &twice);
        }
    }
    for dir in [&params, &stream, &frames] {
        let count = fs::read_dir(dir).expect("seed directory").count();
        println!("prepared {count} seeds in {}", dir.display());
    }
}
