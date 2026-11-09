//! The reusable decompressor and its one-shot API.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::mem::MaybeUninit;
use core::ops::Range;

use super::config::{DecoderConfig, FrameMode};
use super::core::Engine;
use super::core::out::{SliceOut, UninitOut, VecOut};
use super::core::stream::Stream;
use super::error::DecodeError;
use super::session::{DecoderSession, DecoderSessionOwned};
use crate::{Backend, RetentionPolicy};

/// A reusable OpenZL decompressor.
///
/// It owns the Zstandard decoder, the chunk layout and the buffers of
/// intermediate streams and sessions, and reuses them between operations.
///
/// # Examples
///
/// ```
/// use mopenzl::{Compressor, Decompressor};
///
/// let frame = Compressor::default().compress(b"round trip")?;
/// let mut decoder = Decompressor::default();
/// assert_eq!(decoder.decompress(&frame)?, b"round trip");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct Decompressor {
    config: DecoderConfig,
    retention: RetentionPolicy,
    backend: Backend,
    pub(crate) engine: Engine,
    pub(crate) stream: Stream,
}

impl Default for Decompressor {
    fn default() -> Self {
        Self::new(DecoderConfig::DEFAULT)
    }
}

/// Configures a [`Decompressor`] beyond its [`DecoderConfig`].
///
/// # Examples
///
/// ```
/// use mopenzl::{Backend, Decompressor, RetentionPolicy};
///
/// let decoder = Decompressor::builder(Default::default())
///     .with_retention(RetentionPolicy::ReleaseAll)
///     .with_backend(Backend::default())
///     .build();
/// assert_eq!(decoder.retention(), RetentionPolicy::ReleaseAll);
/// ```
#[derive(Copy, Clone, Debug)]
pub struct DecompressorBuilder {
    config: DecoderConfig,
    retention: RetentionPolicy,
    backend: Option<Backend>,
}

impl DecompressorBuilder {
    /// Sets what the decompressor keeps allocated between operations.
    #[must_use]
    pub const fn with_retention(mut self, retention: RetentionPolicy) -> Self {
        self.retention = retention;
        self
    }

    /// Runs the kernels on `backend` instead of the host's best.
    #[must_use]
    pub const fn with_backend(mut self, backend: Backend) -> Self {
        self.backend = Some(backend);
        self
    }

    /// Builds the decompressor.
    #[must_use]
    pub fn build(self) -> Decompressor {
        let backend = self.backend.unwrap_or_default();
        Decompressor {
            config: self.config,
            retention: self.retention,
            backend,
            engine: Engine::new(backend),
            stream: Stream::default(),
        }
    }
}

impl Decompressor {
    /// A decompressor for `config` on the host's best backend.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{Decompressor, FrameMode};
    ///
    /// let decoder = Decompressor::new(FrameMode::Concatenated.into());
    /// assert_eq!(decoder.config().frame_mode(), FrameMode::Concatenated);
    /// ```
    #[must_use]
    pub fn new(config: DecoderConfig) -> Self {
        Self::builder(config).build()
    }

    /// A builder for a decompressor with a retention policy or backend.
    #[must_use]
    pub const fn builder(config: DecoderConfig) -> DecompressorBuilder {
        DecompressorBuilder {
            config,
            retention: RetentionPolicy::Aggressive,
            backend: None,
        }
    }

    /// The configuration operations run with.
    #[must_use]
    pub const fn config(&self) -> &DecoderConfig {
        &self.config
    }

    /// The backend the kernels run on.
    #[must_use]
    pub const fn backend(&self) -> Backend {
        self.backend
    }

    /// What the decompressor keeps allocated between operations.
    #[must_use]
    pub const fn retention(&self) -> RetentionPolicy {
        self.retention
    }

    /// Switches to `config` for the next operation.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{Decompressor, FrameMode};
    ///
    /// let mut decoder = Decompressor::default();
    /// decoder.reconfigure(FrameMode::Concatenated.into());
    /// assert_eq!(decoder.config().frame_mode(), FrameMode::Concatenated);
    /// ```
    pub fn reconfigure(&mut self, config: DecoderConfig) {
        self.config = config;
        self.stream.reset();
    }

    /// Bytes the decompressor keeps allocated between operations.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.engine.retained_bytes() + self.stream.retained_bytes()
    }

    /// Releases what `policy` does not allow to keep, now.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{Compressor, Decompressor, RetentionPolicy};
    ///
    /// let frame = Compressor::default().compress(&[1; 4096])?;
    /// let mut decoder = Decompressor::default();
    /// decoder.decompress(&frame)?;
    /// decoder.trim(RetentionPolicy::ReleaseAll);
    /// assert_eq!(decoder.retained_bytes(), 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn trim(&mut self, policy: RetentionPolicy) {
        if !policy.allows(self.stream.retained_bytes()) {
            self.stream.release();
        }
        self.engine.trim(policy);
    }

    /// Applies the retention policy after an operation.
    pub(crate) fn apply_retention(&mut self) {
        if !self.retention.allows(self.retained_bytes()) {
            self.trim(self.retention);
        }
    }

    /// Starts an incremental operation borrowing the decompressor.
    pub fn start(&mut self) -> DecoderSession<'_> {
        self.stream.reset();
        DecoderSession::new(self)
    }

    /// Starts an incremental operation that owns the decompressor.
    #[must_use]
    pub fn into_session(mut self) -> DecoderSessionOwned {
        self.stream.reset();
        DecoderSessionOwned::new(Box::new(self))
    }

    /// Checks what a one-shot operation consumed.
    fn check_consumed(&self, consumed: usize, src: &[u8]) -> Result<(), DecodeError> {
        if self.config.frame_mode() == FrameMode::Single && consumed != src.len() {
            return Err(DecodeError::TrailingData {
                offset: consumed as u64,
            });
        }
        Ok(())
    }

    /// Decodes `src` into a new buffer.
    ///
    /// # Errors
    ///
    /// Any [`DecodeError`]: invalid, truncated, unsupported or trailing
    /// input, checksum and size mismatches, and exceeded limits.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{Compressor, DecodeError, Decompressor};
    ///
    /// let frame = Compressor::default().compress(b"payload")?;
    /// let mut decoder = Decompressor::default();
    /// assert_eq!(decoder.decompress(&frame)?, b"payload");
    /// let trailing = [&frame[..], b"!"].concat();
    /// assert!(matches!(decoder.decompress(&trailing), Err(DecodeError::TrailingData { .. })));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn decompress(&mut self, src: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut dst = Vec::new();
        self.decompress_into(src, &mut dst)?;
        Ok(dst)
    }

    /// Appends the decoded content of `src` to `dst` and returns where it
    /// landed. On failure `dst` is left as it was.
    ///
    /// # Errors
    ///
    /// As [`Self::decompress`].
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{Compressor, Decompressor};
    ///
    /// let frame = Compressor::default().compress(b"ok")?;
    /// let mut output = b"status: ".to_vec();
    /// let range = Decompressor::default().decompress_into(&frame, &mut output)?;
    /// assert_eq!(&output[range], b"ok");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn decompress_into(
        &mut self,
        src: &[u8],
        dst: &mut Vec<u8>,
    ) -> Result<Range<usize>, DecodeError> {
        let start = dst.len();
        let result = self
            .engine
            .decode_frames(&self.config, src, &mut VecOut::new(dst))
            .and_then(|consumed| self.check_consumed(consumed, src));
        self.apply_retention();
        match result {
            Ok(()) => Ok(start..dst.len()),
            Err(error) => {
                dst.truncate(start);
                Err(error)
            }
        }
    }

    /// Decodes `src` into the front of `dst` and returns the decoded length.
    ///
    /// # Errors
    ///
    /// [`DecodeError::OutputTooSmall`] when the content does not fit, and
    /// otherwise as [`Self::decompress`]. `dst` then holds an unspecified
    /// prefix.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{Compressor, DecodeError, Decompressor};
    ///
    /// let frame = Compressor::default().compress(b"payload")?;
    /// let mut decoder = Decompressor::default();
    /// let mut dst = [0u8; 16];
    /// assert_eq!(decoder.decompress_to_slice(&frame, &mut dst)?, 7);
    /// assert!(matches!(
    ///     decoder.decompress_to_slice(&frame, &mut dst[..3]),
    ///     Err(DecodeError::OutputTooSmall { .. })
    /// ));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn decompress_to_slice(
        &mut self,
        src: &[u8],
        dst: &mut [u8],
    ) -> Result<usize, DecodeError> {
        let mut out = SliceOut::new(dst);
        let result = self
            .engine
            .decode_frames(&self.config, src, &mut out)
            .and_then(|consumed| self.check_consumed(consumed, src));
        let written = super::core::out::Out::len(&out);
        self.apply_retention();
        result.map(|()| written)
    }

    /// [`Self::decompress_to_slice`] into uninitialized memory, such as
    /// [`Vec::spare_capacity_mut`], without zeroing it first.
    ///
    /// It returns the same lengths and errors. On success exactly
    /// `dst[..written]` is initialized and no byte after it has been
    /// written; after an error treat `dst` as uninitialized.
    ///
    /// # Errors
    ///
    /// As [`Self::decompress_to_slice`].
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{Compressor, Decompressor};
    ///
    /// let frame = Compressor::default().compress(b"payload")?;
    /// let mut out: Vec<u8> = Vec::with_capacity(16);
    /// let written = Decompressor::default().decompress_to_uninit(&frame, out.spare_capacity_mut())?;
    /// // SAFETY: `decompress_to_uninit` initialized the first `written` bytes.
    /// unsafe { out.set_len(written) };
    /// assert_eq!(out, b"payload");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn decompress_to_uninit(
        &mut self,
        src: &[u8],
        dst: &mut [MaybeUninit<u8>],
    ) -> Result<usize, DecodeError> {
        let mut out = UninitOut::from(dst);
        let result = self
            .engine
            .decode_frames(&self.config, src, &mut out)
            .and_then(|consumed| self.check_consumed(consumed, src));
        let written = super::core::out::Out::len(&out);
        self.apply_retention();
        result.map(|()| written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concatenated_mode_reads_every_frame() {
        let frame = openzl_ffi::compress(b"ab", openzl_ffi::Params::default()).unwrap();
        let both = [&frame[..], &frame].concat();
        let mut decoder = Decompressor::new(FrameMode::Concatenated.into());
        assert_eq!(decoder.decompress(&both).unwrap(), b"abab");
        let mut decoder = Decompressor::default();
        assert_eq!(
            decoder.decompress(&both),
            Err(DecodeError::TrailingData {
                offset: frame.len() as u64
            })
        );
    }

    #[test]
    fn slices_reconfiguration_and_trimming() {
        let frame = openzl_ffi::compress(b"slice me", openzl_ffi::Params::default()).unwrap();
        let mut decoder = Decompressor::default();
        assert_eq!(decoder.retention(), RetentionPolicy::Aggressive);
        let mut dst = [0u8; 16];
        assert_eq!(decoder.decompress_to_slice(&frame, &mut dst), Ok(8));
        assert_eq!(&dst[..8], b"slice me");
        let trailing = [&frame[..], b"x"].concat();
        assert_eq!(
            decoder.decompress_to_slice(&trailing, &mut dst),
            Err(DecodeError::TrailingData {
                offset: frame.len() as u64
            })
        );
        decoder.reconfigure(FrameMode::Concatenated.into());
        assert_eq!(decoder.config().frame_mode(), FrameMode::Concatenated);
        assert!(decoder.retained_bytes() > 0);
        decoder.trim(RetentionPolicy::ReleaseAll);
        assert_eq!(decoder.retained_bytes(), 0);
    }

    #[test]
    fn failed_appends_roll_back() {
        let mut decoder = Decompressor::builder(DecoderConfig::default())
            .with_retention(RetentionPolicy::ReleaseAll)
            .build();
        let mut out = b"keep".to_vec();
        assert!(decoder.decompress_into(b"bad", &mut out).is_err());
        assert_eq!(out, b"keep");
        assert_eq!(decoder.retained_bytes(), 0);
        assert_eq!(decoder.backend(), Backend::default());
    }
}
