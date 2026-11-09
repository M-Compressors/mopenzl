//! The `bitpack` selector (`SI_selector_bitpack`, `encode_bitpack_binding.c`),
//! graph 10.

use super::super::data::{Stream, Type};
use super::super::exec::SelectorCx;
use super::super::registry::{GraphId, SelectFn};

/// `ZL_GRAPH_BITPACK_SERIAL` (the private `bitpack_serial` static graph).
const BITPACK_SERIAL: GraphId = GraphId::std(40);
/// `ZL_GRAPH_BITPACK_INT` (the private `bitpack_int` static graph).
const BITPACK_INT: GraphId = GraphId::std(41);

/// Numeric input goes to `bitpack_int`, serial input to `bitpack_serial`.
#[derive(Debug)]
pub(crate) struct BitpackSelector;

/// See [`BitpackSelector`].
pub(crate) static BITPACK: BitpackSelector = BitpackSelector;

impl SelectFn for BitpackSelector {
    fn select(&self, _: &mut SelectorCx<'_, '_, '_>, input: &Stream<'_>, _: &[GraphId]) -> GraphId {
        if input.ty() == Type::Numeric {
            BITPACK_INT
        } else {
            BITPACK_SERIAL
        }
    }
}
