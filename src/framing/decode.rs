use super::{FramedError, FramingConfig, MetadataEntry, ResourceInfo, core};
use crate::{DecodeOperation, DecoderConfig, Decompressor, TypedOutput};
use alloc::vec::Vec;

/// Codec and container policy for the framed decoder. Each resource is decoded
/// as exactly one frame regardless of `decoder.frame_mode()`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FramedDecodeConfig {
    /// Inner OpenZL checksums, codec limits and policy.
    pub decoder: DecoderConfig,
    /// Container limits, in addition to inner codec limits.
    pub framing: FramingConfig,
}

/// One independently decoded resource and its preserved output types.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DecodedResource {
    /// Validated location and content size.
    pub info: ResourceInfo,
    /// Resource metadata, in wire order.
    pub metadata: Vec<MetadataEntry>,
    /// Typed frame outputs, in original input order.
    pub outputs: Vec<TypedOutput>,
}

/// A complete validated container.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct FramedOutput {
    /// Resources in wire order, including empty resources.
    pub resources: Vec<DecodedResource>,
}

/// Reusable framed decoder. Convert an existing [`Decompressor`] to preserve
/// its backend, dictionary store, custom codecs and retention policy.
#[derive(Debug)]
pub struct FramedDecompressor {
    pub(super) raw: Decompressor,
    pub(super) config: FramingConfig,
}

impl From<Decompressor> for FramedDecompressor {
    fn from(raw: Decompressor) -> Self {
        Self {
            raw,
            config: FramingConfig::default(),
        }
    }
}

impl FramedDecompressor {
    /// Builds a decoder with the host-selected backend.
    ///
    /// # Errors
    /// Invalid container limits.
    pub fn new(config: FramedDecodeConfig) -> Result<Self, FramedError> {
        core::validate(config.framing)?;
        Ok(Self {
            raw: Decompressor::new(config.decoder),
            config: config.framing,
        })
    }

    /// Changes limits for subsequent containers.
    ///
    /// # Errors
    /// Invalid limits; the old policy stays installed.
    pub fn reconfigure(&mut self, config: FramingConfig) -> Result<(), FramedError> {
        core::validate(config)?;
        self.config = config;
        Ok(())
    }

    /// Starts a borrowed incremental container decoder.
    pub fn start(&mut self) -> FramedDecoderSession<'_> {
        FramedDecoderSession {
            owner: self,
            state: core::decode::Decoder::default(),
        }
    }

    /// Decodes and validates the complete container, directory and footer.
    ///
    /// # Errors
    /// Malformed or truncated input, trailing bytes, limits, metadata, codec,
    /// dictionary, checksum or allocation failures.
    pub fn decompress(&mut self, src: &[u8]) -> Result<FramedOutput, FramedError> {
        let mut session = self.start();
        let mut at = 0;
        let mut resources = Vec::new();
        loop {
            let progress = session
                .process(&src[at..], DecodeOperation::Finish)
                .map_err(|failure| failure.error)?;
            at += progress.consumed;
            if let Some(resource) = progress.resource {
                core::reserve(&mut resources, 1)?;
                resources.push(resource);
            }
            if progress.status == FramedDecoderStatus::Finished {
                return Ok(FramedOutput { resources });
            }
        }
    }
}

/// Decoder input/event status. A Resource event pauses before consuming the
/// next record, so callers can handle output with bounded per-resource memory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FramedDecoderStatus {
    /// Supply more wire bytes, or Finish at the end of the input.
    NeedsInput,
    /// One resource was returned; continue with the unconsumed suffix.
    Resource,
    /// The footer, directory and end of input were validated.
    Finished,
}

/// Accepted bytes and at most one complete resource.
#[derive(Debug)]
pub struct FramedDecodeProgress {
    /// Bytes consumed by this call.
    pub consumed: usize,
    /// A decoded resource, if this call reached its boundary.
    pub resource: Option<DecodedResource>,
    /// Next action.
    pub status: FramedDecoderStatus,
}

/// A terminal decoder failure with the exact accepted prefix of this call.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct FramedDecodeFailure {
    /// Underlying high-level failure.
    #[source]
    pub error: FramedError,
    /// Bytes consumed before failure in this call.
    pub consumed: usize,
}

/// Incremental decoder holding at most one encoded resource and its output,
/// plus directory entries. Resources are tentative until Finished validates
/// the directory and footer. Failure is sticky; drop and start a new session.
#[derive(Debug)]
pub struct FramedDecoderSession<'a> {
    pub(super) owner: &'a mut FramedDecompressor,
    pub(super) state: core::decode::Decoder,
}

impl FramedDecoderSession<'_> {
    /// Feeds container bytes and returns at the next resource boundary.
    /// With Finish, resend any unconsumed suffix with Finish until Finished.
    ///
    /// # Errors
    /// As [`FramedDecompressor::decompress`], with per-call consumed bytes.
    pub fn process(
        &mut self,
        input: &[u8],
        operation: DecodeOperation,
    ) -> Result<FramedDecodeProgress, FramedDecodeFailure> {
        let before = self.state.accepted;
        self.state
            .process(&mut self.owner.raw, self.owner.config, input, operation)
            .map_err(|error| FramedDecodeFailure {
                error,
                consumed: (self.state.accepted - before) as usize,
            })
    }
}
