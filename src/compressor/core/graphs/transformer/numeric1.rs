//! `transformer_numeric` (graph 30) and `transformer_numeric1` (graph 90):
//! `MultiInputGraph_transformerNumeric` and `SI_transformer_numeric_select`
//! (`selector_numeric.c`).

use super::super::super::data::{Stream, Type};
use super::super::super::error::ExecError;
use super::super::super::exec::{Edge, GraphCx, SelectorCx};
use super::super::super::registry::{GraphFn, GraphId, NodeId, SelectFn, gid};
use super::MAX_MODEL_DEPTH;
use super::decide::{
    SupportedOperations, decide8, decide16, decide32, decide64, select_supported_operation,
};
use super::ids;
use super::scorers::Op;

/// `ZL_NODE_CONSTANT_FIXED`.
const NODE_CONSTANT_FIXED: NodeId = NodeId::std(71);
/// `ZL_NODE_CONSTANT_SERIAL`.
const NODE_CONSTANT_SERIAL: NodeId = NodeId::std(70);
/// `ZL_NODE_DIVIDE_BY`.
const NODE_DIVIDE_BY: NodeId = NodeId::std(31);
/// `ZL_NODE_SPARSE_NUM`.
const NODE_SPARSE_NUM: NodeId = NodeId::std(54);

/// `MultiInputGraph_transformerNumeric`: every input goes to
/// `transformer_numeric1`.
#[derive(Debug)]
pub(crate) struct TransformerNumeric;

/// `SI_transformer_numeric_select`: the model-driven selector.
#[derive(Debug)]
pub(crate) struct TransformerNumeric1;

/// See [`TransformerNumeric`].
pub(crate) static TRANSFORMER_NUMERIC: TransformerNumeric = TransformerNumeric;
/// See [`TransformerNumeric1`].
pub(crate) static TRANSFORMER_NUMERIC1: TransformerNumeric1 = TransformerNumeric1;

impl GraphFn for TransformerNumeric {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        for &input in inputs {
            cx.set_destination(&[input], gid::TRANSFORMER_NUMERIC1, None)?;
        }
        Ok(())
    }
}

/// `transformer_graph_for_operation`.
const fn graph_for_operation(operation: Op) -> GraphId {
    match operation {
        Op::Constant => ids::CONSTANT,
        Op::Bitpack => ids::BITPACK,
        Op::DeltaInt => ids::DELTA_INT,
        Op::DivideByGcd => ids::DIVIDE_BY_GCD,
        Op::Entropy => ids::ENTROPY,
        Op::FieldLz => ids::FIELD_LZ,
        Op::Fse => ids::FSE,
        Op::RangePack => ids::RANGE_PACK,
        Op::SparseNum => ids::SPARSE_NUM,
        Op::Store => ids::STORE,
        Op::TokenizeNumeric => ids::TOKENIZE_NUMERIC,
        Op::TokenizeNumericSorted => ids::TOKENIZE_NUMERIC_SORTED,
        Op::Zstd => ids::ZSTD,
        Op::Invalid => ids::STATIC_FALLBACK,
    }
}

impl SelectFn for TransformerNumeric1 {
    fn select(
        &self,
        cx: &mut SelectorCx<'_, '_, '_>,
        input: &Stream<'_>,
        _: &[GraphId],
    ) -> GraphId {
        if input.ty() != Type::Numeric {
            return GraphId::ILLEGAL;
        }
        let supported = SupportedOperations {
            constant: cx.is_node_supported(NODE_CONSTANT_FIXED)
                && cx.is_node_supported(NODE_CONSTANT_SERIAL),
            divide_by_gcd: cx.is_node_supported(NODE_DIVIDE_BY),
            sparse_num: cx.is_node_supported(NODE_SPARSE_NUM),
        };
        numeric1_select(cx.depth(), input.width(), input.bytes(), supported)
    }
}

/// `SI_transformer_numeric_select` on `bytes`, little-endian numbers of
/// `width` bytes (1, 2, 4 or 8), at graph depth `depth`.
pub(super) fn numeric1_select(
    depth: u32,
    width: usize,
    bytes: &[u8],
    supported: SupportedOperations,
) -> GraphId {
    let n_elts = bytes.len() / width;
    if n_elts == 0 {
        return ids::STORE;
    }
    // The trained sub-64 feature contract uses 32-bit element counts.
    if width < 8 && n_elts as u64 > u64::from(u32::MAX) {
        return ids::STATIC_FALLBACK;
    }
    if depth > MAX_MODEL_DEPTH {
        return ids::STATIC_FALLBACK;
    }
    let decision = match width {
        1 => decide8(bytes),
        2 => decide16(bytes),
        4 => decide32(bytes),
        8 => decide64(bytes),
        // The registry only admits widths 1, 2, 4 and 8.
        _ => return ids::STATIC_FALLBACK,
    };
    graph_for_operation(select_supported_operation(&decision, supported))
}
