//! The Transformer's neural scorers (`compress/selectors/transformer/generated/`).
//!
//! `num8`, `num16`, `num32` and `num64` are converted mechanically from the
//! reference's generated C by `scripts/gen_transformer_scorers.py`: the
//! weight tables and the feature expressions, in the reference's evaluation
//! order. What the four C files share, the two dense layers, ReLU and the
//! sigmoid, is written once here; the script refuses to regenerate when the
//! C network differs from this one.
//!
//! # Floating point
//!
//! Every operation is a single IEEE operation in the reference's order and
//! type (`float` network, `double` feature expressions); nothing is fused
//! (`mul_add` is never used and the reference is built with
//! `-ffp-contract=off`). `expf`, `log` and `log2` are the platform libm's
//! (see [`math`]).

pub(super) use super::features::{Features, FeaturesV2};
pub(super) use super::math;

pub(super) mod num16;
pub(super) mod num32;
pub(super) mod num64;
pub(super) mod num8;

/// Operations the scorers rank (`TRS_GenericNumericOpId`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Op {
    /// `TRS_GENERIC_NUMERIC_OP_INVALID`.
    Invalid,
    /// `TRS_GENERIC_NUMERIC_OP_CONSTANT`.
    Constant,
    /// `TRS_GENERIC_NUMERIC_OP_BITPACK`.
    Bitpack,
    /// `TRS_GENERIC_NUMERIC_OP_DELTA_INT`.
    DeltaInt,
    /// `TRS_GENERIC_NUMERIC_OP_DIVIDE_BY_GCD`.
    DivideByGcd,
    /// `TRS_GENERIC_NUMERIC_OP_ENTROPY`.
    Entropy,
    /// `TRS_GENERIC_NUMERIC_OP_FIELD_LZ`.
    FieldLz,
    /// `TRS_GENERIC_NUMERIC_OP_FSE`.
    Fse,
    /// `TRS_GENERIC_NUMERIC_OP_RANGE_PACK`.
    RangePack,
    /// `TRS_GENERIC_NUMERIC_OP_SPARSE_NUM`.
    SparseNum,
    /// `TRS_GENERIC_NUMERIC_OP_STORE`.
    Store,
    /// `TRS_GENERIC_NUMERIC_OP_TOKENIZE_NUMERIC`.
    TokenizeNumeric,
    /// `TRS_GENERIC_NUMERIC_OP_TOKENIZE_NUMERIC_SORTED`.
    TokenizeNumericSorted,
    /// `TRS_GENERIC_NUMERIC_OP_ZSTD`.
    Zstd,
}

impl Op {
    /// The reference's numeric value of the operation.
    #[cfg(all(test, not(miri)))]
    pub(crate) const fn id(self) -> i32 {
        match self {
            Self::Invalid => -1,
            Self::Constant => 0,
            Self::Bitpack => 1,
            Self::DeltaInt => 2,
            Self::DivideByGcd => 3,
            Self::Entropy => 4,
            Self::FieldLz => 5,
            Self::Fse => 6,
            Self::RangePack => 7,
            Self::SparseNum => 8,
            Self::Store => 9,
            Self::TokenizeNumeric => 10,
            Self::TokenizeNumericSorted => 11,
            Self::Zstd => 12,
        }
    }
}

/// `TRS_GENERIC_NUMERIC_MAX_OPERATION_COUNT`.
pub(crate) const MAX_OPERATIONS: usize = 13;

/// `TRSn_relu`.
const fn relu(x: f32) -> f32 {
    if x > 0.0 { x } else { 0.0 }
}

/// `TRSn_sigmoid`.
fn sigmoid(x: f32) -> f32 {
    if x > 15.0 {
        return 1.0;
    }
    if x < -15.0 {
        return 0.0;
    }
    1.0 / (1.0 + math::expf(-x))
}

/// The two dense layers of the scorers: `hidden = relu(b1 + input * w1)`,
/// `logits = b2 + hidden * w2`, `scores = sigmoid(logits)`.
///
/// Each sum adds its terms in the reference's order (bias first, then the
/// inputs in index order), so every `f32` result is bit-identical; the loops
/// are interchanged (inputs outermost) only so the weight rows are read
/// contiguously.
fn forward<const I: usize, const H: usize, const O: usize>(
    input: &[f32; I],
    w1: &[[f32; H]; I],
    b1: &[f32; H],
    w2: &[[f32; O]; H],
    b2: &[f32; O],
    scores: &mut [f32; MAX_OPERATIONS],
    logits: &mut [f32; MAX_OPERATIONS],
) {
    let mut hidden = *b1;
    for (value, row) in input.iter().zip(w1) {
        for (sum, weight) in hidden.iter_mut().zip(row) {
            *sum += value * weight;
        }
    }
    for value in &mut hidden {
        *value = relu(*value);
    }
    let mut output = *b2;
    for (value, row) in hidden.iter().zip(w2) {
        for (sum, weight) in output.iter_mut().zip(row) {
            *sum += value * weight;
        }
    }
    for (j, &logit) in output.iter().enumerate() {
        logits[j] = logit;
        scores[j] = sigmoid(logit);
    }
}

/// `TRS_score_num8_operations_with_logits`: fills `scores` (sigmoid
/// outputs) and `logits` for the first [`num8::N_OUTPUTS`] slots.
pub(crate) fn score_num8(
    f: &Features,
    scores: &mut [f32; MAX_OPERATIONS],
    logits: &mut [f32; MAX_OPERATIONS],
) {
    forward(
        &num8::inputs(f),
        &num8::W1,
        &num8::B1,
        &num8::W2,
        &num8::B2,
        scores,
        logits,
    );
}

/// `TRS_score_num16_operations_with_logits`.
pub(crate) fn score_num16(
    f: &Features,
    scores: &mut [f32; MAX_OPERATIONS],
    logits: &mut [f32; MAX_OPERATIONS],
) {
    forward(
        &num16::inputs(f),
        &num16::W1,
        &num16::B1,
        &num16::W2,
        &num16::B2,
        scores,
        logits,
    );
}

/// `TRS_score_num32_operations_with_logits`.
pub(crate) fn score_num32(
    f: &Features,
    scores: &mut [f32; MAX_OPERATIONS],
    logits: &mut [f32; MAX_OPERATIONS],
) {
    forward(
        &num32::inputs(f),
        &num32::W1,
        &num32::B1,
        &num32::W2,
        &num32::B2,
        scores,
        logits,
    );
}

/// `TRS_score_num64_operations_with_logits`.
pub(crate) fn score_num64(
    f: &FeaturesV2,
    scores: &mut [f32; MAX_OPERATIONS],
    logits: &mut [f32; MAX_OPERATIONS],
) {
    forward(
        &num64::inputs(f),
        &num64::W1,
        &num64::B1,
        &num64::W2,
        &num64::B2,
        scores,
        logits,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activations_follow_the_reference() {
        assert_eq!(relu(-1.0), 0.0);
        assert_eq!(relu(2.5), 2.5);
        assert_eq!(sigmoid(16.0), 1.0);
        assert_eq!(sigmoid(-16.0), 0.0);
        assert_eq!(sigmoid(0.0), 0.5);
    }

    #[test]
    #[cfg(not(miri))]
    fn operation_tables_match_the_reference() {
        let tables: [(usize, &[Op]); 4] = [
            (1, &num8::OPERATION_IDS),
            (2, &num16::OPERATION_IDS),
            (4, &num32::OPERATION_IDS),
            (8, &num64::OPERATION_IDS),
        ];
        for (width, ops) in tables {
            let expected = openzl_ffi::enc_transformer::operation_ids(width);
            let ours: alloc::vec::Vec<i32> = ops.iter().map(|op| op.id()).collect();
            assert_eq!(ours, expected, "width {width}");
        }
        assert_eq!(Op::Invalid.id(), -1);
        assert_eq!(Op::Constant.id(), 0);
    }

    #[test]
    fn scorers_have_the_documented_shapes() {
        assert_eq!(
            (num8::N_FEATURES, num8::B1.len(), num8::N_OUTPUTS),
            (18, 106, 11)
        );
        assert_eq!(
            (num16::N_FEATURES, num16::B1.len(), num16::N_OUTPUTS),
            (19, 88, 12)
        );
        assert_eq!(
            (num32::N_FEATURES, num32::B1.len(), num32::N_OUTPUTS),
            (20, 75, 11)
        );
        assert_eq!(
            (num64::N_FEATURES, num64::B1.len(), num64::N_OUTPUTS),
            (17, 33, 11)
        );
    }
}
