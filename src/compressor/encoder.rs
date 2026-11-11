//! The reusable compressor and its one-shot API.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::mem::MaybeUninit;
use core::ops::Range;

use super::config::Chunking;
use super::config::{ConfigError, EncoderConfig, SizeOverflow};
use super::core::graphs::SERIAL_CHUNK_BYTE_SIZE;
use super::core::params::LocalParams as CoreParams;
use super::core::registry::{RuntimeParams, gid};
use super::core::sink::{SliceSink, UninitSink, VecSink};
use super::core::stream::Stream;
use super::core::{Engine, GraphPlan, Params};
use super::error::EncodeError;
use super::session::{EncoderSession, EncoderSessionOwned, StreamConfig};
use crate::graph::{Comment, CompressorGraph, GraphError};
use crate::input::Input;
use crate::shared::wire::COMMENT_VERSION_MIN;
use crate::{Backend, RetentionPolicy};
use alloc::sync::Arc;

/// `ZL_FRAME_OVERHEAD_MAX`: a frame header and the end marker.
const FRAME_OVERHEAD_MAX: usize = 64;
/// `ZL_CHUNK_OVERHEAD_MAX`: a stored chunk's header and both checksums.
const CHUNK_OVERHEAD_MAX: usize = 16;
/// `ZL_MIN_CHUNK_SIZE`: the chunk size the bound assumes.
const BOUND_CHUNK: usize = 32 << 10;

/// A reusable OpenZL compressor.
///
/// It owns the Zstandard backend's tables and the buffers of a session and
/// reuses them between frames. Frames are byte-identical to the reference's
/// `ZL_CCtx_compress` with the same global parameters, or, with
/// [`Chunking::Fixed`](crate::Chunking::Fixed), to its serial segmenter.
///
/// # Examples
///
/// ```
/// use mopenzl::{Compressor, Decompressor, EncoderConfig, Level};
///
/// let mut encoder = Compressor::new(EncoderConfig::from(Level::try_from(9)?))?;
/// let frame = encoder.compress(b"a payload, a payload, a payload")?;
/// assert_eq!(Decompressor::default().decompress(&frame)?, b"a payload, a payload, a payload");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct Compressor {
    config: EncoderConfig,
    params: Params,
    graph: Option<CompressorGraph>,
    comment: Option<Comment>,
    plan_error: Option<EncodeError>,
    backend: Backend,
    retention: RetentionPolicy,
    pub(crate) engine: Engine,
    pub(crate) stream: Stream,
}

impl Default for Compressor {
    /// A compressor for [`EncoderConfig::default`].
    ///
    /// # Examples
    ///
    /// ```
    /// let mut encoder = mopenzl::Compressor::default();
    /// assert_eq!(encoder.config().level(), mopenzl::Level::DEFAULT);
    /// assert!(!encoder.compress(b"payload")?.is_empty());
    /// # Ok::<(), mopenzl::EncodeError>(())
    /// ```
    fn default() -> Self {
        Self::with_parts(
            EncoderConfig::DEFAULT,
            RetentionPolicy::default(),
            Backend::default(),
            None,
        )
    }
}

impl Compressor {
    /// Temporarily replaces the graph while encoding one framed resource.
    #[cfg(feature = "framed")]
    pub(crate) fn replace_graph(
        &mut self,
        graph: Option<CompressorGraph>,
    ) -> Option<CompressorGraph> {
        core::mem::replace(&mut self.graph, graph)
    }

    fn with_parts(
        config: EncoderConfig,
        retention: RetentionPolicy,
        backend: Backend,
        graph: Option<CompressorGraph>,
    ) -> Self {
        let params = Params::from(&config);
        Self {
            config,
            params,
            graph,
            comment: None,
            plan_error: None,
            backend,
            retention,
            engine: Engine::new(&params, backend),
            stream: Stream::default(),
        }
    }

    /// A compressor for `config` on the host's best backend.
    ///
    /// # Errors
    ///
    /// None today: every [`EncoderConfig`] is valid. The result keeps the
    /// constructor's signature stable as configurations grow.
    ///
    /// # Examples
    ///
    /// ```
    /// let encoder = mopenzl::Compressor::new(Default::default())?;
    /// assert_eq!(encoder.retained_bytes(), 0);
    /// # Ok::<(), mopenzl::ConfigError>(())
    /// ```
    pub fn new(config: EncoderConfig) -> Result<Self, ConfigError> {
        Self::builder(config).build()
    }

    /// A builder for a compressor with a retention policy or backend.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{Backend, Compressor, RetentionPolicy};
    ///
    /// let encoder = Compressor::builder(Default::default())
    ///     .with_retention(RetentionPolicy::ReleaseAll)
    ///     .with_backend(Backend::default())
    ///     .build()?;
    /// assert_eq!(encoder.retention(), RetentionPolicy::ReleaseAll);
    /// # Ok::<(), mopenzl::ConfigError>(())
    /// ```
    #[must_use]
    pub const fn builder(config: EncoderConfig) -> CompressorBuilder {
        CompressorBuilder {
            config,
            retention: RetentionPolicy::Aggressive,
            backend: None,
            graph: None,
        }
    }

    /// The configuration frames are written with.
    #[must_use]
    pub const fn config(&self) -> &EncoderConfig {
        &self.config
    }

    /// The backend the kernels run on.
    ///
    /// # Examples
    ///
    /// ```
    /// let encoder = mopenzl::Compressor::default();
    /// assert_eq!(encoder.backend(), mopenzl::Backend::default());
    /// ```
    #[must_use]
    pub const fn backend(&self) -> Backend {
        self.backend
    }

    /// What the compressor keeps allocated between frames.
    #[must_use]
    pub const fn retention(&self) -> RetentionPolicy {
        self.retention
    }

    /// Switches to `config` for the next frame, keeping the allocations.
    ///
    /// # Errors
    ///
    /// None today; see [`Self::new`].
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{Compressor, EncoderConfig, Level};
    ///
    /// let mut encoder = Compressor::default();
    /// encoder.reconfigure(EncoderConfig::from(Level::MAX))?;
    /// assert_eq!(encoder.config().level(), Level::MAX);
    /// # Ok::<(), mopenzl::ConfigError>(())
    /// ```
    pub fn reconfigure(&mut self, config: EncoderConfig) -> Result<(), ConfigError> {
        if self.comment.is_some() && u32::from(config.format_version()) < COMMENT_VERSION_MIN {
            return Err(ConfigError::CommentUnsupported {
                version: u32::from(config.format_version()),
            });
        }
        self.config = config;
        self.params = Params::from(&config);
        self.engine.reconfigure(&self.params);
        self.stream.reset();
        Ok(())
    }

    /// The largest frame an input of `input_size` bytes compresses to with a
    /// single serial input and chunks of at least 32 KiB
    /// (`ZL_compressBound`).
    ///
    /// Every chunk the encoder cuts itself is that large; flushes can cut
    /// smaller ones, each adding at most 16 bytes.
    ///
    /// # Errors
    ///
    /// [`SizeOverflow`] when the bound does not fit in `usize`.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::Compressor;
    ///
    /// let bound = Compressor::max_compressed_size(1000)?;
    /// let frame = Compressor::default().compress(&[0x5a; 1000])?;
    /// assert!(frame.len() <= bound);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub const fn max_compressed_size(input_size: usize) -> Result<usize, SizeOverflow> {
        let chunks = (input_size / BOUND_CHUNK) + 1;
        let Some(overhead) = chunks.checked_mul(CHUNK_OVERHEAD_MAX) else {
            return Err(SizeOverflow);
        };
        let Some(with_frame) = input_size.checked_add(FRAME_OVERHEAD_MAX) else {
            return Err(SizeOverflow);
        };
        match with_frame.checked_add(overhead) {
            Some(bound) => Ok(bound),
            None => Err(SizeOverflow),
        }
    }

    /// Compresses `src` into a new frame.
    ///
    /// # Errors
    ///
    /// [`EncodeError::Internal`] if the encoder breaks an invariant.
    ///
    /// # Examples
    ///
    /// ```
    /// let frame = mopenzl::Compressor::default().compress(b"payload")?;
    /// assert_eq!(&frame[..4], &[0xdb, 0xa5, 0xb1, 0xd7]);
    /// # Ok::<(), mopenzl::EncodeError>(())
    /// ```
    pub fn compress(&mut self, src: &[u8]) -> Result<Vec<u8>, EncodeError> {
        let mut dst = Vec::with_capacity(frame_capacity(src.len()));
        self.compress_into(src, &mut dst)?;
        Ok(dst)
    }

    /// Appends the frame of `src` to `dst` and returns where it landed.
    ///
    /// On failure `dst` is left as it was.
    ///
    /// # Errors
    ///
    /// As [`Self::compress`].
    ///
    /// # Examples
    ///
    /// ```
    /// let mut encoder = mopenzl::Compressor::default();
    /// let mut out = b"prefix".to_vec();
    /// let range = encoder.compress_into(b"payload", &mut out)?;
    /// assert_eq!(range.start, 6);
    /// assert_eq!(range.end, out.len());
    /// # Ok::<(), mopenzl::EncodeError>(())
    /// ```
    pub fn compress_into(
        &mut self,
        src: &[u8],
        dst: &mut Vec<u8>,
    ) -> Result<Range<usize>, EncodeError> {
        self.sync_plan()?;
        let start = dst.len();
        let result = self.engine.frame(&self.params, src, &mut VecSink::new(dst));
        self.apply_retention();
        match result {
            Ok(()) => Ok(start..dst.len()),
            Err(error) => {
                dst.truncate(start);
                Err(EncodeError::from_core(error, usize::MAX))
            }
        }
    }

    /// Writes the frame of `src` to the front of `dst` and returns its length.
    ///
    /// # Errors
    ///
    /// [`EncodeError::OutputTooSmall`] when the frame does not fit; size `dst`
    /// with [`Self::max_compressed_size`] to rule it out. `dst` then holds an
    /// unspecified prefix.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::Compressor;
    ///
    /// let mut dst = vec![0u8; Compressor::max_compressed_size(7)?];
    /// let written = Compressor::default().compress_to_slice(b"payload", &mut dst)?;
    /// assert!(written > 7);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn compress_to_slice(&mut self, src: &[u8], dst: &mut [u8]) -> Result<usize, EncodeError> {
        self.sync_plan()?;
        let provided = dst.len();
        let mut sink = SliceSink::new(dst);
        let result = self.engine.frame(&self.params, src, &mut sink);
        let written = super::core::sink::Sink::len(&sink);
        self.apply_retention();
        result
            .map(|()| written)
            .map_err(|error| EncodeError::from_core(error, provided))
    }

    /// [`Self::compress_to_slice`] into uninitialized memory, such as
    /// [`Vec::spare_capacity_mut`], without zeroing it first.
    ///
    /// It writes the same bytes and returns the same length and errors. On
    /// success exactly `dst[..written]` is initialized and no byte after it
    /// has been written; after an error treat `dst` as uninitialized.
    ///
    /// # Errors
    ///
    /// As [`Self::compress_to_slice`].
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::Compressor;
    ///
    /// let mut out: Vec<u8> = Vec::with_capacity(Compressor::max_compressed_size(7)?);
    /// let written = Compressor::default().compress_to_uninit(b"payload", out.spare_capacity_mut())?;
    /// // SAFETY: `compress_to_uninit` initialized the first `written` bytes.
    /// unsafe { out.set_len(written) };
    /// assert_eq!(out, Compressor::default().compress(b"payload")?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn compress_to_uninit(
        &mut self,
        src: &[u8],
        dst: &mut [MaybeUninit<u8>],
    ) -> Result<usize, EncodeError> {
        self.sync_plan()?;
        let provided = dst.len();
        let mut sink = UninitSink::from(dst);
        let result = self.engine.frame(&self.params, src, &mut sink);
        let written = super::core::sink::Sink::len(&sink);
        self.apply_retention();
        result
            .map(|()| written)
            .map_err(|error| EncodeError::from_core(error, provided))
    }

    /// Starts an incremental frame borrowing the compressor.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{Compressor, EncoderStatus, Operation};
    ///
    /// let mut encoder = Compressor::default();
    /// let mut session = encoder.start(Default::default());
    /// let mut out = [0u8; 256];
    /// let progress = session.process(b"payload", &mut out, Operation::End)?;
    /// assert_eq!(progress.status, EncoderStatus::Finished);
    /// # Ok::<(), mopenzl::EncodeError>(())
    /// ```
    pub fn start(&mut self, stream: StreamConfig) -> EncoderSession<'_> {
        self.begin_stream();
        EncoderSession::new(self, stream)
    }

    /// Starts an incremental frame that owns the compressor.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{Compressor, EncoderStatus, Operation};
    ///
    /// let mut session = Compressor::default().into_session(Default::default());
    /// let mut out = [0u8; 256];
    /// assert_eq!(session.process(b"x", &mut out, Operation::End)?.status, EncoderStatus::Finished);
    /// # Ok::<(), mopenzl::EncodeError>(())
    /// ```
    #[must_use]
    pub fn into_session(mut self, stream: StreamConfig) -> EncoderSessionOwned {
        self.begin_stream();
        EncoderSessionOwned::new(Box::new(self), stream)
    }

    /// Bytes the compressor keeps allocated between frames.
    ///
    /// # Examples
    ///
    /// ```
    /// let mut encoder = mopenzl::Compressor::default();
    /// encoder.compress(&[1; 4096])?;
    /// assert!(encoder.retained_bytes() > 0);
    /// # Ok::<(), mopenzl::EncodeError>(())
    /// ```
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.engine.retained_bytes() + self.stream.retained_bytes()
    }

    /// Releases what `policy` does not allow to keep, now.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{Compressor, RetentionPolicy};
    ///
    /// let mut encoder = Compressor::default();
    /// encoder.compress(&[1; 4096])?;
    /// encoder.trim(RetentionPolicy::ReleaseAll);
    /// assert_eq!(encoder.retained_bytes(), 0);
    /// # Ok::<(), mopenzl::EncodeError>(())
    /// ```
    pub fn trim(&mut self, policy: RetentionPolicy) {
        if !policy.allows(self.stream.retained_bytes()) {
            self.stream.release();
        }
        self.engine.trim(policy);
    }

    /// Readies the stream state for a new frame.
    pub(crate) fn begin_stream(&mut self) {
        self.stream.reset();
        self.plan_error = self.sync_plan().err();
    }

    /// The error that stopped the plan of the stream just begun, once.
    pub(crate) fn take_plan_error(&mut self) -> Option<EncodeError> {
        self.plan_error.take()
    }

    /// Selects the engine path for the next operation: the specialized
    /// default graph when nothing asks for more, otherwise the graph engine
    /// with the compressor's graphs. A fixed chunk size wraps the start
    /// graph in the serial segmenter, as `ZL_Compressor_buildSerialSegmenter`
    /// would; a start graph that is already a segmenter is left to cut the
    /// input itself.
    fn sync_plan(&mut self) -> Result<(), EncodeError> {
        self.params = Params::from(&self.config);
        let custom = self.graph.is_some() || self.comment.is_some() || !self.params.fast_path();
        if !custom {
            self.engine.set_plan(None);
            return Ok(());
        }
        let graph = self.graph.clone().unwrap_or_default();
        let registry = Arc::clone(&graph.0);
        let mut start = registry.start;
        let mut start_params = registry.start_params.clone();
        if let Chunking::Fixed(size) = self.config.chunking() {
            if matches!(
                registry.graph(start).map(|def| &def.kind),
                Ok(super::core::registry::GraphKind::Segmenter(_))
            ) {
                return Err(GraphError::Invalid(
                    "fixed chunking cannot wrap a graph that is already a segmenter",
                )
                .into());
            }
            let chunk = i32::try_from(usize::from(size)).unwrap_or(i32::MAX);
            let mut params = CoreParams::default();
            params.ints.to_mut().push((SERIAL_CHUNK_BYTE_SIZE, chunk));
            start_params = Some(RuntimeParams {
                params: Some(params),
                custom_graphs: Some(alloc::vec![start]),
                custom_nodes: None,
            });
            start = gid::SEGMENT_SERIAL;
        }
        let segmented = registry.serial_plan(start, start_params.as_ref());
        if let Some((chunk, _)) = segmented {
            self.params.chunk_limit = chunk;
        }
        self.engine.set_plan(Some(GraphPlan {
            registry,
            start,
            start_params,
            comment: self.comment.as_ref().map(Comment::shared),
            segmented,
        }));
        Ok(())
    }

    /// Sets the header comment of the frames this compressor writes, or
    /// removes it with `None`. Unlike the reference, which forgets the
    /// comment after each compression, it stays until changed.
    ///
    /// # Errors
    ///
    /// [`ConfigError::CommentUnsupported`] when the format version predates
    /// header comments (22).
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::graph::Comment;
    /// use mopenzl::Compressor;
    ///
    /// let mut compressor = Compressor::default();
    /// compressor.set_comment(Some(Comment::try_from(&b"made here"[..])?))?;
    /// let frame = compressor.compress(b"payload")?;
    /// # #[cfg(feature = "decompression")]
    /// assert_eq!(mopenzl::Decompressor::default().decompress(&frame)?, b"payload");
    /// assert!(frame.windows(9).any(|window| window == b"made here"));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn set_comment(&mut self, comment: Option<Comment>) -> Result<(), ConfigError> {
        let version = u32::from(self.config.format_version());
        if comment.is_some() && version < COMMENT_VERSION_MIN {
            return Err(ConfigError::CommentUnsupported { version });
        }
        self.comment = comment;
        Ok(())
    }

    /// Compresses typed inputs into one frame.
    ///
    /// Without a graph (see [`CompressorBuilder::with_graph`]) every input
    /// goes through the reference's `compress_generic`.
    ///
    /// # Errors
    ///
    /// [`EncodeError::Graph`] when a graph fails on the data (for example a
    /// codec that is not ported yet, unless the compressor is permissive).
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::input::{Input, Numbers};
    /// use mopenzl::graph::{CompressorGraph, StandardGraph};
    /// use mopenzl::{Compressor, EncoderConfig};
    ///
    /// # #[cfg(feature = "conversion")]
    /// # {
    /// let numbers = [1u8, 0, 0, 0, 2, 0, 0, 0];
    /// let input = Input::from(Numbers::try_from((&numbers[..], 4))?);
    /// let mut compressor = Compressor::builder(EncoderConfig::default())
    ///     .with_graph(CompressorGraph::standard(StandardGraph::Store))
    ///     .build()?;
    /// let frame = compressor.compress_inputs(&[input])?;
    /// # #[cfg(feature = "decompression")]
    /// # {
    /// let outputs = mopenzl::Decompressor::default().decompress_typed(&frame)?;
    /// assert_eq!(outputs[0].content(), numbers);
    /// assert_eq!(outputs[0].width(), 4);
    /// # }
    /// # }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn compress_inputs(&mut self, inputs: &[Input<'_>]) -> Result<Vec<u8>, EncodeError> {
        let total = inputs
            .iter()
            .fold(0usize, |sum, input| sum.saturating_add(input.byte_len()));
        let mut dst = Vec::with_capacity(frame_capacity(total));
        self.compress_inputs_into(inputs, &mut dst)?;
        Ok(dst)
    }

    /// Appends the frame for typed `inputs` to `dst` and returns where it
    /// landed; `dst` keeps its old length on failure.
    ///
    /// # Errors
    ///
    /// As [`Self::compress_inputs`].
    pub fn compress_inputs_into(
        &mut self,
        inputs: &[Input<'_>],
        dst: &mut Vec<u8>,
    ) -> Result<Range<usize>, EncodeError> {
        self.sync_plan()?;
        let streams = inputs.iter().map(|input| input.to_stream()).collect();
        let start = dst.len();
        let result = self
            .engine
            .frame_inputs(&self.params, streams, &mut VecSink::new(dst));
        self.apply_retention();
        match result {
            Ok(()) => Ok(start..dst.len()),
            Err(error) => {
                dst.truncate(start);
                Err(EncodeError::from_core(error, usize::MAX))
            }
        }
    }

    /// Applies the retention policy after an operation.
    pub(crate) fn apply_retention(&mut self) {
        if !self.retention.allows(self.retained_bytes()) {
            self.trim(self.retention);
        }
    }

    /// Writes only the configured frame header for ordered parallel assembly.
    /// Resolving the same plan as compression preserves dictionary metadata.
    #[cfg(not(feature = "no_std"))]
    #[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
    pub(super) fn frame_header_into(
        &mut self,
        total: u64,
        dst: &mut Vec<u8>,
    ) -> Result<(), EncodeError> {
        self.sync_plan()?;
        match self
            .engine
            .header(&self.params, total, &mut VecSink::new(dst))
        {
            Ok(()) => Ok(()),
            Err(error) => Err(EncodeError::from_core(error, usize::MAX)),
        }
    }

    /// The resolved parameters of the next frame.
    pub(crate) const fn params(&self) -> &Params {
        &self.params
    }
}

/// Configures a [`Compressor`] beyond its [`EncoderConfig`].
///
/// # Examples
///
/// ```
/// use mopenzl::{Compressor, RetentionPolicy};
///
/// let encoder = Compressor::builder(Default::default())
///     .with_retention(RetentionPolicy::Bounded { max_bytes: 1 << 20 })
///     .build()?;
/// assert_eq!(encoder.retention(), RetentionPolicy::Bounded { max_bytes: 1 << 20 });
/// # Ok::<(), mopenzl::ConfigError>(())
/// ```
#[derive(Clone, Debug)]
pub struct CompressorBuilder {
    config: EncoderConfig,
    retention: RetentionPolicy,
    backend: Option<Backend>,
    graph: Option<CompressorGraph>,
}

impl CompressorBuilder {
    /// Sets what the compressor keeps allocated between frames.
    #[must_use]
    pub const fn with_retention(mut self, retention: RetentionPolicy) -> Self {
        self.retention = retention;
        self
    }

    /// Compresses through `graph` instead of the reference's default graph.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::graph::{CompressorGraph, StandardGraph};
    /// use mopenzl::{Compressor, EncoderConfig};
    ///
    /// let compressor = Compressor::builder(EncoderConfig::default())
    ///     .with_graph(CompressorGraph::standard(StandardGraph::Store))
    ///     .build()?;
    /// # let _ = compressor;
    /// # Ok::<(), mopenzl::ConfigError>(())
    /// ```
    #[must_use]
    pub fn with_graph(mut self, graph: CompressorGraph) -> Self {
        self.graph = Some(graph);
        self
    }

    /// Runs the kernels on `backend` instead of the host's best.
    ///
    /// Every backend writes the same bytes.
    #[must_use]
    pub const fn with_backend(mut self, backend: Backend) -> Self {
        self.backend = Some(backend);
        self
    }

    /// Builds the compressor.
    ///
    /// # Errors
    ///
    /// None today; see [`Compressor::new`].
    pub fn build(self) -> Result<Compressor, ConfigError> {
        Ok(Compressor::with_parts(
            self.config,
            self.retention,
            self.backend.unwrap_or_default(),
            self.graph,
        ))
    }
}

/// The capacity a new frame `Vec` starts with: the compression bound of
/// `input_size`, as C sizes its destination with `ZL_compressBound`, so the
/// frame is written without the reallocations and copies of a growing `Vec`.
/// Pages of the reservation that the frame never reaches are never touched.
/// Falls back to an empty `Vec` when the bound overflows; it then grows.
fn frame_capacity(input_size: usize) -> usize {
    Compressor::max_compressed_size(input_size).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_matches_the_reference_formula() {
        for size in [0usize, 1, 32_767, 32_768, 1 << 20] {
            assert_eq!(
                Compressor::max_compressed_size(size),
                Ok(openzl_ffi::compress_bound(size))
            );
        }
        assert_eq!(
            Compressor::max_compressed_size(usize::MAX),
            Err(SizeOverflow)
        );
        assert_eq!(
            Compressor::max_compressed_size(usize::MAX - 64),
            Err(SizeOverflow)
        );
    }

    #[test]
    fn slices_too_small_fail_and_vec_appends_roll_back() {
        let mut encoder = Compressor::default();
        let mut tiny = [0u8; 10];
        assert_eq!(
            encoder.compress_to_slice(b"payload", &mut tiny),
            Err(EncodeError::OutputTooSmall { provided: 10 })
        );
        let mut out = b"keep".to_vec();
        let range = encoder.compress_into(b"payload", &mut out).unwrap();
        assert_eq!(&out[..4], b"keep");
        assert_eq!(range.end, out.len());
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn reconfiguring_switches_the_next_frame() {
        let mut encoder = Compressor::default();
        assert_eq!(encoder.retention(), RetentionPolicy::Aggressive);
        let input: Vec<u8> = (0..30_000u32)
            .map(|i| b"abcdefgh"[(i.wrapping_mul(2_654_435_761) >> 29) as usize])
            .collect();
        let default = encoder.compress(&input).unwrap();
        let config = EncoderConfig::from(crate::Level::try_from(1).unwrap());
        encoder.reconfigure(config).unwrap();
        assert_eq!(encoder.config(), &config);
        let fast = encoder.compress(&input).unwrap();
        assert_ne!(fast, default);
        assert_eq!(
            fast,
            Compressor::new(config).unwrap().compress(&input).unwrap()
        );
    }

    #[test]
    fn release_all_keeps_nothing_after_an_operation() {
        let mut encoder = Compressor::builder(EncoderConfig::default())
            .with_retention(RetentionPolicy::ReleaseAll)
            .build()
            .unwrap();
        encoder.compress(&[7u8; 10_000]).unwrap();
        assert_eq!(encoder.retained_bytes(), 0);
        assert_eq!(encoder.params().level, 6);
    }

    #[test]
    fn new_frames_start_at_the_compression_bound() {
        assert_eq!(
            frame_capacity(1000),
            Compressor::max_compressed_size(1000).unwrap()
        );
        assert_eq!(
            frame_capacity(usize::MAX),
            0,
            "an overflowing bound grows instead"
        );
        let frame = Compressor::default().compress(&[3u8; 5000]).unwrap();
        assert!(frame.capacity() >= Compressor::max_compressed_size(5000).unwrap());
    }
}
