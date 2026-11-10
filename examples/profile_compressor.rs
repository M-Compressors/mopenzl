//! Repeatable compression workload for native and Callgrind hotpath reports.
//!
//! Run with `cargo run --release --example profile_compressor --features hotpath-alloc`.

use mopenzl::{Compressor, Decompressor, EncoderConfig, Level};
use std::hint::black_box;

#[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::main)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"the quick brown fox jumps over the lazy dog 0123456789\n".repeat(1300);
    let mut decoder = Decompressor::default();
    for level in [1, 3, 6, 12] {
        let mut encoder = Compressor::new(EncoderConfig::from(Level::try_from(level)?))?;
        let frame = encoder.compress(&input)?;
        assert_eq!(decoder.decompress(&frame)?, input);
        let mut output = Vec::with_capacity(Compressor::max_compressed_size(input.len())?);
        for _ in 0..10 {
            output.clear();
            encoder.compress_into(black_box(&input), &mut output)?;
            black_box(&output);
        }
    }
    Ok(())
}

#[test]
fn profiles_all_levels_with_round_trip_validation() -> Result<(), Box<dyn std::error::Error>> {
    main()
}
