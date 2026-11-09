//! The `merge_sorted` graph (`mergeSortedSelectorFnGraph`,
//! `encode_merge_sorted_binding.c`), private graph 63.
//!
//! Its two custom graphs are the merge-sorted pipeline (the node followed by
//! its bitset and merged-values graphs) and a backup. Inputs that are not
//! 32-bit, or that have more than 64 strictly increasing runs, go to the
//! backup.

use super::super::codecs::numeric::within_run_limit;
use super::super::error::ExecError;
use super::super::exec::{Edge, GraphCx};
use super::super::registry::GraphFn;

/// `merge_sorted`.
#[derive(Debug)]
pub(crate) struct MergeSortedGraph;

/// See [`MergeSortedGraph`].
pub(crate) static MERGE_SORTED_GRAPH: MergeSortedGraph = MergeSortedGraph;

impl GraphFn for MergeSortedGraph {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let input = inputs[0];
        let &[merge, backup] = cx.eff.custom_graphs else {
            return Err(ExecError::Parameter(
                "merge_sorted needs exactly two custom graphs",
            ));
        };
        let destination = {
            let stream = cx.data(input)?;
            if stream.width() != 4 || !within_run_limit(stream.bytes()) {
                backup
            } else {
                merge
            }
        };
        cx.set_destination(&[input], destination, None)
    }
}
