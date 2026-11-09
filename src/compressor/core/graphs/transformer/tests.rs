//! Differential and branch-coverage tests of the Transformer selectors.
//!
//! The scorers and the decision (features, scores, logits, guards) are
//! compared bit for bit with the reference through the `enc_transformer`
//! shim; the static selectors are pure functions of the data, whose every
//! outcome the corpus must reach at least once.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::decide::{SupportedOperations, decide16, select_supported_operation};
use super::ids;
use super::numeric1::numeric1_select;
use super::scorers;
use super::static_select::{core_select, fallback_select, index_select};
use crate::compressor::core::registry::GraphId;

/// A deterministic generator (xorshift64*).
pub(super) struct Rng(u64);

impl Rng {
    pub(super) fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub(super) fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..bound` (`bound > 0`).
    pub(super) fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    /// A value with a uniformly random bit length.
    #[cfg(not(miri))]
    fn magnitude(&mut self) -> u64 {
        let bits = self.below(65);
        if bits == 0 {
            0
        } else {
            self.next() >> (64 - bits)
        }
    }
}

/// Packs `values` into `width`-byte little-endian elements.
pub(super) fn pack(values: &[u64], width: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * width);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes()[..width]);
    }
    out
}

/// Named numeric streams that drive every branch of the Transformer: ramps,
/// small alphabets, sparse data, sorted data, multiples of a common factor,
/// random data, timestamps, floating-point bit patterns and more, each at
/// several lengths. Values are already truncated to `width` bytes.
pub(super) fn shapes(width: usize, lengths: &[usize]) -> Vec<(String, Vec<u8>)> {
    let bits = (width * 8) as u32;
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    for &n in lengths {
        let mut rng = Rng::new(n as u64 * 31 + width as u64);
        let mut add = |name: &str, values: Vec<u64>| {
            let values: Vec<u64> = values.into_iter().map(|v| v & mask).collect();
            out.push((format!("{name}-w{width}-n{n}"), pack(&values, width)));
        };
        add("constant", alloc::vec![7; n]);
        add("zeros", alloc::vec![0; n]);
        add("ramp", (0..n as u64).collect());
        add("ramp-step3", (0..n as u64).map(|i| i * 3 + 5).collect());
        add(
            "ramp-big-offset",
            (0..n as u64).map(|i| (1u64 << (bits - 2)) + i).collect(),
        );
        add("descending", (0..n as u64).rev().collect());
        add(
            "timestamps",
            (0..n as u64)
                .map(|i| 1_600_000_000_000 + i * 1000 + (i * 7919) % 13)
                .collect(),
        );
        add(
            "sorted-random",
            (0..n)
                .scan(0u64, |state, _| {
                    *state += rng.below(50);
                    Some(*state)
                })
                .collect(),
        );
        add("random", (0..n).map(|_| rng.next()).collect());
        add("random-4bit", (0..n).map(|_| rng.below(16)).collect());
        add("random-8bit", (0..n).map(|_| rng.below(256)).collect());
        add("random-12bit", (0..n).map(|_| rng.below(4096)).collect());
        add("random-20bit", (0..n).map(|_| rng.below(1 << 20)).collect());
        for k in [2u64, 3, 5, 16, 40, 200] {
            let alphabet: Vec<u64> = (0..k).map(|_| rng.next()).collect();
            add(
                &format!("alphabet-{k}"),
                (0..n).map(|_| alphabet[rng.below(k) as usize]).collect(),
            );
            let small: Vec<u64> = (0..k).map(|i| i * 2 + 1).collect();
            add(
                &format!("small-alphabet-{k}"),
                (0..n).map(|_| small[rng.below(k) as usize]).collect(),
            );
        }
        add(
            "sparse-1pct",
            (0..n)
                .map(|_| if rng.below(100) == 0 { rng.next() } else { 0 })
                .collect(),
        );
        add(
            "sparse-10pct",
            (0..n)
                .map(|_| {
                    if rng.below(10) == 0 {
                        rng.below(1000)
                    } else {
                        0
                    }
                })
                .collect(),
        );
        add(
            "sparse-60pct",
            (0..n)
                .map(|_| {
                    if rng.below(10) < 4 {
                        rng.below(60) + 1
                    } else {
                        0
                    }
                })
                .collect(),
        );
        for factor in [24u64, 256, 1000, 4096, 65536] {
            add(
                &format!("gcd-{factor}"),
                (0..n).map(|_| rng.below(5000) * factor).collect(),
            );
        }
        add(
            "signed-small",
            (0..n).map(|_| rng.below(40).wrapping_sub(20)).collect(),
        );
        add(
            "signed-wide",
            (0..n)
                .map(|_| rng.below(20000).wrapping_sub(10000))
                .collect(),
        );
        add("runs", {
            let mut values = Vec::new();
            while values.len() < n {
                let run = 1 + rng.below(20) as usize;
                let value = rng.below(300);
                values.extend(core::iter::repeat_n(value, run));
            }
            values.truncate(n);
            values
        });
        add(
            "random-walk",
            (0..n)
                .scan(1u64 << (bits.min(40) - 2), |state, _| {
                    *state = state.wrapping_add(rng.below(7)).wrapping_sub(3);
                    Some(*state)
                })
                .collect(),
        );
        add("sawtooth", (0..n as u64).map(|i| i % 250).collect());
        // A few wide steps of a regular grid: a small dictionary, a tiny
        // delta alphabet.
        add(
            "sawtooth-step200",
            (0..n as u64).map(|i| (i % 100) * 200).collect(),
        );
        // A random walk of wide steps: many raw values, fewer deltas.
        add(
            "walk-wide-steps",
            (0..n)
                .scan(1u64 << 30, |state, _| {
                    *state =
                        state.wrapping_add((rng.below(601) as i64 - 300).wrapping_mul(1000) as u64);
                    Some(*state)
                })
                .collect(),
        );
        add(
            "alternating",
            (0..n as u64)
                .map(|i| if i % 2 == 0 { 3 } else { 1 << (bits - 3) })
                .collect(),
        );
        add("duplicated-ramp", (0..n as u64).map(|i| i / 4).collect());
        add(
            "one-outlier",
            (0..n as u64)
                .map(|i| if i == 0 { 1 << (bits - 1) } else { 5 })
                .collect(),
        );
        add(
            "smooth-signal",
            (0..n)
                .map(|i| (3000.0 + 2500.0 * (i as f64 / 40.0).sin()) as u64)
                .collect(),
        );
        // Floating-point bit patterns of a smooth signal.
        match width {
            4 => add(
                "floats",
                (0..n)
                    .map(|i| u64::from(((i as f32 / 17.0).sin() * 100.0 + 200.0).to_bits()))
                    .collect(),
            ),
            8 => {
                add(
                    "floats",
                    (0..n)
                        .map(|i| ((i as f64 / 17.0).sin() * 100.0 + 200.0).to_bits())
                        .collect(),
                );
                add(
                    "floats-repeated",
                    (0..n)
                        .map(|i| (((i % 90) as f64) * 0.25 + 100.0).to_bits())
                        .collect(),
                );
                add(
                    "floats-quantized",
                    (0..n)
                        .map(|_| (100.0 + rng.below(30000) as f64 * 0.01).to_bits())
                        .collect(),
                );
            }
            2 => add(
                "floats",
                (0..n)
                    .map(|i| 0x4000 + (((i as f32 / 17.0).sin() * 400.0) as u64 & 0x3FF))
                    .collect(),
            ),
            _ => {}
        }
        // Float64-like values: clustered exponents, few repeats.
        add(
            "clustered-exponents",
            (0..n)
                .map(|_| (1023u64 + 5) << 52 | (rng.next() >> 12))
                .collect(),
        );
    }
    out
}

/// The lengths the differential tests run: tiny, around the gates, large.
pub(super) fn lengths() -> &'static [usize] {
    if cfg!(miri) {
        &[1, 40, 300]
    } else {
        &[1, 2, 3, 10, 64, 300, 1000, 2500, 12000]
    }
}

/// Comparisons with the reference's scorers and decision, through the C
/// shim (not available under Miri).
#[cfg(not(miri))]
mod reference {
    use super::*;
    use crate::compressor::core::graphs::transformer::decide::{
        Decision, decide8, decide32, decide64,
    };
    use crate::compressor::core::graphs::transformer::features::{
        Features, FeaturesV2, extract_num8, extract_num64_decision, extract_sub64_decision,
    };
    use crate::compressor::core::graphs::transformer::scorers::MAX_OPERATIONS;
    use openzl_ffi::enc_transformer::{self, LEGACY_FIELDS};

    /// Equal as the reference's bits, NaNs aside.
    ///
    /// With `no_std` (which `--all-features` enables) `expf` comes from the
    /// `libm` crate, which can differ from the platform's in the last bit; the
    /// network's sigmoid then may be one or two ULP away, so that build compares
    /// with that tolerance. The `std` build, whose libm is the reference's, is
    /// exact.
    fn same_floats(ours: &[f32], theirs: &[f32]) -> bool {
        let tolerance: u32 = if cfg!(feature = "no_std") { 2 } else { 0 };
        ours.iter().zip(theirs).all(|(a, b)| {
            (a.is_nan() && b.is_nan())
                || (a.is_sign_negative() == b.is_sign_negative()
                    && (a.to_bits() & 0x7FFF_FFFF).abs_diff(b.to_bits() & 0x7FFF_FFFF) <= tolerance)
        })
    }

    /// The legacy features in the shim's 25-slot layout (fields this crate does
    /// not compute are zero).
    fn legacy_slots(f: &Features) -> [u64; LEGACY_FIELDS] {
        let mut slots = [0u64; LEGACY_FIELDS];
        slots[0] = f.min_u;
        slots[1] = f.max_u;
        slots[4] = f.count as u64;
        slots[5] = f.max_run_length as u64;
        slots[6] = f.zero_count as u64;
        slots[7] = f.delta_up_count as u64;
        slots[8] = f.delta_down_count as u64;
        slots[9] = f.delta_min_s as u64;
        slots[10] = f.delta_max_s as u64;
        slots[11] = f.mean_abs_dev_u_fp;
        slots[12] = f.mean_abs_delta_u_fp;
        slots[13] = f.cardinality_est;
        slots[14] = f.pair_cardinality_est;
        slots[15] = f.d8_cardinality_est;
        slots[16] = f.sorted_gap_cv_fp;
        slots[17] = f.transition_gap_cv_fp;
        slots[18] = f.min_lb0;
        slots[19] = f.match4;
        slots[20] = f.range_u;
        slots[21] = f.range_s;
        slots[22] = f.zero_ratio_fp;
        slots[23] = f.delta_up_ratio_fp;
        slots[24] = f.delta_down_ratio_fp;
        slots
    }

    /// A `Features` from the shim's slots (the fields this crate has).
    fn features_from_slots(slots: &[u64; LEGACY_FIELDS]) -> Features {
        Features {
            min_u: slots[0],
            max_u: slots[1],
            count: slots[4] as usize,
            max_run_length: slots[5] as usize,
            zero_count: slots[6] as usize,
            delta_up_count: slots[7] as usize,
            delta_down_count: slots[8] as usize,
            delta_min_s: slots[9] as i64,
            delta_max_s: slots[10] as i64,
            mean_abs_dev_u_fp: slots[11],
            mean_abs_delta_u_fp: slots[12],
            cardinality_est: slots[13],
            pair_cardinality_est: slots[14],
            d8_cardinality_est: slots[15],
            sorted_gap_cv_fp: slots[16],
            transition_gap_cv_fp: slots[17],
            min_lb0: slots[18],
            match4: slots[19],
            range_u: slots[20],
            range_s: slots[21],
            zero_ratio_fp: slots[22],
            delta_up_ratio_fp: slots[23],
            delta_down_ratio_fp: slots[24],
        }
    }

    /// Zeroes the slots a width does not read, so both sides see equal inputs.
    fn mask_slots(slots: &mut [u64; LEGACY_FIELDS], keep: &[usize]) {
        for (index, slot) in slots.iter_mut().enumerate() {
            if !keep.contains(&index) {
                *slot = 0;
            }
        }
    }

    /// The slots the decision extractors compute for widths 2 and 4.
    const DECISION_SLOTS: [usize; 15] = [0, 1, 9, 10, 12, 13, 14, 15, 16, 18, 20, 21, 22, 23, 24];

    #[test]
    fn legacy_scorers_match_the_reference_bit_for_bit() {
        let mut rng = Rng::new(0xFEED);
        for round in 0..if cfg!(miri) { 20 } else { 600 } {
            let width = [1usize, 2, 4][round % 3];
            let mut slots = [0u64; LEGACY_FIELDS];
            for slot in &mut slots {
                *slot = rng.magnitude();
            }
            // A plausible element count and ratios no larger than 1.0 in Q32.
            slots[4] = 1 + rng.magnitude() % (1 << 32);
            for index in [22, 23, 24] {
                slots[index] %= (1 << 32) + 1;
            }
            // Let the signed delta extrema take both signs.
            if rng.below(2) == 0 {
                slots[9] = (slots[9] as i64).wrapping_neg() as u64;
            }
            let f = features_from_slots(&slots);
            let slots = legacy_slots(&f);
            let (c_scores, c_logits) = enc_transformer::score_legacy(width, &slots);
            let mut scores = [0.0f32; MAX_OPERATIONS];
            let mut logits = [0.0f32; MAX_OPERATIONS];
            match width {
                1 => scorers::score_num8(&f, &mut scores, &mut logits),
                2 => scorers::score_num16(&f, &mut scores, &mut logits),
                _ => scorers::score_num32(&f, &mut scores, &mut logits),
            }
            let n = [11, 12, 11][round % 3];
            assert!(
                same_floats(&scores[..n], &c_scores[..n]),
                "width {width} round {round}: {:?} vs {:?}",
                &scores[..n],
                &c_scores[..n]
            );
            assert!(
                same_floats(&logits[..n], &c_logits[..n]),
                "width {width} round {round} logits"
            );
        }
    }

    #[test]
    fn num64_scorer_matches_the_reference_bit_for_bit() {
        let mut rng = Rng::new(0xBEEF);
        for round in 0..if cfg!(miri) { 20 } else { 400 } {
            let mut ints = [0u64; 16];
            for value in &mut ints[..8] {
                *value = rng.magnitude();
            }
            ints[0] = 1 + rng.magnitude() % (1 << 40);
            let mut floats = [0.0f64; 16];
            for (index, value) in floats[..6].iter_mut().enumerate() {
                *value = if index == 3 {
                    (rng.magnitude() as f64) * (rng.below(1000) as f64 / 1000.0)
                } else {
                    rng.below(1_000_001) as f64 / 1_000_000.0
                };
            }
            let f = FeaturesV2 {
                count: ints[0],
                min_u: ints[1],
                max_u: ints[2],
                range_u: ints[3],
                range_s: ints[4],
                cardinality_est: ints[5],
                pair_cardinality_est: ints[6],
                min_lb0: ints[7],
                zero_ratio: floats[0],
                delta_up_ratio: floats[1],
                delta_down_ratio: floats[2],
                mean_abs_delta_u: floats[3],
                sorted_gap_nmad: floats[4],
                sorted_gap_mode: floats[5],
            };
            let (c_scores, c_logits) = enc_transformer::score_v2(&ints, &floats);
            let mut scores = [0.0f32; MAX_OPERATIONS];
            let mut logits = [0.0f32; MAX_OPERATIONS];
            scorers::score_num64(&f, &mut scores, &mut logits);
            assert!(
                same_floats(&scores[..11], &c_scores[..11]),
                "round {round}: {:?} vs {:?}; logits {:?} vs {:?}",
                &scores[..11],
                &c_scores[..11],
                &logits[..11],
                &c_logits[..11]
            );
            assert!(
                same_floats(&logits[..11], &c_logits[..11]),
                "round {round} logits"
            );
        }
    }

    /// Compares the decision of `bytes` with the reference's.
    fn compare_decision(width: usize, bytes: &[u8], label: &str) -> Decision {
        let c = enc_transformer::decide(width, bytes);
        let ours = match width {
            1 => decide8(bytes),
            2 => decide16(bytes),
            4 => decide32(bytes),
            _ => decide64(bytes),
        };
        match width {
            1 => {
                let f = extract_num8(bytes);
                let mut theirs = c.legacy;
                // The signed extrema only feed `range_s`.
                theirs[2] = 0;
                theirs[3] = 0;
                assert_eq!(legacy_slots(&f), theirs, "{label}: num8 features");
            }
            2 | 4 => {
                let f = extract_sub64_decision(bytes, width);
                let mut ours = legacy_slots(&f);
                let mut theirs = c.legacy;
                mask_slots(&mut ours, &DECISION_SLOTS);
                mask_slots(&mut theirs, &DECISION_SLOTS);
                if width == 4 {
                    // The reference leaves `d8_cardinality_est` at zero for num32.
                    assert_eq!(f.d8_cardinality_est, 0, "{label}");
                }
                assert_eq!(ours, theirs, "{label}: sub64 features");
                assert_eq!(f.count as u64, c.legacy[4], "{label}: count");
            }
            _ => {
                let f = extract_num64_decision(bytes);
                let ints = [
                    f.count,
                    f.min_u,
                    f.max_u,
                    f.range_u,
                    f.range_s,
                    f.cardinality_est,
                    f.pair_cardinality_est,
                    f.min_lb0,
                ];
                assert_eq!(ints[..], c.v2_ints[..8], "{label}: num64 integer features");
                let floats = [
                    f.zero_ratio,
                    f.delta_up_ratio,
                    f.delta_down_ratio,
                    f.mean_abs_delta_u,
                    f.sorted_gap_nmad,
                    f.sorted_gap_mode,
                ];
                let same = floats
                    .iter()
                    .zip(&c.v2_floats)
                    .all(|(a, b)| a.to_bits() == b.to_bits());
                assert!(
                    same,
                    "{label}: num64 float features {floats:?} vs {:?}",
                    &c.v2_floats[..6]
                );
            }
        }
        assert_eq!(ours.best_op.id(), c.best_op, "{label}: best op");
        assert_eq!(
            ours.best_index.map_or(-1, |i| i as i32),
            c.best_index,
            "{label}: best index"
        );
        assert_eq!(
            ours.score_count as i32, c.score_count,
            "{label}: score count"
        );
        let n = ours.score_count;
        assert!(
            same_floats(&ours.scores[..n], &c.scores[..n]),
            "{label}: scores"
        );
        assert!(
            same_floats(&ours.logits[..n], &c.logits[..n]),
            "{label}: logits"
        );
        ours
    }

    #[test]
    fn decisions_match_the_reference_on_every_shape() {
        let mut ops = BTreeSet::new();
        let mut count = 0;
        for width in [1usize, 2, 4, 8] {
            for (name, bytes) in shapes(width, lengths()) {
                let decision = compare_decision(width, &bytes, &name);
                ops.insert((width, decision.best_op.id()));
                count += 1;
            }
        }
        // The corpus exercises both the constant and the scored paths.
        assert!(count > 500, "{count} streams");
        assert!(ops.iter().any(|&(_, op)| op == 0), "no constant decision");
        assert!(
            ops.len() > 12,
            "only {} distinct (width, op) decisions",
            ops.len()
        );
    }

    #[test]
    fn decisions_match_the_reference_on_random_bytes() {
        let mut rng = Rng::new(99);
        for round in 0..if cfg!(miri) { 5 } else { 60 } {
            for width in [1usize, 2, 4, 8] {
                let n = 1 + rng.below(3000) as usize;
                let mut bytes = alloc::vec![0u8; n * width];
                // Mix of dense random bytes and a restricted alphabet.
                let alphabet = 1 + rng.below(255);
                for byte in &mut bytes {
                    *byte = (rng.below(alphabet)) as u8;
                }
                compare_decision(
                    width,
                    &bytes,
                    &format!("random width {width} round {round}"),
                );
            }
        }
    }
}

const ALL_SUPPORTED: SupportedOperations = SupportedOperations {
    constant: true,
    divide_by_gcd: true,
    sparse_num: true,
};

/// Every graph the model-driven selector can route to.
#[test]
fn the_model_reaches_every_operation() {
    let mut reached: BTreeSet<u32> = BTreeSet::new();
    for width in [1usize, 2, 4, 8] {
        for (_, bytes) in shapes(width, lengths()) {
            reached.insert(numeric1_select(1, width, &bytes, ALL_SUPPORTED).index);
        }
    }
    let expected = [
        ids::STORE,
        ids::CONSTANT,
        ids::BITPACK,
        ids::DELTA_INT,
        ids::RANGE_PACK,
        ids::DIVIDE_BY_GCD,
        ids::ENTROPY,
        ids::FIELD_LZ,
        ids::FSE,
        ids::SPARSE_NUM,
        ids::TOKENIZE_NUMERIC,
        ids::TOKENIZE_NUMERIC_SORTED,
        ids::ZSTD,
    ];
    let missing: Vec<u32> = expected
        .iter()
        .map(|id| id.index)
        .filter(|id| !reached.contains(id))
        .collect();
    assert!(
        missing.is_empty(),
        "never routed to graphs {missing:?}; reached {reached:?}"
    );
}

/// The depth guard and the empty stream.
#[test]
fn the_model_selector_guards_depth_and_emptiness() {
    let data = pack(&(0..500u64).collect::<Vec<_>>(), 4);
    assert_eq!(numeric1_select(1, 4, &[], ALL_SUPPORTED), ids::STORE);
    assert_ne!(
        numeric1_select(24, 4, &data, ALL_SUPPORTED),
        ids::STATIC_FALLBACK
    );
    assert_eq!(
        numeric1_select(25, 4, &data, ALL_SUPPORTED),
        ids::STATIC_FALLBACK
    );
    assert_eq!(
        numeric1_select(1, 3, &data, ALL_SUPPORTED),
        ids::STATIC_FALLBACK
    );
}

/// Unsupported operations are replaced by the best supported one, or by the
/// fallback when none remains.
#[test]
fn unsupported_operations_fall_back() {
    let none = SupportedOperations {
        constant: false,
        divide_by_gcd: false,
        sparse_num: false,
    };
    let mut fell_back = 0;
    for width in [1usize, 2, 4, 8] {
        for (_, bytes) in shapes(width, &[300, 2500]) {
            let all = numeric1_select(1, width, &bytes, ALL_SUPPORTED);
            let some = numeric1_select(1, width, &bytes, none);
            assert!(
                some != ids::DIVIDE_BY_GCD && some != ids::SPARSE_NUM && some != ids::CONSTANT,
                "{some:?}"
            );
            if all != some {
                fell_back += 1;
            }
        }
    }
    assert!(fell_back > 10, "{fell_back}");
}

/// The graphs a selector function returns over the corpus, at every format
/// version class and graph depth the guards distinguish.
fn reached(select: impl Fn(u32, u32, usize, &[u8]) -> GraphId, lengths: &[usize]) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    for width in [1usize, 2, 4, 8] {
        for (_, bytes) in shapes(width, lengths) {
            for (version, depth) in [(27, 1), (21, 1), (10, 1), (27, 31)] {
                out.insert(select(version, depth, width, &bytes).index);
            }
        }
    }
    out
}

fn ids_of(list: &[GraphId]) -> BTreeSet<u32> {
    list.iter().map(|id| id.index).collect()
}

#[test]
fn static_core_reaches_every_outcome() {
    let found = reached(core_select, lengths());
    let expected = ids_of(&[
        ids::STORE,
        ids::CONSTANT,
        ids::STATIC_DELTA,
        ids::STATIC_ZIGZAG,
        ids::STATIC_GCD,
        ids::STATIC_RANGE_PACK,
        ids::STATIC_RANGE_PACK_DELTA,
        ids::FSE,
        ids::ZSTD,
        ids::FIELD_LZ,
    ]);
    let missing: Vec<_> = expected.difference(&found).collect();
    assert!(
        missing.is_empty(),
        "static core never returned {missing:?}; found {found:?}"
    );
}

#[test]
fn static_core_empty_and_constant_inputs() {
    assert_eq!(core_select(27, 1, 4, &[]), ids::STORE);
    assert_eq!(core_select(27, 1, 4, &pack(&[5, 5, 5], 4)), ids::CONSTANT);
    assert_eq!(core_select(10, 1, 4, &pack(&[5, 5, 5], 4)), ids::STORE);
    assert_eq!(core_select(27, 1, 4, &pack(&[5], 4)), ids::CONSTANT);
    let noisy = pack(
        &(0..3000u64).map(|i| i * 7919 % 10007).collect::<Vec<_>>(),
        1,
    );
    assert_eq!(core_select(27, 31, 1, &noisy), ids::ZSTD);
    assert_eq!(core_select(27, 31, 2, &noisy), ids::FIELD_LZ);
}

#[test]
fn static_index_reaches_both_outcomes() {
    let found = reached(|_, _, w, b| index_select(w, b), lengths());
    let expected = ids_of(&[ids::STATIC_DELTA, ids::STATIC_CORE]);
    assert_eq!(found, expected);
}

#[test]
fn static_index_recognizes_smooth_indices() {
    // Smooth num8 indices.
    let smooth: Vec<u64> = (0..2000u64).map(|i| (i / 10) % 200 + (i % 3)).collect();
    assert_eq!(index_select(1, &pack(&smooth, 1)), ids::STATIC_DELTA);
    // Random num8 and short streams go to the core selector.
    let random: Vec<u64> = (0..2000u64).map(|i| i * 7919 % 251).collect();
    assert_eq!(index_select(1, &pack(&random, 1)), ids::STATIC_CORE);
    assert_eq!(index_select(1, &pack(&smooth[..30], 1)), ids::STATIC_CORE);
    // Large frequently-changing num8 indices with a small-delta majority.
    let mut rng = Rng::new(5);
    let skewed: Vec<u64> = (0..20000u64)
        .scan(100u64, |state, _| {
            let step = if rng.below(10) < 9 {
                rng.below(5)
            } else {
                rng.below(120)
            };
            *state = (*state + step) % 250;
            Some(*state)
        })
        .collect();
    let _ = index_select(1, &pack(&skewed, 1));
    // Smooth num16 indices.
    let smooth16: Vec<u64> = (0..12000u64)
        .map(|i| 1000 + (i / 3) % 1500 + i % 2)
        .collect();
    assert_eq!(index_select(2, &pack(&smooth16, 2)), ids::STATIC_DELTA);
    let noisy16: Vec<u64> = (0..12000u64).map(|i| i * 7919 % 60001).collect();
    assert_eq!(index_select(2, &pack(&noisy16, 2)), ids::STATIC_CORE);
}

#[test]
fn static_fallback_reaches_every_outcome() {
    let found = reached(|_, _, w, b| fallback_select(w, b), lengths());
    let expected = ids_of(&[
        ids::STATIC_CORE,
        ids::STATIC_DELTA,
        ids::STATIC_TOK_SORTED,
        ids::STATIC_DELTA_TOK,
        ids::STATIC_DELTA_TOK_MONO,
        ids::STATIC_DELTA_TOK_MONO_LZ,
    ]);
    let missing: Vec<_> = expected.difference(&found).collect();
    assert!(
        missing.is_empty(),
        "static fallback never returned {missing:?}; found {found:?}"
    );
}

#[test]
fn static_fallback_trivial_inputs_use_the_core() {
    assert_eq!(fallback_select(4, &[]), ids::STATIC_CORE);
    assert_eq!(fallback_select(4, &pack(&[9], 4)), ids::STATIC_CORE);
    assert_eq!(fallback_select(4, &pack(&[9, 9, 9], 4)), ids::STATIC_CORE);
}

#[test]
fn supported_operation_choice_is_consistent_with_decisions() {
    // `select_supported_operation` never returns an unsupported operation.
    let none = SupportedOperations {
        constant: false,
        divide_by_gcd: false,
        sparse_num: false,
    };
    for (_, bytes) in shapes(2, &[300]) {
        let decision = decide16(&bytes);
        let op = select_supported_operation(&decision, none);
        assert!(!matches!(
            op,
            scorers::Op::Constant | scorers::Op::DivideByGcd | scorers::Op::SparseNum
        ));
    }
}

/// Reaches the rare internal branches of the static selectors that the
/// generic shapes do not.
mod rare_branches {
    use super::*;
    use crate::compressor::core::graphs::transformer::static_select::{fse_gate_check, gcd_vec};

    #[test]
    fn the_fse_gate_rejects_degenerate_and_dead_zone_streams() {
        // Fewer than two symbols, or one distinct symbol.
        assert!(!fse_gate_check(&[1]));
        assert!(!fse_gate_check(&[4; 100]));
        // Two symbols with a negligible zeroth-order entropy.
        let mut skewed = alloc::vec![0u8; 200_000];
        skewed[100_000] = 1;
        assert!(!fse_gate_check(&skewed));
        // Too many distinct symbols, or a sparse domain.
        let many: Vec<u8> = (0..=255u8).cycle().take(4000).collect();
        assert!(!fse_gate_check(&many));
        let sparse: Vec<u8> = (0..12u8).map(|i| i * 20).cycle().take(4000).collect();
        assert!(!fse_gate_check(&sparse));
        // Random symbols of a small alphabet pass.
        let mut rng = Rng::new(3);
        let random: Vec<u8> = (0..4000).map(|_| rng.below(5) as u8).collect();
        assert!(fse_gate_check(&random));
        // Runs of about 12 (off-diagonal share between 1/17 and 1/10) are a
        // dead zone where LZ wins.
        let mut runs = Vec::new();
        while runs.len() < 6000 {
            let symbol = rng.below(4) as u8;
            let run = 6 + rng.below(12) as usize;
            runs.extend(core::iter::repeat_n(symbol, run));
        }
        assert!(!fse_gate_check(&runs));
        // A dominant symbol with frequent sparse events of a wide range.
        let dominant: Vec<u8> = (0..8000)
            .map(|_| {
                if rng.below(100) < 85 {
                    0
                } else {
                    150 + rng.below(3) as u8
                }
            })
            .collect();
        assert!(!fse_gate_check(&dominant));
    }

    #[test]
    fn gcd_of_few_or_zero_elements_is_one() {
        assert_eq!(gcd_vec(&[], 4), 1);
        assert_eq!(gcd_vec(&pack(&[12], 4), 4), 1);
        assert_eq!(gcd_vec(&pack(&[0, 0], 4), 4), 1);
        assert_eq!(gcd_vec(&pack(&[12, 18, 0], 4), 4), 6);
    }

    #[test]
    fn a_rare_reversal_still_selects_delta_on_wide_data() {
        // An ascending ramp with one reversal: the minority direction is
        // below 0.1% of the changes and the values pass half the width.
        let mut values: Vec<u64> = (0..3000u64).map(|i| 1000 + i).collect();
        values[1500] = 5;
        assert_eq!(core_select(27, 1, 2, &pack(&values, 2)), ids::STATIC_DELTA);
    }

    #[test]
    fn a_small_delta_majority_selects_delta_for_large_num8_indices() {
        let mut rng = Rng::new(11);
        let mut state = 50u64;
        let values: Vec<u64> = (0..20_000)
            .map(|_| {
                let step = if rng.below(10) < 9 {
                    rng.below(5)
                } else {
                    rng.below(100)
                };
                state = (state + step) % 100;
                state
            })
            .collect();
        assert_eq!(index_select(1, &pack(&values, 1)), ids::STATIC_DELTA);
    }

    /// Distinct sorted positions on an irregular grid.
    fn irregular_grid(count: usize, low: u64, high: u64, rng: &mut Rng) -> Vec<u64> {
        let mut grid: Vec<u64> = (0..count).map(|_| low + rng.below(high - low)).collect();
        grid.sort_unstable();
        grid.dedup();
        grid
    }

    #[test]
    fn a_large_num16_dictionary_is_not_tokenized() {
        // 2000 distinct 16-bit values over a large domain: low cardinality
        // by the 15% rule, but a dictionary of 1500 or more entries.
        let mut rng = Rng::new(17);
        let grid = irregular_grid(2000, 0, 60_000, &mut rng);
        let values: Vec<u64> = (0..30_000)
            .map(|_| grid[rng.below(grid.len() as u64) as usize])
            .collect();
        assert_eq!(fallback_select(2, &pack(&values, 2)), ids::STATIC_CORE);
    }

    #[test]
    fn sparse_num64_dictionaries_keep_their_byte_patterns() {
        let mut rng = Rng::new(23);
        // Far apart values: mean delta above 1e14.
        let far = irregular_grid(800, 0, u64::MAX / 2, &mut rng);
        let values: Vec<u64> = (0..10_000)
            .map(|_| far[rng.below(far.len() as u64) as usize])
            .collect();
        assert_eq!(fallback_select(8, &pack(&values, 8)), ids::STATIC_CORE);
        // Close values in runs that oscillate in both directions.
        let near = irregular_grid(800, 1, 1_000_000_000_000, &mut rng);
        let mut runs = Vec::new();
        while runs.len() < 10_000 {
            let value = near[rng.below(near.len() as u64) as usize];
            runs.extend(core::iter::repeat_n(value, 1 + rng.below(14) as usize));
        }
        assert_eq!(fallback_select(8, &pack(&runs, 8)), ids::STATIC_CORE);
    }

    /// Dense smooth `u16` data: a sawtooth over 400 values.
    fn dense_sawtooth() -> Vec<u8> {
        pack(&(0..4000u64).map(|i| i % 400).collect::<Vec<_>>(), 2)
    }

    /// Smooth `u16` data over a dense domain whose rare big jumps make
    /// the delta alphabet large: the 600 jumps are all different.
    fn dense_heavy_tail() -> Vec<u8> {
        let mut state = 1500i64;
        let mut jump = 0i64;
        let values: Vec<u64> = (0..12_000i64)
            .map(|i| {
                let step = if i % 20 == 0 {
                    jump += 1;
                    (jump * 37) % 601 - 300
                } else {
                    i % 11 - 5
                };
                state = (state + step).clamp(500, 2500);
                state as u64
            })
            .collect();
        pack(&values, 2)
    }

    /// Smooth `u16` data over 100 irregularly spaced values: low
    /// cardinality, yet many distinct deltas.
    fn grid_walk() -> Vec<u8> {
        let mut rng = Rng::new(31);
        let grid = irregular_grid(100, 1000, 1900, &mut rng);
        let mut index = 50i64;
        let values: Vec<u64> = (0..14_000)
            .map(|_| {
                index = (index + rng.below(13) as i64 - 6).clamp(0, grid.len() as i64 - 1);
                grid[index as usize]
            })
            .collect();
        pack(&values, 2)
    }

    #[test]
    fn smooth_num16_streams_are_rescued_with_delta() {
        assert_eq!(fallback_select(2, &dense_sawtooth()), ids::STATIC_DELTA);
        assert_eq!(fallback_select(2, &dense_heavy_tail()), ids::STATIC_DELTA);
        assert_eq!(fallback_select(2, &grid_walk()), ids::STATIC_CORE);
    }
}
