//! Oracle for the Transformer numeric selector (`shim/enc_transformer.c`):
//! the generated scorers and the decision (feature extraction, scoring and
//! guards) exactly as `SI_transformer_numeric_select` runs them.

use core::ffi::c_int;

unsafe extern "C" {
    fn mzl_trs_score_legacy(width: usize, input: *const u64, scores: *mut f32, logits: *mut f32);
    fn mzl_trs_score_v2(input: *const u64, floats: *const f64, scores: *mut f32, logits: *mut f32);
    fn mzl_trs_decide(
        width: usize,
        data: *const u8,
        n_bytes: usize,
        best_op: *mut c_int,
        best_index: *mut c_int,
        score_count: *mut c_int,
        scores: *mut f32,
        logits: *mut f32,
        legacy: *mut u64,
        v2_u64: *mut u64,
        v2_f64: *mut f64,
    ) -> c_int;
    fn mzl_trs_operation_ids(width: usize, out: *mut c_int) -> c_int;
}

/// Number of score slots of a decision (`TRS_GENERIC_NUMERIC_MAX_OPERATION_COUNT`).
pub const MAX_OPERATIONS: usize = 13;
/// Slots of [`Decision::legacy`]: the 24 legacy fields and `delta_down_ratio_fp`.
pub const LEGACY_FIELDS: usize = 25;

/// What the selector decides for one stream.
#[derive(Clone, Debug)]
pub struct Decision {
    /// `best_op` (`TRS_GenericNumericOpId`, -1 for invalid).
    pub best_op: i32,
    /// `best_index`, -1 for constant or invalid.
    pub best_index: i32,
    /// Number of valid scores.
    pub score_count: i32,
    /// Post-guard scores.
    pub scores: [f32; MAX_OPERATIONS],
    /// Pre-sigmoid outputs.
    pub logits: [f32; MAX_OPERATIONS],
    /// The legacy feature dump (widths 1, 2 and 4), in the order
    /// `min_u, max_u, min_s, max_s, count, max_run_length, zero_count,
    /// delta_up_count, delta_down_count, delta_min_s, delta_max_s,
    /// mean_abs_dev_u_fp, mean_abs_delta_u_fp, cardinality_est,
    /// pair_cardinality_est, d8_cardinality_est, sorted_gap_cv_fp,
    /// transition_gap_cv_fp, min_lb0, match4, range_u, range_s,
    /// zero_ratio_fp, delta_up_ratio_fp, delta_down_ratio_fp`.
    pub legacy: [u64; LEGACY_FIELDS],
    /// The width-8 integer features: `count, min_u, max_u, range_u, range_s,
    /// cardinality_est, pair_cardinality_est, min_lb0`.
    pub v2_ints: [u64; 16],
    /// The width-8 floating features: `zero_ratio, delta_up_ratio,
    /// delta_down_ratio, mean_abs_delta_u, sorted_gap_nmad, sorted_gap_mode`.
    pub v2_floats: [f64; 16],
}

/// Runs the selector's decision on `bytes`, little-endian numbers of `width`
/// bytes (1, 2, 4 or 8).
///
/// # Panics
///
/// Panics when `width` is not 1, 2, 4 or 8, `bytes` is not a whole number of
/// elements, or `bytes` is empty.
#[must_use]
pub fn decide(width: usize, bytes: &[u8]) -> Decision {
    assert!(matches!(width, 1 | 2 | 4 | 8) && bytes.len().is_multiple_of(width));
    assert!(!bytes.is_empty());
    // The extractors read through casts, so the data must be aligned.
    let mut aligned = vec![0u64; bytes.len().div_ceil(8)];
    // SAFETY: `aligned` has at least `bytes.len()` bytes and does not overlap `bytes`.
    unsafe {
        core::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            aligned.as_mut_ptr().cast::<u8>(),
            bytes.len(),
        );
    }
    let mut out = Decision {
        best_op: 0,
        best_index: 0,
        score_count: 0,
        scores: [0.0; MAX_OPERATIONS],
        logits: [0.0; MAX_OPERATIONS],
        legacy: [0; LEGACY_FIELDS],
        v2_ints: [0; 16],
        v2_floats: [0.0; 16],
    };
    // SAFETY: every out-pointer addresses an array of the size the shim
    // writes (13 scores and logits, 25 legacy fields, 16 + 16 V2 slots) and
    // `aligned` holds `bytes.len()` initialized bytes aligned for `u64`.
    let ok = unsafe {
        mzl_trs_decide(
            width,
            aligned.as_ptr().cast::<u8>(),
            bytes.len(),
            &raw mut out.best_op,
            &raw mut out.best_index,
            &raw mut out.score_count,
            out.scores.as_mut_ptr(),
            out.logits.as_mut_ptr(),
            out.legacy.as_mut_ptr(),
            out.v2_ints.as_mut_ptr(),
            out.v2_floats.as_mut_ptr(),
        )
    };
    assert_eq!(ok, 1, "the reference's extractor failed");
    out
}

/// Scores a legacy feature vector with the generated scorer of `width` (1, 2
/// or 4): returns `(scores, logits)` for the scorer's operations.
///
/// `features` has the layout of [`Decision::legacy`].
///
/// # Panics
///
/// Panics when `width` is not 1, 2 or 4.
#[must_use]
pub fn score_legacy(
    width: usize,
    features: &[u64; LEGACY_FIELDS],
) -> ([f32; MAX_OPERATIONS], [f32; MAX_OPERATIONS]) {
    assert!(matches!(width, 1 | 2 | 4));
    let mut scores = [0.0; MAX_OPERATIONS];
    let mut logits = [0.0; MAX_OPERATIONS];
    // SAFETY: `features` has the 25 slots the shim reads; the outputs have
    // room for the 13 values the largest scorer writes.
    unsafe {
        mzl_trs_score_legacy(
            width,
            features.as_ptr(),
            scores.as_mut_ptr(),
            logits.as_mut_ptr(),
        )
    };
    (scores, logits)
}

/// Scores a width-8 feature vector (layouts of [`Decision::v2_ints`] and
/// [`Decision::v2_floats`]).
#[must_use]
pub fn score_v2(
    ints: &[u64; 16],
    floats: &[f64; 16],
) -> ([f32; MAX_OPERATIONS], [f32; MAX_OPERATIONS]) {
    let mut scores = [0.0; MAX_OPERATIONS];
    let mut logits = [0.0; MAX_OPERATIONS];
    // SAFETY: the inputs have the 8 and 6 slots the shim reads; the outputs
    // have room for the 11 values the scorer writes.
    unsafe {
        mzl_trs_score_v2(
            ints.as_ptr(),
            floats.as_ptr(),
            scores.as_mut_ptr(),
            logits.as_mut_ptr(),
        )
    };
    (scores, logits)
}

/// The operation IDs of the scorer of `width`, in output order.
#[must_use]
pub fn operation_ids(width: usize) -> Vec<i32> {
    let mut ids = [0 as c_int; MAX_OPERATIONS];
    // SAFETY: the scorers have at most 12 operations; `ids` has 13 slots.
    let n = unsafe { mzl_trs_operation_ids(width, ids.as_mut_ptr()) };
    ids[..n as usize].to_vec()
}
