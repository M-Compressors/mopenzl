//! Oracle helpers for the structure encoders (`shim/enc_structure.c`):
//! split-family graphs that need custom nodes, and the estimators of
//! `shared/estimate.c`.

use core::ffi::{c_int, c_void};

use crate::graph::{Input, cparam};

/// Raw entry points of `shim/enc_structure.c`.
pub mod sys {
    use core::ffi::{c_int, c_void};

    unsafe extern "C" {
        /// See `mzl_enc_split_graph`.
        pub fn mzl_enc_split_graph(
            dst: *mut c_void,
            dst_capacity: usize,
            mode: u32,
            ty: u32,
            sizes: *const usize,
            n: usize,
            src: *const c_void,
            src_size: usize,
            width: usize,
            cparams: *const c_int,
            n_cparams: usize,
            error: *mut c_int,
        ) -> usize;
        /// See `mzl_est_cardinality_fixed`.
        pub fn mzl_est_cardinality_fixed(
            src: *const c_void,
            nb_elts: usize,
            elt_size: usize,
            early_exit: u64,
            out: *mut u64,
        );
        /// See `mzl_est_cardinality_variable`.
        pub fn mzl_est_cardinality_variable(
            src: *const u8,
            sizes: *const usize,
            nb_elts: usize,
            early_exit: u64,
            out: *mut u64,
        );
        /// See `mzl_est_cardinality_hashed`.
        pub fn mzl_est_cardinality_hashed(
            linear_hashes: *const u64,
            hll_hashes: *const u64,
            nb_hashes: usize,
            upper_bound: usize,
        ) -> u64;
        /// See `mzl_est_dimensionality`.
        pub fn mzl_est_dimensionality(
            src: *const c_void,
            nb_elts: usize,
            elt_size: usize,
            out: *mut usize,
        );
        /// `ZL_guessFloatWidth`.
        pub fn mzl_est_float_width(src: *const c_void, size: usize) -> usize;
        /// `ZL_computeUnsignedRange`.
        pub fn mzl_est_range(src: *const c_void, nb_elts: usize, elt_size: usize, out: *mut u64);
    }
}

/// Which split-family graph [`split_graph`] builds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SplitGraph {
    /// `ZL_Compressor_registerSplitGraph`: segments of the given sizes.
    Split,
    /// `ZL_Compressor_registerSplitByStructGraph`: the sizes are field sizes.
    SplitByStruct,
    /// `ZL_Compressor_registerTransposeSplitGraph`.
    TransposeSplit,
}

/// Compresses one input through a split-family graph whose successors are
/// all `ZL_GRAPH_STORE`. `sizes` are the segment sizes (or field sizes).
///
/// # Errors
///
/// The OpenZL error code, or -1 when the graph could not be registered.
pub fn split_graph(
    input: Input<'_>,
    graph: SplitGraph,
    sizes: &[usize],
    cparams: &[(i32, i32)],
) -> Result<Vec<u8>, crate::Error> {
    let aligned = [0u64; 1];
    let (ty, bytes, width): (u32, &[u8], usize) = match input {
        Input::Serial(bytes) => (0, bytes, 1),
        Input::Struct { bytes, width } => (1, bytes, width),
        Input::Numeric { bytes, width } => (2, bytes, width),
        Input::String { .. } => return Err(crate::Error(-1)),
    };
    let mut flat: Vec<c_int> = Vec::new();
    if !cparams.iter().any(|&(id, _)| id == cparam::FORMAT_VERSION) {
        flat.extend([cparam::FORMAT_VERSION, 27]);
    }
    flat.extend(cparams.iter().flat_map(|&(id, v)| [id, v]));
    let capacity = crate::compress_bound(bytes.len()) + 1024 + sizes.len() * 16;
    let mut dst = vec![0u8; capacity];
    let mut error = 0;
    let mode = match graph {
        SplitGraph::Split => 0,
        SplitGraph::SplitByStruct => 1,
        SplitGraph::TransposeSplit => 2,
    };
    // SAFETY: `bytes` (or an aligned dummy when empty), `sizes`, `flat` and
    // `dst` are readable or writable for the lengths passed.
    let written = unsafe {
        sys::mzl_enc_split_graph(
            dst.as_mut_ptr().cast(),
            capacity,
            mode,
            ty,
            sizes.as_ptr(),
            sizes.len(),
            if bytes.is_empty() {
                aligned.as_ptr().cast()
            } else {
                bytes.as_ptr().cast::<c_void>()
            },
            bytes.len(),
            width,
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

/// `ZL_CardinalityEstimate` as five numbers: lower bound, estimate lower
/// bound, estimate, estimate upper bound, upper bound.
pub type Cardinality = [u64; 5];

/// `ZL_estimateCardinality_fixed` over `nb_elts` little-endian integers.
pub fn cardinality_fixed(src: &[u8], elt_size: usize, early_exit: u64) -> Cardinality {
    let aligned = [0u64; 1];
    let mut out = [0u64; 5];
    let nb_elts = src.len() / elt_size;
    // SAFETY: `src` is readable for `nb_elts * elt_size` bytes (an aligned
    // dummy when empty) and `out` holds five words.
    unsafe {
        sys::mzl_est_cardinality_fixed(
            if src.is_empty() {
                aligned.as_ptr().cast()
            } else {
                src.as_ptr().cast()
            },
            nb_elts,
            elt_size,
            early_exit,
            out.as_mut_ptr(),
        );
    }
    out
}

/// `ZL_estimateCardinality_variable` over consecutive slices of `src`.
pub fn cardinality_variable(src: &[u8], sizes: &[usize], early_exit: u64) -> Cardinality {
    let mut out = [0u64; 5];
    // SAFETY: the sizes add up to at most `src.len()` (the caller's
    // contract) and `out` holds five words.
    unsafe {
        sys::mzl_est_cardinality_variable(
            src.as_ptr(),
            sizes.as_ptr(),
            sizes.len(),
            early_exit,
            out.as_mut_ptr(),
        );
    }
    out
}

/// `ZL_estimateCardinality_hashed` with the hashes precomputed: both slices
/// hold `hashes` entries.
pub fn cardinality_hashed(linear: &[u64], hll: &[u64], upper_bound: usize) -> u64 {
    // SAFETY: both arrays are readable for `linear.len()` hashes.
    unsafe {
        sys::mzl_est_cardinality_hashed(linear.as_ptr(), hll.as_ptr(), linear.len(), upper_bound)
    }
}

/// Bytes past the input [`dimensionality`] keeps readable for C.
const DIMENSIONALITY_SLACK: usize = 8;

/// `ZL_estimateDimensionality`: `[status, stride, strideMatches,
/// totalMatches]`.
pub fn dimensionality(src: &[u8], elt_size: usize) -> [usize; 4] {
    let mut out = [0usize; 4];
    // `ZL_hash3` loads four bytes per three-byte element (the fourth is
    // shifted out), so the last element reads one byte past the input. C
    // gets a copy with slack so that read stays inside the allocation.
    let padded: Vec<u8> = src
        .iter()
        .copied()
        .chain([0; DIMENSIONALITY_SLACK])
        .collect();
    // SAFETY: `padded` is readable for `src.len() / elt_size` elements plus
    // `DIMENSIONALITY_SLACK` bytes, more than any element hash over-reads.
    unsafe {
        sys::mzl_est_dimensionality(
            padded.as_ptr().cast(),
            src.len() / elt_size,
            elt_size,
            out.as_mut_ptr(),
        );
    }
    out
}

/// `ZL_guessFloatWidth`.
pub fn float_width(src: &[u8]) -> usize {
    // SAFETY: `src` is readable for its length.
    unsafe { sys::mzl_est_float_width(src.as_ptr().cast(), src.len()) }
}

/// `ZL_computeUnsignedRange`: `[min, max]`.
pub fn range(src: &[u8], elt_size: usize) -> [u64; 2] {
    let mut out = [0u64; 2];
    // SAFETY: `src` is readable for `src.len() / elt_size` elements.
    unsafe {
        sys::mzl_est_range(
            src.as_ptr().cast(),
            src.len() / elt_size,
            elt_size,
            out.as_mut_ptr(),
        );
    }
    out
}
