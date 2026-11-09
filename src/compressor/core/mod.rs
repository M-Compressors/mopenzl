//! The OpenZL encoder: frame headers, chunks and the session state machine.
//!
//! [`Engine`] owns the reusable Zstandard node and the scratch the chunk
//! header is assembled in, and the SIMD level selected once when the
//! compressor was built. Every checksum below it runs at that level.

pub(crate) mod chunk;
pub(crate) mod codecs;
pub(crate) mod data;
pub(crate) mod error;
pub(crate) mod exec;
pub(crate) mod frame;
pub(crate) mod graphs;
pub(crate) mod layout;
pub(crate) mod params;
pub(crate) mod registry;
pub(crate) mod rtgraph;
pub(crate) mod sink;
pub(crate) mod stream;
#[cfg(feature = "zstd")]
pub(crate) mod zstd;

use alloc::sync::Arc;
use alloc::vec::Vec;

use fearless_simd::Level;

use super::config::EncoderConfig;
use crate::{Backend, RetentionPolicy};
use data::Stream as DataStream;
use error::ExecError;
use registry::{GraphId, Registry, RuntimeParams};
use sink::{Full, Sink};
#[cfg(feature = "zstd")]
use zstd::ZstdNode;

/// The resolved global parameters of a frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Params {
    /// The wire-format version.
    pub(crate) version: u32,
    /// The compression level handed to the Zstandard node.
    pub(crate) level: i32,
    /// Chunks carry content checksums.
    pub(crate) content_checksum: bool,
    /// Chunks carry compressed checksums, the header a check byte.
    pub(crate) compressed_checksum: bool,
    /// The most input bytes one chunk takes.
    pub(crate) chunk_limit: usize,
    /// `ZL_CParam_decompressionLevel`.
    pub(crate) decompression_level: i32,
    /// `ZL_CParam_minStreamSize`.
    pub(crate) min_stream_size: i32,
    /// `ZL_CParam_storeOnExpansion`.
    pub(crate) store_on_expansion: bool,
    /// `ZL_CParam_permissiveCompression`.
    pub(crate) permissive: bool,
}

impl From<&EncoderConfig> for Params {
    fn from(config: &EncoderConfig) -> Self {
        Self {
            version: config.format_version().get(),
            level: config.level().get(),
            content_checksum: config.content_checksum(),
            compressed_checksum: config.compressed_checksum(),
            chunk_limit: config.chunking().limit(),
            decompression_level: i32::from(config.decompression_level()),
            min_stream_size: i32::try_from(config.min_stream_size()).unwrap_or(i32::MAX),
            store_on_expansion: config.store_on_expansion(),
            permissive: config.permissive(),
        }
    }
}

impl Params {
    /// Whether the specialized default-graph path writes the same bytes as
    /// the graph engine for these parameters: it stores below ten bytes,
    /// stores on expansion, and cannot set Zstandard's literal mode.
    pub(crate) const fn fast_path(&self) -> bool {
        self.min_stream_size == 10 && self.store_on_expansion && self.decompression_level != 1
    }
}

/// How the engine compresses when it is not on the specialized
/// default-graph path: the graphs, the starting graph and the comment.
#[derive(Clone, Debug)]
pub(crate) struct GraphPlan {
    /// The graphs.
    pub(crate) registry: Arc<Registry>,
    /// The graph the frame starts at.
    pub(crate) start: GraphId,
    /// Runtime parameters of the starting graph.
    pub(crate) start_params: Option<RuntimeParams>,
    /// The header comment.
    pub(crate) comment: Option<Arc<[u8]>>,
    /// Set when the start is a serial segmenter: `(chunk bytes, successor)`.
    /// Such frames are written incrementally, one chunk at a time; any other
    /// graph needs its whole input.
    pub(crate) segmented: Option<(usize, GraphId)>,
}

/// Why the core could not encode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CoreError {
    /// A graph failed.
    Graph(ExecError),
    /// The destination is full.
    OutputFull,
    /// The Zstandard backend failed, which it never should.
    #[cfg(feature = "zstd")]
    Internal,
}

impl From<ExecError> for CoreError {
    fn from(error: ExecError) -> Self {
        match error {
            ExecError::Full => Self::OutputFull,
            other => Self::Graph(other),
        }
    }
}

impl From<Full> for CoreError {
    fn from(_: Full) -> Self {
        Self::OutputFull
    }
}

/// Reusable encoding state.
#[derive(Debug)]
pub(crate) struct Engine {
    #[cfg(feature = "zstd")]
    zstd: ZstdNode,
    header: Vec<u8>,
    simd: Level,
    plan: Option<GraphPlan>,
}

impl Engine {
    pub(crate) fn new(params: &Params, backend: Backend) -> Self {
        #[cfg(not(feature = "zstd"))]
        let _ = params;
        Self {
            #[cfg(feature = "zstd")]
            zstd: ZstdNode::new(params.level, backend),
            header: Vec::new(),
            simd: backend.0,
            plan: None,
        }
    }

    /// Selects the graphs the next operation uses; `None` is the specialized
    /// default-graph path.
    pub(crate) fn set_plan(&mut self, plan: Option<GraphPlan>) {
        self.plan = plan;
    }

    /// Whether a frame can be written chunk by chunk as input arrives.
    pub(crate) const fn incremental(&self) -> bool {
        match &self.plan {
            None => true,
            Some(plan) => plan.segmented.is_some(),
        }
    }

    /// Adopts new parameters for the next frame.
    pub(crate) fn reconfigure(&mut self, params: &Params) {
        #[cfg(feature = "zstd")]
        self.zstd.set_level(params.level);
        #[cfg(not(feature = "zstd"))]
        let _ = params;
    }

    /// Writes a frame header announcing `total` input bytes.
    pub(crate) fn header(
        &self,
        params: &Params,
        total: u64,
        sink: &mut impl Sink,
    ) -> Result<(), CoreError> {
        if let Some(plan) = &self.plan {
            let desc = layout::InputDesc {
                ty: data::Type::Serial,
                size: usize::try_from(total).map_err(|_| ExecError::Limit("input size"))?,
                n_elts: 0,
            };
            layout::write_frame_header(
                params,
                self.simd,
                &[desc],
                plan.comment.as_deref(),
                plan.registry.dicts.as_ref().map(|state| &state.id),
                sink,
            )?;
            return Ok(());
        }
        Ok(frame::write_header(params, self.simd, total, sink)?)
    }

    /// Compresses one chunk.
    pub(crate) fn chunk(
        &mut self,
        params: &Params,
        input: &[u8],
        sink: &mut impl Sink,
    ) -> Result<(), CoreError> {
        if let Some(plan) = self.plan.clone() {
            return self.chunk_graph(&plan, params, input, sink);
        }
        chunk::write_chunk(self, params, input, sink)
    }

    /// One chunk of a segmented frame: what `ZL_Segmenter_processChunk` does.
    fn chunk_graph(
        &mut self,
        plan: &GraphPlan,
        params: &Params,
        input: &[u8],
        sink: &mut impl Sink,
    ) -> Result<(), CoreError> {
        let (_, successor) = plan.segmented.ok_or(ExecError::GraphInvalid)?;
        let resources = self.resources();
        let stream = DataStream::serial(input);
        let mut exec = exec::Exec::new(
            &plan.registry,
            *params,
            resources,
            None,
            alloc::vec![stream.clone()],
            sink,
        );
        exec.segmenter_depth = 1;
        let id = exec.rt.ref_input(stream.clone());
        exec.run_successor(successor, None, &[id], 2)?;
        exec.flush_chunk(&[stream])?;
        Ok(())
    }

    fn resources(&mut self) -> exec::Resources<'_> {
        exec::Resources {
            #[cfg(feature = "zstd")]
            zstd: &mut self.zstd,
            simd: self.simd,
            #[cfg(not(feature = "zstd"))]
            marker: core::marker::PhantomData,
        }
    }

    /// Compresses a whole frame of `input`, chunked as the segmenter would.
    pub(crate) fn frame(
        &mut self,
        params: &Params,
        input: &[u8],
        sink: &mut impl Sink,
    ) -> Result<(), CoreError> {
        if self.plan.is_some() {
            return self.frame_inputs(params, alloc::vec![DataStream::serial(input)], sink);
        }
        self.header(params, input.len() as u64, sink)?;
        if input.is_empty() {
            self.chunk(params, input, sink)?;
        }
        for piece in input.chunks(params.chunk_limit) {
            self.chunk(params, piece, sink)?;
        }
        Ok(frame::write_end(sink)?)
    }

    /// Bytes kept allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        #[cfg(feature = "zstd")]
        let codecs = self.zstd.retained_bytes();
        #[cfg(not(feature = "zstd"))]
        let codecs = 0;
        codecs + self.header.capacity()
    }

    /// Compresses a frame of `inputs` through the graphs of the plan (the
    /// default `compress_generic` when none is set).
    ///
    /// Writes into `sink`; on error the sink may hold a partial frame.
    pub(crate) fn frame_inputs<'a>(
        &mut self,
        params: &Params,
        inputs: Vec<DataStream<'a>>,
        sink: &mut dyn Sink,
    ) -> Result<(), CoreError> {
        let plan = self.plan.clone().unwrap_or_else(|| GraphPlan {
            registry: Arc::new(Registry::new(0, registry::gid::COMPRESS_GENERIC)),
            start: registry::gid::COMPRESS_GENERIC,
            start_params: None,
            comment: None,
            segmented: None,
        });
        let resources = self.resources();
        let mut exec = exec::Exec::new(
            &plan.registry,
            *params,
            resources,
            plan.comment.as_deref(),
            inputs,
            sink,
        );
        exec.compress_frame(plan.start, plan.start_params.as_ref())?;
        Ok(())
    }

    /// Releases what `policy` does not allow to keep.
    pub(crate) fn trim(&mut self, policy: RetentionPolicy) {
        #[cfg(feature = "zstd")]
        self.zstd.trim(policy);
        if !policy.allows(self.retained_bytes()) {
            self.header = Vec::new();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sink::{SliceSink, VecSink};
    use super::*;

    #[cfg(feature = "zstd")]
    #[test]
    fn whole_frames_split_at_the_chunk_limit() {
        let mut params = Params::from(&EncoderConfig::default());
        params.chunk_limit = 32 << 10;
        let input = alloc::vec![3u8; (64 << 10) + 1];
        let mut engine = Engine::new(&params, Backend::default());
        let mut out = Vec::new();
        engine
            .frame(&params, &input, &mut VecSink::new(&mut out))
            .unwrap();
        let expected = openzl_ffi::compress(
            &input,
            openzl_ffi::Params {
                chunk_size: 32 << 10,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out, expected);
    }

    #[test]
    fn a_full_slice_reports_output_full() {
        let params = Params::from(&EncoderConfig::default());
        let mut engine = Engine::new(&params, Backend::default());
        let mut buffer = [0u8; 12];
        let error = engine.frame(
            &params,
            b"0123456789abcdef",
            &mut SliceSink::new(&mut buffer),
        );
        assert_eq!(error, Err(CoreError::OutputFull));
        assert_eq!(CoreError::from(Full), CoreError::OutputFull);
    }

    #[test]
    fn trimming_releases_scratch() {
        let params = Params::from(&EncoderConfig::default());
        let mut engine = Engine::new(&params, Backend::default());
        let mut out = Vec::new();
        engine
            .frame(&params, &[1u8; 5000], &mut VecSink::new(&mut out))
            .unwrap();
        assert!(engine.retained_bytes() > 0);
        engine.trim(RetentionPolicy::ReleaseAll);
        assert_eq!(engine.header.capacity(), 0);
    }
}
