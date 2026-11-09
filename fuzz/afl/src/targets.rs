//! Target bodies, shared by the AFL binaries, the libFuzzer adapters and the
//! regression replay.

use mopenzl::{
    Compressor, DecodeError, DecodeLimits, DecodeOperation, DecoderConfig, DecoderStatus,
    Decompressor, EncoderStatus, InputSize, Operation,
};

use crate::{MAX_OUTPUT, Schedule, decode_config, take};

/// A decoder that refuses to produce more than [`MAX_OUTPUT`] bytes.
fn bounded_decoder() -> Decompressor {
    let limits = DecodeLimits::default().with_max_output_bytes(Some(MAX_OUTPUT as u64));
    Decompressor::new(DecoderConfig::default().with_limits(limits))
}

/// Whether a decoder error says the input is outside what this port decodes
/// or what the fuzzer allows, rather than that the input is wrong.
fn out_of_scope(error: &DecodeError) -> bool {
    matches!(
        error,
        DecodeError::Unsupported { .. }
            | DecodeError::OutputLimitExceeded { .. }
            | DecodeError::InputLimitExceeded { .. }
    )
}

/// The C reference's verdict on `data`, or `None` when its declared content
/// exceeds [`MAX_OUTPUT`] and the reference is not asked.
fn reference_decode(data: &[u8]) -> Option<Result<Vec<u8>, openzl_ffi::Error>> {
    match openzl_ffi::decompressed_size(data) {
        Ok(size) if size > MAX_OUTPUT => None,
        Ok(_) => Some(openzl_ffi::decompress(data)),
        Err(error) => Some(Err(error)),
    }
}

/// Decodes `src` through a session fed pieces of random size into output
/// windows of random size, checking the session's progress contract.
fn decode_in_pieces(src: &[u8], schedule: &mut Schedule) -> Result<Vec<u8>, DecodeError> {
    let mut decoder = bounded_decoder();
    let mut session = decoder.start();
    let room = schedule.length(1 << 16);
    let mut buffer = vec![0u8; room];
    let mut content = Vec::new();
    let mut pos = 0usize;
    let mut idle = 0u32;
    loop {
        let end = pos
            .saturating_add(schedule.length(src.len()))
            .min(src.len());
        let operation = if end == src.len() {
            DecodeOperation::Finish
        } else {
            DecodeOperation::Process
        };
        let offered = end - pos;
        let progress = session
            .process(&src[pos..end], &mut buffer, operation)
            .map_err(|failure| {
                assert!(
                    failure.consumed <= offered,
                    "{failure:?} over-reports input"
                );
                assert!(failure.produced <= room, "{failure:?} over-reports output");
                failure.into_error()
            })?;
        assert!(
            progress.consumed <= offered,
            "{progress:?} over-reports input"
        );
        assert!(
            progress.produced <= room,
            "{progress:?} over-reports output"
        );
        if progress.status == DecoderStatus::NeedsInput {
            assert_eq!(
                progress.consumed, offered,
                "NeedsInput with offered input left: {progress:?}"
            );
            assert_eq!(
                operation,
                DecodeOperation::Process,
                "NeedsInput after the input ended"
            );
        }
        content.extend_from_slice(&buffer[..progress.produced]);
        pos += progress.consumed;
        if progress.status == DecoderStatus::Finished {
            assert!(session.is_finished());
            // A single-frame session leaves trailing bytes unconsumed where
            // one-shot decoding rejects them.
            if pos != src.len() {
                return Err(DecodeError::TrailingData { offset: pos as u64 });
            }
            return Ok(content);
        }
        if progress.consumed == 0 && progress.produced == 0 {
            idle += 1;
            assert!(idle < 4, "the session stalls at input byte {pos}");
        } else {
            idle = 0;
        }
    }
}

/// Arbitrary bytes never panic either decoder path.
///
/// One-shot decoding, a session fed random pieces and `decompress_to_slice`
/// agree. Where mopenzl decodes a frame the reference decodes it to the same
/// content; where the reference decodes one, mopenzl does too, unless the
/// frame needs a codec or feature this port does not decode yet (or the
/// output cap). The reference is asked only when mopenzl's verdict is in
/// scope, which keeps the fuzzer on the code paths both sides implement.
pub fn decompress(data: &[u8]) {
    let mut decoder = bounded_decoder();
    let ours = decoder.decompress(data);

    let mut schedule = Schedule::from_bytes(data);
    let pieces = decode_in_pieces(data, &mut schedule);
    match (&ours, &pieces) {
        (Ok(whole), Ok(pieces)) => assert_eq!(whole, pieces, "session differs from one-shot"),
        (Err(_), Err(_)) => {}
        (whole, pieces) => panic!("one-shot {whole:?} but session {pieces:?}"),
    }

    if let Ok(content) = &ours {
        let mut exact = vec![0u8; content.len()];
        let written = decoder
            .decompress_to_slice(data, &mut exact)
            .expect("decodes into a slice of its exact size");
        assert_eq!(&exact[..written], &content[..]);
    }

    if ours.as_ref().is_err_and(out_of_scope) {
        return;
    }
    // A chunk claiming more content than the frame header declares: the
    // reference sizes its stream buffers from that claim before it notices,
    // which under AddressSanitizer or libFuzzer's malloc limit is a fatal
    // multi-gigabyte allocation in C, not a finding in mopenzl. The reference
    // rejects every such frame.
    if matches!(ours, Err(DecodeError::ContentSizeMismatch { .. })) {
        return;
    }
    let Some(theirs) = reference_decode(data) else {
        return;
    };
    match (&ours, &theirs) {
        (Ok(ours), Ok(theirs)) => assert_eq!(ours, theirs, "content differs from the reference"),
        (Err(_), Err(_)) => {}
        (Ok(ours), Err(error)) => panic!(
            "mopenzl decodes {} bytes the reference rejects ({error:?})",
            ours.len()
        ),
        (Err(error), Ok(theirs)) => panic!(
            "the reference decodes {} bytes mopenzl rejects ({error:?})",
            theirs.len()
        ),
    }
}

/// One-shot compression is byte-identical to `ZL_CCtx_compress` with the
/// same parameters, and the frame decodes back to the payload.
pub fn roundtrip(data: &[u8]) {
    let Some(decoded) = decode_config(data) else {
        return;
    };
    let mut encoder = Compressor::new(decoded.config).expect("valid configuration");
    let ours = encoder.compress(decoded.rest).expect("compression");
    let theirs = openzl_ffi::compress(decoded.rest, decoded.params).expect("reference compression");
    assert_eq!(ours, theirs, "{:?}", decoded.config);

    let mut decoder = Decompressor::default();
    assert_eq!(
        decoder.decompress(&ours).expect("decoding our frame"),
        decoded.rest
    );
    let mut schedule = Schedule::from_bytes(data);
    assert_eq!(
        decode_in_pieces(&ours, &mut schedule).expect("decoding our frame in pieces"),
        decoded.rest
    );
}

/// A session fed random pieces, flushes and output windows writes a frame
/// that decodes back to the payload, and, with no flush that can move a
/// chunk boundary, writes exactly the one-shot frame.
///
/// After the configuration header, four bytes: the output window (byte 0,
/// `1 + 97 * n` bytes), flags (byte 1: bit 0 declares the input size, bit 1
/// allows flushes, bit 2 drives an owned session) and the seed of the
/// schedule (bytes 1-3).
pub fn stream_compress(data: &[u8]) {
    let Some(decoded) = decode_config(data) else {
        return;
    };
    let mut rest = decoded.rest;
    let Some(control) = take(&mut rest, 4) else {
        return;
    };
    let input = rest;
    let room = 1 + usize::from(control[0]) * 97;
    let declared = control[1] & 1 != 0;
    let flushes = control[1] & 2 != 0;
    let owned = control[1] & 4 != 0;
    let mut schedule = Schedule::new(u32::from_le_bytes([control[1], control[2], control[3], 0]));

    let mut steps: Vec<(&[u8], Operation)> = Vec::new();
    let mut pos = 0;
    let mut flushed = false;
    while pos < input.len() {
        let len = schedule.length(input.len() - pos);
        let operation = if flushes && schedule.bits().is_multiple_of(5) {
            flushed = true;
            Operation::Flush
        } else {
            Operation::Continue
        };
        steps.push((&input[pos..pos + len], operation));
        pos += len;
    }
    if flushes && schedule.bits().is_multiple_of(2) {
        // A flush on an empty input, without taking any.
        steps.push((&[], Operation::Flush));
        flushed = true;
    }
    steps.push((&[], Operation::End));

    let size = if declared {
        InputSize::Exact(input.len() as u64)
    } else {
        InputSize::Unknown
    };
    let mut encoder = Compressor::new(decoded.config).expect("valid configuration");
    let expected = encoder.compress(input).expect("one-shot compression");
    let mut frame = Vec::new();
    let mut buffer = vec![0u8; room];
    if owned {
        let mut session = encoder.into_session(size.into());
        drive(
            &steps,
            &mut buffer,
            &mut frame,
            |chunk, output, operation| session.process(chunk, output, operation),
        );
        assert!(session.is_finished());
    } else {
        let mut session = encoder.start(size.into());
        drive(
            &steps,
            &mut buffer,
            &mut frame,
            |chunk, output, operation| session.process(chunk, output, operation),
        );
        assert!(session.is_finished());
    }

    // With an unknown size nothing is written before the end, so a flush
    // cannot cut a chunk.
    if !flushed || !declared {
        assert_eq!(frame, expected, "session differs from one-shot");
    }
    let mut decoder = Decompressor::default();
    assert_eq!(
        decoder
            .decompress(&frame)
            .expect("decoding the session's frame"),
        input
    );
}

/// Runs every step of an encoder session to completion, appending what it
/// writes to `frame`.
fn drive(
    steps: &[(&[u8], Operation)],
    buffer: &mut [u8],
    frame: &mut Vec<u8>,
    mut process: impl FnMut(
        &[u8],
        &mut [u8],
        Operation,
    ) -> Result<mopenzl::Progress, mopenzl::EncodeError>,
) {
    for &(mut chunk, operation) in steps {
        loop {
            let offered = chunk.len();
            let progress = process(chunk, buffer, operation).expect("session");
            assert!(
                progress.consumed <= offered,
                "{progress:?} over-reports input"
            );
            assert!(
                progress.produced <= buffer.len(),
                "{progress:?} over-reports output"
            );
            frame.extend_from_slice(&buffer[..progress.produced]);
            chunk = &chunk[progress.consumed..];
            let done = match operation {
                Operation::Continue => {
                    chunk.is_empty() && progress.status != EncoderStatus::NeedsOutput
                }
                Operation::Flush => progress.status != EncoderStatus::NeedsOutput,
                Operation::End => progress.status == EncoderStatus::Finished,
            };
            if done {
                break;
            }
            assert!(
                progress.consumed > 0 || progress.produced > 0,
                "the session stalls: {progress:?} on {operation:?}"
            );
        }
    }
}
