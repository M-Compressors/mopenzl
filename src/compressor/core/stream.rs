//! The incremental encoder behind sessions.
//!
//! A frame header states the frame's total input size, and the reference
//! decoder rejects frames that leave it unknown. A stream with a declared
//! size therefore writes its header at once and then holds at most one chunk
//! of input, compressing each chunk as soon as it is complete. A stream of
//! unknown size has to hold all of its input until it ends; it then writes
//! the frame exactly as a declared one would.
//!
//! Chunk boundaries fall every `chunk_limit` input bytes from the start of the
//! frame, which is where the reference's serial segmenter cuts, so a session
//! writes the bytes of the one-shot encoder however its input is split. Only
//! a flush moves the boundaries: it ends the current chunk early.

use alloc::vec::Vec;

use super::sink::VecSink;
use super::{CoreError, Engine, Params};
use crate::shared::destination::{self, Destination};

/// What a call asks of the stream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Op {
    /// Accept input.
    Continue,
    /// End the current chunk.
    Flush,
    /// End the frame.
    End,
}

/// Why a call stopped.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum StepStatus {
    /// Everything offered was handled.
    NeedsInput,
    /// Compressed bytes wait for room.
    NeedsOutput,
    /// The frame is complete and delivered.
    Finished,
}

/// What one call did.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Step {
    pub(crate) consumed: usize,
    pub(crate) produced: usize,
    pub(crate) status: StepStatus,
}

/// Why a stream failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StreamError {
    /// The input contradicts the declared size.
    SizeMismatch,
    /// The encoder failed.
    Core(CoreError),
}

impl From<CoreError> for StreamError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}

/// The state of the frame in progress, with its reusable buffers.
#[derive(Debug, Default)]
pub(crate) struct Stream {
    /// Accepted input not yet compressed, from `input_pos` on.
    input: Vec<u8>,
    input_pos: usize,
    /// Compressed bytes not yet delivered, from `delivered` on.
    output: Vec<u8>,
    delivered: usize,
    /// The frame's size, once the header is written.
    total: Option<u64>,
    /// Input bytes accepted.
    accepted: u64,
    /// Input bytes compressed into chunks.
    chunked: u64,
    /// Chunks written.
    chunks: u64,
    /// The end-of-frame marker is written.
    ended: bool,
}

impl Stream {
    /// Forgets the frame in progress, keeping the buffers.
    pub(crate) fn reset(&mut self) {
        self.input.clear();
        self.input_pos = 0;
        self.output.clear();
        self.delivered = 0;
        self.total = None;
        self.accepted = 0;
        self.chunked = 0;
        self.chunks = 0;
        self.ended = false;
    }

    /// Bytes the stream keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.input.capacity() + self.output.capacity()
    }

    /// Releases both buffers.
    pub(crate) fn release(&mut self) {
        self.input = Vec::new();
        self.output = Vec::new();
    }

    /// Copies pending output into `output` after `at`, returning how much.
    fn drain<D: Destination + ?Sized>(&mut self, output: &mut D, at: usize) -> usize {
        let n = destination::fill(output, at, &self.output[self.delivered..]);
        self.delivered += n;
        if self.delivered == self.output.len() {
            self.output.clear();
            self.delivered = 0;
        }
        n
    }

    /// Compresses one chunk of buffered input of `len` bytes.
    fn chunk_buffered(
        &mut self,
        engine: &mut Engine,
        params: &Params,
        len: usize,
    ) -> Result<(), CoreError> {
        let end = self.input_pos + len;
        engine.chunk(
            params,
            &self.input[self.input_pos..end],
            &mut VecSink::new(&mut self.output),
        )?;
        self.input_pos = end;
        if self.input_pos == self.input.len() {
            self.input.clear();
            self.input_pos = 0;
        }
        self.chunked += len as u64;
        self.chunks += 1;
        Ok(())
    }

    /// Moves the frame forward as far as `input`, `output` and `op` allow.
    ///
    /// `declared` is the frame's size when known in advance.
    pub(crate) fn process<D: Destination + ?Sized>(
        &mut self,
        engine: &mut Engine,
        params: &Params,
        declared: Option<u64>,
        input: &[u8],
        output: &mut D,
        op: Op,
    ) -> Result<Step, StreamError> {
        // Graphs that are not a serial segmenter need their whole input.
        let declared = if engine.incremental() { declared } else { None };
        let mut consumed = 0;
        let mut produced = 0;
        loop {
            produced += self.drain(output, produced);
            if !self.output.is_empty() {
                return Ok(Step {
                    consumed,
                    produced,
                    status: StepStatus::NeedsOutput,
                });
            }
            if self.ended {
                return Ok(Step {
                    consumed,
                    produced,
                    status: StepStatus::Finished,
                });
            }
            let rest = &input[consumed..];
            if let Some(size) = declared
                && self.accepted + rest.len() as u64 > size
            {
                return Err(StreamError::SizeMismatch);
            }
            let Some(total) = self.total.or(declared) else {
                // Unknown size: hold everything until the frame ends.
                if op != Op::End {
                    self.input.extend_from_slice(rest);
                    consumed += rest.len();
                    self.accepted += rest.len() as u64;
                    return Ok(Step {
                        consumed,
                        produced,
                        status: StepStatus::NeedsInput,
                    });
                }
                if self.input.is_empty() {
                    // The whole frame is in the caller's memory.
                    engine.frame(params, rest, &mut VecSink::new(&mut self.output))?;
                    consumed += rest.len();
                    self.accepted += rest.len() as u64;
                    self.chunked = self.accepted;
                    self.total = Some(self.accepted);
                    self.ended = true;
                    continue;
                }
                self.input.extend_from_slice(rest);
                consumed += rest.len();
                self.accepted += rest.len() as u64;
                self.total = Some(self.accepted);
                if !engine.incremental() {
                    let all = core::mem::take(&mut self.input);
                    let result = engine.frame(params, &all, &mut VecSink::new(&mut self.output));
                    self.input = all;
                    self.input.clear();
                    result?;
                    self.chunked = self.accepted;
                    self.ended = true;
                    continue;
                }
                engine.header(params, self.accepted, &mut VecSink::new(&mut self.output))?;
                continue;
            };
            if self.total.is_none() {
                self.total = Some(total);
                engine.header(params, total, &mut VecSink::new(&mut self.output))?;
                continue;
            }
            let rest = &input[consumed..];
            let buffered = self.input.len() - self.input_pos;
            let next = (total - self.chunked).min(params.chunk_limit as u64) as usize;
            if next > 0 && buffered >= next {
                self.chunk_buffered(engine, params, next)?;
                continue;
            }
            if next > 0 && buffered == 0 && rest.len() >= next {
                // A whole chunk is in the caller's memory.
                engine.chunk(params, &rest[..next], &mut VecSink::new(&mut self.output))?;
                consumed += next;
                self.accepted += next as u64;
                self.chunked += next as u64;
                self.chunks += 1;
                continue;
            }
            if !rest.is_empty() {
                if self.input_pos > 0 {
                    self.input.drain(..self.input_pos);
                    self.input_pos = 0;
                }
                let take = (next - buffered).min(rest.len());
                self.input.extend_from_slice(&rest[..take]);
                consumed += take;
                self.accepted += take as u64;
                continue;
            }
            match op {
                Op::Continue => {
                    return Ok(Step {
                        consumed,
                        produced,
                        status: StepStatus::NeedsInput,
                    });
                }
                Op::Flush => {
                    if buffered == 0 {
                        return Ok(Step {
                            consumed,
                            produced,
                            status: StepStatus::NeedsInput,
                        });
                    }
                    self.chunk_buffered(engine, params, buffered)?;
                }
                Op::End => {
                    if self.accepted != total {
                        return Err(StreamError::SizeMismatch);
                    }
                    if buffered > 0 || self.chunks == 0 {
                        self.chunk_buffered(engine, params, buffered)?;
                    }
                    super::frame::write_end(&mut VecSink::new(&mut self.output))
                        .map_err(CoreError::from)?;
                    self.ended = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Backend;
    use crate::compressor::config::EncoderConfig;

    fn setup(limit: usize) -> (Engine, Params) {
        let mut params = Params::from(&EncoderConfig::default());
        params.chunk_limit = limit;
        (Engine::new(&params, Backend::default()), params)
    }

    fn one_shot(engine: &mut Engine, params: &Params, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        engine
            .frame(params, input, &mut VecSink::new(&mut out))
            .unwrap();
        out
    }

    /// Drives a stream with `piece`-byte inputs and `room`-byte outputs.
    fn drive(
        engine: &mut Engine,
        params: &Params,
        declared: Option<u64>,
        input: &[u8],
        piece: usize,
        room: usize,
    ) -> Vec<u8> {
        let mut stream = Stream::default();
        let mut out = Vec::new();
        let mut buffer = alloc::vec![0u8; room];
        let mut rest = input;
        loop {
            let take = rest.len().min(piece);
            let op = if take == rest.len() {
                Op::End
            } else {
                Op::Continue
            };
            let step = stream
                .process(engine, params, declared, &rest[..take], &mut buffer[..], op)
                .unwrap();
            out.extend_from_slice(&buffer[..step.produced]);
            rest = &rest[step.consumed..];
            if step.status == StepStatus::Finished {
                return out;
            }
        }
    }

    #[test]
    fn any_split_writes_the_one_shot_frame() {
        let (mut engine, params) = setup(32 << 10);
        let input: Vec<u8> = (0..100_000u32).map(|i| (i % 253) as u8).collect();
        let expected = one_shot(&mut engine, &params, &input);
        let cases: &[(usize, usize)] = if cfg!(miri) {
            &[(40_000, 50)]
        } else {
            &[
                (1, 7),
                (999, 64),
                (32 << 10, 1 << 20),
                (100_000, 3),
                (77_777, 4096),
            ]
        };
        for &(piece, room) in cases {
            for declared in [None, Some(input.len() as u64)] {
                let out = drive(&mut engine, &params, declared, &input, piece, room);
                assert_eq!(
                    out, expected,
                    "piece {piece}, room {room}, declared {declared:?}"
                );
            }
        }
    }

    #[test]
    fn empty_frames_hold_one_empty_chunk() {
        let (mut engine, params) = setup(32 << 10);
        let expected = one_shot(&mut engine, &params, b"");
        for declared in [None, Some(0)] {
            assert_eq!(drive(&mut engine, &params, declared, b"", 1, 1), expected);
        }
    }

    #[test]
    fn core_errors_convert_to_stream_errors() {
        assert_eq!(
            StreamError::from(CoreError::OutputFull),
            StreamError::Core(CoreError::OutputFull)
        );
    }

    #[test]
    fn declared_sizes_are_enforced() {
        let (mut engine, params) = setup(32 << 10);
        let mut out = [0u8; 64];
        let mut stream = Stream::default();
        assert_eq!(
            stream.process(
                &mut engine,
                &params,
                Some(2),
                b"abc",
                &mut out[..],
                Op::Continue
            ),
            Err(StreamError::SizeMismatch)
        );
        let mut stream = Stream::default();
        stream
            .process(
                &mut engine,
                &params,
                Some(5),
                b"ab",
                &mut out[..],
                Op::Continue,
            )
            .unwrap();
        assert_eq!(
            stream.process(&mut engine, &params, Some(5), b"", &mut out[..], Op::End),
            Err(StreamError::SizeMismatch)
        );
    }

    #[test]
    fn flush_cuts_a_chunk_only_with_a_known_size() {
        let (mut engine, params) = setup(32 << 10);
        let mut out = alloc::vec![0u8; 1024];
        let mut stream = Stream::default();
        let step = stream
            .process(
                &mut engine,
                &params,
                None,
                b"unknown size",
                &mut out[..],
                Op::Flush,
            )
            .unwrap();
        assert_eq!((step.consumed, step.produced), (12, 0));
        assert_eq!(stream.accepted, 12);

        let mut stream = Stream::default();
        assert_eq!(stream.accepted, 0);
        let first = stream
            .process(
                &mut engine,
                &params,
                Some(24),
                b"known size, ",
                &mut out[..],
                Op::Flush,
            )
            .unwrap();
        assert_eq!(first.status, StepStatus::NeedsInput);
        assert!(first.produced > 12);
        let again = stream
            .process(&mut engine, &params, Some(24), b"", &mut out[..], Op::Flush)
            .unwrap();
        assert_eq!(again.produced, 0);
        let last = stream
            .process(
                &mut engine,
                &params,
                Some(24),
                b"second part!",
                &mut out[..],
                Op::End,
            )
            .unwrap();
        assert_eq!(last.status, StepStatus::Finished);
        let finished = stream
            .process(&mut engine, &params, Some(24), b"", &mut out[..], Op::End)
            .unwrap();
        assert_eq!(finished.status, StepStatus::Finished);
        assert!(stream.retained_bytes() > 0);
        stream.release();
        stream.reset();
        assert_eq!(stream.accepted, 0);
    }
}
