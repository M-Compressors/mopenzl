//! Checked planning, exclusive workers, completion slots, and ordered assembly.
mod artifact;
mod batch;
pub(in crate::compressor::parallel) mod source;
mod spool;
mod stream;
mod task;
use super::*;
mod fragment;
use crate::compressor::Compressor as SerialCompressor;
use crate::graph::CompressorGraph;
pub(in crate::compressor::parallel) use batch::Batch;
use fragment::FragmentEncoder;
pub(in crate::compressor::parallel) use stream::{
    BorrowedStream, OwnedStream, QueueHandle, Stream,
};
pub(in crate::compressor::parallel) use task::Task;

pub(in crate::compressor::parallel) const fn staging_memory_bound(
    payload: u64,
    segments: u64,
) -> Result<u64, ParallelEncodeError> {
    let Some(descriptors) = segments.checked_mul(size_of::<artifact::Descriptor>() as u64) else {
        return Err(ParallelEncodeError::SizeOverflow);
    };
    match payload.checked_add(descriptors) {
        Some(bytes) => Ok(bytes),
        None => Err(ParallelEncodeError::SizeOverflow),
    }
}

#[derive(Clone)]
pub(in crate::compressor::parallel) enum Input<'a> {
    Slice(&'a [u8]),
    Source(Arc<dyn RandomAccessSource>),
}
impl Input<'_> {
    fn len(&self) -> Result<u64, ParallelEncodeError> {
        match self {
            Self::Slice(s) => Ok(s.len() as u64),
            Self::Source(s) => s.len().map_err(ParallelEncodeError::SourceMetadata),
        }
    }
    fn identity(&self) -> Option<SourceIdentity> {
        match self {
            Self::Slice(_) => None,
            Self::Source(s) => s.identity(),
        }
    }
}

/// Disabled codecs must store chunks even when generic encoder settings would
/// otherwise select an unavailable node. Envelope parameters are unchanged.
fn serial_compressor(
    config: EncoderConfig,
    backend: Backend,
    graph: Option<&CompressorGraph>,
) -> Result<SerialCompressor, crate::ConfigError> {
    #[cfg(not(feature = "zstd"))]
    let config = if graph.is_none() {
        config
            .with_min_stream_size(10)
            .with_store_on_expansion(true)
            .with_decompression_level(0)
    } else {
        config
    };
    let builder = SerialCompressor::builder(config).with_backend(backend);
    match graph {
        Some(graph) => builder.with_graph(graph.clone()).build(),
        None => builder.build(),
    }
}

pub(in crate::compressor::parallel) struct Worker {
    fragment: FragmentEncoder,
    serial: SerialCompressor,
    input: Vec<u8>,
}
impl Worker {
    fn new(
        config: EncoderConfig,
        backend: Backend,
        graph: Option<&CompressorGraph>,
    ) -> Result<Self, ParallelConfigError> {
        Ok(Self {
            fragment: FragmentEncoder::new(config, backend, graph)?,
            serial: serial_compressor(config, backend, graph)?,
            input: Vec::new(),
        })
    }
    fn retained_bytes(&self) -> usize {
        self.fragment.retained_bytes() + self.serial.retained_bytes() + self.input.capacity()
    }
}
pub(in crate::compressor::parallel) struct Compressor {
    pub(in crate::compressor::parallel) encoder: EncoderConfig,
    pub(in crate::compressor::parallel) parallel: ParallelConfig,
    backend: Backend,
    pub(in crate::compressor::parallel) graph: Option<CompressorGraph>,
    pub(in crate::compressor::parallel) workers: Vec<Worker>,
}
impl Compressor {
    pub(in crate::compressor::parallel) fn new(
        encoder: EncoderConfig,
        parallel: ParallelConfig,
        backend: Backend,
    ) -> Result<Self, ParallelConfigError> {
        let mut compressor = Self {
            encoder,
            parallel,
            backend,
            graph: None,
            workers: Vec::new(),
        };
        compressor.reconfigure(encoder)?;
        Ok(compressor)
    }
    pub(in crate::compressor::parallel) fn reconfigure(
        &mut self,
        encoder: EncoderConfig,
    ) -> Result<(), ParallelConfigError> {
        if self.encoder != encoder {
            self.workers = Vec::new();
            self.encoder = encoder;
        }
        Ok(())
    }
    /// Uses the serial engine's header writer, including the selected bundle ID.
    fn frame_header(&self, total: u64) -> Result<Vec<u8>, ParallelEncodeError> {
        let mut encoder = serial_compressor(self.encoder, self.backend, self.graph.as_ref())
            .map_err(ParallelConfigError::from)?;
        let mut bytes = Vec::new();
        encoder
            .frame_header_into(total, &mut bytes)
            .map_err(ParallelEncodeError::FrameHeader)?;
        Ok(bytes)
    }
    pub(in crate::compressor::parallel) fn retained_bytes(&self) -> usize {
        self.workers
            .iter()
            .map(Worker::retained_bytes)
            .sum::<usize>()
            + self.workers.capacity() * size_of::<Worker>()
    }
    pub(in crate::compressor::parallel) fn trim(&mut self, policy: ParallelRetentionPolicy) {
        match policy {
            ParallelRetentionPolicy::ReleaseAll => self.workers = Vec::new(),
            ParallelRetentionPolicy::Aggressive => (),
            ParallelRetentionPolicy::CurrentPlan => {
                self.workers.truncate(self.parallel.max_retained_workers);
                self.workers.shrink_to_fit();
            }
            ParallelRetentionPolicy::Bounded { max_bytes } => {
                self.workers.truncate(self.parallel.max_retained_workers);
                while self.retained_bytes() > max_bytes && !self.workers.is_empty() {
                    self.workers.pop();
                }
                self.workers.shrink_to_fit();
            }
        }
    }
    /// OpenZL chunks are independent and never reference previous input.
    pub(in crate::compressor::parallel) const fn history_len(&self) -> usize {
        0
    }
    /// Conservative estimate for the Zstandard workspace and retained buffers.
    fn worker_estimate(&self) -> Result<usize, ParallelEncodeError> {
        self.parallel
            .segment_size
            .get()
            .checked_mul(64)
            .and_then(|n| n.checked_add(128 << 20))
            .ok_or(ParallelEncodeError::SizeOverflow)
    }
    pub(in crate::compressor::parallel) fn plan(
        &self,
        len: u64,
        config: &BatchConfig,
    ) -> Result<Plan, ParallelEncodeError> {
        let z = self.parallel.segment_size.get() as u64;
        let segments = len / z + u64::from(!len.is_multiple_of(z));
        let tasks = usize::try_from(segments.min(config.task_count.get() as u64))
            .map_err(|_| ParallelEncodeError::SizeOverflow)?;
        let serial = len < self.parallel.minimum_parallel_size && len <= z || len == 0;
        // Twice the raw bytes plus 1 KiB per part covers prefix, block headers,
        // flush alignment and the serial encoder's uncompressed fallback bound.
        let staged = len
            .checked_mul(2)
            .and_then(|n| {
                segments
                    .max(1)
                    .checked_mul(1024)
                    .and_then(|s| n.checked_add(s))
            })
            .ok_or(ParallelEncodeError::SizeOverflow)?;
        let descriptors = segments
            .checked_mul(size_of::<artifact::Descriptor>() as u64)
            .ok_or(ParallelEncodeError::SizeOverflow)?;
        let segment = self.parallel.segment_size.get();
        let per_worker = self.worker_estimate()?;
        let metadata =
            usize::try_from(descriptors).map_err(|_| ParallelEncodeError::SizeOverflow)?;
        let active = per_worker
            .checked_mul(tasks)
            .and_then(|n| n.checked_add(metadata))
            .and_then(|n| tasks.checked_mul(4096).and_then(|t| n.checked_add(t)))
            .and_then(|n| n.checked_add(128 << 10))
            .ok_or(ParallelEncodeError::SizeOverflow)?;
        let staging_memory = match &config.staging {
            Staging::Memory(m) => {
                let bytes =
                    usize::try_from(staged).map_err(|_| ParallelEncodeError::SizeOverflow)?;
                let required = usize::try_from(staging_memory_bound(staged, segments)?)
                    .map_err(|_| ParallelEncodeError::SizeOverflow)?;
                if required > m.max_total_bytes {
                    return Err(ParallelEncodeError::MemoryStagingLimit);
                }
                bytes
            }
            Staging::Directory(_) => 0,
        };
        let aggregate = active
            .checked_add(staging_memory)
            .and_then(|n| n.checked_add(self.retained_bytes()))
            .ok_or(ParallelEncodeError::SizeOverflow)?;
        if self
            .parallel
            .aggregate_memory_limit
            .is_some_and(|limit| aggregate > limit)
        {
            return Err(ParallelEncodeError::WorkerMemoryLimit);
        }
        Ok(Plan {
            tasks,
            serial,
            segment_size: segment,
            history: if serial { 0 } else { self.history_len() as u64 },
            estimate: ParallelSizeEstimate {
                input_bytes: len,
                segment_count: segments,
                maximum_staged_bytes: staged,
                maximum_final_bytes: staged
                    .checked_add(64)
                    .ok_or(ParallelEncodeError::SizeOverflow)?,
                estimated_active_workspace_bytes: active,
            },
        })
    }
}
pub(in crate::compressor::parallel) struct Plan {
    pub(in crate::compressor::parallel) tasks: usize,
    serial: bool,
    segment_size: usize,
    /// Input before each segment that its part may copy from.
    history: u64,
    pub(in crate::compressor::parallel) estimate: ParallelSizeEstimate,
}
impl Plan {
    /// Where the history of the segment starting at `start` begins.
    const fn history_start(&self, start: u64) -> u64 {
        start.saturating_sub(self.history)
    }
    fn source_range(&self, segment: u64) -> Result<Range<u64>, ParallelEncodeError> {
        let start = segment
            .checked_mul(self.segment_size as u64)
            .ok_or(ParallelEncodeError::SizeOverflow)?;
        let end = start
            .saturating_add(self.segment_size as u64)
            .min(self.estimate.input_bytes);
        if start >= end {
            return Err(ParallelEncodeError::FragmentInvariant(
                "invalid source range",
            ));
        }
        Ok(start..end)
    }
}
