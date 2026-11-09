//! The Transformer numeric graphs (`compress/selectors/transformer/*`,
//! `selector_numeric.c`, and the `transformer_*` rows of `graph_registry.c`).
//!
//! A numeric stream at compression level 7 and above (`select_numeric`)
//! enters `transformer_numeric1`, whose selector extracts statistical
//! features, scores the candidate operations with a small neural network
//! (width-specific, generated from the reference by
//! `scripts/gen_transformer_scorers.py`) and routes the stream to the best
//! supported one. Operations that transform the stream (`delta`,
//! `range_pack`, `divide_by`, `tokenize`, `sparse_num`) come back to
//! `transformer_numeric1` for their outputs, up to a depth of 24. When the
//! model has no supported answer, the heuristic `transformer_static_*`
//! selectors decide.
//!
//! ```mermaid
//! graph TD
//!     N[transformer_numeric 30] -->|each input| N1[transformer_numeric1 90]
//!     N1 -->|model| OPS[delta, range_pack, gcd, tokenize, sparse: 70-75]
//!     N1 -->|model| TERM[constant, bitpack, entropy, fse, field_lz, zstd, store]
//!     N1 -->|no answer| FB[static_fallback 89]
//!     OPS --> N1
//!     FB --> CORE[static_core 76]
//!     FB --> TS[static tok_sorted / delta_tok / mono: 83-88]
//!     TS --> IDX[static_index 82] --> CORE
//! ```
//!
//! Floating point follows the reference's order of evaluation exactly (see
//! [`scorers`]); the module is the only consumer of
//! [`shared::estimate`](crate::shared::estimate).

mod cardinality;
mod decide;
mod features;
mod math;
mod numeric1;
mod scorers;
mod static_select;
mod stats;
mod wide;

pub(crate) use numeric1::{TRANSFORMER_NUMERIC, TRANSFORMER_NUMERIC1};
pub(crate) use static_select::{STATIC_CORE, STATIC_FALLBACK, STATIC_INDEX};

use super::super::registry::GraphId;

/// `TRS_TRANSFORMER_MAX_MODEL_DEPTH`.
const MAX_MODEL_DEPTH: u32 = 24;
/// `TRS_TRANSFORMER_STATIC_DEPTH_MARGIN`.
const STATIC_DEPTH_MARGIN: u32 = 6;
/// `TRS_TRANSFORMER_MAX_STATIC_DEPTH`.
const MAX_STATIC_DEPTH: u32 = MAX_MODEL_DEPTH + STATIC_DEPTH_MARGIN;

/// The graph IDs the selectors choose among (`ZL_GRAPH_*`).
mod ids {
    use super::GraphId;

    pub(super) const STORE: GraphId = GraphId::std(2);
    pub(super) const FSE: GraphId = GraphId::std(3);
    pub(super) const ENTROPY: GraphId = GraphId::std(7);
    pub(super) const CONSTANT: GraphId = GraphId::std(8);
    pub(super) const ZSTD: GraphId = GraphId::std(9);
    pub(super) const BITPACK: GraphId = GraphId::std(10);
    pub(super) const FIELD_LZ: GraphId = GraphId::std(12);
    pub(super) const DELTA_INT: GraphId = GraphId::std(70);
    pub(super) const RANGE_PACK: GraphId = GraphId::std(71);
    pub(super) const DIVIDE_BY_GCD: GraphId = GraphId::std(72);
    pub(super) const TOKENIZE_NUMERIC: GraphId = GraphId::std(73);
    pub(super) const TOKENIZE_NUMERIC_SORTED: GraphId = GraphId::std(74);
    pub(super) const SPARSE_NUM: GraphId = GraphId::std(75);
    pub(super) const STATIC_CORE: GraphId = GraphId::std(76);
    pub(super) const STATIC_DELTA: GraphId = GraphId::std(77);
    pub(super) const STATIC_RANGE_PACK: GraphId = GraphId::std(78);
    pub(super) const STATIC_RANGE_PACK_DELTA: GraphId = GraphId::std(79);
    pub(super) const STATIC_ZIGZAG: GraphId = GraphId::std(80);
    pub(super) const STATIC_GCD: GraphId = GraphId::std(81);
    pub(super) const STATIC_TOK_SORTED: GraphId = GraphId::std(83);
    pub(super) const STATIC_DELTA_TOK: GraphId = GraphId::std(84);
    pub(super) const STATIC_DELTA_TOK_MONO: GraphId = GraphId::std(86);
    pub(super) const STATIC_DELTA_TOK_MONO_LZ: GraphId = GraphId::std(88);
    pub(super) const STATIC_FALLBACK: GraphId = GraphId::std(89);
}

#[cfg(test)]
mod tests;
