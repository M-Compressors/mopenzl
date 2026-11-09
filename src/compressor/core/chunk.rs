//! Chunks: the decoding map, the stored streams and the chunk checksums
//! (`CCTX_flushChunk`, `writeChunkHeaderV8_internal`).
//!
//! A chunk of the default graph is one of two shapes. Inputs below the
//! reference's minimum stream size (10 bytes) are stored. Larger inputs run
//! the Zstandard node, whose output stream is stored, unless the chunk would
//! not be smaller than its input, in which case the reference's
//! store-on-expansion replaces it with the stored input.

use alloc::vec::Vec;

use super::sink::Sink;
use super::{CoreError, Engine, Params};
use crate::shared::wire::codec_id_bits;
#[cfg(feature = "zstd")]
use crate::shared::wire::{CODEC_ZSTD, MIN_STREAM_SIZE};
use crate::shared::{bitpack, varint, xxh3};

/// One codec of a chunk's decoding map.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct CodecRecord {
    /// A standard codec, rather than a custom one.
    pub(crate) standard: bool,
    /// The codec's ID.
    pub(crate) id: u32,
    /// Bytes of its private header in the header stream.
    pub(crate) header_size: u32,
    /// Variable outputs of the encoder: extra inputs of the decoder.
    pub(crate) variable_outputs: u32,
    /// Streams it regenerates.
    pub(crate) regenerated: u32,
}

/// The Zstandard codec: no private header, one stream in, one stream out.
#[cfg(feature = "zstd")]
const ZSTD: CodecRecord = CodecRecord {
    standard: true,
    id: CODEC_ZSTD,
    header_size: 0,
    variable_outputs: 0,
    regenerated: 1,
};

/// Appends a chunk header listing `codecs` in decoding order, the
/// regenerated-stream `distances` (already reduced by one) and the sizes of
/// the `stored` streams, the header stream excluded.
pub(crate) fn push_header(
    out: &mut Vec<u8>,
    version: u32,
    codecs: &[CodecRecord],
    distances: &[u32],
    stored: &[usize],
) {
    varint::push(out, codecs.len() as u64 + 1);
    varint::push(out, stored.len() as u64);
    bitpack::pack(
        out,
        codecs.iter().map(|codec| u32::from(!codec.standard)),
        1,
    );
    let standard = codecs.iter().filter(|codec| codec.standard);
    bitpack::pack(out, standard.map(|codec| codec.id), codec_id_bits(version));
    for codec in codecs.iter().filter(|codec| !codec.standard) {
        varint::push(out, u64::from(codec.id));
    }
    bitpack::pack(
        out,
        codecs.iter().map(|codec| u32::from(codec.header_size > 0)),
        1,
    );
    for codec in codecs.iter().filter(|codec| codec.header_size > 0) {
        varint::push(out, u64::from(codec.header_size - 1));
    }
    bitpack::pack(
        out,
        codecs
            .iter()
            .map(|codec| u32::from(codec.variable_outputs > 0)),
        1,
    );
    for codec in codecs.iter().filter(|codec| codec.variable_outputs > 0) {
        varint::push(out, u64::from(codec.variable_outputs - 1));
    }
    bitpack::pack(
        out,
        codecs.iter().map(|codec| u32::from(codec.regenerated > 1)),
        1,
    );
    for codec in codecs.iter().filter(|codec| codec.regenerated > 1) {
        varint::push(out, u64::from(codec.regenerated - 2));
    }
    let bits = bitpack::bits_for((distances.len() + stored.len()) as u64);
    bitpack::pack(out, distances.iter().copied(), bits);
    for &size in stored {
        varint::push(out, size as u64);
    }
}

/// Compresses `input` as one chunk into `sink`.
#[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
pub(crate) fn write_chunk(
    engine: &mut Engine,
    params: &Params,
    input: &[u8],
    sink: &mut impl Sink,
) -> Result<(), CoreError> {
    let start = sink.len();
    let Engine {
        #[cfg(feature = "zstd")]
        zstd,
        header,
        simd,
        ..
    } = engine;
    header.clear();
    #[cfg_attr(
        not(feature = "zstd"),
        expect(unused_mut, reason = "only Zstandard replaces the stored input")
    )]
    let mut stored = input;
    #[cfg(feature = "zstd")]
    if input.len() >= MIN_STREAM_SIZE {
        let stream = zstd.encode(input).ok_or(CoreError::Internal)?;
        push_header(header, params.version, &[ZSTD], &[0], &[stream.len()]);
        if header.len() + stream.len() < input.len() {
            stored = stream;
        } else {
            header.clear();
        }
    }
    if header.is_empty() {
        push_header(header, params.version, &[], &[], &[input.len()]);
    }
    sink.put(header)?;
    sink.put(stored)?;
    if params.content_checksum {
        sink.put(&xxh3::hash32(*simd, input).to_le_bytes())?;
    }
    if params.compressed_checksum {
        let hash = xxh3::hash32(*simd, sink.since(start));
        sink.put(&hash.to_le_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::sink::VecSink;
    use super::*;
    use crate::Backend;

    fn params(version: u32) -> Params {
        Params {
            version,
            level: 6,
            content_checksum: true,
            compressed_checksum: true,
            chunk_limit: 1 << 31,
        }
    }

    fn chunk(version: u32, input: &[u8]) -> Vec<u8> {
        let mut engine = Engine::new(&params(version), Backend::default());
        let mut out = Vec::new();
        write_chunk(
            &mut engine,
            &params(version),
            input,
            &mut VecSink::new(&mut out),
        )
        .unwrap();
        out
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn store_and_zstd_headers_match_the_reference_bytes() {
        let mut out = Vec::new();
        push_header(&mut out, 27, &[], &[], &[5]);
        assert_eq!(out, [0x01, 0x01, 0x05]);
        let mut out = Vec::new();
        push_header(&mut out, 27, &[ZSTD], &[0], &[21]);
        assert_eq!(out, [0x02, 0x01, 0x00, 0x16, 0x00, 0x00, 0x00, 0x00, 0x15]);
        let mut out = Vec::new();
        push_header(&mut out, 21, &[ZSTD], &[0], &[21]);
        assert_eq!(out, [0x02, 0x01, 0x00, 0x16, 0x00, 0x00, 0x00, 0x00, 0x15]);
    }

    #[test]
    fn custom_codecs_and_multi_stream_records_take_their_varints() {
        let record = CodecRecord {
            standard: false,
            id: 300,
            header_size: 3,
            variable_outputs: 2,
            regenerated: 4,
        };
        let mut out = Vec::new();
        push_header(&mut out, 27, &[record], &[1, 0, 2, 3], &[0, 1]);
        assert_eq!(
            out,
            [
                0x02, 0x02, 0x01, 0xac, 0x02, 0x01, 0x02, 0x01, 0x01, 0x01, 0x02, 0x81, 0x06, 0x00,
                0x01
            ]
        );
    }

    #[test]
    fn small_and_expanding_inputs_are_stored() {
        // `ZL_compress(b"hello")` and of ten incompressible bytes.
        let out = chunk(27, b"hello");
        assert_eq!(&out[..8], [0x01, 0x01, 0x05, b'h', b'e', b'l', b'l', b'o']);
        assert_eq!(out.len(), 8 + 8);
        let out = chunk(27, b"0123456789");
        assert_eq!(&out[..3], [0x01, 0x01, 0x0a]);
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn compressible_inputs_run_zstd() {
        let input: Vec<u8> = (0..300u32).map(|i| (i % 7) as u8).collect();
        let out = chunk(27, &input);
        assert_eq!(
            &out[..10],
            [0x02, 0x01, 0x00, 0x16, 0x00, 0x00, 0x00, 0x00, 0x15, 0x01]
        );
        assert!(out.len() < input.len());
    }
}
