//! Shared helpers and engine-neutral bodies for the AFL fuzz targets.
//!
//! Every target consumes a raw byte string. The helpers here turn its first
//! bytes into an encoder configuration and the matching parameters of the C
//! reference; [`targets`] holds the target bodies. Nothing here depends on
//! AFL: each binary under `src/bin/` is a three line adapter, so a minimised
//! finding replays through `cargo afl test` (see `tests/regressions.rs`), and
//! the libFuzzer adapters in `fuzz/libfuzzer` call the same bodies.

use mopenzl::{ChunkSize, Chunking, EncoderConfig, FormatVersion, Level};
use openzl_ffi::{Params, Ternary};

/// Framed-container state machine and wire-parser oracles.
pub mod framed_targets;
pub mod targets;

/// Largest payload any encoder target compresses.
///
/// Long inputs stop adding coverage quickly while slowing every iteration;
/// a quarter mebibyte still spans eight chunks of the smallest fixed size.
pub const MAX_PAYLOAD: usize = 256 << 10;

/// Largest content any decoder target produces, in either implementation.
///
/// A frame header declares its content size; without a cap a few bytes
/// could ask either decoder for gigabytes.
pub const MAX_OUTPUT: usize = 16 << 20;

/// Compression levels byte 0 selects, by its low three bits: the default
/// first, then the fast, negative, and strong ends of the range.
pub const LEVELS: [i32; 8] = [6, 1, 3, -1, -50, 9, 15, 19];

/// Fixed chunk sizes byte 1 selects, by its low two bits; `0` is
/// [`Chunking::Whole`].
pub const CHUNK_SIZES: [usize; 3] = [32 << 10, 40_000, 100_000];

/// A configuration decoded from fuzz input, and what remains of the input.
#[derive(Debug)]
pub struct Decoded<'a> {
    /// The encoder configuration.
    pub config: EncoderConfig,
    /// The same configuration for `openzl_ffi::compress`.
    pub params: Params,
    /// The payload, at most [`MAX_PAYLOAD`] bytes.
    pub rest: &'a [u8],
}

/// Splits `n` bytes off the front of `data`.
pub fn take<'a>(data: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    if data.len() < n {
        return None;
    }
    let (head, tail) = data.split_at(n);
    *data = tail;
    Some(head)
}

fn ternary(enabled: bool) -> Ternary {
    if enabled {
        Ternary::Enable
    } else {
        Ternary::Disable
    }
}

/// Decodes the two-byte configuration header.
///
/// Byte 0: bits 0-2 index [`LEVELS`]; bits 3-5 pick the format version
/// (`0..=6` map to 21..=27, `7` to the default, 27); bit 6 turns the content
/// checksum off and bit 7 the compressed checksum. Byte 1: bits 0-1 pick
/// the chunking (`0` whole, then [`CHUNK_SIZES`]); the other bits are
/// ignored, so mutations there leave the frame unchanged.
pub fn decode_config(mut data: &[u8]) -> Option<Decoded<'_>> {
    let header = take(&mut data, 2)?;
    let level = LEVELS[usize::from(header[0] & 7)];
    let version = match (header[0] >> 3) & 7 {
        7 => 27,
        n => 21 + u32::from(n),
    };
    let content_checksum = header[0] & 0x40 == 0;
    let compressed_checksum = header[0] & 0x80 == 0;
    let chunking = match header[1] & 3 {
        0 => Chunking::Whole,
        n => Chunking::Fixed(ChunkSize::try_from(CHUNK_SIZES[usize::from(n) - 1]).ok()?),
    };
    let config = EncoderConfig::default()
        .with_level(Level::try_from(level).ok()?)
        .with_format_version(FormatVersion::try_from(version).ok()?)
        .with_content_checksum(content_checksum)
        .with_compressed_checksum(compressed_checksum)
        .with_chunking(chunking);
    let params = Params {
        level,
        format_version: version,
        content_checksum: ternary(content_checksum),
        compressed_checksum: ternary(compressed_checksum),
        chunk_size: match chunking {
            Chunking::Whole => 0,
            Chunking::Fixed(size) => usize::from(size),
        },
    };
    let rest = &data[..data.len().min(MAX_PAYLOAD)];
    Some(Decoded {
        config,
        params,
        rest,
    })
}

/// A xorshift generator for split points and output windows, seeded from
/// fuzz input so every schedule replays exactly.
#[derive(Debug, Clone)]
pub struct Schedule(u32);

impl Schedule {
    /// Seeds the generator; any seed works, zero included.
    pub fn new(seed: u32) -> Self {
        Self(seed | 1)
    }

    /// Seeds the generator from a whole input (FNV-1a), for targets whose
    /// every byte is payload.
    pub fn from_bytes(data: &[u8]) -> Self {
        let hash = data.iter().fold(0x811C_9DC5u32, |hash, &byte| {
            (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
        });
        Self::new(hash)
    }

    /// The next 32 random bits.
    pub fn bits(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    /// A length in `1..=max`, biased towards short ones.
    pub fn length(&mut self, max: usize) -> usize {
        let max = max.max(1);
        let bound = match self.bits() % 4 {
            0 => 16,
            1 => 512,
            2 => 1 << 14,
            _ => max,
        };
        1 + self.bits() as usize % bound.min(max)
    }
}
