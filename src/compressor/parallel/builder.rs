//! Graph and backend selection for a caller-scheduled compressor.
use super::{ParallelCompressor, ParallelConfig, ParallelConfigError, core};
use crate::graph::CompressorGraph;
use crate::{Backend, EncoderConfig};

/// Configures a [`ParallelCompressor`] with the same graph selection as the
/// serial [`CompressorBuilder`](crate::CompressorBuilder).
///
/// A graph receives each independent segment as serial input. It may use
/// standard nodes, application codecs, selectors, segmenters and dictionaries.
/// Callbacks can run concurrently and must not depend on execution order.
/// Graph errors propagate from task execution, as serial encoding errors do.
///
/// # Examples
/// ```
/// use mopenzl::compressor::parallel::{ParallelCompressor, ParallelConfig};
/// use mopenzl::graph::{CompressorGraph, StandardGraph};
/// let compressor = ParallelCompressor::builder(Default::default(), ParallelConfig::default())
///     .with_graph(CompressorGraph::standard(StandardGraph::Store))
///     .build()?;
/// # let _ = compressor;
/// # Ok::<(), mopenzl::compressor::parallel::ParallelConfigError>(())
/// ```
#[derive(Clone, Debug)]
pub struct ParallelCompressorBuilder {
    pub(super) encoder: EncoderConfig,
    pub(super) parallel: ParallelConfig,
    pub(super) backend: Option<Backend>,
    pub(super) graph: Option<CompressorGraph>,
}
impl ParallelCompressorBuilder {
    /// Selects the graph applied to every segment, including serial fallback.
    /// Codec nodes and local codec parameters are selected through
    /// [`GraphBuilder`](crate::graph::GraphBuilder), as for the serial builder.
    #[must_use]
    pub fn with_graph(mut self, graph: CompressorGraph) -> Self {
        self.graph = Some(graph);
        self
    }
    /// Pins every worker to a host-validated backend.
    #[must_use]
    pub const fn with_backend(mut self, backend: Backend) -> Self {
        self.backend = Some(backend);
        self
    }
    /// Builds a planner with lazy worker allocation.
    ///
    /// # Errors
    /// Returns underlying encoder configuration errors. Graph execution and
    /// input-type compatibility are checked when a task compresses its input.
    pub fn build(self) -> Result<ParallelCompressor, ParallelConfigError> {
        let mut inner = core::Compressor::new(
            self.encoder,
            self.parallel,
            self.backend.unwrap_or_default(),
        )?;
        inner.graph = self.graph;
        Ok(ParallelCompressor { inner })
    }
}
