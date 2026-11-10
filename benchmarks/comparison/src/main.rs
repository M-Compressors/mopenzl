//! mopenzl against OpenZL v0.3.0 (C, through `openzl-ffi`) on the Silesia
//! corpus, one standard graph at a time.
//!
//! ```text
//! mopenzl-comparison run --corpus DIR [--cases a,b] [--files x,y] [--out results.csv]
//!                        [--min-time-ms 400] [--rounds 5] [--limit BYTES]
//! mopenzl-comparison profile --corpus DIR --case NAME --file NAME --side rust|c
//!                        --op compress|decompress [--iters 1] [--limit BYTES]
//! mopenzl-comparison verify --corpus DIR [--cases a,b] [--files x,y] [--limit BYTES]
//! mopenzl-comparison list
//! ```
//!
//! `run` compresses every (case, file) pair once on both sides and requires
//! byte-identical frames; both decoders must restore the input. Only then
//! are both sides timed, alternating Rust and C every round, each round
//! running enough iterations to last `--min-time-ms`. A CSV row per side and
//! operation records the median and the minimum nanoseconds per call.
//!
//! The Rust side reuses its `Compressor` and `Decompressor`; the C side
//! creates its contexts per call (`ZL_compress`-style callers), which is
//! negligible on files of megabytes. Both decoders return a fresh `Vec`.
//!
//! `profile` runs one side and operation `--iters` times without timing, for
//! `valgrind --tool=callgrind`, `perf` or the `hotpath` features.

use std::fmt::Write as _;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mopenzl::graph::{CompressorGraph, StandardGraph};
use mopenzl::input::{Input, Numbers, Records};
use mopenzl::{Compressor, Decompressor, EncoderConfig};
use openzl_ffi::graph::{self, Graph, Input as CInput, cparam};

/// The Silesia files, in the corpus's own order.
const FILES: [&str; 12] = [
    "dickens", "mozilla", "mr", "nci", "ooffice", "osdb", "reymont", "samba", "sao", "webster",
    "x-ray", "xml",
];

/// How a case presents a file to the compressor.
#[derive(Clone, Copy, Debug)]
enum Kind {
    /// Raw bytes.
    Serial,
    /// Little-endian integers of this width (the tail is dropped).
    Numeric(usize),
    /// Fixed-width records of this width (the tail is dropped).
    Struct(usize),
}

impl Kind {
    const fn width(self) -> usize {
        match self {
            Self::Serial => 1,
            Self::Numeric(width) | Self::Struct(width) => width,
        }
    }
}

/// One benchmark case: a standard graph over a typed view of some files.
#[derive(Clone, Copy, Debug)]
struct Case {
    name: &'static str,
    graph: u32,
    kind: Kind,
    /// `None` runs every file.
    files: Option<&'static [&'static str]>,
}

/// 16-bit images and 32-bit tables of the corpus.
const IMAGES16: &[&str] = &["mr", "x-ray"];
const TABLES32: &[&str] = &["sao", "osdb", "nci"];

const CASES: &[Case] = &[
    Case {
        name: "zstd",
        graph: 9,
        kind: Kind::Serial,
        files: None,
    },
    Case {
        name: "lz4",
        graph: 22,
        kind: Kind::Serial,
        files: None,
    },
    Case {
        name: "lz",
        graph: 28,
        kind: Kind::Serial,
        files: None,
    },
    Case {
        name: "huffman",
        graph: 4,
        kind: Kind::Serial,
        files: None,
    },
    Case {
        name: "fse",
        graph: 3,
        kind: Kind::Serial,
        files: None,
    },
    Case {
        name: "entropy",
        graph: 7,
        kind: Kind::Serial,
        files: None,
    },
    Case {
        name: "huffman_pivco",
        graph: 6,
        kind: Kind::Serial,
        files: None,
    },
    Case {
        name: "flatpack",
        graph: 11,
        kind: Kind::Serial,
        files: None,
    },
    Case {
        name: "generic",
        graph: 13,
        kind: Kind::Serial,
        files: None,
    },
    Case {
        name: "field_lz_struct4",
        graph: 12,
        kind: Kind::Struct(4),
        files: Some(TABLES32),
    },
    Case {
        name: "bitpack_num16",
        graph: 10,
        kind: Kind::Numeric(2),
        files: Some(IMAGES16),
    },
    Case {
        name: "numeric16",
        graph: 16,
        kind: Kind::Numeric(2),
        files: Some(IMAGES16),
    },
    Case {
        name: "numeric32",
        graph: 16,
        kind: Kind::Numeric(4),
        files: Some(TABLES32),
    },
    Case {
        name: "generic_num16",
        graph: 13,
        kind: Kind::Numeric(2),
        files: Some(IMAGES16),
    },
    Case {
        name: "generic_num32",
        graph: 13,
        kind: Kind::Numeric(4),
        files: Some(TABLES32),
    },
];

/// The global parameters both sides run with: the library defaults.
fn config() -> EncoderConfig {
    EncoderConfig::default()
}

/// [`config`] as C global parameters.
fn c_params() -> Vec<(i32, i32)> {
    let config = config();
    /// `ZL_CParam_decompressionLevel`.
    const DECOMPRESSION_LEVEL: i32 = 3;
    vec![
        (cparam::LEVEL, config.level().get()),
        (DECOMPRESSION_LEVEL, i32::from(config.decompression_level())),
        (
            cparam::FORMAT_VERSION,
            u32::from(config.format_version()) as i32,
        ),
        (
            cparam::CONTENT_CHECKSUM,
            1 + i32::from(!config.content_checksum()),
        ),
        (
            cparam::COMPRESSED_CHECKSUM,
            1 + i32::from(!config.compressed_checksum()),
        ),
        (cparam::PERMISSIVE, 1 + i32::from(!config.permissive())),
        (cparam::MIN_STREAM_SIZE, config.min_stream_size() as i32),
        (
            cparam::STORE_ON_EXPANSION,
            1 + i32::from(!config.store_on_expansion()),
        ),
    ]
}

/// A file prepared for one case on both sides.
struct Prepared {
    case: Case,
    file: String,
    bytes: Vec<u8>,
    compressor: Compressor,
    decompressor: Decompressor,
    c_graph: Graph,
    c_params: Vec<(i32, i32)>,
}

impl Prepared {
    fn new(case: Case, file: &str, mut bytes: Vec<u8>) -> Result<Self, String> {
        bytes.truncate(bytes.len() / case.kind.width() * case.kind.width());
        let standard = StandardGraph::try_from(case.graph)
            .map_err(|error| format!("graph {}: {error}", case.graph))?;
        let compressor = Compressor::builder(config())
            .with_graph(CompressorGraph::standard(standard))
            .build()
            .map_err(|error| format!("compressor: {error}"))?;
        Ok(Self {
            case,
            file: file.to_owned(),
            bytes,
            compressor,
            decompressor: Decompressor::default(),
            c_graph: Graph::Id(case.graph),
            c_params: c_params(),
        })
    }

    fn c_input(&self) -> CInput<'_> {
        match self.case.kind {
            Kind::Serial => CInput::Serial(&self.bytes),
            Kind::Numeric(width) => CInput::Numeric {
                bytes: &self.bytes,
                width,
            },
            Kind::Struct(width) => CInput::Struct {
                bytes: &self.bytes,
                width,
            },
        }
    }

    fn rust_compress(&mut self) -> Result<Vec<u8>, String> {
        self.compressor
            .compress_inputs(&[input(self.case.kind, &self.bytes)])
            .map_err(|error| format!("rust compress: {error}"))
    }

    fn c_compress(&self) -> Result<Vec<u8>, String> {
        graph::compress(&[self.c_input()], &self.c_graph, &self.c_params)
            .map_err(|error| format!("c compress: {error:?}"))
    }

    fn rust_decompress(&mut self, frame: &[u8]) -> Result<Vec<u8>, String> {
        match self.case.kind {
            Kind::Serial => self.decompressor.decompress(frame),
            _ => self
                .decompressor
                .decompress_typed(frame)
                .map(|mut outputs| outputs.swap_remove(0).into_content()),
        }
        .map_err(|error| format!("rust decompress: {error}"))
    }

    fn c_decompress(&self, frame: &[u8]) -> Result<Vec<u8>, String> {
        match self.case.kind {
            Kind::Serial => openzl_ffi::decompress(frame),
            _ => graph::decompress(frame).map(|mut outputs| outputs.swap_remove(0).bytes),
        }
        .map_err(|error| format!("c decompress: {error:?}"))
    }

    /// Compresses on both sides and checks identity and both round trips.
    fn verify(&mut self) -> Result<Vec<u8>, String> {
        let c = self.c_compress()?;
        let rust = self.rust_compress()?;
        if rust != c {
            let first = rust.iter().zip(&c).position(|(a, b)| a != b);
            return Err(format!(
                "frames differ: rust {} bytes, c {} bytes, first difference at {first:?}",
                rust.len(),
                c.len()
            ));
        }
        if self.rust_decompress(&rust)? != self.bytes {
            return Err("rust decoder did not restore the input".into());
        }
        if self.c_decompress(&rust)? != self.bytes {
            return Err("c decoder did not restore the input".into());
        }
        Ok(rust)
    }
}

/// `bytes` as the typed input `kind` describes.
fn input(kind: Kind, bytes: &[u8]) -> Input<'_> {
    match kind {
        Kind::Serial => Input::from(bytes),
        Kind::Numeric(width) => {
            Input::from(Numbers::try_from((bytes, width)).expect("numeric width"))
        }
        Kind::Struct(width) => {
            Input::from(Records::try_from((bytes, width)).expect("record width"))
        }
    }
}

/// One side and operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Op {
    RustCompress,
    CCompress,
    RustDecompress,
    CDecompress,
}

impl Op {
    const ALL: [Self; 4] = [
        Self::RustCompress,
        Self::CCompress,
        Self::RustDecompress,
        Self::CDecompress,
    ];

    const fn side(self) -> &'static str {
        match self {
            Self::RustCompress | Self::RustDecompress => "rust",
            Self::CCompress | Self::CDecompress => "c",
        }
    }

    const fn op(self) -> &'static str {
        match self {
            Self::RustCompress | Self::CCompress => "compress",
            Self::RustDecompress | Self::CDecompress => "decompress",
        }
    }

    fn call(self, prepared: &mut Prepared, frame: &[u8]) {
        let result = match self {
            Self::RustCompress => prepared.rust_compress().map(|v| v.len()),
            Self::CCompress => prepared.c_compress().map(|v| v.len()),
            Self::RustDecompress => prepared.rust_decompress(black_box(frame)).map(|v| v.len()),
            Self::CDecompress => prepared.c_decompress(black_box(frame)).map(|v| v.len()),
        };
        black_box(result.expect("verified operation failed"));
    }
}

/// Nanoseconds per call over `iters` calls.
fn time(op: Op, prepared: &mut Prepared, frame: &[u8], iters: u32) -> f64 {
    let start = Instant::now();
    for _ in 0..iters {
        op.call(prepared, frame);
    }
    start.elapsed().as_nanos() as f64 / f64::from(iters)
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

struct Args {
    command: String,
    corpus: PathBuf,
    cases: Option<Vec<String>>,
    files: Option<Vec<String>>,
    out: Option<PathBuf>,
    min_time: Duration,
    rounds: usize,
    limit: Option<usize>,
    case: Option<String>,
    file: Option<String>,
    side: Option<String>,
    op: Option<String>,
    iters: u32,
}

fn parse_args() -> Result<Args, String> {
    let mut raw = std::env::args().skip(1);
    let command = raw.next().ok_or("missing command: run, profile or list")?;
    let mut args = Args {
        command,
        corpus: std::env::var_os("SILESIA").map_or_else(|| PathBuf::from("silesia"), PathBuf::from),
        cases: None,
        files: None,
        out: None,
        min_time: Duration::from_millis(400),
        rounds: 5,
        limit: None,
        case: None,
        file: None,
        side: None,
        op: None,
        iters: 1,
    };
    let list = |value: String| value.split(',').map(str::to_owned).collect::<Vec<_>>();
    while let Some(flag) = raw.next() {
        let value = raw.next().ok_or_else(|| format!("{flag} needs a value"))?;
        let number = |value: &str| value.parse::<u64>().map_err(|e| format!("{flag}: {e}"));
        match flag.as_str() {
            "--corpus" => args.corpus = PathBuf::from(value),
            "--cases" => args.cases = Some(list(value)),
            "--files" => args.files = Some(list(value)),
            "--out" => args.out = Some(PathBuf::from(value)),
            "--min-time-ms" => args.min_time = Duration::from_millis(number(&value)?),
            "--rounds" => args.rounds = number(&value)? as usize,
            "--limit" => args.limit = Some(number(&value)? as usize),
            "--case" => args.case = Some(value),
            "--file" => args.file = Some(value),
            "--side" => args.side = Some(value),
            "--op" => args.op = Some(value),
            "--iters" => args.iters = number(&value)? as u32,
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    Ok(args)
}

fn read(corpus: &Path, file: &str, limit: Option<usize>) -> Result<Vec<u8>, String> {
    let mut bytes = std::fs::read(corpus.join(file))
        .map_err(|error| format!("{}: {error}", corpus.join(file).display()))?;
    if let Some(limit) = limit {
        bytes.truncate(limit);
    }
    Ok(bytes)
}

fn selected(args: &Args) -> Vec<(Case, &'static str)> {
    CASES
        .iter()
        .filter(|case| {
            args.cases
                .as_ref()
                .is_none_or(|names| names.iter().any(|n| n == case.name))
        })
        .flat_map(|case| {
            let files: &[&str] = case.files.unwrap_or(&FILES);
            files.iter().map(move |&file| (*case, file))
        })
        .filter(|(_, file)| {
            args.files
                .as_ref()
                .is_none_or(|names| names.iter().any(|n| n == file))
        })
        .collect()
}

/// Checks identity and both round trips of every selected pair, untimed.
fn verify(args: &Args) -> Result<(), String> {
    let mut failures = 0;
    for (case, file) in selected(args) {
        let bytes = read(&args.corpus, file, args.limit)?;
        let mut prepared = Prepared::new(case, file, bytes)?;
        match prepared.verify() {
            Ok(frame) => println!(
                "{:<16} {:<8} {:>10} -> {:>10} identical",
                case.name,
                file,
                prepared.bytes.len(),
                frame.len()
            ),
            Err(error) => {
                failures += 1;
                println!("{:<16} {:<8} FAILED: {error}", case.name, file);
            }
        }
    }
    if failures > 0 {
        return Err(format!("{failures} pair(s) failed"));
    }
    Ok(())
}

fn run(args: &Args) -> Result<(), String> {
    let mut csv =
        String::from("case,file,bytes,compressed,side,op,median_ns,min_ns,rounds,iters\n");
    let mut failures = 0;
    for (case, file) in selected(args) {
        let bytes = read(&args.corpus, file, args.limit)?;
        let mut prepared = Prepared::new(case, file, bytes)?;
        let frame = match prepared.verify() {
            Ok(frame) => frame,
            Err(error) => {
                failures += 1;
                eprintln!("{} / {}: SKIPPED: {error}", case.name, prepared.file);
                let _ = writeln!(
                    csv,
                    "{},{},{},,,error,,,,",
                    case.name,
                    file,
                    prepared.bytes.len()
                );
                continue;
            }
        };
        // Calibrate every operation on one call, then alternate sides.
        let iters: Vec<u32> = Op::ALL
            .iter()
            .map(|&op| {
                let once = time(op, &mut prepared, &frame, 1).max(1.0);
                ((args.min_time.as_nanos() as f64 / once).ceil() as u32).max(1)
            })
            .collect();
        let mut samples = vec![Vec::with_capacity(args.rounds); Op::ALL.len()];
        for _ in 0..args.rounds {
            for (index, &op) in Op::ALL.iter().enumerate() {
                samples[index].push(time(op, &mut prepared, &frame, iters[index]));
            }
        }
        let mut line = format!(
            "{:<16} {:<8} {:>10} -> {:>10}",
            case.name,
            file,
            prepared.bytes.len(),
            frame.len()
        );
        for (index, &op) in Op::ALL.iter().enumerate() {
            let min = samples[index].iter().copied().fold(f64::INFINITY, f64::min);
            let med = median(&mut samples[index]);
            let _ = writeln!(
                csv,
                "{},{},{},{},{},{},{:.0},{:.0},{},{}",
                case.name,
                file,
                prepared.bytes.len(),
                frame.len(),
                op.side(),
                op.op(),
                med,
                min,
                args.rounds,
                iters[index]
            );
            let mibs = prepared.bytes.len() as f64 / med * 1e9 / (1 << 20) as f64;
            let _ = write!(line, "  {}-{} {:>8.1}", op.side(), &op.op()[..1], mibs);
        }
        let speed = |rust: usize, c: usize| {
            let r = median(&mut samples[rust].clone());
            let c = median(&mut samples[c].clone());
            c / r
        };
        let _ = write!(
            line,
            "  | rust/c compress {:.3}x decompress {:.3}x",
            speed(0, 1),
            speed(2, 3)
        );
        println!("{line}");
    }
    if let Some(out) = &args.out {
        std::fs::write(out, csv).map_err(|error| format!("{}: {error}", out.display()))?;
    }
    if failures > 0 {
        eprintln!("{failures} pair(s) skipped");
    }
    Ok(())
}

fn profile(args: &Args) -> Result<(), String> {
    let name = args.case.as_deref().ok_or("--case is required")?;
    let file = args.file.as_deref().ok_or("--file is required")?;
    let case = *CASES
        .iter()
        .find(|case| case.name == name)
        .ok_or("unknown case")?;
    let op = match (args.side.as_deref(), args.op.as_deref()) {
        (Some("rust"), Some("compress")) => Op::RustCompress,
        (Some("c"), Some("compress")) => Op::CCompress,
        (Some("rust"), Some("decompress")) => Op::RustDecompress,
        (Some("c"), Some("decompress")) => Op::CDecompress,
        _ => return Err("--side rust|c and --op compress|decompress are required".into()),
    };
    let bytes = read(&args.corpus, file, args.limit)?;
    let mut prepared = Prepared::new(case, file, bytes)?;
    // Decompression needs a frame; C writes it so the Rust compressor never
    // runs inside a decompression profile.
    let frame = if op.op() == "decompress" {
        prepared.c_compress()?
    } else {
        Vec::new()
    };
    let start = Instant::now();
    for _ in 0..args.iters {
        op.call(&mut prepared, &frame);
    }
    let elapsed = start.elapsed();
    eprintln!(
        "{name} {file} {} {}: {} iters, {:.1} MiB/s",
        op.side(),
        op.op(),
        args.iters,
        prepared.bytes.len() as f64 * f64::from(args.iters)
            / elapsed.as_secs_f64()
            / (1 << 20) as f64
    );
    Ok(())
}

#[cfg_attr(feature = "hotpath", hotpath::main)]
fn main() {
    let result = parse_args().and_then(|args| match args.command.as_str() {
        "run" => run(&args),
        "verify" => verify(&args),
        "profile" => profile(&args),
        "list" => {
            for case in CASES {
                println!(
                    "{} graph {} {:?} {:?}",
                    case.name,
                    case.graph,
                    case.kind,
                    case.files.unwrap_or(&FILES)
                );
            }
            Ok(())
        }
        other => Err(format!("unknown command {other}")),
    });
    if let Err(error) = result {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
}
