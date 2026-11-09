//! The incremental encoder: one frame, driven a chunk at a time.
//!
//! A session writes the bytes of the one-shot encoder for the same input,
//! however the input is split across calls and whatever room each call
//! offers. Only [`Operation::Flush`] changes the bytes: it ends the current
//! chunk early.

use alloc::boxed::Box;
use core::mem::MaybeUninit;

use super::core::stream::{Op, StepStatus};
use super::encoder::Compressor;
use super::error::EncodeError;
use crate::shared::destination::Destination;

/// How much input a frame will carry, when that is known in advance.
///
/// The frame header records the frame's size, so a session of unknown size
/// holds all of its input until it ends; a declared size lets it hold only
/// the current chunk. The frame fails if the input differs from a declared
/// size.
///
/// # Examples
///
/// ```
/// use mopenzl::InputSize;
///
/// assert_eq!(InputSize::default(), InputSize::Unknown);
/// assert_eq!(InputSize::from(4096u64), InputSize::Exact(4096));
/// ```
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum InputSize {
    /// Not known.
    #[default]
    Unknown,
    /// Exactly this many bytes.
    Exact(u64),
}

impl From<u64> for InputSize {
    fn from(value: u64) -> Self {
        Self::Exact(value)
    }
}

/// What a single frame knows about itself.
///
/// # Examples
///
/// ```
/// use mopenzl::{InputSize, StreamConfig};
///
/// let stream = StreamConfig::from(InputSize::Exact(10));
/// assert_eq!(stream.input_size(), InputSize::Exact(10));
/// assert_eq!(StreamConfig::default().with_input_size(3.into()).input_size(), InputSize::Exact(3));
/// ```
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct StreamConfig {
    input_size: InputSize,
}

impl StreamConfig {
    /// Sets how much input the frame will carry.
    #[must_use]
    pub const fn with_input_size(mut self, input_size: InputSize) -> Self {
        self.input_size = input_size;
        self
    }

    /// How much input the frame will carry.
    #[must_use]
    pub const fn input_size(&self) -> InputSize {
        self.input_size
    }
}

impl From<InputSize> for StreamConfig {
    fn from(input_size: InputSize) -> Self {
        Self { input_size }
    }
}

/// What a call to [`EncoderSession::process`] should do.
///
/// The decoder's counterpart is [`DecodeOperation`](crate::DecodeOperation):
/// [`Self::Continue`] corresponds to `DecodeOperation::Process` (more input
/// may follow) and [`Self::End`] to `DecodeOperation::Finish` (this is all the
/// input). Only the encoder has [`Self::Flush`], and the decoder needs no
/// equivalent because it emits what it can decode on every call.
///
/// # Examples
///
/// ```
/// assert_eq!(mopenzl::Operation::default(), mopenzl::Operation::Continue);
/// ```
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Operation {
    /// Take input; compress chunks as they fill.
    #[default]
    Continue,
    /// End the current chunk so that everything accepted so far can be
    /// decoded, without ending the frame. It costs ratio, and with
    /// [`InputSize::Unknown`] it has no effect: no byte can be written before
    /// the frame's size is known.
    Flush,
    /// Compress everything left and end the frame.
    End,
}

impl Operation {
    const fn to_core(self) -> Op {
        match self {
            Self::Continue => Op::Continue,
            Self::Flush => Op::Flush,
            Self::End => Op::End,
        }
    }
}

/// What a session needs next.
///
/// # Examples
///
/// ```
/// use mopenzl::EncoderStatus;
///
/// assert_ne!(EncoderStatus::NeedsInput, EncoderStatus::Finished);
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum EncoderStatus {
    /// The operation has done all it can; more input or a new operation is
    /// needed.
    NeedsInput,
    /// Compressed bytes are waiting; call again with room for them.
    NeedsOutput,
    /// The frame is complete and delivered.
    Finished,
}

/// What one [`EncoderSession::process`] call did.
///
/// # Examples
///
/// ```
/// use mopenzl::{EncoderStatus, Progress};
///
/// let progress = Progress { consumed: 4, produced: 0, status: EncoderStatus::NeedsInput };
/// assert_eq!(progress.consumed, 4);
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Progress {
    /// Bytes taken from the input.
    pub consumed: usize,
    /// Bytes written to the output.
    pub produced: usize,
    /// What the session needs next.
    pub status: EncoderStatus,
}

/// Lifecycle of a session.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum State {
    Open,
    Finished,
    Failed,
}

/// The shared state machine of borrowed and owned sessions.
#[derive(Debug)]
struct Core {
    state: State,
    declared: Option<u64>,
}

impl Core {
    const fn new(stream: StreamConfig) -> Self {
        Self {
            state: State::Open,
            declared: match stream.input_size() {
                InputSize::Exact(size) => Some(size),
                InputSize::Unknown => None,
            },
        }
    }

    fn process<D: Destination + ?Sized>(
        &mut self,
        compressor: &mut Compressor,
        input: &[u8],
        output: &mut D,
        operation: Operation,
    ) -> Result<Progress, EncodeError> {
        match self.state {
            State::Failed => return Err(EncodeError::InvalidState),
            State::Finished => {
                return Ok(Progress {
                    consumed: 0,
                    produced: 0,
                    status: EncoderStatus::Finished,
                });
            }
            State::Open => {}
        }
        if let Some(error) = compressor.take_plan_error() {
            self.state = State::Failed;
            return Err(error);
        }
        let params = *compressor.params();
        let result = compressor.stream.process(
            &mut compressor.engine,
            &params,
            self.declared,
            input,
            output,
            operation.to_core(),
        );
        let step = match result {
            Ok(step) => step,
            Err(error) => {
                self.state = State::Failed;
                compressor.stream.reset();
                compressor.apply_retention();
                return Err(EncodeError::from_stream(error, self.declared));
            }
        };
        let status = match step.status {
            StepStatus::NeedsInput => EncoderStatus::NeedsInput,
            StepStatus::NeedsOutput => EncoderStatus::NeedsOutput,
            StepStatus::Finished => {
                self.state = State::Finished;
                compressor.stream.reset();
                compressor.apply_retention();
                EncoderStatus::Finished
            }
        };
        Ok(Progress {
            consumed: step.consumed,
            produced: step.produced,
            status,
        })
    }
}

macro_rules! session_methods {
    () => {
        /// Moves the frame forward by one step: takes what it can from
        /// `input`, writes what it can into `output`, and reports exactly how
        /// much of each it moved and what it needs next.
        ///
        /// An [`Operation::End`] that returns [`EncoderStatus::NeedsOutput`]
        /// must be repeated until it reports [`EncoderStatus::Finished`];
        /// afterwards calls are idempotent.
        ///
        /// # Errors
        ///
        /// [`EncodeError::InputSizeMismatch`] when the input contradicts a
        /// declared size: when more arrives than was declared, or when the
        /// frame ends short of it. [`EncodeError::InvalidState`] after a
        /// failure. A failed session encodes nothing further.
        ///
        /// # Examples
        ///
        /// ```
        /// use mopenzl::{Compressor, EncoderStatus, Operation};
        ///
        /// let mut encoder = Compressor::default();
        /// let mut session = encoder.start(Default::default());
        /// let mut out = [0u8; 64];
        /// let progress = session.process(b"", &mut out, Operation::End)?;
        /// assert_eq!(progress.status, EncoderStatus::Finished);
        /// assert!(progress.produced > 0);
        /// # Ok::<(), Box<dyn std::error::Error>>(())
        /// ```
        pub fn process(
            &mut self,
            input: &[u8],
            output: &mut [u8],
            operation: Operation,
        ) -> Result<Progress, EncodeError> {
            self.core
                .process(&mut self.compressor, input, output, operation)
        }

        /// [`Self::process`] into uninitialized memory, such as
        /// [`Vec::spare_capacity_mut`](alloc::vec::Vec::spare_capacity_mut),
        /// without a temporary buffer or zeroing.
        ///
        /// It produces the same bytes, `consumed` and `produced` counts,
        /// statuses and errors as [`Self::process`], and calls of the two may
        /// be mixed on one frame. On success exactly `output[..produced]` is
        /// initialized; nothing after it is written.
        ///
        /// # Errors
        ///
        /// As [`Self::process`].
        ///
        /// # Examples
        ///
        /// ```
        /// use mopenzl::{Compressor, EncoderStatus, Operation};
        ///
        /// let mut encoder = Compressor::default();
        /// let mut session = encoder.start(Default::default());
        /// let mut out: Vec<u8> = Vec::with_capacity(64);
        /// let progress = session.process_uninit(b"data", out.spare_capacity_mut(), Operation::End)?;
        /// // SAFETY: `process_uninit` initialized the first `produced` bytes.
        /// unsafe { out.set_len(progress.produced) };
        /// assert_eq!(progress.status, EncoderStatus::Finished);
        /// assert_eq!(out, Compressor::default().compress(b"data")?);
        /// # Ok::<(), Box<dyn std::error::Error>>(())
        /// ```
        pub fn process_uninit(
            &mut self,
            input: &[u8],
            output: &mut [MaybeUninit<u8>],
            operation: Operation,
        ) -> Result<Progress, EncodeError> {
            self.core
                .process(&mut self.compressor, input, output, operation)
        }

        /// Makes everything accepted so far decodable, without taking input.
        ///
        /// # Errors
        ///
        /// As [`Self::process`].
        ///
        /// # Examples
        ///
        /// ```
        /// use mopenzl::{Compressor, EncoderStatus, InputSize, Operation};
        ///
        /// let mut encoder = Compressor::default();
        /// let mut session = encoder.start(InputSize::Exact(30).into());
        /// let mut out = [0u8; 256];
        /// session.process(b"flushed payload", &mut out, Operation::Continue)?;
        /// let progress = session.flush(&mut out)?;
        /// assert_eq!(progress.status, EncoderStatus::NeedsInput);
        /// assert!(progress.produced > 0);
        /// # Ok::<(), Box<dyn std::error::Error>>(())
        /// ```
        pub fn flush(&mut self, output: &mut [u8]) -> Result<Progress, EncodeError> {
            self.process(&[], output, Operation::Flush)
        }

        /// Ends the frame, without taking input.
        ///
        /// # Errors
        ///
        /// As [`Self::process`].
        ///
        /// # Examples
        ///
        /// ```
        /// use mopenzl::{Compressor, EncoderStatus, Operation};
        ///
        /// let mut encoder = Compressor::default();
        /// let mut session = encoder.start(Default::default());
        /// let mut out = [0u8; 256];
        /// session.process(b"payload", &mut out, Operation::Continue)?;
        /// assert_eq!(session.finish(&mut out)?.status, EncoderStatus::Finished);
        /// # Ok::<(), Box<dyn std::error::Error>>(())
        /// ```
        pub fn finish(&mut self, output: &mut [u8]) -> Result<Progress, EncodeError> {
            self.process(&[], output, Operation::End)
        }

        /// Whether the frame has been completed and delivered.
        #[must_use]
        pub fn is_finished(&self) -> bool {
            self.core.state == State::Finished
        }
    };
}

/// One incremental frame borrowing its compressor.
///
/// Dropping a session before it finishes abandons the frame; the compressor
/// is left ready for the next one either way.
///
/// # Examples
///
/// ```
/// use mopenzl::{Compressor, Decompressor, EncoderStatus, InputSize, Operation};
///
/// let mut encoder = Compressor::default();
/// let payload = b"a payload compressed one chunk at a time".repeat(10);
/// let mut compressed = Vec::new();
/// let mut buffer = [0u8; 64];
/// let mut input = payload.as_slice();
/// {
///     let mut session = encoder.start(InputSize::Exact(payload.len() as u64).into());
///     loop {
///         let progress = session.process(input, &mut buffer, Operation::End)?;
///         input = &input[progress.consumed..];
///         compressed.extend_from_slice(&buffer[..progress.produced]);
///         if progress.status == EncoderStatus::Finished {
///             break;
///         }
///     }
/// }
/// assert_eq!(compressed, encoder.compress(&payload)?);
/// assert_eq!(Decompressor::default().decompress(&compressed)?, payload);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct EncoderSession<'c> {
    compressor: &'c mut Compressor,
    core: Core,
}

impl<'c> EncoderSession<'c> {
    pub(crate) const fn new(compressor: &'c mut Compressor, stream: StreamConfig) -> Self {
        Self {
            compressor,
            core: Core::new(stream),
        }
    }

    session_methods!();
}

/// One incremental frame that owns its compressor.
///
/// # Examples
///
/// ```
/// use mopenzl::{Compressor, EncoderStatus, Operation};
///
/// let mut session = Compressor::default().into_session(Default::default());
/// let mut out = [0u8; 64];
/// assert_eq!(session.process(b"x", &mut out, Operation::End)?.status, EncoderStatus::Finished);
/// let compressor = session.into_compressor();
/// assert!(compressor.retained_bytes() > 0);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct EncoderSessionOwned {
    compressor: Box<Compressor>,
    core: Core,
}

impl EncoderSessionOwned {
    pub(crate) const fn new(compressor: Box<Compressor>, stream: StreamConfig) -> Self {
        Self {
            compressor,
            core: Core::new(stream),
        }
    }

    session_methods!();

    /// Starts a new frame on the same compressor, abandoning an unfinished
    /// one.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{Compressor, Operation};
    ///
    /// let mut session = Compressor::default().into_session(Default::default());
    /// let mut out = [0u8; 64];
    /// session.process(b"one", &mut out, Operation::End)?;
    /// session.reinit(Default::default());
    /// assert!(!session.is_finished());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn reinit(&mut self, stream: StreamConfig) {
        self.compressor.begin_stream();
        self.core = Core::new(stream);
    }

    /// Returns the compressor, abandoning an unfinished frame.
    #[must_use]
    pub fn into_compressor(self) -> Compressor {
        let mut compressor = *self.compressor;
        compressor.begin_stream();
        compressor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EncoderConfig;
    use alloc::vec::Vec;

    #[test]
    fn stream_configs_carry_their_input_size() {
        assert_eq!(InputSize::from(7u64), InputSize::Exact(7));
        let stream = StreamConfig::default().with_input_size(InputSize::Exact(3));
        assert_eq!(stream.input_size(), InputSize::Exact(3));
        assert_eq!(
            StreamConfig::from(InputSize::Unknown),
            StreamConfig::default()
        );
    }

    #[test]
    fn a_failed_session_refuses_further_work() {
        let mut encoder = Compressor::default();
        let mut session = encoder.start(InputSize::Exact(2).into());
        let mut out = [0u8; 64];
        assert_eq!(
            session.process(b"abc", &mut out, Operation::Continue),
            Err(EncodeError::InputSizeMismatch { declared: 2 })
        );
        assert_eq!(session.flush(&mut out), Err(EncodeError::InvalidState));
        assert!(!session.is_finished());
    }

    #[test]
    fn owned_sessions_restart_and_return_the_compressor() {
        let mut session = Compressor::new(EncoderConfig::default())
            .unwrap()
            .into_session(InputSize::Unknown.into());
        let mut out = [0u8; 128];
        let first = session.process(b"one", &mut out, Operation::End).unwrap();
        assert!(session.is_finished());
        let frame: Vec<u8> = out[..first.produced].to_vec();
        session.reinit(Default::default());
        let second = session.process(b"one", &mut out, Operation::End).unwrap();
        assert_eq!(&out[..second.produced], frame.as_slice());
        let mut compressor = session.into_compressor();
        assert_eq!(compressor.compress(b"one").unwrap(), frame);
    }

    #[test]
    fn small_outputs_are_resumed_until_finished() {
        let mut encoder = Compressor::default();
        let expected = encoder.compress(b"resumable").unwrap();
        let mut session = encoder.start(Default::default());
        let mut frame = Vec::new();
        let mut byte = [0u8; 1];
        let mut input: &[u8] = b"resumable";
        loop {
            let progress = session.process(input, &mut byte, Operation::End).unwrap();
            input = &input[progress.consumed..];
            frame.extend_from_slice(&byte[..progress.produced]);
            if progress.status == EncoderStatus::Finished {
                break;
            }
            assert_eq!(progress.status, EncoderStatus::NeedsOutput);
        }
        assert_eq!(frame, expected);
        assert_eq!(
            session.finish(&mut byte).unwrap().status,
            EncoderStatus::Finished
        );
    }
}
