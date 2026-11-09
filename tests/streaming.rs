//! Sessions and `std::io` adapters against the one-shot codecs.
#![cfg(all(feature = "compression", feature = "decompression"))]

mod support;

use mopenzl::{
    ChunkSize, Chunking, Compressor, DecodeError, DecodeOperation, DecoderStatus, Decompressor,
    EncodeError, EncoderConfig, EncoderStatus, InputSize, Operation,
};

fn segmented() -> EncoderConfig {
    EncoderConfig::default().with_chunking(Chunking::Fixed(ChunkSize::MIN))
}

/// Encodes `input` through a session fed `piece` bytes per call with `room`
/// bytes of output per call.
fn encode_session(
    config: EncoderConfig,
    size: InputSize,
    input: &[u8],
    piece: usize,
    room: usize,
) -> Vec<u8> {
    let mut encoder = Compressor::new(config).unwrap();
    let mut session = encoder.start(size.into());
    let mut frame = Vec::new();
    let mut buffer = vec![0u8; room];
    let mut rest = input;
    loop {
        let take = rest.len().min(piece);
        let operation = if take == rest.len() {
            Operation::End
        } else {
            Operation::Continue
        };
        let progress = session
            .process(&rest[..take], &mut buffer, operation)
            .unwrap();
        frame.extend_from_slice(&buffer[..progress.produced]);
        rest = &rest[progress.consumed..];
        if progress.status == EncoderStatus::Finished {
            return frame;
        }
    }
}

/// Decodes `src` through a session fed `piece` bytes per call.
fn decode_session(src: &[u8], piece: usize, room: usize) -> Result<Vec<u8>, DecodeError> {
    let mut decoder = Decompressor::default();
    let mut session = decoder.start();
    let mut content = Vec::new();
    let mut buffer = vec![0u8; room];
    let mut pos = 0usize;
    loop {
        let end = pos.saturating_add(piece).min(src.len());
        let operation = if end == src.len() {
            DecodeOperation::Finish
        } else {
            DecodeOperation::Process
        };
        let progress = session.process(&src[pos..end], &mut buffer, operation)?;
        content.extend_from_slice(&buffer[..progress.produced]);
        pos += progress.consumed;
        if progress.status == DecoderStatus::Finished {
            return Ok(content);
        }
    }
}

#[test]
fn encoder_sessions_write_the_one_shot_frame() {
    for config in [EncoderConfig::default(), segmented()] {
        let mut encoder = Compressor::new(config).unwrap();
        for (name, input) in support::corpus() {
            let expected = encoder.compress(&input).unwrap();
            for (piece, room) in [(7, 13), (4096, 1 << 16), (usize::MAX, 1)] {
                for size in [InputSize::Unknown, InputSize::Exact(input.len() as u64)] {
                    let frame = encode_session(config, size, &input, piece, room);
                    assert_eq!(
                        frame, expected,
                        "{name}, piece {piece}, room {room}, {size:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn decoder_sessions_match_one_shot_decoding() {
    let mut encoder = Compressor::new(segmented()).unwrap();
    for (name, input) in support::corpus() {
        let frame = encoder.compress(&input).unwrap();
        for (piece, room) in [(1, 5), (999, 70_000), (usize::MAX, 1)] {
            assert_eq!(
                decode_session(&frame, piece, room).unwrap(),
                input,
                "{name}"
            );
        }
    }
}

#[test]
fn flushed_frames_decode_in_the_reference_library() {
    let input = support::text(100_000, 11);
    let mut encoder = Compressor::new(segmented()).unwrap();
    let mut session = encoder.start(InputSize::Exact(input.len() as u64).into());
    let mut frame = Vec::new();
    let mut buffer = vec![0u8; 1 << 18];
    for (index, piece) in input.chunks(7_000).enumerate() {
        let mut rest = piece;
        while !rest.is_empty() {
            let progress = session
                .process(rest, &mut buffer, Operation::Continue)
                .unwrap();
            frame.extend_from_slice(&buffer[..progress.produced]);
            rest = &rest[progress.consumed..];
        }
        if index % 3 == 0 {
            let progress = session.flush(&mut buffer).unwrap();
            frame.extend_from_slice(&buffer[..progress.produced]);
            // Everything accepted so far can be decoded right away.
            let mut decoder = Decompressor::default();
            let mut partial = decoder.start();
            let mut out = vec![0u8; input.len()];
            let got = partial
                .process(&frame, &mut out, DecodeOperation::Process)
                .unwrap();
            assert_eq!(&out[..got.produced], &input[..(index + 1) * 7_000]);
        }
    }
    let progress = session.finish(&mut buffer).unwrap();
    frame.extend_from_slice(&buffer[..progress.produced]);
    assert_eq!(openzl_ffi::decompress(&frame).unwrap(), input);
    assert_eq!(Decompressor::default().decompress(&frame).unwrap(), input);
}

#[test]
fn declared_sizes_are_enforced_by_sessions() {
    let mut encoder = Compressor::default();
    let mut session = encoder.start(InputSize::Exact(10).into());
    let mut out = [0u8; 128];
    session
        .process(b"short", &mut out, Operation::Continue)
        .unwrap();
    assert_eq!(
        session.finish(&mut out),
        Err(EncodeError::InputSizeMismatch { declared: 10 })
    );
}
