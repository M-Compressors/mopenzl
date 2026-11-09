//! Frames of the structure codecs the generic graph builder of [`graph`]
//! cannot express (`shim/structure.c`): multi-input nodes such as
//! `concat_*`, `dedup_num`, `interleave_string` and `mux_lengths`, the
//! pointer-parameterized `dispatchN_byTag` and the general `sentinel`.
//!
//! [`graph`]: crate::graph

use core::ffi::{c_int, c_void};

use crate::graph::{Input, cparam};

/// Raw entry points of `shim/structure.c`.
pub mod sys {
    use core::ffi::{c_int, c_void};

    unsafe extern "C" {
        /// See `mzl_structure_compress` in `shim/structure.c`.
        pub fn mzl_structure_compress(
            dst: *mut c_void,
            dst_capacity: usize,
            n_inputs: usize,
            types: *const u32,
            srcs: *const *const c_void,
            sizes: *const usize,
            widths: *const usize,
            lens: *const *const u32,
            n_lens: *const usize,
            mode: u32,
            node: u32,
            ints: *const c_int,
            n_ints: usize,
            copy_ids: *const c_int,
            copy_ptrs: *const *const c_void,
            copy_sizes: *const usize,
            n_copies: usize,
            segments: *const usize,
            tags: *const u32,
            n_segments: usize,
            n_tags: u32,
            value: u64,
            cparams: *const c_int,
            n_cparams: usize,
            error: *mut c_int,
        ) -> usize;
        /// The node ID `ZL_NODE_*` number `which` stands for.
        pub fn mzl_structure_node(which: u32) -> u32;
    }
}

/// Nodes the generic builder cannot name by a public number.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Node {
    /// `ZL_NODE_CONCAT_SERIAL`.
    ConcatSerial = 0,
    /// `ZL_NODE_CONCAT_NUMERIC`.
    ConcatNumeric = 1,
    /// `ZL_NODE_CONCAT_STRUCT`.
    ConcatStruct = 2,
    /// `ZL_NODE_CONCAT_STRING`.
    ConcatString = 3,
    /// `ZL_NODE_DEDUP_NUMERIC`.
    DedupNumeric = 4,
    /// `ZL_NODE_INTERLEAVE_STRING`.
    InterleaveString = 5,
    /// `ZL_NODE_MUX_LENGTHS`.
    MuxLengths = 6,
    /// `ZL_NODE_SENTINEL_BYTE`.
    SentinelByte = 7,
    /// `ZL_NODE_TRANSPOSE_SPLIT`.
    TransposeSplit = 8,
    /// `ZL_NODE_DISPATCH_STRING`.
    DispatchString = 9,
    /// `ZL_NODE_SPLIT_BY_STRUCT` (private).
    SplitByStruct = 10,
    /// `ZL_PrivateStandardNodeID_splitN`.
    SplitN = 11,
    /// `ZL_PrivateStandardNodeID_splitN_struct`.
    SplitNStruct = 12,
    /// `ZL_PrivateStandardNodeID_splitN_num`.
    SplitNNum = 13,
}

impl Node {
    /// The `ZL_NodeID` number, for [`crate::graph::Graph::node`].
    pub fn id(self) -> u32 {
        // SAFETY: a pure lookup.
        unsafe { sys::mzl_structure_node(self as u32) }
    }
}

/// How the function graph drives the codec.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Run {
    /// Runs `node` over all the inputs, with local parameters.
    Node {
        /// The node.
        node: Node,
        /// `(paramId, value)` local integer parameters.
        ints: Vec<(i32, i32)>,
        /// `(paramId, bytes)` local copy parameters.
        copies: Vec<(i32, Vec<u8>)>,
    },
    /// Runs the node with the raw numeric ID `node` over all the inputs,
    /// with local parameters.
    RawNode {
        /// The `ZL_NodeID` number.
        node: u32,
        /// `(paramId, value)` local integer parameters.
        ints: Vec<(i32, i32)>,
        /// `(paramId, bytes)` local copy parameters.
        copies: Vec<(i32, Vec<u8>)>,
    },
    /// `ZL_Edge_runDispatchNode` on the single input: segment `i` of
    /// `sizes[i]` bytes goes to stream `tags[i]` of `tags_count`.
    Dispatch {
        /// Segment sizes.
        sizes: Vec<usize>,
        /// Segment tags.
        tags: Vec<u32>,
        /// Number of streams.
        tags_count: u32,
    },
    /// `ZL_Edge_runSentinelNode`: the input elements at `indices` become
    /// exceptions, marked by `sentinel`.
    Sentinel {
        /// Sorted element indices.
        indices: Vec<usize>,
        /// The sentinel value.
        sentinel: u64,
    },
}

/// Compresses `inputs` through a function graph that runs `run` and stores
/// every output, with global parameters `cparams` (see [`cparam`]).
///
/// # Errors
///
/// The OpenZL error code, or -1 for a graph the shim could not build.
pub fn compress(
    inputs: &[Input<'_>],
    run: &Run,
    cparams: &[(i32, i32)],
) -> Result<Vec<u8>, crate::Error> {
    let mut types = Vec::new();
    let mut srcs: Vec<*const c_void> = Vec::new();
    let mut sizes = Vec::new();
    let mut widths = Vec::new();
    let mut lens: Vec<*const u32> = Vec::new();
    let mut n_lens = Vec::new();
    let mut total = 0usize;
    // An empty slice points at a dangling, unaligned address that the
    // library rejects for numeric inputs.
    let aligned = [0u64; 1];
    for input in inputs {
        let (code, bytes, width, string_lens): (u32, &[u8], usize, &[u32]) = match *input {
            Input::Serial(bytes) => (0, bytes, 1, &[]),
            Input::Struct { bytes, width } => (1, bytes, width, &[]),
            Input::Numeric { bytes, width } => (2, bytes, width, &[]),
            Input::String { bytes, lens } => (3, bytes, 1, lens),
        };
        types.push(code);
        srcs.push(if bytes.is_empty() {
            aligned.as_ptr().cast()
        } else {
            bytes.as_ptr().cast()
        });
        sizes.push(bytes.len());
        widths.push(width);
        lens.push(if string_lens.is_empty() {
            aligned.as_ptr().cast()
        } else {
            string_lens.as_ptr()
        });
        n_lens.push(string_lens.len());
        total += bytes.len() + string_lens.len() * 4;
    }
    let mut copy_ids: Vec<c_int> = Vec::new();
    let mut copy_ptrs: Vec<*const c_void> = Vec::new();
    let mut copy_sizes = Vec::new();
    let (mode, node, ints, segments, tags, tags_count, value) = match run {
        Run::Node { ints, copies, .. } | Run::RawNode { ints, copies, .. } => {
            let node_id = match run {
                Run::Node { node, .. } => node.id(),
                Run::RawNode { node, .. } => *node,
                _ => unreachable!("matched above"),
            };
            for (id, bytes) in copies {
                copy_ids.push(*id);
                copy_ptrs.push(if bytes.is_empty() {
                    aligned.as_ptr().cast()
                } else {
                    bytes.as_ptr().cast()
                });
                copy_sizes.push(bytes.len());
            }
            (
                0,
                node_id,
                ints.iter().flat_map(|&(id, v)| [id, v]).collect(),
                Vec::new(),
                Vec::new(),
                0,
                0,
            )
        }
        Run::Dispatch {
            sizes,
            tags,
            tags_count,
        } => (
            1,
            0,
            Vec::new(),
            sizes.clone(),
            tags.clone(),
            *tags_count,
            0,
        ),
        Run::Sentinel { indices, sentinel } => {
            (2, 0, Vec::new(), indices.clone(), Vec::new(), 0, *sentinel)
        }
    };
    let mut flat: Vec<c_int> = Vec::new();
    if !cparams.iter().any(|&(id, _)| id == cparam::FORMAT_VERSION) {
        flat.extend([cparam::FORMAT_VERSION, 27]);
    }
    flat.extend(cparams.iter().flat_map(|&(id, v)| [id, v]));
    let capacity = crate::compress_bound(total) + 1024 + inputs.len() * 64;
    let mut dst = vec![0u8; capacity];
    let mut error = 0;
    // SAFETY: every per-input array holds `inputs.len()` entries pointing
    // into the borrowed inputs, the parameter arrays are readable for the
    // lengths passed, and `dst` is writable for `capacity` bytes.
    let written = unsafe {
        sys::mzl_structure_compress(
            dst.as_mut_ptr().cast(),
            capacity,
            inputs.len(),
            types.as_ptr(),
            srcs.as_ptr(),
            sizes.as_ptr(),
            widths.as_ptr(),
            lens.as_ptr(),
            n_lens.as_ptr(),
            mode,
            node,
            ints.as_ptr(),
            ints.len() / 2,
            copy_ids.as_ptr(),
            copy_ptrs.as_ptr(),
            copy_sizes.as_ptr(),
            copy_ids.len(),
            segments.as_ptr(),
            tags.as_ptr(),
            segments.len(),
            tags_count,
            value,
            flat.as_ptr(),
            flat.len() / 2,
            &mut error,
        )
    };
    if error != 0 {
        return Err(crate::Error(error));
    }
    dst.truncate(written);
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concat_frames_decode_with_the_c_library() {
        let frame = compress(
            &[Input::Serial(b"abc"), Input::Serial(b"defg")],
            &Run::Node {
                node: Node::ConcatSerial,
                ints: Vec::new(),
                copies: Vec::new(),
            },
            &[],
        )
        .unwrap();
        let outputs = crate::graph::decompress(&frame).unwrap();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].bytes, b"abc");
        assert_eq!(outputs[1].bytes, b"defg");
        assert_eq!(Node::ConcatSerial.id(), Node::ConcatSerial.id());
        assert_ne!(Node::ConcatSerial.id(), Node::DedupNumeric.id());
    }

    #[test]
    fn every_node_has_an_id() {
        for node in [
            Node::ConcatSerial,
            Node::ConcatNumeric,
            Node::ConcatStruct,
            Node::ConcatString,
            Node::DedupNumeric,
            Node::InterleaveString,
            Node::MuxLengths,
            Node::SentinelByte,
            Node::TransposeSplit,
            Node::DispatchString,
            Node::SplitByStruct,
            Node::SplitN,
            Node::SplitNStruct,
            Node::SplitNNum,
        ] {
            assert_ne!(node.id(), 0, "{node:?}");
        }
    }
}
