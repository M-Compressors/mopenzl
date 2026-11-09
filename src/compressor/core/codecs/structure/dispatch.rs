//! The dispatch nodes: `dispatchN_byTag` 5 (`encode_dispatchN_byTag_binding.c`)
//! and `dispatch_string` 32 (`encode_dispatch_string_binding.c`).
//!
//! `dispatchN_byTag` cuts a serial input into segments and routes every
//! segment to the output stream of its tag. Its outputs are the tag stream,
//! the segment-size stream and one serial stream per tag (each carrying the
//! channel metadata `ZL_DISPATCH_CHANNEL_ID`). The instructions
//! ([`DispatchInstructions`]) come from the reference parameter
//! `ZL_DISPATCH_INSTRUCTIONS_PID`; the parsing-function form
//! (`ZL_DISPATCH_PARSINGFN_PID`) is not supported and fails the node.
//!
//! `dispatch_string` routes whole strings to one of up to 2048 string
//! streams by a `u16` index array (`ZL_DISPATCH_STRING_INDICES_PID`, a
//! reference parameter or the little-endian bytes of a copy parameter) and
//! also emits that index array.

use alloc::vec::Vec;

use super::{DispatchInstructions, INSTRUCTIONS_PID, STRING_INDICES_PID, STRING_NUM_OUTPUTS_PID};
use crate::compressor::core::codecs::FnEncoder;
use super::util::{Param, local_param, push_le, width_for};
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;

/// `ZL_DISPATCH_PARSINGFN_PID`: a parsing function, not supported.
const PARSING_FN_PID: i32 = 519;
/// `ZL_DISPATCH_CHANNEL_ID`: the metadata every segment stream carries.
pub(crate) const DISPATCH_CHANNEL_ID: i32 = crate::graph::params::dispatch::CHANNEL_ID;
/// `ZL_DISPATCH_STRING_MAX_DISPATCHES`.
const STRING_MAX_DISPATCHES: usize = 2048;

/// `EI_dispatchN_byTag`.
pub(crate) static DISPATCH_N_BY_TAG: FnEncoder = FnEncoder(dispatch_n_by_tag);
/// `EI_dispatch_string`.
pub(crate) static DISPATCH_STRING: FnEncoder = FnEncoder(dispatch_string);

/// `getSplitInstructions` of the dispatch node.
fn instructions(
    cx: &NodeCx<'_, '_>,
    input: &Stream<'_>,
) -> Result<DispatchInstructions, NodeError> {
    if input.n_elts() == 0 {
        // An empty input has no segments.
        return Ok(DispatchInstructions::default());
    }
    if let Some(object) = cx.ref_param(INSTRUCTIONS_PID) {
        return object
            .downcast_ref::<DispatchInstructions>()
            .cloned()
            .ok_or(NodeError::InvalidParam(
                "the dispatch instructions have the wrong type",
            ));
    }
    if cx.copy_param(PARSING_FN_PID).is_some() {
        return Err(NodeError::UnsupportedParam("dispatch parsing functions"));
    }
    Err(NodeError::InvalidParam("dispatchN parser not provided"))
}

fn dispatch_n_by_tag<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let instructions = instructions(cx, input)?;
    let input_size = input.n_elts();
    let sizes = &instructions.segment_sizes;
    let max_segment_size = sizes.iter().copied().max().unwrap_or(0);
    let tag_limit = if cx.format_version() < 20 {
        1 << 8
    } else {
        1 << 16
    };
    if instructions.nb_tags >= tag_limit {
        return Err(NodeError::InvalidParam("too many dispatch tags"));
    }
    if max_segment_size > input_size {
        return Err(NodeError::InvalidParam(
            "one of the segment sizes is bigger than the input size",
        ));
    }
    let total = sizes
        .iter()
        .try_fold(0usize, |total, &size| total.checked_add(size));
    if total != Some(input_size) {
        return Err(NodeError::InvalidParam(
            "the dispatch instructions have an invalid total size",
        ));
    }
    let nb_segments = sizes.len();
    let tags = instructions
        .tags
        .get(..nb_segments)
        .ok_or(NodeError::InvalidParam("one dispatch tag per segment"))?;
    if !tags.iter().all(|&tag| (tag as usize) < instructions.nb_tags) {
        return Err(NodeError::InvalidParam("invalid dispatch tags"));
    }

    let tags_width = if instructions.nb_tags == 0 {
        1
    } else {
        width_for(instructions.nb_tags as u64 - 1)
    };
    let sizes_width = width_for(max_segment_size as u64);

    let mut tag_bytes = Vec::with_capacity(nb_segments * tags_width);
    for &tag in tags {
        push_le(&mut tag_bytes, u64::from(tag), tags_width);
    }
    cx.emit(0, tags_width, tag_bytes)?;
    let mut size_bytes = Vec::with_capacity(nb_segments * sizes_width);
    for &size in sizes {
        push_le(&mut size_bytes, size as u64, sizes_width);
    }
    cx.emit(1, sizes_width, size_bytes)?;

    let mut streams: Vec<Vec<u8>> = (0..instructions.nb_tags).map(|_| Vec::new()).collect();
    let mut rest = input.bytes();
    for (&size, &tag) in sizes.iter().zip(tags) {
        let (segment, tail) = rest.split_at(size);
        streams[tag as usize].extend_from_slice(segment);
        rest = tail;
    }
    for (n, bytes) in streams.into_iter().enumerate() {
        let output = cx.emit(2, 1, bytes)?;
        cx.set_output_metadata(output, DISPATCH_CHANNEL_ID, n as i32)?;
    }
    Ok(())
}

fn dispatch_string<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let nb_elts = input.n_elts();
    // `(size_t)int`: a negative count wraps to a huge one and is rejected.
    let nb_outputs = cx.int(STRING_NUM_OUTPUTS_PID).unwrap_or(0) as usize;
    if nb_outputs > STRING_MAX_DISPATCHES {
        return Err(NodeError::InvalidParam(
            "dispatch_string: invalid number of outputs",
        ));
    }
    if nb_elts > 0 && nb_outputs == 0 {
        return Err(NodeError::InvalidParam(
            "dispatch_string: ill-formed degenerate case",
        ));
    }
    let indices: Vec<u16> = match local_param(cx, STRING_INDICES_PID) {
        Some(Param::Object(object)) => object
            .downcast_ref::<Vec<u16>>()
            .cloned()
            .ok_or(NodeError::InvalidParam(
                "dispatch_string: the indices have the wrong type",
            ))?,
        Some(Param::Bytes(bytes)) => bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect(),
        None => {
            return Err(NodeError::InvalidParam(
                "dispatch_string: indices pointer is null",
            ));
        }
    };
    let indices = indices
        .get(..nb_elts)
        .ok_or(NodeError::InvalidParam(
            "dispatch_string: one index per string",
        ))?;
    if !indices.iter().all(|&index| usize::from(index) < nb_outputs) {
        return Err(NodeError::InvalidParam(
            "dispatch index out of bounds",
        ));
    }

    let mut index_bytes = Vec::with_capacity(nb_elts * 2);
    for &index in indices {
        index_bytes.extend_from_slice(&index.to_le_bytes());
    }
    cx.emit(0, 2, index_bytes)?;

    if nb_elts > 0 {
        let mut contents: Vec<Vec<u8>> = (0..nb_outputs).map(|_| Vec::new()).collect();
        let mut lens: Vec<Vec<u32>> = (0..nb_outputs).map(|_| Vec::new()).collect();
        let mut rest = input.bytes();
        for (&len, &index) in input.lens().iter().zip(indices) {
            let (string, tail) = rest.split_at(len as usize);
            contents[usize::from(index)].extend_from_slice(string);
            lens[usize::from(index)].push(len);
            rest = tail;
        }
        for (content, lens) in contents.into_iter().zip(lens) {
            cx.emit_strings(1, content, lens)?;
        }
    }
    Ok(())
}
