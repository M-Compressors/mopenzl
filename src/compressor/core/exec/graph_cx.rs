//! The context of one function-graph invocation (`ZL_Graph`/`ZL_Edge`,
//! dyngraph_interface.c).
//!
//! The graph's inputs are edges `0..n`; every node it runs appends its
//! outputs as further edges. Nodes run eagerly inside the graph function.
//! Destinations are only recorded: after the function returns, the successors
//! run in **edge-handle order** (a multi-edge destination triggers at the
//! handle of its first edge), each to completion before the next starts.

use alloc::vec::Vec;
use core::any::Any;

use super::super::data::Stream;
use super::super::error::ExecError;
use super::super::params::LocalParams;
use super::super::registry::{GraphId, NodeId, RuntimeParams, gid};
use super::Exec;

/// A handle to a stream inside a graph invocation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) struct Edge(pub(crate) u32);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Dest {
    Unassigned,
    Processed,
    Trigger,
    Follow,
}

#[derive(Copy, Clone, Debug)]
struct EdgeState {
    rtsid: u32,
    dest: Dest,
    succession_pos: usize,
}

#[derive(Debug)]
struct DestDesc {
    gid: GraphId,
    params: Option<RuntimeParams>,
    n_inputs: usize,
    first_rtsid: usize,
}

/// A graph's parameters after runtime overrides.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Effective<'g> {
    pub(crate) params: &'g LocalParams,
    pub(crate) custom_graphs: &'g [GraphId],
    pub(crate) custom_nodes: &'g [NodeId],
    /// The graph's materialized MParam object, if any
    /// (`ZL_Graph_getMParam`). It belongs to the graph itself, so runtime
    /// parameter overrides keep it.
    pub(crate) mparam: Option<&'g (dyn Any + Send + Sync)>,
}

/// A successor to run once the graph function returned.
#[derive(Debug)]
pub(crate) struct Successor {
    pub(crate) gid: GraphId,
    pub(crate) params: Option<RuntimeParams>,
    pub(crate) inputs: Vec<u32>,
}

/// The state of one invocation of a function graph.
pub(crate) struct GraphCx<'g, 'e, 'a> {
    pub(crate) ex: &'g mut Exec<'e, 'a>,
    pub(crate) eff: Effective<'g>,
    pub(crate) depth: u32,
    edges: Vec<EdgeState>,
    dests: Vec<DestDesc>,
    rtsids: Vec<u32>,
    status: Result<(), ExecError>,
}

impl<'g, 'e, 'a> GraphCx<'g, 'e, 'a> {
    pub(crate) fn new(
        ex: &'g mut Exec<'e, 'a>,
        eff: Effective<'g>,
        depth: u32,
        inputs: &[u32],
    ) -> Self {
        let edges = inputs
            .iter()
            .map(|&rtsid| EdgeState {
                rtsid,
                dest: Dest::Unassigned,
                succession_pos: 0,
            })
            .collect();
        Self {
            ex,
            eff,
            depth,
            edges,
            dests: Vec::new(),
            rtsids: Vec::new(),
            status: Ok(()),
        }
    }

    /// The edges of the graph's inputs.
    pub(crate) fn input_edges(&self) -> Vec<Edge> {
        (0..self.edges.len()).map(|n| Edge(n as u32)).collect()
    }

    /// `ZL_Edge_getData`.
    ///
    /// # Errors
    ///
    /// An edge that does not exist.
    pub(crate) fn data(&self, edge: Edge) -> Result<&Stream<'a>, ExecError> {
        let state = self
            .edges
            .get(edge.0 as usize)
            .ok_or(ExecError::GraphInvalid)?;
        self.ex
            .rt
            .stream(state.rtsid)
            .ok_or(ExecError::GraphInvalid)
    }

    /// The runtime-graph stream an edge refers to.
    ///
    /// # Errors
    ///
    /// An edge that does not exist.
    pub(crate) fn edge_rtsid(&self, edge: Edge) -> Result<u32, ExecError> {
        self.edges
            .get(edge.0 as usize)
            .map(|state| state.rtsid)
            .ok_or(ExecError::GraphInvalid)
    }

    /// `ZL_Edge_setIntMetadata`.
    ///
    /// # Errors
    ///
    /// An edge that does not exist.
    pub(crate) fn set_metadata(
        &mut self,
        edge: Edge,
        id: i32,
        value: i32,
    ) -> Result<(), ExecError> {
        let rtsid = self
            .edges
            .get(edge.0 as usize)
            .ok_or(ExecError::GraphInvalid)?
            .rtsid;
        self.ex
            .rt
            .streams
            .get_mut(rtsid as usize)
            .and_then(|slot| slot.stream.as_mut())
            .ok_or(ExecError::GraphInvalid)?
            .set_metadata(id, value);
        Ok(())
    }

    /// `ZL_Edge_runMultiInputNode_withParams`: runs `node` now and returns
    /// the edges of its outputs, in output order.
    ///
    /// # Errors
    ///
    /// An edge that already has a destination, or a failing node.
    pub(crate) fn run_node(
        &mut self,
        node: NodeId,
        inputs: &[Edge],
        local: Option<&LocalParams>,
    ) -> Result<Vec<Edge>, ExecError> {
        let mut rtsids = Vec::with_capacity(inputs.len());
        for edge in inputs {
            let state = self
                .edges
                .get(edge.0 as usize)
                .ok_or(ExecError::GraphInvalid)?;
            if state.dest != Dest::Unassigned {
                return Err(ExecError::SuccessorAlreadySet);
            }
            rtsids.push(state.rtsid);
        }
        let run = self.ex.run_node(node, &rtsids, local)?;
        for edge in inputs {
            self.edges[edge.0 as usize].dest = Dest::Processed;
        }
        let first = self.edges.len() as u32;
        for n in 0..run.n_out {
            self.edges.push(EdgeState {
                rtsid: run.start_out + n,
                dest: Dest::Unassigned,
                succession_pos: 0,
            });
        }
        Ok((0..run.n_out).map(|n| Edge(first + n)).collect())
    }

    /// `ZL_Edge_setParameterizedDestination`: records that `edges` go to
    /// `graph`, optionally with overriding parameters.
    ///
    /// # Errors
    ///
    /// A graph that does not exist, a wrong number of edges for it, or an edge
    /// that already has a destination (which also fails the invocation).
    pub(crate) fn set_destination(
        &mut self,
        edges: &[Edge],
        graph: GraphId,
        params: Option<RuntimeParams>,
    ) -> Result<(), ExecError> {
        if edges.is_empty() {
            return Err(ExecError::SuccessorInputs);
        }
        let def = self.ex.reg.graph(graph)?;
        let n_ports = def.inputs.len();
        let count_ok = if def.last_variable {
            edges.len() + 1 >= n_ports
        } else {
            edges.len() == n_ports
        };
        if !count_ok {
            return Err(ExecError::SuccessorInputs);
        }
        for (n, edge) in edges.iter().enumerate() {
            let state = self
                .edges
                .get_mut(edge.0 as usize)
                .ok_or(ExecError::GraphInvalid)?;
            if state.dest != Dest::Unassigned {
                self.status = Err(ExecError::SuccessorAlreadySet);
                return Err(ExecError::SuccessorAlreadySet);
            }
            state.dest = if n == 0 { Dest::Trigger } else { Dest::Follow };
            state.succession_pos = self.dests.len();
            self.rtsids.push(state.rtsid);
        }
        self.dests.push(DestDesc {
            gid: graph,
            params: params.filter(|params| !params.is_empty()),
            n_inputs: edges.len(),
            first_rtsid: self.rtsids.len() - edges.len(),
        });
        Ok(())
    }

    /// Ends the invocation (`CCTX_runGraph_internal` after the function):
    /// checks every edge has a destination, or in permissive mode gives the
    /// unassigned ones to `compress_generic`, and lists the successors in
    /// edge-handle order.
    pub(crate) fn finish(self, result: Result<(), ExecError>) -> Result<Vec<Successor>, ExecError> {
        result?;
        self.status?;
        let permissive = self.ex.p.permissive;
        let mut successors = Vec::new();
        for state in &self.edges {
            match state.dest {
                Dest::Trigger => {
                    let desc = &self.dests[state.succession_pos];
                    successors.push(Successor {
                        gid: desc.gid,
                        params: desc.params.clone(),
                        inputs: self.rtsids[desc.first_rtsid..desc.first_rtsid + desc.n_inputs]
                            .to_vec(),
                    });
                }
                Dest::Unassigned if permissive => successors.push(Successor {
                    gid: gid::COMPRESS_GENERIC,
                    params: None,
                    inputs: alloc::vec![state.rtsid],
                }),
                Dest::Unassigned => return Err(ExecError::SuccessorMissing),
                Dest::Processed | Dest::Follow => {}
            }
        }
        Ok(successors)
    }

    /// Whether `node` exists at the frame's format version
    /// (`ZL_Graph_isNodeSupported`).
    pub(crate) fn is_node_supported(&self, node: NodeId) -> bool {
        self.ex.node_supported(node)
    }

    // ---- MParam and metadata (ML selector and clustering package) ----

    /// `ZL_Graph_getMParam`: the object materialized from the graph's MParam
    /// blob at registration; `None` when the graph has none.
    pub(crate) fn mparam(&self) -> Option<&'g (dyn Any + Send + Sync)> {
        self.eff.mparam
    }

    /// `ZL_Input_getIntMetadata` on an edge's data: the value of metadata
    /// `id`, `None` when it was never set.
    ///
    /// # Errors
    ///
    /// An edge that does not exist.
    pub(crate) fn int_metadata(&self, edge: Edge, id: i32) -> Result<Option<i32>, ExecError> {
        Ok(self
            .data(edge)?
            .metadata()
            .iter()
            .find(|(key, _)| *key == id)
            .map(|&(_, value)| value))
    }
}

/// The local reference-parameter ID under which a graph keeps its
/// materialized MParam object (see [`GraphCx::mparam`]). Negative, so no
/// parameter a user can name collides with it.
pub(crate) const MPARAM_REF_ID: i32 = i32::MIN + 1;
