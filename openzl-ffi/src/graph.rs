//! Compression through arbitrary graphs, typed and multiple inputs, and
//! typed outputs (`shim/graph.c`).
//!
//! A [`Graph`] names standard graphs and nodes by their numeric IDs
//! (`ZL_StandardGraphID_*`, `ZL_StandardNodeID_*` and the private IDs the
//! standard graphs use), so a test can reach every codec the C library
//! registers.
//!
//! ```no_run
//! use openzl_ffi::graph::{self, Graph, Input};
//!
//! // ZL_NODE_DELTA_INT (ID 5) after interpreting bytes as 32-bit numbers,
//! // stored. The IDs here are illustrative; take them from the headers.
//! let graph = Graph::node(5, [Graph::STORE]);
//! let frame = graph::compress(&[Input::Numeric { bytes: &[0; 16], width: 4 }], &graph, &[]);
//! ```

use core::ffi::{c_int, c_void};

/// `ZL_StandardGraphID_store`.
pub const GRAPH_STORE: u32 = 2;

/// An aligned, readable address for empty inputs.
static ALIGNED: [u64; 1] = [0];

/// Raw entry points of `shim/graph.c`.
pub mod sys {
    use core::ffi::{c_int, c_void};

    /// Opaque decoded outputs.
    #[repr(C)]
    pub struct Decoded {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        /// See `mzl_compress_graph_comment` in `shim/graph.c`.
        pub fn mzl_compress_graph_comment(
            comment: *const c_void,
            comment_len: usize,
            dst: *mut c_void,
            dst_capacity: usize,
            n_inputs: usize,
            types: *const u32,
            srcs: *const *const c_void,
            sizes: *const usize,
            widths: *const usize,
            lens: *const *const u32,
            n_lens: *const usize,
            program: *const u32,
            program_len: usize,
            cparams: *const c_int,
            n_cparams: usize,
            error: *mut c_int,
        ) -> usize;
        /// See `mzl_compress_graph` in `shim/graph.c`.
        pub fn mzl_compress_graph(
            dst: *mut c_void,
            dst_capacity: usize,
            n_inputs: usize,
            types: *const u32,
            srcs: *const *const c_void,
            sizes: *const usize,
            widths: *const usize,
            lens: *const *const u32,
            n_lens: *const usize,
            program: *const u32,
            program_len: usize,
            cparams: *const c_int,
            n_cparams: usize,
            error: *mut c_int,
        ) -> usize;
        /// Decodes every output of a frame.
        pub fn mzl_decode_typed(
            src: *const c_void,
            src_size: usize,
            error: *mut c_int,
        ) -> *mut Decoded;
        /// Number of outputs.
        pub fn mzl_decoded_count(d: *const Decoded) -> usize;
        /// Type code of output `i`.
        pub fn mzl_decoded_type(d: *const Decoded, i: usize) -> u32;
        /// Element width of output `i`.
        pub fn mzl_decoded_width(d: *const Decoded, i: usize) -> usize;
        /// Bytes of output `i`.
        pub fn mzl_decoded_bytes(d: *const Decoded, i: usize) -> usize;
        /// Content of output `i`.
        pub fn mzl_decoded_ptr(d: *const Decoded, i: usize) -> *const c_void;
        /// Elements of output `i`.
        pub fn mzl_decoded_elts(d: *const Decoded, i: usize) -> usize;
        /// String lengths of output `i`.
        pub fn mzl_decoded_lens(d: *const Decoded, i: usize) -> *const u32;
        /// Releases the outputs.
        pub fn mzl_decoded_free(d: *mut Decoded);
    }
}

/// `ZL_CParam` IDs for [`compress`]'s global parameters.
pub mod cparam {
    /// `ZL_CParam_compressionLevel`.
    pub const LEVEL: i32 = 2;
    /// `ZL_CParam_formatVersion`.
    pub const FORMAT_VERSION: i32 = 4;
    /// `ZL_CParam_permissiveCompression`.
    pub const PERMISSIVE: i32 = 5;
    /// `ZL_CParam_compressedChecksum`.
    pub const COMPRESSED_CHECKSUM: i32 = 6;
    /// `ZL_CParam_contentChecksum`.
    pub const CONTENT_CHECKSUM: i32 = 7;
    /// `ZL_CParam_minStreamSize`.
    pub const MIN_STREAM_SIZE: i32 = 11;
    /// `ZL_CParam_storeOnExpansion`.
    pub const STORE_ON_EXPANSION: i32 = 12;
}

/// A local parameter set: integer parameters and copy parameters.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Params {
    /// `(paramId, value)` integer parameters.
    pub ints: Vec<(i32, i32)>,
    /// `(paramId, bytes)` copy parameters.
    pub copies: Vec<(i32, Vec<u8>)>,
}

/// A compression graph, built in C by `mzl_compress_graph`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Graph {
    /// A registered graph, by `ZL_GraphID`.
    Id(u32),
    /// Node `node` (a `ZL_NodeID`), parameterized by `params` when any,
    /// whose outputs go to `successors` in order.
    Node {
        /// The node ID.
        node: u32,
        /// Its local parameters.
        params: Params,
        /// One graph per output.
        successors: Vec<Graph>,
    },
    /// Graph `graph` parameterized by `params`, with `custom` graphs.
    Parameterized {
        /// The graph ID.
        graph: u32,
        /// Its local parameters.
        params: Params,
        /// Its custom successor graphs; empty keeps them.
        custom: Vec<Graph>,
    },
    /// Plain nodes run in order, ending in `last`.
    Pipeline {
        /// Node IDs.
        nodes: Vec<u32>,
        /// The graph the last node's output goes to.
        last: Box<Graph>,
    },
}

impl Graph {
    /// `ZL_GRAPH_STORE`.
    pub const STORE: Self = Self::Id(GRAPH_STORE);

    /// Node `node` without parameters, feeding `successors`.
    pub fn node(node: u32, successors: impl IntoIterator<Item = Graph>) -> Self {
        Self::Node {
            node,
            params: Params::default(),
            successors: successors.into_iter().collect(),
        }
    }

    /// Node `node` with integer parameters `ints`, feeding `successors`.
    pub fn node_with(
        node: u32,
        ints: impl IntoIterator<Item = (i32, i32)>,
        successors: impl IntoIterator<Item = Graph>,
    ) -> Self {
        Self::Node {
            node,
            params: Params {
                ints: ints.into_iter().collect(),
                copies: Vec::new(),
            },
            successors: successors.into_iter().collect(),
        }
    }

    fn params(program: &mut Vec<u32>, params: &Params) {
        program.push(params.ints.len() as u32);
        for &(id, value) in &params.ints {
            program.extend([id as u32, value as u32]);
        }
        program.push(params.copies.len() as u32);
        for (id, bytes) in &params.copies {
            program.extend([*id as u32, bytes.len() as u32]);
            for word in bytes.chunks(4) {
                let mut padded = [0u8; 4];
                padded[..word.len()].copy_from_slice(word);
                program.push(u32::from_le_bytes(padded));
            }
        }
    }

    /// Appends the program words describing this graph.
    pub fn encode(&self, program: &mut Vec<u32>) {
        match self {
            Self::Id(id) => program.extend([0, *id]),
            Self::Node {
                node,
                params,
                successors,
            } => {
                program.extend([1, *node]);
                Self::params(program, params);
                program.push(successors.len() as u32);
                for successor in successors {
                    successor.encode(program);
                }
            }
            Self::Parameterized {
                graph,
                params,
                custom,
            } => {
                program.extend([2, *graph]);
                Self::params(program, params);
                program.push(custom.len() as u32);
                for successor in custom {
                    successor.encode(program);
                }
            }
            Self::Pipeline { nodes, last } => {
                program.push(3);
                program.push(nodes.len() as u32);
                program.extend(nodes);
                last.encode(program);
            }
        }
    }
}

/// A typed input to [`compress`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Input<'a> {
    /// Raw bytes.
    Serial(&'a [u8]),
    /// Fixed-width records.
    Struct {
        /// Concatenated records.
        bytes: &'a [u8],
        /// Bytes per record.
        width: usize,
    },
    /// Host-endian integers (little-endian on every supported host).
    Numeric {
        /// Concatenated integers.
        bytes: &'a [u8],
        /// Bytes per integer: 1, 2, 4 or 8.
        width: usize,
    },
    /// Variable-length strings.
    String {
        /// Concatenated contents.
        bytes: &'a [u8],
        /// Length of each string.
        lens: &'a [u32],
    },
}

/// A decoded typed output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Output {
    /// Type code: 0 serial, 1 struct, 2 numeric, 3 string.
    pub kind: u32,
    /// Element width (1 for serial and strings).
    pub width: usize,
    /// Content bytes.
    pub bytes: Vec<u8>,
    /// Number of elements.
    pub elts: usize,
    /// String lengths (strings only).
    pub lens: Vec<u32>,
}

/// Compresses `inputs` through `graph` with global parameters `cparams`
/// (`ZL_CParam` ID and value pairs, see [`cparam`]); the format version
/// defaults to 27.
///
/// # Errors
///
/// The OpenZL error code, or -1 for a graph the shim could not build.
pub fn compress(
    inputs: &[Input<'_>],
    graph: &Graph,
    cparams: &[(i32, i32)],
) -> Result<Vec<u8>, crate::Error> {
    compress_with_comment(inputs, graph, cparams, &[])
}

/// [`compress`] with a header comment (`ZL_CCtx_addHeaderComment`); an
/// empty `comment` adds none.
///
/// # Errors
///
/// The OpenZL error code, or -1 for a graph the shim could not build.
pub fn compress_with_comment(
    inputs: &[Input<'_>],
    graph: &Graph,
    cparams: &[(i32, i32)],
    comment: &[u8],
) -> Result<Vec<u8>, crate::Error> {
    let mut program = Vec::new();
    graph.encode(&mut program);
    let mut types = Vec::new();
    let mut srcs: Vec<*const c_void> = Vec::new();
    let mut sizes = Vec::new();
    let mut widths = Vec::new();
    let mut lens: Vec<*const u32> = Vec::new();
    let mut n_lens = Vec::new();
    let mut total = 0usize;
    for input in inputs {
        let (code, bytes, width, string_lens): (u32, &[u8], usize, &[u32]) = match *input {
            Input::Serial(bytes) => (0, bytes, 1, &[]),
            Input::Struct { bytes, width } => (1, bytes, width, &[]),
            Input::Numeric { bytes, width } => (2, bytes, width, &[]),
            Input::String { bytes, lens } => (3, bytes, 1, lens),
        };
        types.push(code);
        // An empty slice's dangling pointer is unaligned for wide numbers,
        // which `ZL_TypedRef_createNumeric` rejects; lend an aligned one.
        srcs.push(if bytes.is_empty() {
            ALIGNED.as_ptr().cast()
        } else {
            bytes.as_ptr().cast()
        });
        sizes.push(bytes.len());
        widths.push(width);
        lens.push(if string_lens.is_empty() {
            ALIGNED.as_ptr().cast()
        } else {
            string_lens.as_ptr()
        });
        n_lens.push(string_lens.len());
        total += bytes.len() + string_lens.len() * 4;
    }
    let mut flat: Vec<c_int> = Vec::new();
    if !cparams.iter().any(|&(id, _)| id == cparam::FORMAT_VERSION) {
        // The library has no default format version.
        flat.extend([cparam::FORMAT_VERSION, 27]);
    }
    flat.extend(cparams.iter().flat_map(|&(id, v)| [id, v]));
    let capacity = crate::compress_bound(total) + 1024 + inputs.len() * 64 + comment.len();
    let mut dst = vec![0u8; capacity];
    let mut error = 0;
    // SAFETY: every array holds `inputs.len()` entries pointing into the
    // borrowed inputs, `program` and `flat` are readable for their lengths,
    // and `dst` is writable for `capacity` bytes.
    let written = unsafe {
        sys::mzl_compress_graph_comment(
            comment.as_ptr().cast(),
            comment.len(),
            dst.as_mut_ptr().cast(),
            capacity,
            inputs.len(),
            types.as_ptr(),
            srcs.as_ptr(),
            sizes.as_ptr(),
            widths.as_ptr(),
            lens.as_ptr(),
            n_lens.as_ptr(),
            program.as_ptr(),
            program.len(),
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

/// Decodes every typed output of the frame `src` with the C library.
///
/// # Errors
///
/// The OpenZL error code when the frame is invalid.
pub fn decompress(src: &[u8]) -> Result<Vec<Output>, crate::Error> {
    let mut error = 0;
    // SAFETY: `src` is readable for its length.
    let decoded = unsafe { sys::mzl_decode_typed(src.as_ptr().cast(), src.len(), &mut error) };
    if decoded.is_null() {
        return Err(crate::Error(if error == 0 { 1 } else { error }));
    }
    // SAFETY: `decoded` is a live handle; every index is below its count,
    // and each pointer is readable for the byte or element count reported
    // alongside it until the handle is freed.
    let outputs = unsafe {
        let count = sys::mzl_decoded_count(decoded);
        let outputs = (0..count)
            .map(|i| {
                let kind = sys::mzl_decoded_type(decoded, i);
                let bytes_len = sys::mzl_decoded_bytes(decoded, i);
                let ptr = sys::mzl_decoded_ptr(decoded, i).cast::<u8>();
                let bytes = if bytes_len == 0 {
                    Vec::new()
                } else {
                    core::slice::from_raw_parts(ptr, bytes_len).to_vec()
                };
                let elts = sys::mzl_decoded_elts(decoded, i);
                let lens = if kind == 3 && elts > 0 {
                    core::slice::from_raw_parts(sys::mzl_decoded_lens(decoded, i), elts).to_vec()
                } else {
                    Vec::new()
                };
                Output {
                    kind,
                    width: sys::mzl_decoded_width(decoded, i),
                    bytes,
                    elts,
                    lens,
                }
            })
            .collect();
        sys::mzl_decoded_free(decoded);
        outputs
    };
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_typed_inputs_round_trip_through_c() {
        let numbers: Vec<u8> = (0..64u32).flat_map(u32::to_le_bytes).collect();
        let frame = compress(
            &[
                Input::Numeric {
                    bytes: &numbers,
                    width: 4,
                },
                Input::String {
                    bytes: b"abcdef",
                    lens: &[1, 2, 3],
                },
            ],
            &Graph::STORE,
            &[(cparam::FORMAT_VERSION, 27)],
        )
        .unwrap();
        let outputs = decompress(&frame).unwrap();
        assert_eq!(outputs.len(), 2);
        assert_eq!((outputs[0].kind, outputs[0].width), (2, 4));
        assert_eq!(outputs[0].bytes, numbers);
        assert_eq!(outputs[1].kind, 3);
        assert_eq!(outputs[1].lens, [1, 2, 3]);
        assert_eq!(outputs[1].bytes, b"abcdef");
    }

    #[test]
    fn empty_typed_inputs_compress() {
        let inputs = [
            Input::Numeric {
                bytes: &[],
                width: 8,
            },
            Input::String {
                bytes: &[],
                lens: &[],
            },
        ];
        let frame = compress(&inputs, &Graph::STORE, &[]).unwrap();
        let outputs = decompress(&frame).unwrap();
        assert_eq!((outputs[0].kind, outputs[0].elts), (2, 0));
        assert_eq!((outputs[1].kind, outputs[1].elts), (3, 0));
    }

    #[test]
    fn truncated_programs_are_rejected() {
        // A node with no parameter counts: the shim never reaches C.
        let src = b"x";
        let types = [0u32];
        let srcs = [src.as_ptr().cast::<c_void>()];
        let sizes = [1usize];
        let program = [1u32, 4];
        let mut dst = [0u8; 256];
        let mut error = 0;
        // SAFETY: every pointer is valid for the lengths passed.
        unsafe {
            sys::mzl_compress_graph(
                dst.as_mut_ptr().cast(),
                dst.len(),
                1,
                types.as_ptr(),
                srcs.as_ptr(),
                sizes.as_ptr(),
                sizes.as_ptr(),
                core::ptr::null(),
                sizes.as_ptr(),
                program.as_ptr(),
                program.len(),
                core::ptr::null(),
                0,
                &mut error,
            );
        }
        assert_eq!(error, -1);
    }
}
