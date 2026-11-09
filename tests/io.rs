//! The `std::io` adapters against the one-shot codecs.
#![cfg(all(
    feature = "compression",
    feature = "decompression",
    not(feature = "no_std")
))]

mod support;

use std::io::{Read, Write};

use mopenzl::{
    ChunkSize, Chunking, Compressor, DecodeError, Decompressor, EncoderConfig, FrameMode, InputSize,
};

fn segmented() -> EncoderConfig {
    EncoderConfig::default().with_chunking(Chunking::Fixed(ChunkSize::MIN))
}

#[test]
fn io_adapters_round_trip_through_both_directions() {
    let input = support::text(250_000, 12);
    let mut encoder = Compressor::new(segmented()).unwrap();
    let expected = encoder.compress(&input).unwrap();

    let mut writer = encoder.writer(Vec::new(), InputSize::Exact(input.len() as u64).into());
    for piece in input.chunks(3_333) {
        writer.write_all(piece).unwrap();
    }
    let written = writer
        .finish()
        .map_err(mopenzl::io::FinishError::into_error)
        .unwrap();
    assert_eq!(written, expected);

    let mut pulled = Vec::new();
    encoder
        .reader(&input[..], Default::default())
        .read_to_end(&mut pulled)
        .unwrap();
    assert_eq!(pulled, expected);

    let mut decoder = Decompressor::default();
    let mut content = Vec::new();
    decoder
        .reader(&expected[..])
        .read_to_end(&mut content)
        .unwrap();
    assert_eq!(content, input);

    let mut writer = decoder.writer(Vec::new());
    for piece in expected.chunks(1_000) {
        writer.write_all(piece).unwrap();
    }
    assert_eq!(
        writer
            .finish()
            .map_err(mopenzl::io::FinishError::into_error)
            .unwrap(),
        input
    );
}

#[test]
fn readers_leave_bytes_after_a_single_frame() {
    let frame = Compressor::default().compress(b"first").unwrap();
    let src = [&frame[..], b"rest of the stream"].concat();
    let mut decoder = Decompressor::new(FrameMode::Single.into());
    let mut reader = decoder.reader(&src[..]);
    let mut content = Vec::new();
    reader.read_to_end(&mut content).unwrap();
    assert_eq!(content, b"first");
    let parts = reader.into_parts();
    assert!(parts.finished);
    let mut rest = parts.unread_input;
    let mut source = parts.source;
    source.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, b"rest of the stream");
}

#[test]
fn writer_errors_surface_decode_errors() {
    let frame = Compressor::default().compress(&[1u8; 1000]).unwrap();
    let mut decoder = Decompressor::default();
    let mut writer = decoder.writer(Vec::new());
    writer.write_all(&frame[..frame.len() - 2]).unwrap();
    let error = writer.try_finish().unwrap_err();
    let decode = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<DecodeError>());
    assert_eq!(decode, Some(&DecodeError::UnexpectedEndOfInput));
}

/// A sink that refuses every other write with `WouldBlock`.
#[derive(Debug, Default)]
struct Flaky {
    written: Vec<u8>,
    refuse: bool,
    flushes: usize,
}

impl Write for Flaky {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.refuse = !self.refuse;
        if self.refuse {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn encoder_writer_finish_failures_return_the_writer_for_a_retry() {
    let input = support::text(70_000, 13);
    let mut encoder = Compressor::new(segmented()).unwrap();
    let expected = encoder.compress(&input).unwrap();
    let mut writer = encoder.writer(
        Flaky::default(),
        InputSize::Exact(input.len() as u64).into(),
    );
    assert!(format!("{writer:?}").contains("EncoderWriter"));
    let mut rest = &input[..];
    while !rest.is_empty() {
        match writer.write(rest) {
            Ok(n) => rest = &rest[n..],
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock),
        }
    }
    let _ = writer.flush();
    assert!(writer.get_ref().flushes <= 1);
    let mut attempt = writer.finish();
    let sink = loop {
        match attempt {
            Ok(sink) => break sink,
            Err(failure) => {
                assert_eq!(failure.error().kind(), std::io::ErrorKind::WouldBlock);
                assert!(!failure.to_string().is_empty());
                assert!(format!("{failure:?}").contains("FinishError"));
                assert!(std::error::Error::source(&failure).is_some());
                attempt = failure.into_inner().finish();
            }
        }
    };
    assert_eq!(sink.written, expected);
}

#[test]
fn finish_errors_split_into_their_parts() {
    let mut encoder = Compressor::default();
    let mut writer = encoder.writer(Flaky::default(), Default::default());
    writer.write_all(b"x").unwrap();
    let failure = writer.finish().unwrap_err();
    let (error, writer) = failure.into_parts();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert!(writer.finish().is_ok());
    let mut encoder = Compressor::default();
    let mut writer = encoder.writer(Flaky::default(), Default::default());
    writer.write_all(b"y").unwrap();
    let io: std::io::Error = writer.finish().unwrap_err().into();
    assert_eq!(io.kind(), std::io::ErrorKind::WouldBlock);
    let mut writer = encoder.writer(Flaky::default(), Default::default());
    writer.write_all(b"z").unwrap();
    let error = writer
        .finish()
        .map_err(mopenzl::io::FinishError::into_error);
    assert_eq!(error.unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
}

#[test]
fn encoder_readers_return_their_read_ahead() {
    let mut encoder = Compressor::default();
    let reader = encoder.reader(&b"read ahead"[..], Default::default());
    assert!(format!("{reader:?}").contains("EncoderReader"));
    assert_eq!(reader.get_ref().len(), 10);
    let parts = reader.into_parts();
    assert!(parts.unread_input.is_empty());
    assert_eq!(parts.source, b"read ahead");
}

#[test]
fn decoder_adapters_expose_their_ends_and_flush() {
    let frame = Compressor::new(segmented())
        .unwrap()
        .compress(&support::text(80_000, 14))
        .unwrap();
    let mut decoder = Decompressor::default();
    let mut reader = decoder.reader(&frame[..]);
    assert_eq!(reader.get_ref().len(), frame.len());
    let _ = reader.get_mut();
    let mut first = [0u8; 10];
    reader.read_exact(&mut first).unwrap();

    let mut writer = decoder.writer(Vec::new());
    writer.write_all(&frame[..frame.len() / 2]).unwrap();
    writer.flush().unwrap();
    assert!(!writer.get_ref().is_empty());
    let _ = writer.get_mut();
    assert!(!writer.is_finished());
    writer.write_all(&frame[frame.len() / 2..]).unwrap();
    writer.try_finish().unwrap();
    assert!(writer.is_finished());
    writer.flush().unwrap();
}
