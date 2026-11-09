//! The incremental decoder behind sessions.
//!
//! A frame is a sequence of units: the frame header, each chunk, and the
//! end-of-frame marker. A unit is decoded once all of its bytes are present.
//! When a unit lies whole in the caller's input it is decoded in place;
//! otherwise its bytes are gathered in a buffer across calls. A unit's
//! length is known from its header alone, so the stream never accepts a byte
//! past the unit it is assembling: in [`FrameMode::Single`] the bytes after
//! the frame stay with the caller.
//!
//! Headers that straddle calls are measured over the buffered prefix and a
//! window of the new input that doubles until the header parses, so
//! measuring costs time linear in the header's length.

use alloc::vec::Vec;

use super::frame::{self, FrameInfo};
use super::out::VecOut;
use super::reader::Parse;
use super::{Engine, Unit};
use crate::decompressor::config::{DecoderConfig, FrameMode};
use crate::decompressor::error::DecodeError;
use crate::shared::destination::{self, Destination};

/// Smallest window of new input a straddling header is measured with.
const PEEK_MIN: usize = 64;

/// Whether more input may follow a call.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Op {
    /// More input may follow.
    Process,
    /// This is all the remaining input.
    Finish,
}

/// Why a call stopped.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum StepStatus {
    NeedsInput,
    NeedsOutput,
    Finished,
}

/// What one call did.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Step {
    pub(crate) consumed: usize,
    pub(crate) produced: usize,
    pub(crate) status: StepStatus,
}

/// A failed call, with its progress.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StepFailure {
    pub(crate) consumed: usize,
    pub(crate) produced: usize,
    pub(crate) error: DecodeError,
}

/// Bytes a call has accepted and delivered so far.
#[derive(Copy, Clone, Debug, Default)]
struct Moved {
    consumed: usize,
    produced: usize,
}

/// Where in the frame the stream is.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
enum Phase {
    /// Expecting a frame header.
    #[default]
    Frame,
    /// Expecting a chunk or the end-of-frame marker of `FrameInfo`.
    Chunk,
    /// The operation is complete.
    Done,
}

/// The state of an operation in progress, with its reusable buffers.
#[derive(Debug, Default)]
pub(crate) struct Stream {
    phase: Phase,
    frame: Option<FrameInfo>,
    /// The header of the frame whose header unit is being assembled.
    pending_frame: Option<FrameInfo>,
    /// Bytes the current frame has regenerated, and its chunks so far.
    decoded: u64,
    chunks: u64,
    /// The current unit's bytes gathered so far, and its length once known.
    unit: Vec<u8>,
    need: Option<usize>,
    peek: Vec<u8>,
    /// Decoded bytes not yet delivered, from `delivered` on.
    output: Vec<u8>,
    delivered: usize,
    total_in: u64,
    total_out: u64,
    frames: u64,
}

impl Stream {
    /// Forgets the operation in progress, keeping the buffers.
    pub(crate) fn reset(&mut self) {
        self.phase = Phase::Frame;
        self.frame = None;
        self.pending_frame = None;
        self.decoded = 0;
        self.chunks = 0;
        self.unit.clear();
        self.need = None;
        self.peek.clear();
        self.output.clear();
        self.delivered = 0;
        self.total_in = 0;
        self.total_out = 0;
        self.frames = 0;
    }

    pub(crate) const fn is_finished(&self) -> bool {
        matches!(self.phase, Phase::Done)
    }

    pub(crate) const fn total_in(&self) -> u64 {
        self.total_in
    }

    pub(crate) const fn total_out(&self) -> u64 {
        self.total_out
    }

    pub(crate) const fn frames(&self) -> u64 {
        self.frames
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.unit.capacity() + self.peek.capacity() + self.output.capacity()
    }

    pub(crate) fn release(&mut self) {
        self.unit = Vec::new();
        self.peek = Vec::new();
        self.output = Vec::new();
    }

    fn drain<D: Destination + ?Sized>(&mut self, output: &mut D, at: usize) -> usize {
        let n = destination::fill(output, at, &self.output[self.delivered..]);
        self.delivered += n;
        if self.delivered == self.output.len() {
            self.output.clear();
            self.delivered = 0;
        }
        n
    }

    /// The length of the unit at the front of `bytes`, and the frame header
    /// when the unit is one.
    fn unit_len(
        &self,
        engine: &mut Engine,
        config: &DecoderConfig,
        bytes: &[u8],
    ) -> Result<(usize, Option<FrameInfo>), Parse> {
        match (self.phase, &self.frame) {
            (Phase::Chunk, Some(info)) => match engine.measure(bytes, info)? {
                Unit::End => Ok((1, None)),
                Unit::Chunk(len) => Ok((len, None)),
            },
            _ => {
                let info = frame::parse(bytes, engine.simd(), config.compressed_checksum_check())?;
                Ok((info.header_len, Some(info)))
            }
        }
    }

    /// The current unit's length, if `rest` completes its header.
    fn measure(
        &mut self,
        engine: &mut Engine,
        config: &DecoderConfig,
        rest: &[u8],
    ) -> Result<Option<usize>, DecodeError> {
        if self.unit.is_empty() {
            let parsed = self.unit_len(engine, config, rest);
            return self.settle(parsed);
        }
        let mut window = rest.len().min(PEEK_MIN.max(self.unit.len()));
        let mut peek = core::mem::take(&mut self.peek);
        let parsed = loop {
            peek.clear();
            peek.extend_from_slice(&self.unit);
            peek.extend_from_slice(&rest[..window]);
            match self.unit_len(engine, config, &peek) {
                Err(Parse::Truncated) if window < rest.len() => {
                    window = rest.len().min(window * 2);
                }
                parsed => break parsed,
            }
        };
        self.peek = peek;
        self.settle(parsed)
    }

    /// Records a measurement: the unit's length, if its header is complete.
    fn settle(
        &mut self,
        parsed: Result<(usize, Option<FrameInfo>), Parse>,
    ) -> Result<Option<usize>, DecodeError> {
        match parsed {
            Ok((len, frame)) => {
                self.pending_frame = frame;
                Ok(Some(len))
            }
            Err(Parse::Truncated) => Ok(None),
            Err(Parse::Failed(error)) => Err(error),
        }
    }

    /// Acts on one complete unit.
    fn finish_unit(
        &mut self,
        engine: &mut Engine,
        config: &DecoderConfig,
        bytes: &[u8],
    ) -> Result<(), DecodeError> {
        match (self.phase, self.frame) {
            (Phase::Chunk, Some(info)) => {
                if bytes.len() == 1 && bytes[0] == crate::shared::wire::END_OF_FRAME {
                    if self.chunks == 0 {
                        return Err(DecodeError::invalid(
                            crate::decompressor::error::InvalidDataKind::EmptyFrame,
                        ));
                    }
                    if self.decoded != info.size {
                        return Err(DecodeError::ContentSizeMismatch {
                            declared: info.size,
                            decoded: self.decoded,
                        });
                    }
                    self.frames += 1;
                    self.frame = None;
                    self.phase = match config.frame_mode() {
                        FrameMode::Single => Phase::Done,
                        FrameMode::Concatenated => Phase::Frame,
                    };
                    return Ok(());
                }
                let produced = engine.decode_chunk(
                    bytes,
                    &info,
                    config,
                    info.size - self.decoded,
                    &mut VecOut::new(&mut self.output),
                )?;
                self.decoded += produced;
                self.chunks += 1;
                self.total_out += produced;
                Ok(())
            }
            _ => {
                // Measuring the unit parsed its header.
                let Some(info) = self.pending_frame.take() else {
                    return Err(DecodeError::InvalidState);
                };
                if let Some(limit) = config.limits().max_output_bytes()
                    && self.total_out.saturating_add(info.size) > limit
                {
                    return Err(DecodeError::OutputLimitExceeded { limit });
                }
                engine.open_frame(&info)?;
                self.frame = Some(info);
                self.decoded = 0;
                self.chunks = 0;
                self.phase = Phase::Chunk;
                Ok(())
            }
        }
    }

    /// Moves the operation forward as far as `input`, `output` and `op`
    /// allow.
    pub(crate) fn process<D: Destination + ?Sized>(
        &mut self,
        engine: &mut Engine,
        config: &DecoderConfig,
        input: &[u8],
        output: &mut D,
        op: Op,
    ) -> Result<Step, StepFailure> {
        let mut moved = Moved::default();
        let result = self.run(engine, config, input, output, op, &mut moved);
        let Moved { consumed, produced } = moved;
        self.total_in += consumed as u64;
        match result {
            Ok(status) => Ok(Step {
                consumed,
                produced,
                status,
            }),
            Err(error) => Err(StepFailure {
                consumed,
                produced,
                error,
            }),
        }
    }

    fn run<D: Destination + ?Sized>(
        &mut self,
        engine: &mut Engine,
        config: &DecoderConfig,
        input: &[u8],
        output: &mut D,
        op: Op,
        moved: &mut Moved,
    ) -> Result<StepStatus, DecodeError> {
        let Moved { consumed, produced } = moved;
        loop {
            *produced += self.drain(output, *produced);
            if !self.output.is_empty() {
                return Ok(StepStatus::NeedsOutput);
            }
            if self.phase == Phase::Done {
                return Ok(StepStatus::Finished);
            }
            if let Some(limit) = config.limits().max_input_bytes()
                && self.total_in + *consumed as u64 > limit
            {
                return Err(DecodeError::InputLimitExceeded { limit });
            }
            let rest = &input[*consumed..];
            let between_frames = self.phase == Phase::Frame && self.unit.is_empty();
            if between_frames && self.frames > 0 && rest.is_empty() {
                if op == Op::Finish {
                    self.phase = Phase::Done;
                    continue;
                }
                return Ok(StepStatus::NeedsInput);
            }
            let len = match self.need {
                Some(len) => len,
                None => match self.measure(engine, config, rest)? {
                    Some(len) => {
                        self.need = Some(len);
                        len
                    }
                    None => {
                        self.unit.extend_from_slice(rest);
                        *consumed += rest.len();
                        return match op {
                            Op::Finish => Err(DecodeError::UnexpectedEndOfInput),
                            Op::Process => Ok(StepStatus::NeedsInput),
                        };
                    }
                },
            };
            if self.unit.is_empty() && rest.len() >= len {
                self.finish_unit(engine, config, &rest[..len])?;
                *consumed += len;
            } else {
                let take = (len - self.unit.len()).min(rest.len());
                self.unit.extend_from_slice(&rest[..take]);
                *consumed += take;
                if self.unit.len() < len {
                    return match op {
                        Op::Finish => Err(DecodeError::UnexpectedEndOfInput),
                        Op::Process => Ok(StepStatus::NeedsInput),
                    };
                }
                let unit = core::mem::take(&mut self.unit);
                let outcome = self.finish_unit(engine, config, &unit);
                self.unit = unit;
                self.unit.clear();
                outcome?;
            }
            self.need = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Backend;

    fn reference(input: &[u8], chunk_size: usize) -> Vec<u8> {
        openzl_ffi::compress(
            input,
            openzl_ffi::Params {
                chunk_size,
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn drive(
        config: &DecoderConfig,
        src: &[u8],
        piece: usize,
        room: usize,
    ) -> Result<(Vec<u8>, usize), DecodeError> {
        let mut engine = Engine::new(Backend::default());
        let mut stream = Stream::default();
        let mut out = Vec::new();
        let mut buffer = alloc::vec![0u8; room];
        let mut pos = 0;
        loop {
            let end = (pos + piece).min(src.len());
            let op = if end == src.len() {
                Op::Finish
            } else {
                Op::Process
            };
            let step = stream
                .process(&mut engine, config, &src[pos..end], &mut buffer[..], op)
                .map_err(|failure| failure.error)?;
            out.extend_from_slice(&buffer[..step.produced]);
            pos += step.consumed;
            match step.status {
                StepStatus::Finished => return Ok((out, pos)),
                StepStatus::NeedsInput if op == Op::Finish => {
                    return Err(DecodeError::UnexpectedEndOfInput);
                }
                _ => {}
            }
        }
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn any_split_decodes_and_stops_at_the_frame_end() {
        let input: Vec<u8> = (0..90_000u32)
            .map(|i| (i % 241) as u8 ^ (i >> 9) as u8)
            .collect();
        let frame = reference(&input, 32 << 10);
        let src = [&frame[..], b"trailer"].concat();
        let cases: &[(usize, usize)] = if cfg!(miri) {
            &[(5000, 700)]
        } else {
            &[
                (1, 1),
                (3, 4096),
                (64, 7),
                (70_000, 1 << 20),
                (src.len(), 100),
            ]
        };
        for &(piece, room) in cases {
            let (out, consumed) = drive(&DecoderConfig::default(), &src, piece, room).unwrap();
            assert_eq!(out, input, "piece {piece}, room {room}");
            assert_eq!(consumed, frame.len());
        }
    }

    #[test]
    fn concatenated_frames_decode_until_the_input_ends() {
        let first = reference(b"first frame payload", 0);
        let second = reference(b"", 0);
        let src = [&first[..], &second, &first].concat();
        let config = DecoderConfig::from(FrameMode::Concatenated);
        let (out, consumed) = drive(&config, &src, 5, 3).unwrap();
        assert_eq!(out, b"first frame payloadfirst frame payload");
        assert_eq!(consumed, src.len());
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn truncated_and_empty_inputs_fail_at_finish() {
        let frame = reference(&[5u8; 2000], 0);
        for cut in [0, 3, 12, frame.len() - 1] {
            assert_eq!(
                drive(&DecoderConfig::default(), &frame[..cut], 4, 64),
                Err(DecodeError::UnexpectedEndOfInput),
                "{cut}"
            );
        }
    }

    #[test]
    fn errors_report_their_progress_and_bookkeeping_resets() {
        let mut engine = Engine::new(Backend::default());
        let mut stream = Stream::default();
        let mut out = [0u8; 16];
        let failure = stream
            .process(
                &mut engine,
                &DecoderConfig::default(),
                b"not a frame",
                &mut out[..],
                Op::Process,
            )
            .unwrap_err();
        assert_eq!((failure.consumed, failure.produced), (0, 0));
        let frame = reference(b"payload!", 0);
        stream.reset();
        let step = stream
            .process(
                &mut engine,
                &DecoderConfig::default(),
                &frame,
                &mut out[..],
                Op::Finish,
            )
            .unwrap();
        assert_eq!(step.status, StepStatus::Finished);
        assert!(stream.is_finished());
        assert_eq!(
            (stream.total_in(), stream.total_out(), stream.frames()),
            (frame.len() as u64, 8, 1)
        );
        assert!(stream.retained_bytes() > 0 || stream.output.is_empty());
        stream.release();
        assert_eq!(stream.retained_bytes(), 0);
    }

    #[test]
    fn limits_apply_to_streams() {
        let frame = reference(&[5u8; 2000], 0);
        let limits = crate::decompressor::config::DecodeLimits::DEFAULT;
        let config = DecoderConfig::from(limits.with_max_output_bytes(Some(100)));
        assert_eq!(
            drive(&config, &frame, 3, 64),
            Err(DecodeError::OutputLimitExceeded { limit: 100 })
        );
        let config = DecoderConfig::from(limits.with_max_input_bytes(Some(5)));
        assert_eq!(
            drive(&config, &frame, 3, 64),
            Err(DecodeError::InputLimitExceeded { limit: 5 })
        );
    }
}
