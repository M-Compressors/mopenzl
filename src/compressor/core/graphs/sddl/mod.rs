//! The SDDL graph (`ZL_GRAPH_SDDL`, graph 20): `ZL_SDDL_dynGraph` of
//! `compress/graphs/sddl/simple_data_description_language.c`.
//!
//! The graph parses a serial input with a compiled *description* (a CBOR
//! program, the copy parameter [`DESCRIPTION_PID`]) and cuts the input into
//! fields routed to numbered destinations:
//!
//! 1. the [`state`] interpreter runs the description and produces dispatch
//!    instructions plus, per destination, the type, width and endianness of
//!    what it received;
//! 2. the `dispatchN_byTag` node runs with those instructions; its first two
//!    outputs are the tags and segment sizes, then one stream per
//!    destination;
//! 3. every numeric destination is converted from serial with the
//!    little- or big-endian `convert_serial_to_num` node of its width;
//! 4. every stream gets the integer metadata `0 = its position`, and all of
//!    them go to the single custom graph as one multi-input destination.
//!
//! The description is read with the shared CBOR codec (`shared::cbor`) in
//! the configuration the reference loader uses (`DecoderConfig::STRICT`).

mod error;
mod program;
mod state;

use super::super::codecs::structure::DispatchInstructions;
use super::super::error::ExecError;
use super::super::exec::{Edge, GraphCx};
use super::super::registry::{GraphFn, NodeId};
use super::run_dispatch_node;
use program::AtomType;

/// `ZL_SDDL_DESCRIPTION_PID`: the copy parameter holding the compiled
/// description.
pub(crate) const DESCRIPTION_PID: i32 = crate::graph::params::sddl::DESCRIPTION;

/// `ZL_SDDL_dynGraph`.
#[derive(Debug)]
pub(crate) struct Sddl;

/// See [`Sddl`].
pub(crate) static SDDL: Sddl = Sddl;

/// Whether `description` is a compiled SDDL description the interpreter can
/// load (`ZL_SDDL_Program_load`). It does not run it.
pub(crate) fn validate(description: &[u8]) -> bool {
    program::load(description).is_ok()
}

/// The conversion node for a numeric destination of `width` bytes
/// (`ZL_Node_convertSerialToNum{LE,BE}`): `convert_serial_to_num8` is shared
/// by both endiannesses.
fn conversion_node(width: usize, big_endian: bool) -> Option<NodeId> {
    let index = match (width, big_endian) {
        (1, _) => 19,
        (2, false) => 20,
        (4, false) => 21,
        (8, false) => 22,
        (2, true) => 23,
        (4, true) => 24,
        (8, true) => 25,
        _ => return None,
    };
    Some(NodeId::std(index))
}

impl GraphFn for Sddl {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let [input] = inputs else {
            return Err(ExecError::Parameter("sddl takes exactly one input"));
        };
        if !cx.eff.custom_nodes.is_empty() {
            return Err(ExecError::Parameter("sddl takes no custom node"));
        }
        let [successor] = cx.eff.custom_graphs else {
            return Err(ExecError::Parameter("sddl needs exactly one custom graph"));
        };
        let successor = *successor;
        let description = cx
            .eff
            .params
            .copy(DESCRIPTION_PID)
            .filter(|description| !description.is_empty())
            .ok_or(ExecError::Parameter("sddl needs a description"))?;
        let program = program::load(description)?;
        let instructions = state::run(&program, cx.data(*input)?.bytes())?;

        let dispatch = DispatchInstructions {
            segment_sizes: instructions.segment_sizes,
            tags: instructions.tags,
            nb_tags: instructions.outputs.len(),
        };
        let edges = run_dispatch_node(cx, *input, &dispatch)?;
        if edges.len() != instructions.outputs.len() + 2 {
            return Err(ExecError::Parameter(
                "dispatch produced an unexpected number of outputs",
            ));
        }
        let mut converted = edges.clone();
        for (index, info) in instructions.outputs.iter().enumerate() {
            if info.width == 0 {
                continue;
            }
            if info.ty == AtomType::Numeric {
                let node = conversion_node(info.width, info.big_endian).ok_or(
                    ExecError::Parameter("unhandled output stream width from dispatch"),
                )?;
                let out = cx.run_node(node, &[edges[index + 2]], None)?;
                let [edge] = out[..] else {
                    return Err(ExecError::SuccessorInputs);
                };
                converted[index + 2] = edge;
            }
        }
        for (index, &edge) in converted.iter().enumerate() {
            cx.set_metadata(edge, 0, index as i32)?;
        }
        cx.set_destination(&converted, successor, None)
    }
}

#[cfg(test)]
mod tests;
