//! Compresses a file into an OpenZL frame, or decompresses one.
//!
//! ```sh
//! cargo run --release --example compress -- [-d] [-l LEVEL] [--chunk BYTES] INPUT OUTPUT
//! ```
//!
//! Compression streams the input through `Compressor::writer` with its size
//! declared, so with `--chunk` it holds one chunk of input at a time.

#[cfg(not(feature = "no_std"))]
use std::fs::File;
#[cfg(not(feature = "no_std"))]
use std::io::{BufReader, BufWriter};
use std::process::ExitCode;

#[cfg(not(feature = "no_std"))]
use mopenzl::{ChunkSize, Chunking, Compressor, Decompressor, EncoderConfig, InputSize, Level};

#[cfg(not(feature = "no_std"))]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut decompress, mut level, mut chunk) = (false, Level::DEFAULT, Chunking::Whole);
    let mut paths = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-d" => decompress = true,
            "-l" => {
                level = Level::try_from(args.next().ok_or("-l needs a level")?.parse::<i32>()?)?
            }
            "--chunk" => {
                let bytes: usize = args.next().ok_or("--chunk needs a size")?.parse()?;
                chunk = Chunking::Fixed(ChunkSize::try_from(bytes)?);
            }
            _ => paths.push(arg),
        }
    }
    let [input, output] = paths.as_slice() else {
        return Err("usage: compress [-d] [-l LEVEL] [--chunk BYTES] INPUT OUTPUT".into());
    };
    let source = File::open(input)?;
    let size = source.metadata()?.len();
    let mut source = BufReader::new(source);
    let mut sink = BufWriter::new(File::create(output)?);
    if decompress {
        let mut decoder = Decompressor::default();
        std::io::copy(&mut decoder.reader(&mut source), &mut sink)?;
    } else {
        let config = EncoderConfig::from(level).with_chunking(chunk);
        let mut encoder = Compressor::new(config)?;
        let mut writer = encoder.writer(&mut sink, InputSize::Exact(size).into());
        std::io::copy(&mut source, &mut writer)?;
        writer
            .finish()
            .map_err(mopenzl::io::FinishError::into_error)?;
    }
    Ok(())
}

/// The `std::io` adapters are unavailable with `no_std`.
#[cfg(feature = "no_std")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    Err("this example needs the `std` I/O adapters".into())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
