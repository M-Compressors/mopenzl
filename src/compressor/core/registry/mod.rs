//! The registry of nodes and graphs (`ZL_Compressor`, cgraph.c).
//!
//! Standard nodes and graphs are static tables ([`nodes`], [`graphs`]) with
//! the reference's numeric IDs; parameterized and user-defined ones live in
//! a [`Registry`] that a [`CompressorGraph`](crate::graph::CompressorGraph)
//! shares between compressions.
//!
//! # Plugging a codec in
//!
//! * A **node encoder** implements [`Encode`](super::exec::Encode); the row
//!   of the node in [`nodes::STANDARD_NODES`] points its `encoder` at it,
//!   behind `if cfg!(feature = "...") { &codecs::MY_ENCODER } else {
//!   &codecs::DISABLED }`.
//! * A **graph function, selector or segmenter** implements
//!   [`GraphFn`], [`SelectFn`] or [`SegmentFn`]; the row of the graph in
//!   [`graphs::STANDARD_GRAPHS`] replaces the `unported_*` stub.

pub(crate) mod dict;
pub(crate) mod graphs;
pub(crate) mod nodes;

use alloc::borrow::Cow;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ops::Deref;

use super::data::Stream;
use super::data::Type;
use super::error::ExecError;
use super::exec::{Edge, Encode, GraphCx, SegmenterCx, SelectorCx};
use super::params::LocalParams;

/// Where IDs allocated by a [`Registry`] start: past the standard nodes.
pub(crate) const FIRST_CUSTOM_NODE: u32 = 88;
/// Where graph IDs allocated by a [`Registry`] start: past the standard
/// graphs (`ZL_PrivateStandardGraphID_end`).
pub(crate) const FIRST_CUSTOM_GRAPH: u32 = 91;

/// A node: a standard one by its `ZL_StandardNodeID`, or one a registry
/// created.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct NodeId {
    pub(crate) index: u32,
    /// The registry that created it; 0 for standard nodes.
    pub(crate) owner: u32,
}

impl NodeId {
    /// A standard node.
    pub(crate) const fn std(index: u32) -> Self {
        Self { index, owner: 0 }
    }
}

/// A graph: a standard one by its `ZL_StandardGraphID`, or one a registry
/// created.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct GraphId {
    pub(crate) index: u32,
    /// The registry that created it; 0 for standard graphs.
    pub(crate) owner: u32,
}

impl GraphId {
    /// A standard graph.
    pub(crate) const fn std(index: u32) -> Self {
        Self { index, owner: 0 }
    }

    /// `ZL_GRAPH_ILLEGAL`.
    pub(crate) const ILLEGAL: Self = Self::std(0);
}

/// Standard graph IDs by name (`ZL_StandardGraphID`, `ZL_PrivateStandardGraphID`).
pub(crate) mod gid {
    use super::GraphId;

    pub(crate) const SERIAL_STORE: GraphId = GraphId::std(1);
    pub(crate) const STORE: GraphId = GraphId::std(2);
    pub(crate) const ZSTD: GraphId = GraphId::std(9);
    pub(crate) const FIELD_LZ: GraphId = GraphId::std(12);
    pub(crate) const COMPRESS_GENERIC: GraphId = GraphId::std(13);
    pub(crate) const SELECT_NUMERIC: GraphId = GraphId::std(16);
    pub(crate) const SEGMENT_SERIAL: GraphId = GraphId::std(29);
    pub(crate) const STORE1: GraphId = GraphId::std(32);
    pub(crate) const COMPRESS1: GraphId = GraphId::std(34);
    pub(crate) const STRING_STORE: GraphId = GraphId::std(33);
    pub(crate) const SERIAL_COMPRESS: GraphId = GraphId::std(35);
    pub(crate) const STRUCT_COMPRESS: GraphId = GraphId::std(36);
    pub(crate) const NUMERIC_COMPRESS: GraphId = GraphId::std(37);
    pub(crate) const STRING_COMPRESS: GraphId = GraphId::std(38);
    pub(crate) const STRING_SEPARATE_COMPRESS: GraphId = GraphId::std(39);
    pub(crate) const TRANSFORMER_NUMERIC1: GraphId = GraphId::std(90);
}

/// Standard node IDs by name (`ZL_StandardNodeID`).
pub(crate) mod nid {
    use super::NodeId;

    pub(crate) const CONVERT_STRUCT_TO_SERIAL: NodeId = NodeId::std(10);
    pub(crate) const CONVERT_NUM_TO_STRUCT_LE: NodeId = NodeId::std(11);
    pub(crate) const CONVERT_NUM_TO_SERIAL_LE: NodeId = NodeId::std(12);
}

/// A node's static description (`CNode`, `ZL_MIGraphDesc`).
pub(crate) struct NodeSpec {
    pub(crate) name: &'static str,
    /// The wire codec ID (`ZL_StandardTransformID`).
    pub(crate) codec: u32,
    pub(crate) min_version: u32,
    pub(crate) max_version: u32,
    /// Input types, one per port; the last is variable with `variable_input`.
    pub(crate) inputs: &'static [Type],
    pub(crate) variable_input: bool,
    /// Singleton outputs, which every run creates exactly once.
    pub(crate) singletons: &'static [Type],
    /// Types of the variable outputs, created any number of times.
    pub(crate) variable: &'static [Type],
    /// Integer parameters the node has before parameterization.
    pub(crate) defaults: &'static [(i32, i32)],
    pub(crate) encoder: &'static dyn Encode,
}

impl core::fmt::Debug for NodeSpec {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NodeSpec")
            .field("name", &self.name)
            .field("codec", &self.codec)
            .finish_non_exhaustive()
    }
}

impl NodeSpec {
    /// `CNODE_getInputType`: the type of input `index`.
    pub(crate) fn input_type(&self, index: usize) -> Type {
        self.inputs[index.min(self.inputs.len() - 1)]
    }

    /// `CNODE_isNbInputsCompatible`.
    pub(crate) fn accepts(&self, n_inputs: usize) -> bool {
        if self.variable_input {
            n_inputs + 1 >= self.inputs.len()
        } else {
            n_inputs == self.inputs.len()
        }
    }

    /// `CNODE_getOutStreamType`: the type of output outcome `outcome`.
    pub(crate) fn output_type(&self, outcome: usize) -> Option<Type> {
        match outcome.checked_sub(self.singletons.len()) {
            None => self.singletons.get(outcome).copied(),
            Some(variable) => self.variable.get(variable).copied(),
        }
    }
}

/// A node of a registry: a standard node, possibly with replaced parameters.
#[derive(Debug)]
pub(crate) struct NodeDef {
    #[cfg(feature = "custom-codec")]
    pub(crate) custom: Option<crate::custom_codec::Registered>,
    pub(crate) spec: Callback<NodeSpec>,
    /// Replacement parameters (`ZL_Compressor_parameterizeNode`).
    pub(crate) params: Option<LocalParams>,
    /// The dictionary the node references by ID.
    pub(crate) dict_id: Option<crate::dict::DictId>,
    /// Index of that dictionary in the bundle, resolved when the graphs are
    /// frozen (`CGraph_resolveDictIndices`).
    pub(crate) dict_index: Option<u32>,
}

/// A graph or selector callback, static for standard graphs and shared for
/// user ones.
pub(crate) enum Callback<T: ?Sized + 'static> {
    Static(&'static T),
    Shared(Arc<T>),
}

impl<T: ?Sized> Clone for Callback<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Static(value) => Self::Static(value),
            Self::Shared(value) => Self::Shared(Arc::clone(value)),
        }
    }
}

impl<T: ?Sized> Deref for Callback<T> {
    type Target = T;

    fn deref(&self) -> &T {
        match self {
            Self::Static(callback) => callback,
            Self::Shared(callback) => callback,
        }
    }
}

impl<T: ?Sized> core::fmt::Debug for Callback<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Callback")
    }
}

/// A function graph (`ZL_FunctionGraphFn`).
pub(crate) trait GraphFn: Send + Sync {
    /// Runs nodes on `inputs` and assigns a destination to every edge.
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError>;
}

/// A selector (`ZL_SelectorFn`).
pub(crate) trait SelectFn: Send + Sync {
    /// Chooses the successor of `input`.
    fn select(
        &self,
        cx: &mut SelectorCx<'_, '_, '_>,
        input: &Stream<'_>,
        successors: &[GraphId],
    ) -> GraphId;
}

/// A segmenter (`ZL_SegmenterFn`).
pub(crate) trait SegmentFn: Send + Sync {
    /// Cuts the inputs into chunks.
    fn segment(&self, cx: &mut SegmenterCx<'_, '_, '_>) -> Result<(), ExecError>;

    /// For a segmenter that cuts one serial input every N bytes and hands
    /// each piece to one successor: `(N, successor)`. A session then writes
    /// the frame incrementally, one chunk at a time, through the very
    /// routine the one-shot segmenter uses.
    fn serial_chunk(
        &self,
        _params: &LocalParams,
        _custom_graphs: &[GraphId],
    ) -> Option<(usize, GraphId)> {
        None
    }
}

/// What kind of graph an ID names (`GraphType_e`).
#[derive(Debug)]
pub(crate) enum GraphKind {
    /// No graph.
    Illegal,
    /// `serial_store`: marks the stream to be written to the frame.
    Store,
    /// A node followed by its successors (`GR_staticGraphWrapper`).
    Static,
    /// A selector (`GR_selectorWrapper`).
    Selector(Callback<dyn SelectFn>),
    /// A function graph.
    Function(Callback<dyn GraphFn>),
    /// A segmenter.
    Segmenter(Callback<dyn SegmentFn>),
}

/// A graph's description (`InternalGraphDesc`).
#[derive(Debug)]
pub(crate) struct GraphDef {
    pub(crate) name: Cow<'static, str>,
    pub(crate) kind: GraphKind,
    /// Accepted type mask of each input port.
    pub(crate) inputs: Cow<'static, [u8]>,
    /// The last port accepts any number of inputs.
    pub(crate) last_variable: bool,
    pub(crate) custom_graphs: Cow<'static, [GraphId]>,
    pub(crate) custom_nodes: Cow<'static, [NodeId]>,
    pub(crate) params: LocalParams,
}

impl GraphDef {
    /// `GR_illegal`.
    pub(crate) const ILLEGAL: Self = Self {
        name: Cow::Borrowed("illegal"),
        kind: GraphKind::Illegal,
        inputs: Cow::Borrowed(&[]),
        last_variable: false,
        custom_graphs: Cow::Borrowed(&[]),
        custom_nodes: Cow::Borrowed(&[]),
        params: LocalParams::EMPTY,
    };

    /// The type mask of the port input `index` goes to
    /// (`n >= nbPorts - 1` shares the last port).
    pub(crate) fn port_mask(&self, index: usize) -> u8 {
        self.inputs
            .get(index.min(self.inputs.len().saturating_sub(1)))
            .copied()
            .unwrap_or(0)
    }
}

/// Parameters a successor runs with, replacing the graph's own
/// (`ZL_RuntimeGraphParameters`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RuntimeParams {
    pub(crate) params: Option<LocalParams>,
    pub(crate) custom_graphs: Option<Vec<GraphId>>,
    pub(crate) custom_nodes: Option<Vec<NodeId>>,
}

impl RuntimeParams {
    /// Whether nothing is overridden.
    pub(crate) const fn is_empty(&self) -> bool {
        self.params.is_none() && self.custom_graphs.is_none() && self.custom_nodes.is_none()
    }
}

/// The graphs and nodes of one compressor, and its starting graph.
#[derive(Debug)]
pub(crate) struct Registry {
    /// Identifies the registry that created an ID.
    pub(crate) owner: u32,
    pub(crate) nodes: Vec<NodeDef>,
    pub(crate) graphs: Vec<GraphDef>,
    pub(crate) start: GraphId,
    /// The runtime parameters of the starting graph.
    pub(crate) start_params: Option<RuntimeParams>,
    /// The loaded dictionary bundle.
    pub(crate) dicts: Option<dict::DictState>,
}

impl Registry {
    /// A registry with the standard nodes and graphs only, starting at
    /// `start`.
    pub(crate) const fn new(owner: u32, start: GraphId) -> Self {
        Self {
            owner,
            nodes: Vec::new(),
            graphs: Vec::new(),
            start,
            start_params: None,
            dicts: None,
        }
    }

    /// A node's spec, parameters and dictionary
    /// (`CGRAPH_getCNode`).
    pub(crate) fn node(&self, id: NodeId) -> Result<NodeView<'_>, ExecError> {
        if id.owner == 0 {
            let spec = nodes::standard(id.index).ok_or(ExecError::NodeInvalid)?;
            return Ok(NodeView {
                spec,
                #[cfg(feature = "custom-codec")]
                custom: None,
                params: None,
                dict_id: None,
                dict_index: None,
            });
        }
        if id.owner != self.owner {
            return Err(ExecError::NodeInvalid);
        }
        let def = id
            .index
            .checked_sub(FIRST_CUSTOM_NODE)
            .and_then(|index| self.nodes.get(index as usize))
            .ok_or(ExecError::NodeInvalid)?;
        Ok(NodeView {
            spec: &def.spec,
            #[cfg(feature = "custom-codec")]
            custom: def.custom.as_ref(),
            params: def.params.as_ref(),
            dict_id: def.dict_id,
            dict_index: def.dict_index,
        })
    }

    /// A graph's description (`CGRAPH_getMultiInputGraphDesc`).
    pub(crate) fn graph(&self, id: GraphId) -> Result<&GraphDef, ExecError> {
        let def = if id.owner == 0 {
            graphs::STANDARD_GRAPHS.get(id.index as usize)
        } else if id.owner == self.owner {
            id.index
                .checked_sub(FIRST_CUSTOM_GRAPH)
                .and_then(|index| self.graphs.get(index as usize))
        } else {
            None
        };
        match def {
            Some(def) if !matches!(def.kind, GraphKind::Illegal) => Ok(def),
            _ => Err(ExecError::GraphInvalid),
        }
    }

    /// If `start` (with runtime parameters `over`) is a serial segmenter,
    /// its chunk size and successor.
    pub(crate) fn serial_plan(
        &self,
        start: GraphId,
        over: Option<&RuntimeParams>,
    ) -> Option<(usize, GraphId)> {
        let def = self.graph(start).ok()?;
        let GraphKind::Segmenter(segmenter) = &def.kind else {
            return None;
        };
        let params = over
            .and_then(|over| over.params.as_ref())
            .unwrap_or(&def.params);
        let custom = over
            .and_then(|over| over.custom_graphs.as_deref())
            .unwrap_or(&def.custom_graphs);
        segmenter.serial_chunk(params, custom)
    }

    /// Adds a node, returning its ID.
    pub(crate) fn add_node(&mut self, def: NodeDef) -> NodeId {
        self.nodes.push(def);
        NodeId {
            index: FIRST_CUSTOM_NODE + (self.nodes.len() - 1) as u32,
            owner: self.owner,
        }
    }

    /// Adds a graph, returning its ID.
    pub(crate) fn add_graph(&mut self, def: GraphDef) -> GraphId {
        self.graphs.push(def);
        GraphId {
            index: FIRST_CUSTOM_GRAPH + (self.graphs.len() - 1) as u32,
            owner: self.owner,
        }
    }
}

/// A resolved node.
#[derive(Copy, Clone, Debug)]
pub(crate) struct NodeView<'r> {
    #[cfg(feature = "custom-codec")]
    pub(crate) custom: Option<&'r crate::custom_codec::Registered>,
    pub(crate) spec: &'r NodeSpec,
    /// Replacement parameters, if the node was parameterized.
    pub(crate) params: Option<&'r LocalParams>,
    /// The dictionary the node references.
    pub(crate) dict_id: Option<crate::dict::DictId>,
    /// Its index in the bundle, once resolved.
    pub(crate) dict_index: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_resolve_against_their_owner_only() {
        let mut registry = Registry::new(7, gid::COMPRESS_GENERIC);
        assert!(registry.node(NodeId::std(62)).is_ok());
        assert!(registry.node(NodeId::std(0)).is_err());
        assert!(registry.node(NodeId::std(500)).is_err());
        let spec = nodes::standard(62).unwrap();
        let node = registry.add_node(NodeDef {
            spec: Callback::Static(spec),
            #[cfg(feature = "custom-codec")]
            custom: None,
            params: Some(LocalParams::default()),
            dict_id: None,
            dict_index: Some(2),
        });
        assert_eq!(node.index, FIRST_CUSTOM_NODE);
        let view = registry.node(node).unwrap();
        assert_eq!((view.spec.name, view.dict_index), ("zstd", Some(2)));
        assert!(view.params.is_some());
        let foreign = NodeId { owner: 8, ..node };
        assert!(registry.node(foreign).is_err());
        assert!(registry.node(NodeId { index: 1, owner: 7 }).is_err());

        assert!(registry.graph(gid::STORE).is_ok());
        assert!(registry.graph(GraphId::ILLEGAL).is_err());
        assert!(registry.graph(GraphId::std(200)).is_err());
        let graph = registry.add_graph(GraphDef {
            name: Cow::Borrowed("mine"),
            ..GraphDef::ILLEGAL
        });
        assert!(registry.graph(graph).is_err(), "illegal kind stays invalid");
        assert!(registry.graph(GraphId { owner: 9, ..graph }).is_err());
        assert!(registry.graph(GraphId { index: 5, owner: 7 }).is_err());
    }

    #[test]
    fn node_specs_answer_port_questions() {
        let separate = nodes::standard(26).unwrap();
        assert!(separate.accepts(1) && !separate.accepts(2));
        assert_eq!(separate.output_type(0), Some(Type::Serial));
        assert_eq!(separate.output_type(1), Some(Type::Numeric));
        assert_eq!(separate.output_type(2), None);
        let concat = nodes::standard(33).unwrap();
        assert!(concat.accepts(0) && concat.accepts(5));
        assert_eq!(concat.input_type(9), Type::Serial);
        let transpose = nodes::standard(3).unwrap();
        assert_eq!(transpose.output_type(4), None);
        assert_eq!(transpose.output_type(0), Some(Type::Serial));
        assert!(RuntimeParams::default().is_empty());
        assert!(alloc::format!("{separate:?}").contains("separate_string_components"));
    }

    #[test]
    fn callbacks_and_encoders_have_opaque_debug_output() {
        let graph = graphs::STANDARD_GRAPHS.get(13).unwrap();
        assert!(alloc::format!("{:?}", graph.kind).contains("Callback"));
        let encoder = &crate::compressor::core::codecs::conversion::CONVERT_NUM_TO_STRUCT;
        assert_eq!(alloc::format!("{encoder:?}"), "FnEncoder");
    }

    #[test]
    fn port_masks_share_the_last_port() {
        let store = graphs::STANDARD_GRAPHS.get(2).unwrap();
        assert_eq!(store.port_mask(0), 15);
        assert_eq!(store.port_mask(7), 15);
        assert_eq!(GraphDef::ILLEGAL.port_mask(0), 0);
    }
}
