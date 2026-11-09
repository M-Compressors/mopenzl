//! The store graphs (`selector_store.c`).

use super::super::data::{Stream, Type};
use super::super::error::ExecError;
use super::super::exec::{Edge, GraphCx, SelectorCx};
use super::super::registry::{GraphFn, GraphId, SelectFn, gid};

/// `MultiInputGraph_store` (`ZL_GRAPH_STORE`): every input goes to `store1`.
#[derive(Debug)]
pub(crate) struct StoreGraph;

/// `SI_selector_store` (`store1`): strings are separated into components,
/// everything else goes to `serial_store` (after an implicit conversion for
/// struct and numeric data, which is why storing typed data emits a codec).
#[derive(Debug)]
pub(crate) struct Store1;

/// See [`StoreGraph`].
pub(crate) static STORE: StoreGraph = StoreGraph;
/// See [`Store1`].
pub(crate) static STORE1: Store1 = Store1;

impl GraphFn for StoreGraph {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        for &input in inputs {
            cx.set_destination(&[input], gid::STORE1, None)?;
        }
        Ok(())
    }
}

impl SelectFn for Store1 {
    fn select(&self, _: &mut SelectorCx<'_, '_, '_>, input: &Stream<'_>, _: &[GraphId]) -> GraphId {
        if input.ty() == Type::String {
            gid::STRING_STORE
        } else {
            gid::SERIAL_STORE
        }
    }
}
