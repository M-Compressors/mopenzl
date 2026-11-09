//! The `constant` graph (`compress/selectors/selector_constant.c`).

use super::super::data::{Stream, Type};
use super::super::exec::SelectorCx;
use super::super::registry::{GraphId, SelectFn};

/// `SI_selector_constant` (`ZL_GRAPH_CONSTANT`): sends a verified constant
/// stream to `constant_serial` (serial input, or any element whose bytes
/// are all equal, which is cheaper) or `constant_fixed`.
///
/// No verification happens here: a stream that is not constant fails later,
/// at the constant node.
#[derive(Debug)]
pub(crate) struct ConstantSelector;

/// See [`ConstantSelector`].
pub(crate) static CONSTANT: ConstantSelector = ConstantSelector;

const GRAPH_CONSTANT_SERIAL: GraphId = GraphId::std(42);
const GRAPH_CONSTANT_FIXED: GraphId = GraphId::std(43);

/// `isSingleBytePattern`: whether every byte of the first element is the
/// same (`0x00..00`, `0xFF..FF`, `0x55..55`, ...).
fn is_single_byte_pattern(input: &Stream<'_>) -> bool {
    if input.n_elts() == 0 {
        return false;
    }
    let first_element = &input.bytes()[..input.width()];
    first_element.iter().all(|&byte| byte == first_element[0])
}

impl SelectFn for ConstantSelector {
    fn select(&self, _: &mut SelectorCx<'_, '_, '_>, input: &Stream<'_>, _: &[GraphId]) -> GraphId {
        if is_single_byte_pattern(input) {
            return GRAPH_CONSTANT_SERIAL;
        }
        match input.ty() {
            Type::Serial => GRAPH_CONSTANT_SERIAL,
            Type::Struct | Type::Numeric => GRAPH_CONSTANT_FIXED,
            // Strings are not accepted by the graph.
            Type::String => GraphId::ILLEGAL,
        }
    }
}
