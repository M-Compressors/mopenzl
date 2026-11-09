//! The model decision (`generic_numeric_decide{8,16,32,64}.c`,
//! `score_guard.h` and the operation choice of `selector_numeric.c`).
//!
//! Extract the features of the stream, score every operation, mask the
//! provably useless ones with the guards and take the highest logit among
//! the survivors.

use super::features::{
    Features, FeaturesV2, extract_num8, extract_num64_decision, extract_sub64_decision,
    half_width_threshold,
};
use super::scorers::{self, MAX_OPERATIONS, Op, num8, num16, num32, num64};

/// `TRS_GenericNumericDecisionCore`.
#[derive(Clone, Debug)]
pub(super) struct Decision {
    /// `best_op`.
    pub(super) best_op: Op,
    /// `best_index`: the scorer's output of the best operation, `None` for
    /// constant or invalid.
    pub(super) best_index: Option<usize>,
    /// `score_count`.
    pub(super) score_count: usize,
    /// Post-guard scores.
    pub(super) scores: [f32; MAX_OPERATIONS],
    /// Pre-sigmoid outputs.
    pub(super) logits: [f32; MAX_OPERATIONS],
    /// The operation of every output (`TRS_score_numN_operation_ids`).
    pub(super) operations: &'static [Op],
}

impl Decision {
    /// A decision with no scores.
    const fn unscored(best_op: Op, operations: &'static [Op]) -> Self {
        Self {
            best_op,
            best_index: None,
            score_count: 0,
            scores: [0.0; MAX_OPERATIONS],
            logits: [0.0; MAX_OPERATIONS],
            operations,
        }
    }
}

/// The score-guard view of the features (`TRS_ScoreGuardFeatures`).
#[derive(Copy, Clone, Debug)]
struct GuardFeatures {
    cardinality_est: u64,
    range_u: u64,
    min_lb0: u64,
    min_u: u64,
    max_u: u64,
}

/// `TRS_apply_score_guards_common`: zeroes the scores of operations that
/// provably do nothing.
fn apply_score_guards(scores: &mut [f32], operations: &[Op], f: GuardFeatures, rp_threshold: u64) {
    // Range density is cardinality / domain size; widen before adding one so
    // a full-width span does not wrap.
    let rd = f.cardinality_est as f64 / (f.range_u as f64 + 1.0);
    // The float-rounded cutoff of the training runtime, not the double 0.9.
    let dense_range_threshold = f64::from(0.90f32);

    for (score, &op) in scores.iter_mut().zip(operations) {
        // min_u == 1 guarantees divide_by_gcd is a no-op; min_lb0 == 0
        // preserves the training policy for inputs with an odd nonzero value.
        if (f.min_lb0 == 0 || f.min_u == 1) && op == Op::DivideByGcd {
            *score = 0.0;
        }
        // Tokenize is pointless on dense-range data.
        if rd > dense_range_threshold
            && (op == Op::TokenizeNumeric || op == Op::TokenizeNumericSorted)
        {
            *score = 0.0;
        }
        // Range pack is useless when the minimum is already zero and the
        // range exceeds half the width.
        if f.min_u == 0 && f.max_u >= rp_threshold && op == Op::RangePack {
            *score = 0.0;
        }
    }
}

/// `TRS_score_guard_argmax_masked`: the surviving (score above zero)
/// output with the highest logit, the first on ties.
fn argmax_masked(logits: &[f32], scores: &[f32]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, &score) in scores.iter().enumerate() {
        if score <= 0.0 {
            continue;
        }
        if best.is_none_or(|b| logits[i] > logits[b]) {
            best = Some(i);
        }
    }
    best
}

/// The scoring tail shared by every width: scores, guards, argmax.
fn finish(
    decision: &mut Decision,
    guard: GuardFeatures,
    width: usize,
    score: impl FnOnce(&mut [f32; MAX_OPERATIONS], &mut [f32; MAX_OPERATIONS]),
) {
    let count = decision.operations.len();
    decision.score_count = count;
    score(&mut decision.scores, &mut decision.logits);
    apply_score_guards(
        &mut decision.scores[..count],
        decision.operations,
        guard,
        half_width_threshold(width),
    );
    decision.best_index = argmax_masked(&decision.logits[..count], &decision.scores[..count]);
    if let Some(index) = decision.best_index {
        decision.best_op = decision.operations[index];
    }
}

fn legacy_guard(f: &Features) -> GuardFeatures {
    GuardFeatures {
        cardinality_est: f.cardinality_est,
        range_u: f.range_u,
        min_lb0: f.min_lb0,
        min_u: f.min_u,
        max_u: f.max_u,
    }
}

/// `TRS_generic_numeric_decide8`.
pub(super) fn decide8(data: &[u8]) -> Decision {
    let f = extract_num8(data);
    decide_with(&f, &num8::OPERATION_IDS, 1, |scores, logits| {
        scorers::score_num8(&f, scores, logits);
    })
}

/// `TRS_generic_numeric_decide16_fast`.
pub(super) fn decide16(data: &[u8]) -> Decision {
    let f = extract_sub64_decision(data, 2);
    decide_with(&f, &num16::OPERATION_IDS, 2, |scores, logits| {
        scorers::score_num16(&f, scores, logits);
    })
}

/// `TRS_generic_numeric_decide32_fast`.
pub(super) fn decide32(data: &[u8]) -> Decision {
    let f = extract_sub64_decision(data, 4);
    decide_with(&f, &num32::OPERATION_IDS, 4, |scores, logits| {
        scorers::score_num32(&f, scores, logits);
    })
}

/// `generic_numeric_finish_decide{8,16,32}`.
fn decide_with(
    f: &Features,
    operations: &'static [Op],
    width: usize,
    score: impl FnOnce(&mut [f32; MAX_OPERATIONS], &mut [f32; MAX_OPERATIONS]),
) -> Decision {
    let mut decision = Decision::unscored(Op::Invalid, operations);
    if f.range_s == 0 {
        decision.best_op = Op::Constant;
        return decision;
    }
    finish(&mut decision, legacy_guard(f), width, score);
    decision
}

/// `TRS_generic_numeric_decide64_v2_fast`.
pub(super) fn decide64(data: &[u8]) -> Decision {
    let f: FeaturesV2 = extract_num64_decision(data);
    let mut decision = Decision::unscored(Op::Invalid, &num64::OPERATION_IDS);
    if f.range_s == 0 {
        decision.best_op = Op::Constant;
        return decision;
    }
    let guard = GuardFeatures {
        cardinality_est: f.cardinality_est,
        range_u: f.range_u,
        min_lb0: f.min_lb0,
        min_u: f.min_u,
        max_u: f.max_u,
    };
    finish(&mut decision, guard, 8, |scores, logits| {
        scorers::score_num64(&f, scores, logits);
    });
    decision
}

/// `SI_TransformerSupportedOperations`.
#[derive(Copy, Clone, Debug)]
pub(super) struct SupportedOperations {
    /// Both constant nodes are available.
    pub(super) constant: bool,
    /// `divide_by` is available.
    pub(super) divide_by_gcd: bool,
    /// `sparse_num` is available.
    pub(super) sparse_num: bool,
}

/// `transformer_operation_is_supported`.
const fn operation_is_supported(op: Op, supported: SupportedOperations) -> bool {
    match op {
        Op::Invalid => false,
        Op::Constant => supported.constant,
        Op::DivideByGcd => supported.divide_by_gcd,
        Op::SparseNum => supported.sparse_num,
        Op::Bitpack
        | Op::DeltaInt
        | Op::Entropy
        | Op::FieldLz
        | Op::Fse
        | Op::RangePack
        | Op::Store
        | Op::TokenizeNumeric
        | Op::TokenizeNumericSorted
        | Op::Zstd => true,
    }
}

/// `transformer_select_supported_operation`: the decision's operation when
/// the format version supports it, else the supported survivor with the
/// highest logit; `Invalid` when none.
pub(super) fn select_supported_operation(
    decision: &Decision,
    supported: SupportedOperations,
) -> Op {
    let Some(best_index) = decision.best_index else {
        return if operation_is_supported(decision.best_op, supported) {
            decision.best_op
        } else {
            Op::Invalid
        };
    };
    if decision.scores[best_index] <= 0.0 {
        return Op::Invalid;
    }
    let best_operation = decision.operations[best_index];
    if operation_is_supported(best_operation, supported) {
        return best_operation;
    }
    let mut best_supported: Option<usize> = None;
    for index in 0..decision.score_count {
        let operation = decision.operations[index];
        if decision.scores[index] <= 0.0 || !operation_is_supported(operation, supported) {
            continue;
        }
        if best_supported.is_none_or(|b| decision.logits[index] > decision.logits[b]) {
            best_supported = Some(index);
        }
    }
    best_supported.map_or(Op::Invalid, |index| decision.operations[index])
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: SupportedOperations = SupportedOperations {
        constant: true,
        divide_by_gcd: true,
        sparse_num: true,
    };

    fn scored(scores: &[f32], logits: &[f32], ops: &'static [Op]) -> Decision {
        let mut decision = Decision::unscored(Op::Invalid, ops);
        decision.score_count = ops.len();
        decision.scores[..scores.len()].copy_from_slice(scores);
        decision.logits[..logits.len()].copy_from_slice(logits);
        decision.best_index =
            argmax_masked(&decision.logits[..ops.len()], &decision.scores[..ops.len()]);
        decision
    }

    #[test]
    fn guards_mask_useless_operations() {
        let ops = [
            Op::DivideByGcd,
            Op::TokenizeNumeric,
            Op::RangePack,
            Op::Zstd,
        ];
        let mut f = GuardFeatures {
            cardinality_est: 10,
            range_u: 1000,
            min_lb0: 3,
            min_u: 5,
            max_u: 300,
        };
        let mut scores = [1.0f32; 4];
        apply_score_guards(&mut scores, &ops, f, 256);
        assert_eq!(scores, [1.0; 4]);
        f.min_lb0 = 0;
        f.cardinality_est = 1000;
        f.min_u = 0;
        let mut scores = [1.0f32; 4];
        apply_score_guards(&mut scores, &ops, f, 256);
        assert_eq!(scores, [0.0, 0.0, 0.0, 1.0]);
        // The cutoff is the float 0.9, a hair below 90%: exactly 90% dense is
        // above it, 89% is not.
        f.min_lb0 = 1;
        f.min_u = 2;
        f.range_u = 99;
        f.cardinality_est = 90;
        let mut scores = [1.0f32; 4];
        apply_score_guards(&mut scores, &ops, f, 256);
        assert_eq!(scores, [1.0, 0.0, 1.0, 1.0]);
        f.cardinality_est = 89;
        let mut scores = [1.0f32; 4];
        apply_score_guards(&mut scores, &ops, f, 256);
        assert_eq!(scores, [1.0; 4]);
    }

    #[test]
    fn argmax_takes_the_first_best_logit_among_survivors() {
        assert_eq!(argmax_masked(&[1.0, 2.0, 2.0], &[1.0, 1.0, 1.0]), Some(1));
        assert_eq!(argmax_masked(&[1.0, 5.0, 2.0], &[1.0, 0.0, 1.0]), Some(2));
        assert_eq!(argmax_masked(&[1.0, 5.0], &[0.0, 0.0]), None);
    }

    #[test]
    fn unsupported_best_operation_falls_back_to_the_next_logit() {
        let ops: &'static [Op] = &[Op::DivideByGcd, Op::Zstd, Op::SparseNum];
        let decision = scored(&[1.0, 1.0, 1.0], &[3.0, 1.0, 2.0], ops);
        assert_eq!(select_supported_operation(&decision, ALL), Op::DivideByGcd);
        let no_gcd = SupportedOperations {
            divide_by_gcd: false,
            ..ALL
        };
        assert_eq!(select_supported_operation(&decision, no_gcd), Op::SparseNum);
        let neither = SupportedOperations {
            sparse_num: false,
            ..no_gcd
        };
        assert_eq!(select_supported_operation(&decision, neither), Op::Zstd);
    }

    #[test]
    fn invalid_and_constant_decisions() {
        let ops: &'static [Op] = &[Op::DivideByGcd];
        let mut decision = scored(&[1.0], &[1.0], ops);
        let no_gcd = SupportedOperations {
            divide_by_gcd: false,
            ..ALL
        };
        assert_eq!(select_supported_operation(&decision, no_gcd), Op::Invalid);
        decision.best_index = None;
        decision.best_op = Op::Constant;
        assert_eq!(select_supported_operation(&decision, ALL), Op::Constant);
        let no_constant = SupportedOperations {
            constant: false,
            ..ALL
        };
        assert_eq!(
            select_supported_operation(&decision, no_constant),
            Op::Invalid
        );
        // A best index whose score was zeroed after the fact is invalid.
        let mut zeroed = scored(&[1.0], &[1.0], ops);
        zeroed.scores[0] = 0.0;
        assert_eq!(select_supported_operation(&zeroed, ALL), Op::Invalid);
    }

    #[test]
    fn constant_streams_are_decided_without_scoring() {
        assert_eq!(decide8(&[7; 40]).best_op, Op::Constant);
        assert_eq!(decide16(&[7; 40]).best_op, Op::Constant);
        assert_eq!(decide32(&[7; 40]).best_op, Op::Constant);
        assert_eq!(decide64(&[7; 40]).best_op, Op::Constant);
        assert_eq!(decide8(&[7; 40]).score_count, 0);
    }

    #[test]
    fn varied_streams_are_scored() {
        let bytes: alloc::vec::Vec<u8> = (0..200u32).map(|i| (i * 37 % 251) as u8).collect();
        for decision in [
            decide8(&bytes),
            decide16(&bytes),
            decide32(&bytes),
            decide64(&bytes[..192]),
        ] {
            assert!(decision.score_count > 0);
            assert!(decision.best_index.is_some() || decision.best_op == Op::Invalid);
        }
    }
}
