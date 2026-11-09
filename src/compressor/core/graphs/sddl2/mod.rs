//! The SDDL2 graphs: the `sddl2` segmenter (`ZL_GRAPH_SDDL2`, graph 21,
//! `SDDL2_segment`) and the private `sddl2_chunk` replay graph (61,
//! `SDDL2_replayChunk`) of `compress/graphs/sddl2/sddl2.c`.
//!
//! # Segmenter
//!
//! 1. The [`vm`] runs the bytecode (copy parameter [`BYTECODE_PARAM`]) once
//!    over the whole input and produces a list of typed, tagged segments. A
//!    program that creates none sends the whole input to `store`.
//! 2. The segments are packed into chunks of at most the chunk size
//!    ([`CHUNK_BYTE_SIZE_PARAM`], default 16 MiB; unlimited below format
//!    version 21): a segment that would overflow the current chunk closes it,
//!    and a segment larger than the chunk size is split on its element
//!    boundaries (the size of one structure for a structure segment).
//! 3. Every chunk goes to the replay graph with the chunk-local segment list
//!    (reference parameter [`REPLAY_SEGMENTS_PARAM`]), the first stream id
//!    ([`REPLAY_START_STREAM_ID_PARAM`]) and the destination as its custom
//!    graph (`compress_generic` unless the segmenter has one).
//!
//! # Replay
//!
//! The replay graph cuts its serial input at the segment boundaries
//! (`splitN`), then for each segment: a `bytes` segment goes to the
//! destination as it is; a primitive segment is converted to numbers of its
//! width and endianness first; a structure segment is split by field
//! (`split_by_struct`) and each field converted to numbers (a `bytes` field
//! stays a struct stream). Each routed stream carries the integer metadata
//! `0 =` a running stream id (the clustering tag); zero-size segments consume
//! ids without a stream.
//!
//! The bytecode disassembler of the reference is debug-only and not ported.

mod vm;

use alloc::sync::Arc;
use alloc::vec::Vec;

use self::vm::{Kind, Type, VmError};
use super::super::error::ExecError;
use super::super::exec::{Edge, GraphCx, SegmenterCx};
use super::super::params::LocalParams;
use super::super::registry::{GraphFn, GraphId, NodeId, RuntimeParams, SegmentFn, gid};
use super::run_split_node;
use super::segmenters::DEFAULT_CHUNK_BYTE_SIZE;

/// `SDDL2_BYTECODE_PARAM`: the copy parameter holding the bytecode.
pub(crate) const BYTECODE_PARAM: i32 = crate::graph::params::sddl2::BYTECODE;
/// `SDDL2_CHUNK_BYTE_SIZE_PARAM`: the integer chunk size parameter.
pub(crate) const CHUNK_BYTE_SIZE_PARAM: i32 = crate::graph::params::sddl2::CHUNK_BYTE_SIZE;
/// `SDDL2_REPLAY_SEGMENTS_PARAM`: the reference parameter of the chunk
/// graph, an `Arc<Vec<ReplaySegment>>`.
const REPLAY_SEGMENTS_PARAM: i32 = 7687;
/// `SDDL2_REPLAY_START_STREAM_ID_PARAM`.
const REPLAY_START_STREAM_ID_PARAM: i32 = 7688;
/// `ZL_SPLITBYSTRUCT_FIELDSIZES_PID`.
const SPLIT_BY_STRUCT_FIELD_SIZES_PID: i32 = crate::graph::params::split_by_struct::FIELD_SIZES;
/// `ZL_CLUSTERING_TAG_METADATA_ID`.
const CLUSTERING_TAG_METADATA_ID: i32 = crate::graph::params::clustering::TAG_METADATA_ID;
/// `ZL_GRAPH_SDDL2_CHUNK`.
const GRAPH_SDDL2_CHUNK: GraphId = GraphId::std(61);
/// `ZL_NODE_SPLIT_BY_STRUCT`.
const NODE_SPLIT_BY_STRUCT: NodeId = NodeId::std(69);
/// `ZL_NODE_CONVERT_STRUCT_TO_NUM_LE`.
const NODE_CONVERT_STRUCT_TO_NUM_LE: NodeId = NodeId::std(17);
/// `ZL_NODE_CONVERT_STRUCT_TO_NUM_BE`.
const NODE_CONVERT_STRUCT_TO_NUM_BE: NodeId = NodeId::std(18);
/// `ZL_CHUNK_VERSION_MIN`: older formats cannot be segmented.
const CHUNK_VERSION_MIN: u32 = 21;
/// `INT_MAX`.
const INT_MAX: u32 = i32::MAX as u32;

/// `SDDL2_segment`.
#[derive(Debug)]
pub(crate) struct Sddl2;

/// See [`Sddl2`].
pub(crate) static SDDL2: Sddl2 = Sddl2;

/// `SDDL2_replayChunk`.
#[derive(Debug)]
pub(crate) struct Sddl2Chunk;

/// See [`Sddl2Chunk`].
pub(crate) static SDDL2_CHUNK: Sddl2Chunk = Sddl2Chunk;

/// `SDDL2_ReplaySegment`: what the replay graph needs of a segment.
#[derive(Clone, Debug)]
struct ReplaySegment {
    size_bytes: usize,
    ty: Type,
}

/// The message of a VM failure, as the reference reports it.
const fn vm_message(error: VmError) -> &'static str {
    match error {
        VmError::InvalidBytecode => "SDDL2 bytecode is malformed or contains invalid instructions",
        VmError::StackOverflow => "SDDL2 VM stack overflow",
        VmError::StackUnderflow => "SDDL2 VM stack underflow",
        VmError::MathOverflow => "SDDL2 VM mathematical operation overflows",
        VmError::TypeMismatch => "SDDL2 VM type error: operation received incompatible values",
        VmError::LoadBounds => "SDDL2 VM attempted to load data beyond the input",
        VmError::SegmentBounds => "SDDL2 VM segment extends beyond the input",
        VmError::LimitExceeded => "SDDL2 VM capacity limit exceeded",
        VmError::DivZero => "SDDL2 VM division by zero",
        VmError::ValidationFailed => "SDDL2 VM validation failed: expect_true condition not met",
    }
}

impl From<VmError> for ExecError {
    fn from(error: VmError) -> Self {
        Self::Parameter(vm_message(error))
    }
}

/// `sddl2_try_count_primitive_fields`: the primitive fields of a type, or
/// `None` when a structure has no data or the count overflows.
fn count_primitive_fields(ty: &Type) -> Option<usize> {
    if ty.kind != Kind::Structure {
        return Some(1);
    }
    ty.struct_data
        .as_ref()?
        .members
        .iter()
        .try_fold(0usize, |total, member| {
            total.checked_add(count_primitive_fields(member)?)
        })
}

/// `sddl2_determine_endianness`: whether a primitive kind is little endian
/// (one-byte kinds count as little endian).
fn is_little_endian(kind: Kind) -> Result<bool, ExecError> {
    match kind {
        Kind::U8
        | Kind::I8
        | Kind::F8
        | Kind::U16Le
        | Kind::I16Le
        | Kind::U32Le
        | Kind::I32Le
        | Kind::U64Le
        | Kind::I64Le
        | Kind::F16Le
        | Kind::Bf16Le
        | Kind::F32Le
        | Kind::F64Le => Ok(true),
        Kind::U16Be
        | Kind::I16Be
        | Kind::U32Be
        | Kind::I32Be
        | Kind::U64Be
        | Kind::I64Be
        | Kind::F16Be
        | Kind::Bf16Be
        | Kind::F32Be
        | Kind::F64Be => Ok(false),
        Kind::Bytes => Err(ExecError::Parameter("bytes cannot be converted to numbers")),
        Kind::Structure => Err(ExecError::Parameter(
            "a structure cannot be converted to numbers",
        )),
    }
}

/// `sddl2_flatten_field_sizes` and `sddl2_flatten_field_types`: the sizes and
/// kinds of the primitive fields of a structure, nested structures expanded
/// in place. Widths of nested structures are ignored, as in the reference.
fn flatten(ty: &Type, sizes: &mut Vec<usize>, kinds: &mut Vec<Kind>) -> Result<(), ExecError> {
    if ty.kind == Kind::Structure {
        let data = ty
            .struct_data
            .as_ref()
            .ok_or(ExecError::Parameter("a structure type has no data"))?;
        for member in &data.members {
            flatten(member, sizes, kinds)?;
        }
    } else {
        sizes.push(
            ty.size()
                .map_err(|_| ExecError::Parameter("cannot compute the size of a field"))?,
        );
        kinds.push(ty.kind);
    }
    Ok(())
}

// ----- the chunk replay graph -----

/// `sddl2_set_next_stream_tag`.
fn set_next_stream_tag(
    cx: &mut GraphCx<'_, '_, '_>,
    edge: Edge,
    next_stream_id: &mut u32,
) -> Result<(), ExecError> {
    if *next_stream_id > INT_MAX {
        return Err(ExecError::Parameter(
            "SDDL2 clustering tag exceeded int metadata capacity",
        ));
    }
    let tag = *next_stream_id as i32;
    *next_stream_id += 1;
    cx.set_metadata(edge, CLUSTERING_TAG_METADATA_ID, tag)
}

/// `sddl2_apply_struct_field_conversion`.
fn convert_struct_field(
    cx: &mut GraphCx<'_, '_, '_>,
    edge: Edge,
    kind: Kind,
) -> Result<Edge, ExecError> {
    let node = if is_little_endian(kind)? {
        NODE_CONVERT_STRUCT_TO_NUM_LE
    } else {
        NODE_CONVERT_STRUCT_TO_NUM_BE
    };
    let out = cx.run_node(node, &[edge], None)?;
    let [converted] = out[..] else {
        return Err(ExecError::Parameter(
            "struct to numeric conversion should produce one edge",
        ));
    };
    Ok(converted)
}

/// `sddl2_apply_structure_split`.
fn split_structure(
    cx: &mut GraphCx<'_, '_, '_>,
    edge: Edge,
    ty: &Type,
    dest: GraphId,
    next_stream_id: &mut u32,
) -> Result<(), ExecError> {
    if ty.kind != Kind::Structure {
        return Err(ExecError::Parameter("expected a structure type"));
    }
    let total = count_primitive_fields(ty).ok_or(ExecError::Parameter(
        "failed to count primitive fields for an SDDL2 type",
    ))?;
    if total == 0 {
        return Err(ExecError::Parameter(
            "a structure has no valid primitive fields",
        ));
    }
    let mut sizes = Vec::with_capacity(total);
    let mut kinds = Vec::with_capacity(total);
    flatten(ty, &mut sizes, &mut kinds)?;
    if sizes.len() != total {
        return Err(ExecError::Parameter("field count mismatch"));
    }
    let nonzero: Vec<usize> = sizes.iter().copied().filter(|&size| size > 0).collect();
    let field_sizes: Vec<u8> = nonzero.iter().flat_map(|size| size.to_ne_bytes()).collect();
    let params = LocalParams {
        copies: alloc::vec![(SPLIT_BY_STRUCT_FIELD_SIZES_PID, Arc::from(field_sizes))],
        ..LocalParams::EMPTY
    };
    let outputs = cx.run_node(NODE_SPLIT_BY_STRUCT, &[edge], Some(&params))?;
    if outputs.len() != nonzero.len() {
        return Err(ExecError::Parameter(
            "split-by-struct produced an unexpected number of edges",
        ));
    }
    let mut outputs = outputs.into_iter();
    for (&size, &kind) in sizes.iter().zip(&kinds) {
        if size == 0 {
            *next_stream_id += 1;
            continue;
        }
        let mut field_edge = outputs.next().ok_or(ExecError::GraphInvalid)?;
        if kind != Kind::Bytes {
            field_edge = convert_struct_field(cx, field_edge, kind)?;
        }
        set_next_stream_tag(cx, field_edge, next_stream_id)?;
        cx.set_destination(&[field_edge], dest, None)?;
    }
    Ok(())
}

/// `ZL_Node_convertSerialToNum{LE,BE}` for the primitive widths.
fn serial_to_num_node(bits: usize, little_endian: bool) -> Result<NodeId, ExecError> {
    let index = match (bits, little_endian) {
        (8, _) => 19,
        (16, true) => 20,
        (32, true) => 21,
        (64, true) => 22,
        (16, false) => 23,
        (32, false) => 24,
        (64, false) => 25,
        _ => return Err(ExecError::NodeInvalid),
    };
    Ok(NodeId::std(index))
}

/// `sddl2_apply_type_conversion`.
fn convert_segment(cx: &mut GraphCx<'_, '_, '_>, edge: Edge, ty: &Type) -> Result<Edge, ExecError> {
    let element_size = ty.kind.size().ok_or(ExecError::Parameter(
        "invalid SDDL2 type kind for a segment",
    ))?;
    let node = serial_to_num_node(element_size * 8, is_little_endian(ty.kind)?)?;
    let out = cx.run_node(node, &[edge], None)?;
    let [converted] = out[..] else {
        return Err(ExecError::Parameter(
            "type conversion should produce one edge",
        ));
    };
    Ok(converted)
}

/// `sddl2_process_segment`.
fn process_segment(
    cx: &mut GraphCx<'_, '_, '_>,
    edge: Edge,
    ty: &Type,
    dest: GraphId,
    next_stream_id: &mut u32,
) -> Result<(), ExecError> {
    match ty.kind {
        Kind::Bytes => {
            set_next_stream_tag(cx, edge, next_stream_id)?;
            cx.set_destination(&[edge], dest, None)
        }
        Kind::Structure => split_structure(cx, edge, ty, dest, next_stream_id),
        _ => {
            let edge = convert_segment(cx, edge, ty)?;
            set_next_stream_tag(cx, edge, next_stream_id)?;
            cx.set_destination(&[edge], dest, None)
        }
    }
}

/// `sddl2_extract_single_destination`.
fn single_destination(custom_graphs: &[GraphId]) -> Result<GraphId, ExecError> {
    match custom_graphs {
        [] => Ok(gid::COMPRESS_GENERIC),
        [dest] => Ok(*dest),
        _ => Err(ExecError::Parameter(
            "SDDL2 supports at most one custom graph",
        )),
    }
}

/// `sddl2_replay_segments`.
fn replay_segments(
    cx: &mut GraphCx<'_, '_, '_>,
    input: Edge,
    segments: &[ReplaySegment],
) -> Result<(), ExecError> {
    if segments.is_empty() {
        return cx.set_destination(&[input], gid::STORE, None);
    }
    let sizes: Vec<usize> = segments
        .iter()
        .map(|segment| segment.size_bytes)
        .filter(|&size| size > 0)
        .collect();
    let outputs = run_split_node(cx, input, &sizes)?;
    if outputs.len() != sizes.len() {
        return Err(ExecError::Parameter(
            "SDDL2 replay split produced an unexpected number of edges",
        ));
    }
    let dest = single_destination(cx.eff.custom_graphs)?;
    let mut next_stream_id = match cx.eff.params.int(REPLAY_START_STREAM_ID_PARAM) {
        None => 0,
        Some(id) if id < 0 => {
            return Err(ExecError::Parameter(
                "SDDL2 replay start stream id must be non-negative",
            ));
        }
        Some(id) => id as u32,
    };
    let mut outputs = outputs.into_iter();
    for segment in segments {
        if segment.size_bytes == 0 {
            let fields = count_primitive_fields(&segment.ty).ok_or(ExecError::Parameter(
                "failed to count primitive fields for an SDDL2 type",
            ))?;
            next_stream_id = next_stream_id.wrapping_add(fields as u32);
            continue;
        }
        let edge = outputs.next().ok_or(ExecError::GraphInvalid)?;
        process_segment(cx, edge, &segment.ty, dest, &mut next_stream_id)?;
    }
    Ok(())
}

impl GraphFn for Sddl2Chunk {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let [input] = inputs else {
            return Err(ExecError::Parameter("sddl2 replay takes exactly one input"));
        };
        let params = cx.eff.params;
        let segments = params
            .reference(REPLAY_SEGMENTS_PARAM)
            .and_then(|object| object.downcast_ref::<Vec<ReplaySegment>>())
            .ok_or(ExecError::Parameter(
                "SDDL2 replay requires a segments parameter",
            ))?;
        replay_segments(cx, *input, segments)
    }
}

// ----- the segmenter -----

/// A chunk being filled (`SDDL2_ChunkAccumulator`).
struct Chunk {
    segments: Vec<ReplaySegment>,
    capacity: usize,
    bytes: usize,
    start_stream_id: u32,
}

/// `SDDL2_ChunkPlanner`.
struct Planner {
    destination: GraphId,
    remaining_input: usize,
    chunk_limit: usize,
    next_stream_id: u32,
    chunk: Chunk,
}

impl Planner {
    /// `sddl2_append_chunk_replay_slice`.
    fn append(&mut self, stream_id: u32, ty: &Type, slice_bytes: usize) -> Result<(), ExecError> {
        let chunk = &mut self.chunk;
        let bytes = chunk
            .bytes
            .checked_add(slice_bytes)
            .ok_or(ExecError::Limit("SDDL2 chunk byte count"))?;
        if chunk.segments.is_empty() {
            chunk.start_stream_id = stream_id;
        }
        if chunk.segments.len() == chunk.capacity {
            return Err(ExecError::Limit("SDDL2 chunk replay-slice buffer"));
        }
        chunk.segments.push(ReplaySegment {
            size_bytes: slice_bytes,
            ty: ty.clone(),
        });
        chunk.bytes = bytes;
        Ok(())
    }

    /// `sddl2_flush_chunk_segments` and `sddl2_process_segment_chunk`.
    fn flush(&mut self, cx: &mut SegmenterCx<'_, '_, '_>) -> Result<(), ExecError> {
        if self.chunk.segments.is_empty() {
            return Ok(());
        }
        if self.chunk.start_stream_id > INT_MAX {
            return Err(ExecError::Limit("SDDL2 chunk stream id"));
        }
        let params = RuntimeParams {
            params: Some(LocalParams {
                ints: alloc::borrow::Cow::Owned(alloc::vec![(
                    REPLAY_START_STREAM_ID_PARAM,
                    self.chunk.start_stream_id as i32
                )]),
                refs: alloc::vec![(
                    REPLAY_SEGMENTS_PARAM,
                    super::super::params::RefParam(Arc::new(self.chunk.segments.clone()))
                )],
                ..LocalParams::EMPTY
            }),
            custom_graphs: Some(alloc::vec![self.destination]),
            custom_nodes: None,
        };
        cx.process_chunk(&[self.chunk.bytes], GRAPH_SDDL2_CHUNK, Some(&params))?;
        self.chunk.segments.clear();
        self.chunk.bytes = 0;
        Ok(())
    }

    /// `sddl2_reserve_segment_stream_id`.
    fn reserve_stream_id(&mut self, ty: &Type) -> Result<u32, ExecError> {
        let streams = count_primitive_fields(ty)
            .filter(|&streams| streams != 0)
            .ok_or(ExecError::Parameter(
                "failed to count replay streams for an SDDL2 segment",
            ))?;
        if streams > INT_MAX as usize {
            return Err(ExecError::Limit("SDDL2 logical output streams"));
        }
        let streams = streams as u32;
        if self.next_stream_id > INT_MAX - streams {
            return Err(ExecError::Limit("SDDL2 clustering tag capacity"));
        }
        let first = self.next_stream_id;
        self.next_stream_id += streams;
        Ok(first)
    }

    /// `sddl2_plan_split_segment_chunks`: splits an oversized segment on
    /// element boundaries.
    fn plan_split(
        &mut self,
        cx: &mut SegmenterCx<'_, '_, '_>,
        segment: &vm::Segment,
        stream_id: u32,
    ) -> Result<(), ExecError> {
        let ty = &segment.ty;
        let element_bytes = if ty.kind == Kind::Structure {
            let data = ty.struct_data.as_ref().ok_or(ExecError::Parameter(
                "an SDDL2 structure segment has no type metadata",
            ))?;
            if data.total_size_bytes == 0 {
                return Err(ExecError::Parameter(
                    "an SDDL2 structure segment has zero-sized split units",
                ));
            }
            data.total_size_bytes
        } else {
            ty.kind.size().ok_or(ExecError::Parameter(
                "failed to derive a split unit size for SDDL2 segment chunking",
            ))?
        };
        if !segment.size_bytes.is_multiple_of(element_bytes) {
            return Err(ExecError::Parameter(
                "an SDDL2 segment size is not aligned to its split unit size",
            ));
        }
        if element_bytes > self.chunk_limit {
            return Err(ExecError::Parameter(
                "the SDDL2 chunk size cannot fit one element",
            ));
        }
        let max_slice = self.chunk_limit / element_bytes * element_bytes;
        let mut remaining = segment.size_bytes;
        while remaining > 0 {
            let slice = remaining.min(max_slice);
            self.append(stream_id, ty, slice)?;
            remaining -= slice;
            if remaining > 0 {
                self.flush(cx)?;
            }
        }
        Ok(())
    }

    /// `sddl2_plan_segment_chunk`.
    fn plan_segment(
        &mut self,
        cx: &mut SegmenterCx<'_, '_, '_>,
        segment: &vm::Segment,
    ) -> Result<(), ExecError> {
        if segment.size_bytes > self.remaining_input {
            return Err(ExecError::Parameter(
                "split instructions do not map exactly the entire input",
            ));
        }
        self.remaining_input -= segment.size_bytes;
        let stream_id = self.reserve_stream_id(&segment.ty)?;
        if self.chunk_limit == usize::MAX || segment.size_bytes <= self.chunk_limit {
            let would_exceed = !self.chunk.segments.is_empty()
                && self.chunk_limit != usize::MAX
                && (self.chunk.bytes >= self.chunk_limit
                    || segment.size_bytes > self.chunk_limit - self.chunk.bytes);
            if would_exceed {
                self.flush(cx)?;
            }
            return self.append(stream_id, &segment.ty, segment.size_bytes);
        }
        self.flush(cx)?;
        self.plan_split(cx, segment, stream_id)
    }
}

impl SegmentFn for Sddl2 {
    fn segment(&self, cx: &mut SegmenterCx<'_, '_, '_>) -> Result<(), ExecError> {
        if cx.n_inputs() != 1 {
            return Err(ExecError::Parameter("sddl2 takes exactly one input"));
        }
        let input = cx.input(0).ok_or(ExecError::GraphInvalid)?;
        if input.ty() != super::super::data::Type::Serial {
            return Err(ExecError::Parameter("sddl2 takes a serial input"));
        }
        let params = cx.params;
        let bytecode = params
            .copy(BYTECODE_PARAM)
            .ok_or(ExecError::Parameter("sddl2 needs bytecode"))?;
        let destination = single_destination(cx.custom_graphs)?;
        let mut chunk_limit = DEFAULT_CHUNK_BYTE_SIZE;
        match params.int(CHUNK_BYTE_SIZE_PARAM) {
            Some(size) if size < 0 => {
                return Err(ExecError::Parameter(
                    "the SDDL2 chunk size must be non-negative",
                ));
            }
            Some(size) if size > 0 => chunk_limit = size as usize,
            _ => {}
        }
        if cx.format_version() < CHUNK_VERSION_MIN {
            chunk_limit = usize::MAX;
        }

        let data = input.bytes();
        let segments = vm::execute(bytecode, data)?;
        if segments.is_empty() {
            return cx.process_chunk(&[data.len()], gid::STORE, None);
        }
        let mut planner = Planner {
            destination,
            remaining_input: data.len(),
            chunk_limit,
            next_stream_id: 0,
            chunk: Chunk {
                segments: Vec::new(),
                capacity: segments.len(),
                bytes: 0,
                start_stream_id: 0,
            },
        };
        for segment in &segments {
            planner.plan_segment(cx, segment)?;
        }
        if planner.remaining_input != 0 {
            return Err(ExecError::Parameter(
                "split instructions do not map exactly the entire input",
            ));
        }
        planner.flush(cx)
    }
}
