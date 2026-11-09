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
//! Three more corpora cover the typed and configuration boundaries:
//!
//! - `typed_frames/`: typed and multi-input frames of standard graphs, with
//!   truncated and corrupted copies, for `decompress_typed`;
//! - `typed_params/`: a `typed_roundtrip` header and typed inputs;
//! - `configs/`: a dictionary bundle, ML selector and clustering CBOR blobs, an
//!   SDDL description and SDDL2 bytecode, each behind the selector byte
//!   `graph_configs` reads. The SDDL programs are the hand-built encoders of
//!   `tests/support/sddl.rs`, included as a module.
//!
//! When `REGRESSIONS_DIR` names a directory, the small core seeds of these
//! three targets are also written to `<dir>/<target>/seed-<name>.bin`, the
//! committed corpus `cargo afl test` replays.
//!
//! Payloads are synthetic (text, noise, runs, the store/zstd threshold) plus
//! the first 64 KiB of every file named on the command line.

use std::fs;
use std::path::{Path, PathBuf};

use mopenzl::dict::{Dict, DictBundle};
use mopenzl::graph::{ClusteringConfig, CompressorGraph, MlSelectorConfig, StandardGraph};
use mopenzl::input::{Input, Numbers, Records, Strings};
use mopenzl::{Compressor, DataType, EncoderConfig};
use mopenzl_afl::{Schedule, decode_config};

#[path = "../../../tests/support/sddl.rs"]
mod sddl;

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

/// The inputs of every typed seed: text, 32-bit numbers, 4-byte records and
/// five strings.
struct Corpus {
    text: Vec<u8>,
    numbers: Vec<u8>,
    records: Vec<u8>,
    words: Vec<u8>,
    lens: Vec<u32>,
}

impl Corpus {
    fn new() -> Self {
        Self {
            text: text(300, 9),
            numbers: (0..100u32)
                .flat_map(|i| (i * i % 311).to_le_bytes())
                .collect(),
            records: (0..160u32).map(|i| (i / 4) as u8).collect(),
            words: b"alphabetagammadeltaepsilon".to_vec(),
            lens: vec![5, 4, 5, 5, 7],
        }
    }

    fn inputs(&self, kinds: &[u8]) -> Vec<Input<'_>> {
        kinds
            .iter()
            .map(|kind| match kind {
                0 => Input::from(&self.text[..]),
                1 => Input::from(Records::try_from((&self.records[..], 4)).expect("records")),
                2 => Input::from(Numbers::try_from((&self.numbers[..], 4)).expect("numbers")),
                _ => Input::from(
                    Strings::try_from((&self.words[..], &self.lens[..])).expect("strings"),
                ),
            })
            .collect()
    }
}

/// `(name, frame)` of typed frames: a graph and input kinds (0 serial, 1
/// struct, 2 numeric, 3 string).
fn typed_frames(corpus: &Corpus) -> Vec<(String, Vec<u8>)> {
    let cases: [(StandardGraph, &[u8]); 8] = [
        (StandardGraph::Store, &[0]),
        (StandardGraph::Zstd, &[2]),
        (StandardGraph::Fse, &[0]),
        (StandardGraph::CompressGeneric, &[0, 2, 1, 3]),
        (StandardGraph::StringCompress, &[3]),
        (StandardGraph::StructCompress, &[1]),
        (StandardGraph::SelectNumeric, &[2, 2]),
        (StandardGraph::Lz4, &[0]),
    ];
    let config = EncoderConfig::default().with_min_stream_size(1);
    let mut frames = Vec::new();
    for (graph, kinds) in cases {
        let Ok(mut encoder) = Compressor::builder(config)
            .with_graph(CompressorGraph::standard(graph))
            .build()
        else {
            continue;
        };
        let Ok(frame) = encoder.compress_inputs(&corpus.inputs(kinds)) else {
            continue;
        };
        frames.push((format!("{graph:?}-{}", kinds.len()).to_lowercase(), frame));
    }
    frames
}

/// A `typed_roundtrip` input: header bytes, then `(spec, bytes)` per input.
fn typed_params(first: u8, graph: u8, inputs: &[(u8, &[u8])]) -> Vec<u8> {
    let mut seed = vec![first, graph, (inputs.len() - 1) as u8];
    for (spec, bytes) in inputs {
        seed.push(*spec);
        seed.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        seed.extend_from_slice(bytes);
    }
    seed
}

/// `(name, seed)` of the `graph_configs` corpus.
fn config_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let mut seeds = Vec::new();
    let dicts =
        vec![Dict::zstd(b"openzl graph codec zstd frame chunk the of", 3).expect("dictionary")];
    let bundle = DictBundle::try_from(dicts).expect("bundle");
    seeds.push(("bundle", [&[0u8][..], &Vec::from(&bundle)].concat()));

    let selector = MlSelectorConfig::untrained(2).expect("two successors");
    seeds.push(("ml-selector", [&[1u8][..], selector.as_ref()].concat()));

    let clustering = ClusteringConfig::default()
        .with_cluster(DataType::Numeric, 4, 0, 0, [1, 2])
        .with_type_default(DataType::Serial, 1, 1, 0);
    seeds.push(("clustering", [&[2u8][..], &Vec::from(&clustering)].concat()));

    // `{"exprs": []}`, the smallest description, and a record array.
    let empty = [0xA1, 0x65, b'e', b'x', b'p', b'r', b's', 0x80];
    seeds.push(("sddl-empty", [&[3u8][..], &empty].concat()));
    let mut exprs = sddl::dests(2);
    exprs.push(sddl::consume(sddl::array(
        sddl::record(vec![sddl::eat("u2l", 0), sddl::eat("byte", 1)]),
        sddl::num(4),
    )));
    seeds.push(("sddl-records", [&[3u8][..], &sddl::program(exprs)].concat()));

    let halt = sddl::assemble("push.i32 4\nsegment.create_unspecified\nhalt\n");
    seeds.push(("sddl2-segment", [&[4u8, 0][..], &halt].concat()));
    let tagged = sddl::assemble(
        "push.i32 8\npush.type.u32le\ntype.fixed_array\nsegment.create_unspecified\nhalt\n",
    );
    seeds.push(("sddl2-array", [&[4u8, 1][..], &tagged].concat()));
    seeds
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
    let corpus = Corpus::new();
    let typed_frames_dir = root.join("typed_frames");
    let typed_params_dir = root.join("typed_params");
    let configs_dir = root.join("configs");
    for dir in [&typed_frames_dir, &typed_params_dir, &configs_dir] {
        let _ = fs::remove_dir_all(dir);
        fs::create_dir_all(dir).expect("seed directory");
    }
    let commit = std::env::var_os("REGRESSIONS_DIR").map(PathBuf::from);
    let commit_seed = |target: &str, name: &str, bytes: &[u8]| {
        if let Some(dir) = &commit {
            let dir = dir.join(target);
            fs::create_dir_all(&dir).expect("regression directory");
            write(&dir, &format!("seed-{name}.bin"), bytes);
        }
    };

    for (name, frame) in typed_frames(&corpus) {
        write(&typed_frames_dir, &name, &frame);
        commit_seed("decompress_typed", &name, &frame);
        write(
            &typed_frames_dir,
            &format!("{name}-half"),
            &frame[..frame.len() / 2],
        );
        let mut flipped = frame.clone();
        let middle = flipped.len() / 2;
        flipped[middle] ^= 0x40;
        write(&typed_frames_dir, &format!("{name}-flipped"), &flipped);
        let mut twice = frame.clone();
        twice.extend_from_slice(&frame);
        write(&typed_frames_dir, &format!("{name}-twice"), &twice);
    }
    commit_seed("decompress_typed", "empty", &[]);

    // Graph indexes follow `TYPED_GRAPHS`: 0 store, 7 zstd, 11 compress
    // generic, 21 string compress, 22 struct compress.
    let seeds: Vec<(String, Vec<u8>)> = {
        let numbers = &corpus.numbers[..];
        let text = &corpus.text[..];
        let records = &corpus.records[..];
        let strings = &corpus.words[..];
        let mut seeds = vec![
            ("store-text", typed_params(0x38, 0x80, &[(0, text)])),
            (
                "zstd-numbers",
                typed_params(0x38, 0x87, &[(2 | 8, numbers)]),
            ),
            (
                "generic-all",
                typed_params(
                    0x38,
                    0x8B,
                    &[(0, text), (2 | 8, numbers), (1 | 12, records), (3, strings)],
                ),
            ),
            ("string-compress", typed_params(0x38, 0x95, &[(3, strings)])),
            (
                "struct-compress",
                typed_params(0x30, 0x96, &[(1 | 12, records)]),
            ),
            (
                "empty-inputs",
                typed_params(0x38, 0x80, &[(0, &[]), (2, &[])]),
            ),
        ];
        for (index, level) in [0x39u8, 0x3F, 0x00, 0x7E].into_iter().enumerate() {
            seeds.push((
                "level",
                typed_params(
                    level,
                    0x8B,
                    &[(2 | 8, numbers), (0, &text[..index * 40 + 1])],
                ),
            ));
        }
        seeds
            .into_iter()
            .enumerate()
            .map(|(i, (name, bytes))| (format!("{i}-{name}"), bytes))
            .collect()
    };
    for (name, seed) in &seeds {
        write(&typed_params_dir, name, seed);
        commit_seed("typed_roundtrip", name, seed);
    }
    for (name, seed) in config_seeds() {
        write(&configs_dir, name, &seed);
        commit_seed("graph_configs", name, &seed);
        write(
            &configs_dir,
            &format!("{name}-half"),
            &seed[..seed.len() / 2],
        );
    }

    for dir in [
        &params,
        &stream,
        &frames,
        &typed_frames_dir,
        &typed_params_dir,
        &configs_dir,
    ] {
        let count = fs::read_dir(dir).expect("seed directory").count();
        println!("prepared {count} seeds in {}", dir.display());
    }
}
