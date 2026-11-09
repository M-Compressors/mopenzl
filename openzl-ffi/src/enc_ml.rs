//! Test oracle of the ML selector, clustering and CBOR ports
//! (`shim/enc_ml.c`): the A1CBOR decoder and encoder, the integer feature
//! generator and GBT predictor, and a graph builder with MParam blobs,
//! custom nodes, typed clustering configurations and typed inputs carrying
//! integer metadata.

use core::ffi::{c_int, c_void};

use crate::graph::{Graph, Input, cparam};

/// Raw entry points of `shim/enc_ml.c`.
pub mod sys {
    use core::ffi::{c_int, c_void};

    unsafe extern "C" {
        /// See `mzl_cbor_roundtrip`.
        pub fn mzl_cbor_roundtrip(
            data: *const u8,
            size: usize,
            max_depth: usize,
            limit_bytes: usize,
            reference_source: c_int,
            reject_unknown_simple: c_int,
            out: *mut u8,
            out_cap: usize,
            err: *mut usize,
        ) -> usize;
        /// See `mzl_cbor_encode_capped`.
        pub fn mzl_cbor_encode_capped(
            data: *const u8,
            size: usize,
            cap: usize,
            out: *mut u8,
            size_out: *mut usize,
            err: *mut usize,
        ) -> usize;
        /// See `mzl_cbor_encode_simple`.
        pub fn mzl_cbor_encode_simple(
            value: u8,
            cap: usize,
            out: *mut u8,
            err: *mut usize,
        ) -> usize;
        /// See `mzl_ml_features`.
        pub fn mzl_ml_features(
            src: *const c_void,
            n_elts: usize,
            width: usize,
            out: *mut f32,
        ) -> c_int;
        /// See `mzl_ml_predict`.
        pub fn mzl_ml_predict(
            config: *const u8,
            config_size: usize,
            src: *const c_void,
            n_elts: usize,
            width: usize,
        ) -> i64;
        /// See `mzl_enc_ml_compress`.
        pub fn mzl_enc_ml_compress(
            dst: *mut c_void,
            dst_capacity: usize,
            n_inputs: usize,
            types: *const u32,
            srcs: *const *const c_void,
            sizes: *const usize,
            widths: *const usize,
            lens: *const *const u32,
            n_lens: *const usize,
            meta_counts: *const usize,
            meta: *const c_int,
            program: *const u32,
            program_len: usize,
            cparams: *const c_int,
            n_cparams: usize,
            error: *mut c_int,
        ) -> usize;
    }
}

/// An aligned, readable address for empty inputs.
static ALIGNED: [u64; 1] = [0];

/// A CBOR failure as A1CBOR reports it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CborError {
    /// `A1C_ErrorType`.
    pub kind: usize,
    /// `A1C_Error.srcPos`.
    pub position: usize,
    /// `A1C_Error.depth`.
    pub depth: usize,
}

/// The decoder configuration of `A1C_DecoderConfig`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct CborConfig {
    /// Maximum depth; 0 means the default.
    pub max_depth: usize,
    /// Allocation limit; 0 means none.
    pub limit_bytes: usize,
    /// Reference strings in the source instead of copying.
    pub reference_source: bool,
    /// Reject unknown simple values.
    pub reject_unknown_simple: bool,
}

fn cbor_error(err: [usize; 3]) -> CborError {
    CborError {
        kind: err[0],
        position: err[1],
        depth: err[2],
    }
}

/// Decodes `data` with A1CBOR and re-encodes the item canonically.
///
/// # Errors
///
/// The decoder's error; an encoder failure is reported with kind
/// `100 + A1C_ErrorType`.
pub fn cbor_roundtrip(data: &[u8], config: CborConfig) -> Result<Vec<u8>, CborError> {
    let mut out = vec![0u8; data.len() * 40 + 64];
    let mut err = [0usize; 3];
    // SAFETY: `data` and `out` are valid for their lengths; `err` holds 3 words.
    let written = unsafe {
        sys::mzl_cbor_roundtrip(
            data.as_ptr(),
            data.len(),
            config.max_depth,
            config.limit_bytes,
            c_int::from(config.reference_source),
            c_int::from(config.reject_unknown_simple),
            out.as_mut_ptr(),
            out.len(),
            err.as_mut_ptr(),
        )
    };
    if written == 0 {
        return Err(cbor_error(err));
    }
    out.truncate(written);
    Ok(out)
}

/// Decodes `data` permissively and encodes the item into `cap` bytes:
/// `(bytes written, A1C_Item_encodedSize)`.
///
/// # Errors
///
/// The encoder's error (`kind` 200 when the permissive decode fails).
pub fn cbor_encode_capped(data: &[u8], cap: usize) -> Result<(Vec<u8>, usize), CborError> {
    let mut out = vec![0u8; cap.max(1)];
    let mut err = [0usize; 3];
    let mut size = 0usize;
    // SAFETY: `data` and `out` are valid for the lengths passed.
    let written = unsafe {
        sys::mzl_cbor_encode_capped(
            data.as_ptr(),
            data.len(),
            cap,
            out.as_mut_ptr(),
            &mut size,
            err.as_mut_ptr(),
        )
    };
    if written == 0 {
        return Err(cbor_error(err));
    }
    out.truncate(written);
    Ok((out, size))
}

/// Encodes the simple value `value` into `cap` bytes.
///
/// # Errors
///
/// The encoder's error.
pub fn cbor_encode_simple(value: u8, cap: usize) -> Result<Vec<u8>, CborError> {
    let mut out = vec![0u8; cap.max(1)];
    let mut err = [0usize; 3];
    // SAFETY: `out` is valid for `cap` bytes.
    let written =
        unsafe { sys::mzl_cbor_encode_simple(value, cap, out.as_mut_ptr(), err.as_mut_ptr()) };
    if written == 0 {
        return Err(cbor_error(err));
    }
    out.truncate(written);
    Ok(out)
}

/// `FeatureGen_integer` on `bytes` read as numbers of `width` bytes: the 11
/// features in order (`nbElts`, `eltWidth`, `cardinality`,
/// `cardinality_upper`, `cardinality_lower`, `range_size`, `mean`,
/// `variance`, `stddev`, `skewness`, `kurtosis`).
///
/// # Panics
///
/// When `bytes` is not a whole number of elements of `width`.
#[must_use]
pub fn ml_features(bytes: &[u8], width: usize) -> Option<[f32; 11]> {
    assert!(bytes.len().is_multiple_of(width));
    let mut out = [0f32; 11];
    let src = if bytes.is_empty() {
        ALIGNED.as_ptr().cast()
    } else {
        bytes.as_ptr().cast()
    };
    // SAFETY: `src` is readable for `bytes.len()` bytes; `out` holds 11 floats.
    let status = unsafe { sys::mzl_ml_features(src, bytes.len() / width, width, out.as_mut_ptr()) };
    (status == 0).then_some(out)
}

/// Deserializes the CBOR ML selector `config` and predicts the successor of
/// `bytes` read as numbers of `width` bytes; `None` when the reference
/// rejects the configuration or the prediction.
///
/// # Panics
///
/// When `bytes` is not a whole number of elements of `width`.
#[must_use]
pub fn ml_predict(config: &[u8], bytes: &[u8], width: usize) -> Option<usize> {
    assert!(bytes.len().is_multiple_of(width));
    let src = if bytes.is_empty() {
        ALIGNED.as_ptr().cast()
    } else {
        bytes.as_ptr().cast()
    };
    // SAFETY: `config` and `src` are readable for the lengths passed.
    let index = unsafe {
        sys::mzl_ml_predict(
            config.as_ptr(),
            config.len(),
            src,
            bytes.len() / width,
            width,
        )
    };
    usize::try_from(index).ok()
}

/// One cluster of a [`ClusteringConfig`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cluster {
    /// `ZL_Type` code (1 serial, 2 struct, 4 numeric, 8 string).
    pub ty: u32,
    /// Element width.
    pub width: u32,
    /// Index into the successors.
    pub successor: u32,
    /// Index into the clustering codecs.
    pub codec: u32,
    /// The member tags.
    pub tags: Vec<i32>,
}

/// A type default of a [`ClusteringConfig`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TypeDefault {
    /// `ZL_Type` code.
    pub ty: u32,
    /// Element width.
    pub width: u32,
    /// Index into the successors.
    pub successor: u32,
    /// Index into the clustering codecs.
    pub codec: u32,
}

/// `ZL_ClusteringConfig`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClusteringConfig {
    /// The configured clusters.
    pub clusters: Vec<Cluster>,
    /// The per-type defaults.
    pub defaults: Vec<TypeDefault>,
}

/// A graph for [`compress`]: a plain [`Graph`] or one of the shapes
/// `graph.c` cannot express.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MlGraph {
    /// A registered graph, a node graph or a parameterized graph.
    Plain(Graph),
    /// `ZL_Compressor_registerParameterizedGraph` with custom graphs, custom
    /// nodes and an MParam blob.
    Parameterized {
        /// The base graph ID.
        graph: u32,
        /// Integer and copy parameters.
        params: crate::graph::Params,
        /// Custom successor graphs.
        custom: Vec<MlGraph>,
        /// Custom nodes.
        nodes: Vec<u32>,
        /// The MParam blob; empty for none.
        mparam: Vec<u8>,
    },
    /// `ZL_Clustering_registerGraph` (no `nodes`) or
    /// `ZL_Clustering_registerGraphWithCustomClusteringCodecs`.
    Clustering {
        /// The typed configuration.
        config: ClusteringConfig,
        /// The successors.
        successors: Vec<MlGraph>,
        /// Custom clustering codecs; empty uses the four concat nodes.
        nodes: Vec<u32>,
    },
}

impl MlGraph {
    /// `ZL_GRAPH_STORE`.
    pub const STORE: Self = Self::Plain(Graph::STORE);

    fn bytes_words(program: &mut Vec<u32>, bytes: &[u8]) {
        program.push(bytes.len() as u32);
        for word in bytes.chunks(4) {
            let mut padded = [0u8; 4];
            padded[..word.len()].copy_from_slice(word);
            program.push(u32::from_le_bytes(padded));
        }
    }

    fn encode(&self, program: &mut Vec<u32>) {
        match self {
            Self::Plain(graph) => graph.encode(program),
            Self::Parameterized {
                graph,
                params,
                custom,
                nodes,
                mparam,
            } => {
                program.extend([4, *graph]);
                program.push(params.ints.len() as u32);
                for &(id, value) in &params.ints {
                    program.extend([id as u32, value as u32]);
                }
                program.push(params.copies.len() as u32);
                for (id, bytes) in &params.copies {
                    program.push(*id as u32);
                    Self::bytes_words(program, bytes);
                }
                program.push(custom.len() as u32);
                for successor in custom {
                    successor.encode(program);
                }
                program.push(nodes.len() as u32);
                program.extend(nodes);
                Self::bytes_words(program, mparam);
            }
            Self::Clustering {
                config,
                successors,
                nodes,
            } => {
                program.extend([5, config.clusters.len() as u32]);
                for cluster in &config.clusters {
                    program.extend([
                        cluster.ty,
                        cluster.width,
                        cluster.successor,
                        cluster.codec,
                        cluster.tags.len() as u32,
                    ]);
                    program.extend(cluster.tags.iter().map(|&tag| tag as u32));
                }
                program.push(config.defaults.len() as u32);
                for default in &config.defaults {
                    program.extend([default.ty, default.width, default.successor, default.codec]);
                }
                program.push(successors.len() as u32);
                for successor in successors {
                    successor.encode(program);
                }
                program.push(nodes.len() as u32);
                program.extend(nodes);
            }
        }
    }
}

/// An input with integer metadata.
#[derive(Clone, Copy, Debug)]
pub struct MlInput<'a> {
    /// The data.
    pub input: Input<'a>,
    /// `(id, value)` integer metadata set on the input.
    pub metadata: &'a [(i32, i32)],
}

/// Compresses `inputs` through `graph` with global parameters `cparams`;
/// the format version defaults to 27.
///
/// # Errors
///
/// The OpenZL error code, or -1 for a graph the shim could not build.
pub fn compress(
    inputs: &[MlInput<'_>],
    graph: &MlGraph,
    cparams: &[(i32, i32)],
) -> Result<Vec<u8>, crate::Error> {
    let mut program = Vec::new();
    graph.encode(&mut program);
    let mut types = Vec::new();
    let mut srcs: Vec<*const c_void> = Vec::new();
    let mut sizes = Vec::new();
    let mut widths = Vec::new();
    let mut lens: Vec<*const u32> = Vec::new();
    let mut n_lens = Vec::new();
    let mut meta_counts = Vec::new();
    let mut meta: Vec<c_int> = Vec::new();
    let mut total = 0usize;
    for item in inputs {
        let (code, bytes, width, string_lens): (u32, &[u8], usize, &[u32]) = match item.input {
            Input::Serial(bytes) => (0, bytes, 1, &[]),
            Input::Struct { bytes, width } => (1, bytes, width, &[]),
            Input::Numeric { bytes, width } => (2, bytes, width, &[]),
            Input::String { bytes, lens } => (3, bytes, 1, lens),
        };
        types.push(code);
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
        meta_counts.push(item.metadata.len());
        meta.extend(item.metadata.iter().flat_map(|&(id, value)| [id, value]));
        total += bytes.len() + string_lens.len() * 4;
    }
    let mut flat: Vec<c_int> = Vec::new();
    if !cparams.iter().any(|&(id, _)| id == cparam::FORMAT_VERSION) {
        flat.extend([cparam::FORMAT_VERSION, 27]);
    }
    flat.extend(cparams.iter().flat_map(|&(id, v)| [id, v]));
    let capacity = crate::compress_bound(total) + 1024 + inputs.len() * 64;
    let mut dst = vec![0u8; capacity];
    let mut error = 0;
    // SAFETY: every array holds `inputs.len()` entries pointing into the
    // borrowed inputs; `meta` holds the pairs `meta_counts` announces;
    // `program` and `flat` are readable for their lengths, and `dst` is
    // writable for `capacity` bytes.
    let written = unsafe {
        sys::mzl_enc_ml_compress(
            dst.as_mut_ptr().cast(),
            capacity,
            inputs.len(),
            types.as_ptr(),
            srcs.as_ptr(),
            sizes.as_ptr(),
            widths.as_ptr(),
            lens.as_ptr(),
            n_lens.as_ptr(),
            meta_counts.as_ptr(),
            meta.as_ptr(),
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
