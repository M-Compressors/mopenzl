//! Decoder policy: how many frames to read, what to verify, and limits.

/// How many frames one operation decodes.
///
/// The reference's `ZL_decompress` decodes exactly one frame and rejects
/// trailing bytes, which is the default.
///
/// # Examples
///
/// ```
/// use mopenzl::{DecoderConfig, FrameMode};
///
/// assert_eq!(FrameMode::default(), FrameMode::Single);
/// assert_eq!(DecoderConfig::from(FrameMode::Concatenated).frame_mode(), FrameMode::Concatenated);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum FrameMode {
    /// One frame; trailing bytes are an error for one-shot calls and are
    /// left unconsumed by sessions.
    #[default]
    Single,
    /// Every frame until the input ends.
    Concatenated,
}

/// Resource limits for untrusted input. Unlimited by default.
///
/// # Examples
///
/// ```
/// use mopenzl::DecodeLimits;
///
/// let limits = DecodeLimits::default()
///     .with_max_input_bytes(Some(1 << 20))
///     .with_max_output_bytes(Some(8 << 20));
/// assert_eq!(limits.max_input_bytes(), Some(1 << 20));
/// assert_eq!(limits.max_output_bytes(), Some(8 << 20));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct DecodeLimits {
    input: Option<u64>,
    output: Option<u64>,
}

impl DecodeLimits {
    /// No limits.
    pub const DEFAULT: Self = Self {
        input: None,
        output: None,
    };

    /// Caps the compressed bytes one operation accepts.
    #[must_use]
    pub const fn with_max_input_bytes(mut self, limit: Option<u64>) -> Self {
        self.input = limit;
        self
    }

    /// Caps the bytes one operation regenerates. A frame whose header
    /// declares more fails before any of its chunks is decoded.
    #[must_use]
    pub const fn with_max_output_bytes(mut self, limit: Option<u64>) -> Self {
        self.output = limit;
        self
    }

    /// The input cap.
    #[must_use]
    pub const fn max_input_bytes(&self) -> Option<u64> {
        self.input
    }

    /// The output cap.
    #[must_use]
    pub const fn max_output_bytes(&self) -> Option<u64> {
        self.output
    }
}

/// Everything that shapes a decoding operation.
///
/// Both checksums are verified by default, as the reference does
/// (`ZL_DParam_checkContentChecksum`, `ZL_DParam_checkCompressedChecksum`).
///
/// # Examples
///
/// ```
/// use mopenzl::{DecodeLimits, DecoderConfig, FrameMode};
///
/// let config = DecoderConfig::default()
///     .with_frame_mode(FrameMode::Concatenated)
///     .with_limits(DecodeLimits::default().with_max_output_bytes(Some(1 << 30)))
///     .with_content_checksum_check(false);
/// assert!(!config.content_checksum_check());
/// assert!(config.compressed_checksum_check());
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct DecoderConfig {
    frame_mode: FrameMode,
    limits: DecodeLimits,
    content_checksum: bool,
    compressed_checksum: bool,
}

impl DecoderConfig {
    /// One frame, no limits, both checksums verified.
    pub const DEFAULT: Self = Self {
        frame_mode: FrameMode::Single,
        limits: DecodeLimits::DEFAULT,
        content_checksum: true,
        compressed_checksum: true,
    };

    /// Sets how many frames an operation decodes.
    #[must_use]
    pub const fn with_frame_mode(mut self, mode: FrameMode) -> Self {
        self.frame_mode = mode;
        self
    }

    /// Sets the resource limits.
    #[must_use]
    pub const fn with_limits(mut self, limits: DecodeLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Whether chunk content checksums are verified.
    #[must_use]
    pub const fn with_content_checksum_check(mut self, enabled: bool) -> Self {
        self.content_checksum = enabled;
        self
    }

    /// Whether chunk compressed checksums and the header check byte are
    /// verified.
    #[must_use]
    pub const fn with_compressed_checksum_check(mut self, enabled: bool) -> Self {
        self.compressed_checksum = enabled;
        self
    }

    /// How many frames an operation decodes.
    #[must_use]
    pub const fn frame_mode(&self) -> FrameMode {
        self.frame_mode
    }

    /// The resource limits.
    #[must_use]
    pub const fn limits(&self) -> DecodeLimits {
        self.limits
    }

    /// Whether content checksums are verified.
    #[must_use]
    pub const fn content_checksum_check(&self) -> bool {
        self.content_checksum
    }

    /// Whether compressed checksums are verified.
    #[must_use]
    pub const fn compressed_checksum_check(&self) -> bool {
        self.compressed_checksum
    }
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl From<FrameMode> for DecoderConfig {
    fn from(mode: FrameMode) -> Self {
        Self::DEFAULT.with_frame_mode(mode)
    }
}

impl From<DecodeLimits> for DecoderConfig {
    fn from(limits: DecodeLimits) -> Self {
        Self::DEFAULT.with_limits(limits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_set_every_field() {
        let limits = DecodeLimits::DEFAULT
            .with_max_input_bytes(Some(1))
            .with_max_output_bytes(Some(2));
        let config = DecoderConfig::from(limits)
            .with_frame_mode(FrameMode::Concatenated)
            .with_content_checksum_check(false)
            .with_compressed_checksum_check(false);
        assert_eq!(config.limits(), limits);
        assert_eq!(config.frame_mode(), FrameMode::Concatenated);
        assert!(!config.content_checksum_check());
        assert!(!config.compressed_checksum_check());
        assert_eq!(DecoderConfig::default(), DecoderConfig::DEFAULT);
        assert_eq!(DecodeLimits::default(), DecodeLimits::DEFAULT);
    }
}
