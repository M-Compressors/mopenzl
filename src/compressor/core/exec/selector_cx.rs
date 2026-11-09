//! The context of a selector (`ZL_Selector`, selector.c).

use super::super::data::Stream;
use super::super::error::ExecError;
use super::super::params::LocalParams;
use super::super::registry::{GraphId, NodeId, RuntimeParams};
use super::Exec;

/// What a selector sees: global and local parameters, trial compression,
/// and the means to parameterize the successor it picks.
pub(crate) struct SelectorCx<'s, 'e, 'a> {
    pub(crate) ex: &'s mut Exec<'e, 'a>,
    pub(crate) params: &'s LocalParams,
    pub(crate) depth: u32,
    pub(crate) successor_params: Option<LocalParams>,
}

impl<'e, 'a> SelectorCx<'_, 'e, 'a> {
    /// `ZL_CParam_compressionLevel`.
    pub(crate) const fn level(&self) -> i32 {
        self.ex.p.level
    }

    /// `ZL_CParam_formatVersion`.
    pub(crate) const fn format_version(&self) -> u32 {
        self.ex.p.version
    }

    /// `ZL_Selector_getGraphDepth`.
    pub(crate) const fn depth(&self) -> u32 {
        self.depth
    }

    /// `ZL_Selector_isNodeSupported`.
    pub(crate) fn is_node_supported(&self, node: NodeId) -> bool {
        self.ex.node_supported(node)
    }

    /// `ZL_Selector_setSuccessorParams`: the successor runs with these
    /// local parameters.
    pub(crate) fn set_successor_params(&mut self, params: LocalParams) {
        self.successor_params = Some(params);
    }

    /// `ZL_Selector_getInput0MaskForGraph`.
    pub(crate) fn input0_mask(&self, graph: GraphId) -> Option<u8> {
        self.ex.reg.graph(graph).ok().map(|def| def.port_mask(0))
    }

    /// `ZL_Selector_tryGraph`: the size of the whole frame `graph` would
    /// produce for `input`.
    ///
    /// # Errors
    ///
    /// Whatever fails the trial compression.
    pub(crate) fn try_graph(
        &mut self,
        input: &Stream<'_>,
        graph: GraphId,
    ) -> Result<usize, ExecError> {
        self.ex.try_graph(core::slice::from_ref(input), graph, None)
    }

    /// The runtime parameters the selected successor runs with.
    pub(crate) fn runtime_params(&mut self) -> Option<RuntimeParams> {
        self.successor_params.take().map(|params| RuntimeParams {
            params: Some(params),
            ..RuntimeParams::default()
        })
    }
}
