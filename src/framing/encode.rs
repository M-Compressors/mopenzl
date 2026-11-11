use super::{FramedError, FramingConfig, MetadataField, core};
use crate::graph::CompressorGraph;
use crate::input::Input;
use crate::{Compressor, EncoderConfig};
use ::core::ops::Range;
use alloc::vec::Vec;

/// Codec and container policy for the framed encoder.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FramedEncodeConfig {
    /// Global OpenZL parameters shared by resources.
    pub encoder: EncoderConfig,
    /// Container limits.
    pub framing: FramingConfig,
}

/// Borrowed inputs of one independent OpenZL frame.
#[derive(Clone, Copy, Debug)]
pub enum ResourceInput<'a> {
    /// A single serial byte stream.
    Serial(&'a [u8]),
    /// Typed or multiple streams, in output order.
    Typed(&'a [Input<'a>]),
}

/// One resource, its optional graph override, and its metadata.
/// `None` keeps the underlying compressor's graph; an override applies only
/// to this resource, even if encoding fails.
#[derive(Clone, Copy, Debug)]
pub struct FramedResource<'a> {
    /// Inputs for this resource's frame.
    pub input: ResourceInput<'a>,
    /// An independent graph for this frame, including custom graphs/dictionaries.
    pub graph: Option<&'a CompressorGraph>,
    /// Resource metadata, serialized outside the OpenZL frame.
    pub metadata: &'a [MetadataField<'a>],
}

impl<'a> From<&'a [u8]> for FramedResource<'a> {
    fn from(bytes: &'a [u8]) -> Self {
        Self {
            input: ResourceInput::Serial(bytes),
            graph: None,
            metadata: &[],
        }
    }
}

impl<'a> From<&'a [Input<'a>]> for FramedResource<'a> {
    fn from(inputs: &'a [Input<'a>]) -> Self {
        Self {
            input: ResourceInput::Typed(inputs),
            graph: None,
            metadata: &[],
        }
    }
}

/// An ordered borrowed resource list.
#[derive(Clone, Copy, Debug)]
pub struct FramedInput<'a> {
    /// Resources in wire order; an empty list is a valid container.
    pub resources: &'a [FramedResource<'a>],
}

impl<'a> From<&'a [FramedResource<'a>]> for FramedInput<'a> {
    fn from(resources: &'a [FramedResource<'a>]) -> Self {
        Self { resources }
    }
}

/// Reusable graph-aware framed encoder. Convert an existing [`Compressor`]
/// to preserve its backend, default graph and retention policy.
#[derive(Debug)]
pub struct FramedCompressor {
    pub(super) raw: Compressor,
    pub(super) config: FramingConfig,
}

impl From<Compressor> for FramedCompressor {
    fn from(raw: Compressor) -> Self {
        Self {
            raw,
            config: FramingConfig::default(),
        }
    }
}

impl FramedCompressor {
    /// Builds an encoder with the host-selected backend.
    ///
    /// # Errors
    /// Invalid container limits or encoder configuration.
    pub fn new(config: FramedEncodeConfig) -> Result<Self, FramedError> {
        core::validate(config.framing)?;
        Ok(Self {
            raw: Compressor::new(config.encoder)?,
            config: config.framing,
        })
    }

    /// Changes limits for subsequent containers.
    ///
    /// # Errors
    /// Invalid container limits; the previous policy stays installed.
    pub fn reconfigure(&mut self, config: FramingConfig) -> Result<(), FramedError> {
        core::validate(config)?;
        self.config = config;
        Ok(())
    }

    /// Starts a container. Drain its header before adding resources.
    ///
    /// # Errors
    /// A framing allocation failed.
    pub fn start(&mut self) -> Result<FramedEncoderSession<'_>, FramedError> {
        Ok(FramedEncoderSession {
            owner: self,
            state: core::encode::Encoder::start()?,
        })
    }

    /// Encodes all resources and the mandatory seek index.
    ///
    /// # Errors
    /// Metadata, graph, codec, configured-limit or allocation failures.
    pub fn compress(&mut self, input: FramedInput<'_>) -> Result<Vec<u8>, FramedError> {
        let mut dst = Vec::new();
        self.compress_into(input, &mut dst)?;
        Ok(dst)
    }

    /// Appends a complete container, rolling back the destination length on error.
    ///
    /// # Errors
    /// As [`Self::compress`].
    pub fn compress_into(
        &mut self,
        input: FramedInput<'_>,
        dst: &mut Vec<u8>,
    ) -> Result<Range<usize>, FramedError> {
        let start = dst.len();
        let result = (|| {
            let mut session = self.start()?;
            session.append(dst, FramedEncodeOperation::Process)?;
            for &resource in input.resources {
                session.resource(resource)?;
                session.append(dst, FramedEncodeOperation::Process)?;
            }
            session.append(dst, FramedEncodeOperation::Finish)
        })();
        if let Err(error) = result {
            dst.truncate(start);
            return Err(error);
        }
        Ok(start..dst.len())
    }
}

/// Whether to drain queued resource bytes or finish the container.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FramedEncodeOperation {
    /// Drain the header or current resource; then accept another resource.
    Process,
    /// Drain resources, then emit the directory and footer. Retry with Finish.
    Finish,
}

/// Encoder output/backpressure status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FramedEncoderStatus {
    /// A new resource may be submitted.
    NeedsInput,
    /// Supply output space and continue draining.
    NeedsOutput,
    /// All bytes including the footer were delivered.
    Finished,
}

/// The initialized prefix of an output slice and the next action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FramedEncodeProgress {
    /// Bytes written in this call.
    pub produced: usize,
    /// Next action.
    pub status: FramedEncoderStatus,
}

/// Borrowed sans-I/O container session. Resource commands borrow their complete
/// input only for the call; at most one encoded resource is queued at a time.
/// Dropping the session abandons the container and releases the owner.
#[derive(Debug)]
pub struct FramedEncoderSession<'a> {
    pub(super) owner: &'a mut FramedCompressor,
    pub(super) state: core::encode::Encoder,
}

impl FramedEncoderSession<'_> {
    /// Queues one frame. A failure commits no resource and may be corrected.
    ///
    /// # Errors
    /// Output pending, completion, invalid metadata, limits or codec failure.
    pub fn resource(&mut self, resource: FramedResource<'_>) -> Result<(), FramedError> {
        self.state
            .resource(&mut self.owner.raw, self.owner.config, resource)
    }

    /// Copies queued bytes to `output`. Process accepts the next resource after
    /// NeedsInput; Finish permanently closes the resource list once drained.
    ///
    /// # Errors
    /// Directory allocation or container size overflow.
    pub fn process(
        &mut self,
        output: &mut [u8],
        operation: FramedEncodeOperation,
    ) -> Result<FramedEncodeProgress, FramedError> {
        self.state
            .process(&self.owner.raw, self.owner.config, output, operation)
    }

    fn append(&mut self, dst: &mut Vec<u8>, op: FramedEncodeOperation) -> Result<(), FramedError> {
        let mut buffer = [0; 8192];
        loop {
            let progress = self.process(&mut buffer, op)?;
            core::reserve(dst, progress.produced)?;
            dst.extend_from_slice(&buffer[..progress.produced]);
            if progress.status != FramedEncoderStatus::NeedsOutput {
                return Ok(());
            }
        }
    }
}
