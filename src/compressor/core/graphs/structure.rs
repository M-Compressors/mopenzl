//! Graphs of the structure codecs: `try_parse_int` (`encode_parse_int_binding.c`),
//! the private `split_*` graphs and `n_to_n` (`graphs/split_graph.c`), the
//! `transpose_split` width selector (`encode_transpose_binding.c`), and the
//! helpers the public graph API offers to function graphs that want to run
//! these nodes with runtime parameters: `ZL_Edge_runSplitNode`,
//! `ZL_Edge_runDispatchNode` and `ZL_Edge_runDispatchStringNode`.
//!
//! The nodes take their parameters as reference parameters here: segment
//! sizes as a `Vec<usize>`, dispatch instructions as a
//! [`DispatchInstructions`], indices as a `Vec<u16>`, pre-parsed integers as
//! a `Vec<i64>`.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

use super::super::codecs::structure::{
    DispatchInstructions, INSTRUCTIONS_PID, PREPARSED_PARAMS_PID, SEGMENT_SIZES_PID,
    STRING_INDICES_PID, STRING_NUM_OUTPUTS_PID, parse_int64,
};
use super::super::data::Type;
use super::super::error::ExecError;
use super::super::exec::{Edge, GraphCx};
use super::super::params::{LocalParams, RefParam};
use super::super::registry::{GraphFn, NodeId};

/// `ZL_NODE_DISPATCH` (`dispatchN_byTag`).
const NODE_DISPATCH: NodeId = NodeId::std(5);
/// `ZL_NODE_DISPATCH_STRING`.
const NODE_DISPATCH_STRING: NodeId = NodeId::std(32);
/// `ZL_NODE_PARSE_INT`.
const NODE_PARSE_INT: NodeId = NodeId::std(38);
/// `ZL_PrivateStandardNodeID_splitN`.
const NODE_SPLIT_N: NodeId = NodeId::std(66);
/// `ZL_PrivateStandardNodeID_splitN_struct`.
const NODE_SPLIT_N_STRUCT: NodeId = NodeId::std(67);
/// `ZL_PrivateStandardNodeID_splitN_num`.
const NODE_SPLIT_N_NUM: NodeId = NodeId::std(68);

/// `ZL_DISPATCH_STRING_MAX_DISPATCHES`.
const DISPATCH_STRING_MAX: i32 = 2048;

/// Local parameters made of reference parameters only.
fn ref_params(refs: Vec<(i32, Arc<dyn Any + Send + Sync>)>) -> LocalParams {
    LocalParams {
        refs: refs
            .into_iter()
            .map(|(id, object)| (id, RefParam(object)))
            .collect(),
        ..LocalParams::EMPTY
    }
}

/// `ZL_Edge_runSplitNode`: runs the `splitN` node that fits the type of
/// `edge`, cutting it into consecutive segments of `segment_sizes` elements.
/// A last size of 0 takes the rest.
///
/// # Errors
///
/// More than `i32::MAX` segments, a string input (there is no string split
/// node), or whatever fails the node.
pub(crate) fn run_split_node(
    cx: &mut GraphCx<'_, '_, '_>,
    edge: Edge,
    segment_sizes: &[usize],
) -> Result<Vec<Edge>, ExecError> {
    if segment_sizes.len() > i32::MAX as usize {
        return Err(ExecError::Parameter(
            "nbSegments is too large (temporary limitation)",
        ));
    }
    let node = match cx.data(edge)?.ty() {
        Type::Serial => NODE_SPLIT_N,
        Type::Struct => NODE_SPLIT_N_STRUCT,
        Type::Numeric => NODE_SPLIT_N_NUM,
        Type::String => return Err(ExecError::NodeInvalid),
    };
    let params = ref_params(alloc::vec![(
        SEGMENT_SIZES_PID,
        Arc::new(segment_sizes.to_vec())
    )]);
    cx.run_node(node, &[edge], Some(&params))
}

/// `ZL_Edge_runDispatchNode`: runs `dispatchN_byTag` on a serial `edge`
/// with `instructions`.
///
/// # Errors
///
/// Whatever fails the node.
pub(crate) fn run_dispatch_node(
    cx: &mut GraphCx<'_, '_, '_>,
    edge: Edge,
    instructions: &DispatchInstructions,
) -> Result<Vec<Edge>, ExecError> {
    let params = ref_params(alloc::vec![(
        INSTRUCTIONS_PID,
        Arc::new(instructions.clone())
    )]);
    cx.run_node(NODE_DISPATCH, &[edge], Some(&params))
}

/// `ZL_Edge_runDispatchStringNode`: runs `dispatch_string` on a string
/// `edge`, sending string `i` to output `indices[i]` of `nb_outputs`.
///
/// # Errors
///
/// A number of outputs outside `0..=2048`, or whatever fails the node.
pub(crate) fn run_dispatch_string_node(
    cx: &mut GraphCx<'_, '_, '_>,
    edge: Edge,
    nb_outputs: i32,
    indices: &[u16],
) -> Result<Vec<Edge>, ExecError> {
    if !(0..=DISPATCH_STRING_MAX).contains(&nb_outputs) {
        return Err(ExecError::Parameter(
            "dispatch_string: invalid number of outputs",
        ));
    }
    let mut params = ref_params(alloc::vec![(
        STRING_INDICES_PID,
        Arc::new(indices.to_vec())
    )]);
    params.ints = alloc::borrow::Cow::Owned(alloc::vec![(STRING_NUM_OUTPUTS_PID, nb_outputs)]);
    cx.run_node(NODE_DISPATCH_STRING, &[edge], Some(&params))
}

/// `parseIntSafeFnGraph` (`ZL_GRAPH_TRY_PARSE_INT`): parses every string of
/// a string input as an integer.
///
/// Custom graphs: `[integers, exceptions, indices]`. If every string parses,
/// the integers go to the first. If none does, the whole input goes to the
/// second. Otherwise `dispatch_string` separates the parsable strings (to
/// `parse_int`, then the first graph) from the exceptions (the second
/// graph), and its index stream goes to the third.
#[derive(Debug)]
pub(crate) struct TryParseInt;

/// See [`TryParseInt`].
pub(crate) static TRY_PARSE_INT: TryParseInt = TryParseInt;

impl GraphFn for TryParseInt {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let input = cx.data(inputs[0])?;
        let nb_elts = input.n_elts();
        let mut parsed: Vec<i64> = Vec::new();
        let mut indices: Vec<u16> = Vec::with_capacity(nb_elts);
        let mut rest = input.bytes();
        for &len in input.lens() {
            let (text, tail) = rest.split_at(len as usize);
            match parse_int64(text) {
                Some(value) => {
                    parsed.push(value);
                    indices.push(0);
                }
                None => indices.push(1),
            }
            rest = tail;
        }
        let successors = cx.eff.custom_graphs;
        let [numbers, exceptions, dispatch_indices] = successors else {
            return Err(ExecError::Parameter(
                "try_parse_int needs exactly three custom graphs",
            ));
        };
        let (numbers, exceptions, dispatch_indices) = (*numbers, *exceptions, *dispatch_indices);
        let num_parsed = parsed.len();
        let preparsed =
            |values: Vec<i64>| ref_params(alloc::vec![(PREPARSED_PARAMS_PID, Arc::new(values))]);
        if num_parsed == nb_elts {
            let params = preparsed(parsed);
            let out = cx.run_node(NODE_PARSE_INT, &[inputs[0]], Some(&params))?;
            cx.set_destination(&out[..1], numbers, None)?;
        } else if num_parsed == 0 {
            cx.set_destination(&inputs[..1], exceptions, None)?;
        } else {
            let dispatched = run_dispatch_string_node(cx, inputs[0], 2, &indices)?;
            cx.set_destination(&dispatched[..1], dispatch_indices, None)?;
            let params = preparsed(parsed);
            let out = cx.run_node(NODE_PARSE_INT, &dispatched[1..2], Some(&params))?;
            cx.set_destination(&out[..1], numbers, None)?;
            cx.set_destination(&dispatched[2..3], exceptions, None)?;
        }
        Ok(())
    }
}

/// `ZL_splitFnGraph` (the `split_serial`, `split_struct`, `split_numeric`
/// and `split_string` graphs): runs the one custom node on the input and
/// sends output `i` to custom graph `i`.
#[derive(Debug)]
pub(crate) struct SplitGraph;

/// See [`SplitGraph`].
pub(crate) static SPLIT: SplitGraph = SplitGraph;

impl GraphFn for SplitGraph {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let [node] = cx.eff.custom_nodes else {
            return Err(ExecError::Parameter("a split graph needs exactly one node"));
        };
        let node = *node;
        let fields = cx.run_node(node, &inputs[..1], None)?;
        let graphs = cx.eff.custom_graphs;
        if graphs.len() != fields.len() {
            return Err(ExecError::Parameter(
                "a split graph needs one custom graph per output",
            ));
        }
        for (field, &graph) in fields.iter().zip(graphs) {
            cx.set_destination(&[*field], graph, None)?;
        }
        Ok(())
    }
}

/// `ZL_nToNFnGraph` (`n_to_n`): input `i` goes to custom graph `i`.
#[derive(Debug)]
pub(crate) struct NToN;

/// See [`NToN`].
pub(crate) static N_TO_N: NToN = NToN;

impl GraphFn for NToN {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let graphs = cx.eff.custom_graphs;
        if graphs.len() != inputs.len() {
            return Err(ExecError::Parameter(
                "n_to_n needs one custom graph per input",
            ));
        }
        for (&input, &graph) in inputs.iter().zip(graphs) {
            cx.set_destination(&[input], graph, None)?;
        }
        Ok(())
    }
}

/// `transposeSplitSelectorFnGraph` (`ZL_GRAPH_TRANSPOSE_SPLIT`): picks the
/// transpose implementation. Custom graphs: the width 1, 2, 4 and 8
/// deprecated implementations, then the `transpose_split` node's graph.
/// From format version 11 the last is always used.
#[derive(Debug)]
pub(crate) struct TransposeSplitSelector;

/// See [`TransposeSplitSelector`].
pub(crate) static TRANSPOSE_SPLIT_SELECTOR: TransposeSplitSelector = TransposeSplitSelector;

/// The first format version with the `transpose_split` node.
const TRANSPOSE_SPLIT_MIN_VERSION: u32 = 11;

impl GraphFn for TransposeSplitSelector {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let [width1, width2, width4, width8, split] = cx.eff.custom_graphs else {
            return Err(ExecError::Parameter(
                "transpose_split needs exactly five custom graphs",
            ));
        };
        let (width1, width2, width4, width8, split) = (*width1, *width2, *width4, *width8, *split);
        let destination = if cx.ex.p.version >= TRANSPOSE_SPLIT_MIN_VERSION {
            split
        } else {
            match cx.data(inputs[0])?.width() {
                1 => width1,
                2 => width2,
                4 => width4,
                8 => width8,
                _ => return Err(ExecError::Parameter("invalid input element width")),
            }
        };
        cx.set_destination(&inputs[..1], destination, None)
    }
}
