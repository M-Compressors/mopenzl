//! The incremental decoder: frames fed and drained through caller slices.

use alloc::boxed::Box;
use core::mem::MaybeUninit;

use super::core::stream::{Op, StepStatus};
use super::{DecodeError, Decompressor};
use crate::shared::destination::Destination;

/// Whether more input may follow a call.
///
/// Use [`Self::Process`] while more input may arrive. Once the input is
/// known to end, use [`Self::Finish`] on every remaining call, each time
/// passing exactly the input not yet consumed.
///
/// The encoder's counterpart is [`Operation`](crate::Operation):
/// [`Self::Process`] corresponds to `Operation::Continue` and [`Self::Finish`]
/// to `Operation::End`. The decoder has no `Flush`: it emits everything it can
/// decode on every call.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum DecodeOperation {
    /// More input may follow; an empty slice does not end the input.
    #[default]
    Process,
    /// This is all the remaining input.
    Finish,
}

/// Why an incremental call stopped.
///
/// Handle the counts in [`DecodeProgress`] before acting on the status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum DecoderStatus {
    /// All offered input was consumed and more is needed.
    NeedsInput,
    /// Decoded output is waiting for output space.
    NeedsOutput,
    /// The operation is complete: every frame was decoded and validated,
    /// and all output delivered.
    Finished,
}

/// Bytes accepted and delivered by one successful call.
///
/// Advance the input by `consumed` and use `output[..produced]`. The counts
/// are per call, not cumulative.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct DecodeProgress {
    /// Length of the accepted input prefix.
    pub consumed: usize,
    /// Length of the written output prefix.
    pub produced: usize,
    /// Why the call stopped.
    pub status: DecoderStatus,
}

/// A failed incremental call, with that call's exact progress.
///
/// Output counted by `produced` was written before the failure and is not
/// rolled back; it is a partial, unvalidated result. The session stays
/// failed: later calls report [`DecodeError::InvalidState`].
///
/// # Examples
///
/// ```
/// use mopenzl::{DecodeError, DecodeOperation, Decompressor};
///
/// let mut decoder = Decompressor::default();
/// let mut session = decoder.start();
/// // The input ends before any frame.
/// let failure = session.process(&[], &mut [], DecodeOperation::Finish).unwrap_err();
/// assert_eq!((failure.consumed, failure.produced), (0, 0));
/// assert!(matches!(failure.into_error(), DecodeError::UnexpectedEndOfInput));
/// ```
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
#[error("{error}")]
pub struct DecodeFailure {
    /// The underlying error.
    #[source]
    pub error: DecodeError,
    /// Length of the input prefix the call accepted.
    pub consumed: usize,
    /// Length of the output prefix the call wrote.
    pub produced: usize,
}

impl DecodeFailure {
    /// The error, once the progress has been handled.
    #[must_use]
    pub fn into_error(self) -> DecodeError {
        self.error
    }
}

impl From<DecodeFailure> for DecodeError {
    fn from(failure: DecodeFailure) -> Self {
        failure.error
    }
}

/// Runs one call on `decoder`, keeping `failed` up to date.
fn process<D: Destination + ?Sized>(
    decoder: &mut Decompressor,
    failed: &mut bool,
    input: &[u8],
    output: &mut D,
    operation: DecodeOperation,
) -> Result<DecodeProgress, DecodeFailure> {
    if *failed {
        return Err(DecodeFailure {
            error: DecodeError::InvalidState,
            consumed: 0,
            produced: 0,
        });
    }
    let op = match operation {
        DecodeOperation::Process => Op::Process,
        DecodeOperation::Finish => Op::Finish,
    };
    let config = *decoder.config();
    match decoder
        .stream
        .process(&mut decoder.engine, &config, input, output, op)
    {
        Ok(step) => {
            let status = match step.status {
                StepStatus::NeedsInput => DecoderStatus::NeedsInput,
                StepStatus::NeedsOutput => DecoderStatus::NeedsOutput,
                StepStatus::Finished => {
                    decoder.apply_retention();
                    DecoderStatus::Finished
                }
            };
            Ok(DecodeProgress {
                consumed: step.consumed,
                produced: step.produced,
                status,
            })
        }
        Err(failure) => {
            *failed = true;
            decoder.apply_retention();
            Err(DecodeFailure {
                error: failure.error,
                consumed: failure.consumed,
                produced: failure.produced,
            })
        }
    }
}

macro_rules! session_methods {
    () => {
        /// Decodes as much of `input` into `output` as possible.
        ///
        /// # Errors
        ///
        /// A [`DecodeFailure`] with the call's progress: invalid, truncated
        /// (with [`DecodeOperation::Finish`]) or unsupported input, checksum
        /// and size mismatches, exceeded limits, and
        /// [`DecodeError::InvalidState`] after an earlier failure.
        ///
        /// # Examples
        ///
        /// ```
        /// use mopenzl::{Compressor, DecodeOperation, Decompressor, DecoderStatus};
        ///
        /// let frame = Compressor::default().compress(&b"chunked ".repeat(100))?;
        /// let mut decoder = Decompressor::default();
        /// let mut session = decoder.start();
        /// let (mut content, mut buffer) = (Vec::new(), [0u8; 64]);
        /// let mut input = frame.as_slice();
        /// loop {
        ///     let progress = session.process(input, &mut buffer, DecodeOperation::Finish)?;
        ///     input = &input[progress.consumed..];
        ///     content.extend_from_slice(&buffer[..progress.produced]);
        ///     if progress.status == DecoderStatus::Finished {
        ///         break;
        ///     }
        /// }
        /// assert_eq!(content, b"chunked ".repeat(100));
        /// # Ok::<(), Box<dyn std::error::Error>>(())
        /// ```
        pub fn process(
            &mut self,
            input: &[u8],
            output: &mut [u8],
            operation: DecodeOperation,
        ) -> Result<DecodeProgress, DecodeFailure> {
            process(
                &mut self.decoder,
                &mut self.failed,
                input,
                output,
                operation,
            )
        }

        /// [`Self::process`] into uninitialized memory, such as
        /// [`Vec::spare_capacity_mut`](alloc::vec::Vec::spare_capacity_mut),
        /// without zeroing it first.
        ///
        /// It returns the same counts, statuses and errors as
        /// [`Self::process`], and calls of the two may be mixed. On success
        /// or failure exactly `output[..produced]` is initialized; nothing
        /// after it is written.
        ///
        /// # Errors
        ///
        /// As [`Self::process`].
        ///
        /// # Examples
        ///
        /// ```
        /// use mopenzl::{Compressor, DecodeOperation, Decompressor, DecoderStatus};
        ///
        /// let frame = Compressor::default().compress(b"uninit")?;
        /// let mut decoder = Decompressor::default();
        /// let mut session = decoder.start();
        /// let mut out: Vec<u8> = Vec::with_capacity(16);
        /// let progress = session.process_uninit(&frame, out.spare_capacity_mut(), DecodeOperation::Finish)?;
        /// // SAFETY: `process_uninit` initialized the first `produced` bytes.
        /// unsafe { out.set_len(progress.produced) };
        /// assert_eq!((out.as_slice(), progress.status), (&b"uninit"[..], DecoderStatus::Finished));
        /// # Ok::<(), Box<dyn std::error::Error>>(())
        /// ```
        pub fn process_uninit(
            &mut self,
            input: &[u8],
            output: &mut [MaybeUninit<u8>],
            operation: DecodeOperation,
        ) -> Result<DecodeProgress, DecodeFailure> {
            process(
                &mut self.decoder,
                &mut self.failed,
                input,
                output,
                operation,
            )
        }

        /// Delivers decoded output without taking input.
        ///
        /// # Errors
        ///
        /// As [`Self::process`].
        pub fn flush(&mut self, output: &mut [u8]) -> Result<DecodeProgress, DecodeFailure> {
            self.process(&[], output, DecodeOperation::Process)
        }

        /// Declares the end of the input and delivers what remains.
        ///
        /// # Errors
        ///
        /// As [`Self::process`]; [`DecodeError::UnexpectedEndOfInput`] if the
        /// input ended inside a frame.
        pub fn finish(&mut self, output: &mut [u8]) -> Result<DecodeProgress, DecodeFailure> {
            self.process(&[], output, DecodeOperation::Finish)
        }

        /// Whether every frame was decoded, validated and delivered.
        #[must_use]
        pub fn is_finished(&self) -> bool {
            !self.failed && self.decoder.stream.is_finished()
        }

        /// Compressed bytes accepted so far.
        #[must_use]
        pub fn total_in(&self) -> u64 {
            self.decoder.stream.total_in()
        }

        /// Decoded bytes regenerated so far, delivered or not.
        #[must_use]
        pub fn total_out(&self) -> u64 {
            self.decoder.stream.total_out()
        }

        /// Frames completed so far.
        #[must_use]
        pub fn frames_decoded(&self) -> u64 {
            self.decoder.stream.frames()
        }
    };
}

/// An incremental decoding operation borrowing its [`Decompressor`].
///
/// Created by [`Decompressor::start`]. Input and output move through
/// caller-owned slices of any size; the session keeps its own buffers for a
/// unit split across calls and for decoded output not yet delivered.
/// Dropping the session abandons the operation.
#[derive(Debug)]
pub struct DecoderSession<'d> {
    decoder: &'d mut Decompressor,
    failed: bool,
}

impl<'d> DecoderSession<'d> {
    pub(crate) const fn new(decoder: &'d mut Decompressor) -> Self {
        Self {
            decoder,
            failed: false,
        }
    }

    session_methods!();
}

/// An incremental decoding operation that owns its [`Decompressor`].
///
/// # Examples
///
/// ```
/// use mopenzl::{Compressor, DecodeOperation, Decompressor, DecoderStatus};
///
/// let frame = Compressor::default().compress(b"owned")?;
/// let mut session = Decompressor::default().into_session();
/// let mut out = [0u8; 16];
/// let progress = session.process(&frame, &mut out, DecodeOperation::Finish)?;
/// assert_eq!((&out[..progress.produced], progress.status), (&b"owned"[..], DecoderStatus::Finished));
/// session.reinit();
/// assert!(!session.is_finished());
/// let _decoder = session.into_decompressor();
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct DecoderSessionOwned {
    decoder: Box<Decompressor>,
    failed: bool,
}

impl DecoderSessionOwned {
    pub(crate) const fn new(decoder: Box<Decompressor>) -> Self {
        Self {
            decoder,
            failed: false,
        }
    }

    session_methods!();

    /// Starts a new operation on the same decompressor.
    pub fn reinit(&mut self) {
        self.decoder.stream.reset();
        self.failed = false;
    }

    /// Returns the decompressor, abandoning an unfinished operation.
    #[must_use]
    pub fn into_decompressor(mut self) -> Decompressor {
        self.decoder.stream.reset();
        *self.decoder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec::Vec;

    #[test]
    fn failures_stick_and_counters_report_progress() {
        let frame = openzl_ffi::compress(b"counted", openzl_ffi::Params::default()).unwrap();
        let mut decoder = Decompressor::default();
        let mut session = decoder.start();
        let mut out = [0u8; 32];
        let progress = session
            .process(&frame, &mut out, DecodeOperation::Process)
            .unwrap();
        assert_eq!(progress.status, DecoderStatus::Finished);
        assert!(session.is_finished());
        assert_eq!(session.total_in(), frame.len() as u64);
        assert_eq!((session.total_out(), session.frames_decoded()), (7, 1));
        assert_eq!(
            session.finish(&mut out).unwrap().status,
            DecoderStatus::Finished
        );

        let mut session = decoder.start();
        let failure = session
            .process(b"junk", &mut out, DecodeOperation::Process)
            .unwrap_err();
        assert!(matches!(failure.error, DecodeError::InvalidData { .. }));
        assert_eq!(
            session.flush(&mut out).unwrap_err().into_error(),
            DecodeError::InvalidState
        );
        assert!(!session.is_finished());
        assert_eq!(DecodeError::from(failure.clone()), failure.error);
        assert!(!failure.to_string().is_empty());
    }

    #[test]
    fn owned_sessions_can_restart_after_failure() {
        let frame = openzl_ffi::compress(b"again", openzl_ffi::Params::default()).unwrap();
        let mut session = Decompressor::default().into_session();
        let mut out = [0u8; 8];
        assert!(
            session
                .process(b"xx", &mut out, DecodeOperation::Finish)
                .is_err()
        );
        session.reinit();
        assert_eq!(session.total_in(), 0);
        let mut content = Vec::new();
        let progress = session
            .process(&frame, &mut out, DecodeOperation::Finish)
            .unwrap();
        content.extend_from_slice(&out[..progress.produced]);
        assert_eq!(content, b"again");
        assert_eq!(
            session.finish(&mut out).unwrap().status,
            DecoderStatus::Finished
        );
        let mut decoder = session.into_decompressor();
        assert_eq!(decoder.decompress(&frame).unwrap(), b"again");
    }
}
