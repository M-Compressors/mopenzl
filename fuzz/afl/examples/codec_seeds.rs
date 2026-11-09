//! Writes the codec seed corpora: `prepare-seeds.sh` runs this.
//!
//! Usage: `cargo run --release --example codec_seeds -- <seeds dir>`
//!
//! The decoders of the standard codecs are reached only by frames that name
//! them, which mutation rarely invents. This generator asks the C reference,
//! through `openzl_ffi`, for one small frame of every standard node and one per
//! standard graph, so a campaign starts with every decoder on its path:
//!
//! - `codec_frames/`: for `decompress` and `decompress_typed`. `n<node>-v<version>`
//!   is the node run alone over a small input of its type, with every output
//!   stored (the shape `tests/encode_*.rs` compare); `g<graph>-<case>` is a
//!   standard graph over one of a few typed inputs. Frames the reference
//!   refuses (a node outside its format version, a graph that cannot take the
//!   type) are skipped and listed on standard error.
//! - `node_params/`: for `node_roundtrip`, one input per node with default
//!   parameters and one with a single integer parameter.
//!
//! When `REGRESSIONS_DIR` names a directory, the `codec_frames` under 4 KiB
//! are also written to `<dir>/decompress_typed/codec-<name>.bin` and the
//! `node_params` to `<dir>/node_roundtrip/seed-<name>.bin`, the committed
//! corpus `cargo afl test` replays.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use mopenzl::DataType;
use mopenzl_afl::targets::{NODE_IDS, node_input_type};
use openzl_ffi::graph::{self, Graph, Input, cparam};
use openzl_ffi::structure::{self, Run};

/// Elements per generated input.
const N: usize = 64;

/// Standard graphs runnable without extra configuration (the list of
/// `tests/standard_graphs.rs`).
const GRAPHS: &[u32] = &[
    2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 19, 22, 23, 24, 25, 26, 27, 28, 29, 45, 46,
    69,
];

fn text(len: usize) -> Vec<u8> {
    const WORDS: [&[u8]; 6] = [b"openzl ", b"frame ", b"codec ", b"graph ", b"the ", b"of "];
    let mut out = Vec::with_capacity(len + 8);
    let mut state = 12345u32;
    while out.len() < len {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
        out.extend_from_slice(WORDS[(state >> 16) as usize % WORDS.len()]);
    }
    out.truncate(len);
    out
}

fn numbers(width: usize) -> Vec<u8> {
    (0..N as u64)
        .flat_map(|i| (i * 3 + (i % 7) * 1000).to_le_bytes()[..width].to_vec())
        .collect()
}

fn records(width: usize) -> Vec<u8> {
    (0..N)
        .flat_map(|i| (0..width).map(move |j| ((i / 3) as u8).wrapping_add(j as u8)))
        .collect()
}

fn strings() -> (Vec<u8>, Vec<u32>) {
    let words: Vec<String> = (0..16).map(|i| format!("w{}", i * 37 % 101)).collect();
    (
        words.concat().into_bytes(),
        words.iter().map(|word| word.len() as u32).collect(),
    )
}

fn digits() -> (Vec<u8>, Vec<u32>) {
    let words: Vec<String> = (0..16).map(|i| format!("{}", i * 37 % 1013)).collect();
    (
        words.concat().into_bytes(),
        words.iter().map(|word| word.len() as u32).collect(),
    )
}

/// An owned reference input.
#[derive(Clone)]
enum Own {
    Serial(Vec<u8>),
    Struct(Vec<u8>, usize),
    Numeric(Vec<u8>, usize),
    Strings(Vec<u8>, Vec<u32>),
}

impl Own {
    fn view(&self) -> Input<'_> {
        match self {
            Self::Serial(bytes) => Input::Serial(bytes),
            Self::Struct(bytes, width) => Input::Struct {
                bytes,
                width: *width,
            },
            Self::Numeric(bytes, width) => Input::Numeric {
                bytes,
                width: *width,
            },
            Self::Strings(bytes, lens) => Input::String { bytes, lens },
        }
    }
}

fn input_of(data_type: DataType, width: usize) -> Own {
    match data_type {
        DataType::Serial => Own::Serial(text(4 * N)),
        DataType::Struct => Own::Struct(records(width), width),
        DataType::Numeric => Own::Numeric(numbers(width), width),
        DataType::String => {
            let (bytes, lens) = strings();
            Own::Strings(bytes, lens)
        }
    }
}

fn usizes(values: &[usize]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_ne_bytes()).collect()
}

/// The run (node, parameters) that gives a node a valid configuration.
fn run_for(id: u32) -> Run {
    let ints = |ints: &[(i32, i32)]| Run::RawNode {
        node: id,
        ints: ints.to_vec(),
        copies: vec![],
    };
    let copies = |copies: Vec<(i32, Vec<u8>)>| Run::RawNode {
        node: id,
        ints: vec![],
        copies,
    };
    match id {
        5 => Run::Dispatch {
            sizes: vec![4 * N / 2, 4 * N / 2],
            tags: vec![0, 1],
            tags_count: 2,
        },
        13 => ints(&[(1, 4)]),
        27 => ints(&[(1, 3)]),
        32 => Run::RawNode {
            node: id,
            ints: vec![(47, 2)],
            copies: vec![(48, (0..16u16).flat_map(|i| (i % 2).to_le_bytes()).collect())],
        },
        50 | 51 => Run::Sentinel {
            indices: vec![3, 10],
            sentinel: u64::MAX,
        },
        66 => copies(vec![(323, usizes(&[2 * N, 2 * N]))]),
        67 | 68 => copies(vec![(323, usizes(&[N / 2, N / 2]))]),
        69 => copies(vec![(387, usizes(&[2, 2]))]),
        _ => ints(&[]),
    }
}

/// The inputs of a node: two for variable-count nodes and `mux_lengths`.
fn node_inputs(id: u32, width: usize) -> Vec<Own> {
    let one = match id {
        70 => Own::Serial(vec![b'a'; 4 * N]),
        71 => Own::Struct(vec![7; width * N], width),
        38 => Own::Strings(digits().0, digits().1),
        43 | 44 => Own::Numeric(
            numbers(width)
                .chunks(width)
                .flat_map(|e| {
                    let mut e = e.to_vec();
                    e[0] |= 1;
                    e
                })
                .collect(),
            width,
        ),
        _ => input_of(node_input_type(id), width),
    };
    match id {
        33..=37 | 39 | 53 | 74 => vec![one.clone(), one],
        _ => vec![one],
    }
}

fn params(version: u32) -> Vec<(i32, i32)> {
    vec![
        (cparam::LEVEL, 6),
        (cparam::FORMAT_VERSION, version as i32),
        (cparam::CONTENT_CHECKSUM, 1),
        (cparam::COMPRESSED_CHECKSUM, 1),
        (cparam::PERMISSIVE, 2),
        (cparam::MIN_STREAM_SIZE, 1),
        (cparam::STORE_ON_EXPANSION, 2),
    ]
}

struct Out {
    frames: PathBuf,
    node_params: PathBuf,
    commit: Option<PathBuf>,
    seen: BTreeSet<Vec<u8>>,
}

impl Out {
    fn frame(&mut self, name: &str, frame: &[u8]) {
        if !self.seen.insert(frame.to_vec()) {
            return;
        }
        fs::write(self.frames.join(name), frame).expect("write seed");
        if let Some(dir) = &self.commit
            && frame.len() < 4096
        {
            let dir = dir.join("decompress_typed");
            fs::create_dir_all(&dir).expect("regression directory");
            fs::write(dir.join(format!("codec-{name}.bin")), frame).expect("write regression");
        }
    }

    fn node_param(&self, name: &str, seed: &[u8]) {
        fs::write(self.node_params.join(name), seed).expect("write seed");
        if let Some(dir) = &self.commit {
            let dir = dir.join("node_roundtrip");
            fs::create_dir_all(&dir).expect("regression directory");
            fs::write(dir.join(format!("seed-{name}.bin")), seed).expect("write regression");
        }
    }
}

fn fresh(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).expect("seed directory");
}

fn main() {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("usage: codec_seeds <seeds dir>"),
    );
    let mut out = Out {
        frames: root.join("codec_frames"),
        node_params: root.join("node_params"),
        commit: std::env::var_os("REGRESSIONS_DIR").map(PathBuf::from),
        seen: BTreeSet::new(),
    };
    // Committed seeds this generator wrote earlier are replaced, not kept.
    if let Some(dir) = &out.commit {
        for (target, prefix) in [("decompress_typed", "codec-"), ("node_roundtrip", "seed-n")] {
            let Ok(entries) = fs::read_dir(dir.join(target)) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with(prefix) {
                    fs::remove_file(entry.path()).expect("stale seed is removable");
                }
            }
        }
    }
    fresh(&out.frames);
    fresh(&out.node_params);
    let mut skipped = Vec::new();

    for id in NODE_IDS {
        let mut written = false;
        for (version, width) in [(27, 4), (24, 4), (27, 8), (22, 2), (27, 1), (27, 2)] {
            if written && (version != 27 || width == 4) && version != 24 {
                continue;
            }
            let inputs = node_inputs(id, width);
            let views: Vec<Input<'_>> = inputs.iter().map(Own::view).collect();
            if let Ok(frame) = structure::compress(&views, &run_for(id), &params(version)) {
                if std::env::var_os("SEED_DEBUG").is_some() {
                    eprintln!(
                        "node {id} v{version} w{width}: {} bytes {:02x?}",
                        frame.len(),
                        &frame[..frame.len().min(12)]
                    );
                }
                out.frame(&format!("n{id}-v{version}-w{width}"), &frame);
                written = true;
            }
        }
        if !written {
            skipped.push(format!("node {id}"));
        }
    }

    let cases: Vec<(&str, Own)> = vec![
        ("text", input_of(DataType::Serial, 1)),
        ("num1", input_of(DataType::Numeric, 1)),
        ("num2", input_of(DataType::Numeric, 2)),
        ("num4", input_of(DataType::Numeric, 4)),
        ("num8", input_of(DataType::Numeric, 8)),
        ("struct4", input_of(DataType::Struct, 4)),
        ("strings", input_of(DataType::String, 1)),
        ("digits", Own::Strings(digits().0, digits().1)),
        ("digitserial", Own::Serial(digits().0)),
        ("constant", Own::Serial(vec![b'z'; 4 * N])),
    ];
    for &graph_id in GRAPHS {
        let mut written = false;
        for (name, case) in &cases {
            for version in [27, 23] {
                if version == 23 && !matches!(*name, "text" | "num4" | "digits") {
                    continue;
                }
                if let Ok(frame) =
                    graph::compress(&[case.view()], &Graph::Id(graph_id), &params(version))
                {
                    out.frame(&format!("g{graph_id}-v{version}-{name}"), &frame);
                    written = true;
                }
            }
        }
        if !written {
            skipped.push(format!("graph {graph_id}"));
        }
    }

    // `node_roundtrip` inputs: header (node index, v27 level 6, shape, type),
    // an optional integer parameter, then the payload.
    for (index, id) in NODE_IDS.into_iter().enumerate() {
        let data_type = node_input_type(id);
        let (shape, payload): (u8, Vec<u8>) = match data_type {
            DataType::Serial => (0, text(4 * N)),
            DataType::Struct => (0x30, records(4)),
            DataType::Numeric => (0x20, numbers(4)),
            DataType::String => (0, text(4 * N)),
        };
        let shape = shape
            | if matches!(id, 33..=37 | 39 | 74) {
                4
            } else {
                0
            };
        let mut seed = vec![index as u8, 0x30, shape, 0];
        seed.extend_from_slice(&payload);
        out.node_param(&format!("n{id}"), &seed);
        let mut seed = vec![index as u8, 0x30, shape | 1, 0, 0, 3, 0, 0, 0];
        seed.extend_from_slice(&payload);
        out.node_param(&format!("n{id}-param"), &seed);
    }

    println!(
        "wrote {} codec frames and {} node seeds; the reference refused: {}",
        fs::read_dir(&out.frames).map_or(0, Iterator::count),
        fs::read_dir(&out.node_params).map_or(0, Iterator::count),
        skipped.join(", "),
    );
}
