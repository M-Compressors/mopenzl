//! The graph engine: runs a compressor's graphs over one frame.
//!
//! This is a line-by-line port of the reference's `CCTX_*` control flow
//! (`cctx.c`): [`Exec::run_successor`] / [`Exec::run_supervised`] dispatch a
//! graph with the min-stream-size rule, implicit conversions and the
//! permissive fallback; [`Exec::run_node`] executes one node and enters its
//! outputs into the runtime graph; [`Exec::flush_chunk`] writes a finished
//! chunk with the store-on-expansion fallback; [`Exec::try_graph`] derives a
//! second engine that compresses a complete frame into a capped buffer.

pub(crate) mod graph_cx;
pub(crate) mod node_cx;
pub(crate) mod segmenter_cx;
pub(crate) mod selector_cx;

use alloc::vec::Vec;

use fearless_simd::Level;

pub(crate) use graph_cx::{Edge, Effective, GraphCx, MPARAM_REF_ID};
pub(crate) use node_cx::NodeCx;
pub(crate) use segmenter_cx::SegmenterCx;
pub(crate) use selector_cx::SelectorCx;

use super::Params;
use super::data::{Stream, Type, type_mask};
use super::error::{ExecError, NodeError};
use super::layout;
use super::params::LocalParams;
use super::registry::{GraphDef, GraphId, GraphKind, NodeId, Registry, RuntimeParams, gid, nid};
use super::rtgraph::{RtError, RtGraph};
use super::sink::{CappedSink, Sink};
#[cfg(feature = "zstd")]
use super::zstd::ZstdNode;

/// `ZL_MAX_GRAPH_DEPTH`.
const MAX_GRAPH_DEPTH: u32 = 1024;
/// `ZL_runtimeNodeLimit` for v20+.
const NODE_LIMIT: usize = 20_000;
/// `ZL_runtimeStreamLimit` for v16+.
const STREAM_LIMIT: usize = 110_000;
/// `ZL_transformOutStreamsLimit` for v16+.
const NODE_OUTPUT_LIMIT: usize = 100_000;
/// `ZL_runtimeInputLimit` for v21+.
pub(crate) const INPUT_LIMIT: usize = 2048;

/// A node encoder (`ZL_MIEncoderFn`): reads its inputs, creates its outputs
/// and sends its codec header through the [`NodeCx`].
pub(crate) trait Encode: Send + Sync {
    /// Runs the node.
    fn encode<'a>(&self, cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError>;
}

/// The reusable resources a compression borrows from the engine.
#[derive(Debug)]
pub(crate) struct Resources<'e> {
    #[cfg(feature = "zstd")]
    pub(crate) zstd: &'e mut ZstdNode,
    pub(crate) simd: Level,
    #[cfg(not(feature = "zstd"))]
    pub(crate) marker: core::marker::PhantomData<&'e ()>,
}

impl<'e> Resources<'e> {
    /// Reborrows the resources for a derived engine.
    fn reborrow(&mut self) -> Resources<'_> {
        Resources {
            #[cfg(feature = "zstd")]
            zstd: &mut *self.zstd,
            simd: self.simd,
            #[cfg(not(feature = "zstd"))]
            marker: core::marker::PhantomData,
        }
    }
}

/// Where a node run left its outputs.
#[derive(Copy, Clone, Debug)]
pub(crate) struct NodeRun {
    /// Index of the first output stream.
    pub(crate) start_out: u32,
    /// Outputs created.
    pub(crate) n_out: u32,
}

/// One compression: a registry, the resolved parameters, the inputs and the
/// runtime graph of the chunk in progress (`ZL_CCtx`).
pub(crate) struct Exec<'e, 'a> {
    pub(crate) reg: &'e Registry,
    pub(crate) p: Params,
    pub(crate) res: Resources<'e>,
    pub(crate) comment: Option<&'e [u8]>,
    /// The frame's inputs.
    pub(crate) inputs: Vec<Stream<'a>>,
    pub(crate) rt: RtGraph<'a>,
    pub(crate) num_segments: usize,
    pub(crate) segmenter_depth: u32,
    pub(crate) in_backup: bool,
    pub(crate) sink: &'e mut dyn Sink,
}

impl<'e, 'a> Exec<'e, 'a> {
    pub(crate) fn new(
        reg: &'e Registry,
        p: Params,
        res: Resources<'e>,
        comment: Option<&'e [u8]>,
        inputs: Vec<Stream<'a>>,
        sink: &'e mut dyn Sink,
    ) -> Self {
        Self {
            reg,
            p,
            res,
            comment,
            inputs,
            rt: RtGraph::default(),
            num_segments: 0,
            segmenter_depth: 0,
            in_backup: false,
            sink,
        }
    }

    /// `CCTX_isNodeSupported`.
    pub(crate) fn node_supported(&self, node: NodeId) -> bool {
        self.reg.node(node).is_ok_and(|view| {
            (view.spec.min_version..=view.spec.max_version).contains(&self.p.version)
        })
    }

    /// Compresses the frame: header, chunks, end marker
    /// (`CCTX_compressInputs_withGraphSet` + `CCTX_startCompression`).
    pub(crate) fn compress_frame(
        &mut self,
        start: GraphId,
        start_params: Option<&RuntimeParams>,
    ) -> Result<(), ExecError> {
        if self.inputs.is_empty() || self.inputs.len() > INPUT_LIMIT {
            return Err(ExecError::Limit("inputs"));
        }
        layout::write_frame_header_for(
            &self.p,
            self.res.simd,
            &self.inputs,
            self.comment,
            self.reg.dicts.as_ref().map(|state| &state.id),
            self.sink,
        )?;
        let rtsids: Vec<u32> = self
            .inputs
            .clone()
            .into_iter()
            .map(|input| self.rt.ref_input(input))
            .collect();
        self.num_segments = 0;
        self.segmenter_depth = 0;
        self.run_successor(start, start_params, &rtsids, 1)?;
        if self.num_segments == 0 {
            let chunk = self.inputs.clone();
            self.flush_chunk(&chunk)?;
        }
        self.sink.put(&[crate::shared::wire::END_OF_FRAME])?;
        Ok(())
    }

    /// `CCTX_runSuccessor`.
    pub(crate) fn run_successor(
        &mut self,
        graph: GraphId,
        params: Option<&RuntimeParams>,
        rtsids: &[u32],
        depth: u32,
    ) -> Result<(), ExecError> {
        if depth >= MAX_GRAPH_DEPTH && !self.in_backup {
            return Err(ExecError::DepthExceeded);
        }
        let segmentable = rtsids[0] == 0
            && rtsids.len() == self.inputs.len()
            && self.num_segments == 0
            && self.segmenter_depth == 0
            && self.rt.nodes.is_empty();
        if let Ok(def) = self.reg.graph(graph)
            && let GraphKind::Segmenter(_) = def.kind
        {
            if segmentable {
                return self.run_segmenter(graph, params, rtsids, depth);
            }
            return Err(ExecError::SegmenterPlacement);
        }
        self.run_successor_internal(graph, params, rtsids, depth)
    }

    /// `CCTX_runSuccessor_internal`: the min-stream-size rule and the
    /// permissive fallback around [`Self::run_supervised`].
    fn run_successor_internal(
        &mut self,
        graph: GraphId,
        params: Option<&RuntimeParams>,
        rtsids: &[u32],
        depth: u32,
    ) -> Result<(), ExecError> {
        if graph != gid::SERIAL_STORE && rtsids.len() == 1 {
            let stream = self.rt.stream(rtsids[0]).ok_or(ExecError::GraphInvalid)?;
            if stream.ty() != Type::String {
                let size = stream.content_size().min(i32::MAX as usize) as i64;
                if size < i64::from(self.p.min_stream_size) {
                    return self.run_supervised(gid::STORE1, None, rtsids, depth);
                }
            }
        }
        let nodes_before = self.rt.nodes.len();
        let Err(error) = self.run_supervised(graph, params, rtsids, depth) else {
            return Ok(());
        };
        if !self.p.permissive || self.in_backup {
            return Err(error);
        }
        if self.segmenter_depth != 0 && depth <= self.segmenter_depth {
            // Permissive fallback above a segmenter is not supported.
            return Err(error);
        }
        self.rt.clear_nodes_from(nodes_before);
        self.in_backup = true;
        let outcome = self.run_supervised(gid::COMPRESS_GENERIC, None, rtsids, depth);
        if outcome.is_ok() {
            self.in_backup = false;
        }
        outcome
    }

    /// `CCTX_runSupervisedGraphID_internal`: checks input types, inserts
    /// implicit conversions, routes `serial_store` to a store mark, then runs
    /// the graph.
    fn run_supervised(
        &mut self,
        graph: GraphId,
        params: Option<&RuntimeParams>,
        rtsids: &[u32],
        depth: u32,
    ) -> Result<(), ExecError> {
        let reg: &'e Registry = self.reg;
        let def = reg.graph(graph)?;
        let mut ids = rtsids.to_vec();
        for (n, id) in ids.iter_mut().enumerate() {
            let stream = self.rt.stream(*id).ok_or(ExecError::GraphInvalid)?;
            let ty = stream.ty();
            let port = def.port_mask(n);
            if type_mask(ty) & port != 0 {
                continue;
            }
            let node = implicit_conversion(ty, port).ok_or(ExecError::NoConversion)?;
            let run = self.run_node(node, &[*id], None)?;
            debug_assert_eq!(run.n_out, 1);
            *id = run.start_out;
        }
        if let GraphKind::Store = def.kind {
            self.rt.store(ids[0]);
            return Ok(());
        }
        self.run_graph_desc(def, params, &ids, depth)
    }

    /// `CCTX_runGraphDesc` + `CCTX_runGraph_internal`: runs the graph
    /// function, then its successors in edge-handle order.
    fn run_graph_desc(
        &mut self,
        def: &'e GraphDef,
        params: Option<&RuntimeParams>,
        ids: &[u32],
        depth: u32,
    ) -> Result<(), ExecError> {
        let eff = Effective {
            params: params
                .and_then(|over| over.params.as_ref())
                .unwrap_or(&def.params),
            custom_graphs: params
                .and_then(|over| over.custom_graphs.as_deref())
                .unwrap_or(&def.custom_graphs),
            custom_nodes: params
                .and_then(|over| over.custom_nodes.as_deref())
                .unwrap_or(&def.custom_nodes),
            mparam: def.params.reference(MPARAM_REF_ID),
        };
        let mut cx = GraphCx::new(self, eff, depth, ids);
        let edges = cx.input_edges();
        let result = match &def.kind {
            GraphKind::Static => run_static(&mut cx, &edges),
            GraphKind::Selector(selector) => run_selector(&mut cx, &**selector, &edges),
            GraphKind::Function(function) => function.run(&mut cx, &edges),
            GraphKind::Illegal | GraphKind::Store | GraphKind::Segmenter(_) => {
                Err(ExecError::GraphInvalid)
            }
        };
        let successors = cx.finish(result)?;
        for successor in successors {
            self.run_successor(
                successor.gid,
                successor.params.as_ref(),
                &successor.inputs,
                depth + 1,
            )?;
        }
        Ok(())
    }

    /// `CCTX_runSegmenter`.
    fn run_segmenter(
        &mut self,
        graph: GraphId,
        params: Option<&RuntimeParams>,
        rtsids: &[u32],
        depth: u32,
    ) -> Result<(), ExecError> {
        let reg: &'e Registry = self.reg;
        let def = reg.graph(graph)?;
        let GraphKind::Segmenter(segmenter) = &def.kind else {
            return Err(ExecError::GraphInvalid);
        };
        for (n, &id) in rtsids.iter().enumerate() {
            let ty = self.rt.stream(id).ok_or(ExecError::GraphInvalid)?.ty();
            if type_mask(ty) & def.port_mask(n) == 0 {
                return Err(ExecError::SegmenterConversion);
            }
        }
        let local = params
            .and_then(|over| over.params.as_ref())
            .unwrap_or(&def.params);
        let custom = params
            .and_then(|over| over.custom_graphs.as_deref())
            .unwrap_or(&def.custom_graphs);
        let n_inputs = rtsids.len();
        let mut cx = SegmenterCx::new(self, local, custom, n_inputs)?;
        cx.ex.segmenter_depth = depth;
        segmenter.segment(&mut cx)?;
        cx.check_done()
    }

    /// `CCTX_runNodeID_wParams` + `CCTX_runCNode_wParams`: runs `node` on the
    /// streams `input_ids` and enters its outputs into the runtime graph.
    pub(crate) fn run_node(
        &mut self,
        node: NodeId,
        input_ids: &[u32],
        local: Option<&LocalParams>,
    ) -> Result<NodeRun, ExecError> {
        let view = self.reg.node(node)?;
        let spec = view.spec;
        if !spec.accepts(input_ids.len()) {
            return Err(ExecError::NodeInputCount(spec.name));
        }
        if input_ids.len() > INPUT_LIMIT {
            return Err(ExecError::NodeVersion(spec.name));
        }
        let mut inputs = Vec::with_capacity(input_ids.len());
        for (n, &id) in input_ids.iter().enumerate() {
            let stream = self.rt.stream(id).ok_or(ExecError::GraphInvalid)?;
            if stream.ty() != spec.input_type(n) {
                return Err(ExecError::NodeInputType(spec.name));
            }
            inputs.push(stream.clone());
        }
        let version = self.p.version;
        if version < spec.min_version || version > spec.max_version {
            return Err(ExecError::NodeVersion(spec.name));
        }
        if view.dict_index.is_some() && version < 25 {
            return Err(ExecError::NodeVersion(spec.name));
        }
        let rt_node = self
            .rt
            .create_node(
                spec.codec,
                spec.singletons.len(),
                input_ids,
                view.dict_index,
                NODE_LIMIT,
                STREAM_LIMIT,
            )
            .map_err(|RtError::Limit| ExecError::Limit("nodes or streams"))?;
        #[cfg(feature = "custom-codec")]
        {
            self.rt.nodes[rt_node].standard = view.custom.is_none();
        }
        let dictionary = view
            .dict_index
            .and_then(|index| self.reg.dicts.as_ref()?.prepared(index));
        let outputs = self.execute(
            view.params,
            local,
            spec,
            #[cfg(feature = "custom-codec")]
            view.custom,
            &inputs,
            dictionary,
        );
        let outputs = match outputs {
            Ok(outputs) => outputs,
            Err(source) => {
                self.rt.clear_nodes_from(rt_node);
                return Err(ExecError::Node {
                    node: spec.name,
                    source,
                });
            }
        };
        let total = outputs.singles.len() + outputs.variable.len();
        if total > NODE_OUTPUT_LIMIT {
            self.rt.clear_nodes_from(rt_node);
            return Err(ExecError::Limit("node outputs"));
        }
        let start_out = self.rt.nodes[rt_node].start_out;
        for (outcome, stream) in outputs.singles.into_iter().enumerate() {
            self.rt.fill_singleton(rt_node, outcome, stream);
        }
        for (outcome, stream) in outputs.variable {
            if self
                .rt
                .push_variable(rt_node, outcome, stream, STREAM_LIMIT)
                .is_err()
            {
                self.rt.clear_nodes_from(rt_node);
                return Err(ExecError::Limit("streams"));
            }
        }
        if let Some(header) = outputs.header {
            self.rt.set_header(rt_node, &header);
        }
        Ok(NodeRun {
            start_out,
            n_out: total as u32,
        })
    }

    /// Runs the encoder of `spec` (`ENC_runTransform`): the parameters passed
    /// to the run replace the node's own, which replace its defaults.
    fn execute(
        &mut self,
        own: Option<&LocalParams>,
        passed: Option<&LocalParams>,
        spec: &super::registry::NodeSpec,
        #[cfg(feature = "custom-codec")] custom: Option<&crate::custom_codec::Registered>,
        inputs: &[Stream<'a>],
        dictionary: Option<&super::registry::dict::Prepared>,
    ) -> Result<node_cx::NodeOutputs<'a>, NodeError> {
        let local = passed.or(own);
        let mut cx = NodeCx::new(
            spec,
            local,
            &self.p,
            #[cfg(feature = "zstd")]
            &mut *self.res.zstd,
        );
        cx.dictionary = dictionary;
        cx.simd = self.res.simd;
        #[cfg(feature = "custom-codec")]
        if let Some(custom) = custom {
            crate::custom_codec::encode(custom, &mut cx, inputs)?;
            return cx.finish().map_err(|_| {
                NodeError::Custom(crate::custom_codec::CodecError::Invalid(
                    "custom encoder left a singleton uncommitted",
                ))
            });
        }
        spec.encoder.encode(&mut cx, inputs)?;
        cx.finish()
    }

    /// `CCTX_flushChunk`: writes the chunk the runtime graph describes, or its
    /// stored replacement when it would expand.
    pub(crate) fn flush_chunk(&mut self, chunk_inputs: &[Stream<'a>]) -> Result<(), ExecError> {
        let version = self.p.version;
        let lane = version >= crate::shared::wire::BUNDLE_VERSION_MIN && self.reg.dicts.is_some();
        let mut plan = layout::final_graph(&self.rt, self.inputs.len(), version)?;
        let start = self.sink.len();
        let mut header = Vec::new();
        let mut use_store = false;
        let room = self.sink.remaining();
        let fits = room.is_none_or(|room| room >= layout::header_bound(&plan));
        if fits {
            layout::write_chunk_header(&plan, version, lane, &self.rt, &mut header);
        } else if !plan.codecs.is_empty() && self.p.store_on_expansion {
            use_store = true;
        } else {
            return Err(ExecError::Full);
        }
        if !plan.codecs.is_empty() && !use_store && self.p.store_on_expansion {
            let compressed = header.len() + layout::stored_size(&plan, &self.rt);
            let input_total: usize = chunk_inputs.iter().map(Stream::weight).sum();
            if compressed >= input_total {
                use_store = true;
            }
        }
        if use_store {
            self.replace_chunk_with_store(chunk_inputs)?;
            plan = layout::final_graph(&self.rt, self.inputs.len(), version)?;
            if room.is_some_and(|room| room < layout::header_bound(&plan)) {
                return Err(ExecError::Full);
            }
            header.clear();
            layout::write_chunk_header(&plan, version, lane, &self.rt, &mut header);
        }
        self.sink.put(&header)?;
        layout::put_stored(&plan, &self.rt, self.sink)?;
        if self.p.content_checksum {
            let hash = layout::content_checksum(chunk_inputs, version, self.res.simd);
            self.sink.put(&hash.to_le_bytes())?;
        }
        if self.p.compressed_checksum {
            let hash = crate::shared::xxh3::hash32(self.res.simd, self.sink.since(start));
            self.sink.put(&hash.to_le_bytes())?;
        }
        self.num_segments += 1;
        Ok(())
    }

    /// `CCTX_replaceChunkWithStore`.
    fn replace_chunk_with_store(&mut self, chunk_inputs: &[Stream<'a>]) -> Result<(), ExecError> {
        self.rt.reset();
        let rtsids: Vec<u32> = chunk_inputs
            .iter()
            .map(|input| self.rt.ref_input(input.clone()))
            .collect();
        self.run_supervised(gid::STORE, None, &rtsids, 1)
    }

    /// `CCTX_tryGraph`: the size of the complete frame `graph` produces for
    /// `inputs`, compressed by a derived engine with checksums off and no
    /// comment, into a buffer of `ZL_compressBound` bytes.
    pub(crate) fn try_graph<'b>(
        &mut self,
        inputs: &[Stream<'b>],
        graph: GraphId,
        params: Option<&RuntimeParams>,
    ) -> Result<usize, ExecError> {
        if inputs.is_empty() {
            return Err(ExecError::SuccessorInputs);
        }
        let total: usize = inputs.iter().map(Stream::weight).sum();
        let capacity = total + 64 + (total / 32_768 + 1) * 16;
        let mut sink = CappedSink::new(capacity);
        let mut p = self.p;
        p.content_checksum = false;
        p.compressed_checksum = false;
        {
            let mut derived = Exec::new(
                self.reg,
                p,
                self.res.reborrow(),
                None,
                inputs.to_vec(),
                &mut sink,
            );
            derived.compress_frame(graph, params)?;
        }
        Ok(sink.len())
    }
}

/// `ICONV_implicitConversionNodeID`.
fn implicit_conversion(from: Type, to_mask: u8) -> Option<NodeId> {
    let from_mask = type_mask(from);
    if from_mask & 4 != 0 && to_mask & 2 != 0 {
        Some(nid::CONVERT_NUM_TO_STRUCT_LE)
    } else if from_mask & 2 != 0 && to_mask & 1 != 0 {
        Some(nid::CONVERT_STRUCT_TO_SERIAL)
    } else if from_mask & 4 != 0 && to_mask & 1 != 0 {
        Some(nid::CONVERT_NUM_TO_SERIAL_LE)
    } else {
        None
    }
}

/// `ICONV_isCompatible`.
pub(crate) fn conversion_compatible(from: Type, to_mask: u8) -> bool {
    type_mask(from) & to_mask != 0 || implicit_conversion(from, to_mask).is_some()
}

/// `GR_staticGraphWrapper` / `GR_VOGraphWrapper`: the head node with the
/// graph's own parameters if it has any, then one successor per output.
fn run_static(cx: &mut GraphCx<'_, '_, '_>, edges: &[Edge]) -> Result<(), ExecError> {
    let head = *cx.eff.custom_nodes.first().ok_or(ExecError::NodeInvalid)?;
    let spec = cx.ex.reg.node(head)?.spec;
    let local = (!cx.eff.params.is_empty()).then_some(cx.eff.params);
    let outputs = cx.run_node(head, edges, local)?;
    let successors = cx.eff.custom_graphs;
    if spec.variable.is_empty() {
        if successors.len() != outputs.len() {
            return Err(ExecError::SuccessorInputs);
        }
        for (edge, &successor) in outputs.iter().zip(successors) {
            cx.set_destination(&[*edge], successor, None)?;
        }
        return Ok(());
    }
    let n_single = spec.singletons.len();
    if outputs.len() < n_single {
        return Err(ExecError::SuccessorInputs);
    }
    for (n, edge) in outputs.iter().enumerate() {
        let rtsid = cx.edge_rtsid(*edge)?;
        let outcome = cx.ex.rt.streams[rtsid as usize].outcome as usize;
        let valid = if n < n_single {
            outcome == n
        } else {
            outcome >= n_single && outcome < successors.len()
        };
        if !valid {
            return Err(ExecError::SuccessorInputs);
        }
        cx.set_destination(&[*edge], successors[outcome], None)?;
    }
    Ok(())
}

/// `GR_selectorWrapper`.
fn run_selector(
    cx: &mut GraphCx<'_, '_, '_>,
    selector: &dyn super::registry::SelectFn,
    edges: &[Edge],
) -> Result<(), ExecError> {
    let input = cx.data(edges[0])?.clone();
    let custom = cx.eff.custom_graphs;
    let mut sel = SelectorCx {
        ex: &mut *cx.ex,
        params: cx.eff.params,
        depth: cx.depth,
        successor_params: None,
    };
    let chosen = selector.select(&mut sel, &input, custom);
    let runtime = sel.runtime_params();
    cx.set_destination(edges, chosen, runtime)
}

#[cfg(all(test, feature = "zstd"))]
mod tests;
