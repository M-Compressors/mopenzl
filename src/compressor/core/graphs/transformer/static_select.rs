//! The heuristic Transformer selectors: `transformer_static_core` (76),
//! `transformer_static_index` (82) and `transformer_static_fallback` (89)
//! (`static_core_select.c`, `static_index_select.c`,
//! `static_fallback_select.c`).
//!
//! They use no model: ordered rules over a few statistics of the stream pick
//! a transform (each of which re-enters the core selector) or a terminal
//! graph.

use alloc::vec;
use alloc::vec::Vec;

use super::super::super::data::{Stream, Type};
use super::super::super::exec::SelectorCx;
use super::super::super::registry::{GraphId, SelectFn};
use super::features::half_width_threshold;
use super::ids;
use super::wide;
use super::{MAX_STATIC_DEPTH, math};
use crate::shared::estimate::{compute_unsigned_range, estimate_cardinality_fixed};

/// `SI_transformer_static_core_select`.
#[derive(Debug)]
pub(crate) struct StaticCore;

/// `SI_transformer_static_fallback_select`.
#[derive(Debug)]
pub(crate) struct StaticFallback;

/// `SI_transformer_static_index_select`.
#[derive(Debug)]
pub(crate) struct StaticIndex;

/// See [`StaticCore`].
pub(crate) static STATIC_CORE: StaticCore = StaticCore;
/// See [`StaticFallback`].
pub(crate) static STATIC_FALLBACK: StaticFallback = StaticFallback;
/// See [`StaticIndex`].
pub(crate) static STATIC_INDEX: StaticIndex = StaticIndex;

/// The elements of `bytes`, little-endian unsigned numbers of `width` bytes
/// (`static_load_unsigned` and its siblings).
fn elements(bytes: &[u8], width: usize) -> impl Iterator<Item = u64> + Clone + '_ {
    bytes.chunks_exact(width).map(move |chunk| {
        let mut word = [0u8; 8];
        word[..width].copy_from_slice(chunk);
        u64::from_le_bytes(word)
    })
}

/// `static_interpret_signed`.
const fn interpret_signed(value: u64, width: usize) -> i64 {
    match width {
        1 => value as u8 as i8 as i64,
        2 => value as u16 as i16 as i64,
        4 => value as u32 as i32 as i64,
        _ => value as i64,
    }
}

/// `static_compute_delta`: preserves the typed subtraction of the
/// reference. Widths 1 and 2 are subtracted as promoted `int` and widened
/// with sign extension; widths 4 and 8 wrap at their width.
const fn compute_delta(current: u64, previous: u64, width: usize) -> u64 {
    match width {
        1 => (current as u8 as i32 - previous as u8 as i32) as i64 as u64,
        2 => (current as u16 as i32 - previous as u16 as i32) as i64 as u64,
        4 => (current as u32).wrapping_sub(previous as u32) as u64,
        _ => current.wrapping_sub(previous),
    }
}

/// `STATIC_DCARD_LIMIT`: unique deltas tracked.
const DCARD_LIMIT: usize = 4;

/// `StaticAnalysis`.
#[derive(Default)]
struct Analysis {
    up: usize,
    down: usize,
    /// Unique deltas, capped at `DCARD_LIMIT + 1` ("more than the limit").
    delta_card: usize,
    min: u64,
    max: u64,
    signed_min: i64,
    signed_max: i64,
}

/// `static_analyze_fixed`.
fn analyze(bytes: &[u8], width: usize) -> Analysis {
    let mut analysis = Analysis::default();
    let n = bytes.len() / width;
    if n < 2 {
        return analysis;
    }
    let mut unique = [0u64; DCARD_LIMIT];
    let mut n_unique = 0usize;
    let mut values = elements(bytes, width);
    let first = values.next().unwrap_or_default();
    analysis.min = first;
    analysis.max = first;
    analysis.signed_min = interpret_signed(first, width);
    analysis.signed_max = analysis.signed_min;
    let mut previous = first;
    for current in values {
        let signed_current = interpret_signed(current, width);
        analysis.min = analysis.min.min(current);
        analysis.max = analysis.max.max(current);
        analysis.signed_min = analysis.signed_min.min(signed_current);
        analysis.signed_max = analysis.signed_max.max(signed_current);
        if current > previous {
            analysis.up += 1;
        } else if current < previous {
            analysis.down += 1;
        }
        if n_unique <= DCARD_LIMIT {
            let delta = compute_delta(current, previous, width);
            if !unique[..n_unique.min(DCARD_LIMIT)].contains(&delta) {
                if n_unique < DCARD_LIMIT {
                    unique[n_unique] = delta;
                }
                n_unique += 1;
            }
        }
        previous = current;
    }
    analysis.delta_card = n_unique;
    analysis
}

/// `FSE_GATE_*`.
const FSE_GATE_MAX_CARDINALITY: usize = 16;
/// `FSE_GATE_DOMINANT_SHARE_NUMERATOR`.
const FSE_GATE_DOMINANT_SHARE_NUMERATOR: u64 = 4;
/// `FSE_GATE_DOMINANT_SHARE_DENOMINATOR`.
const FSE_GATE_DOMINANT_SHARE_DENOMINATOR: u64 = 5;
/// `FSE_GATE_MIN_PAIR_TO_SYMBOL_ENTROPY_RATIO`.
const FSE_GATE_MIN_PAIR_TO_SYMBOL_ENTROPY_RATIO: f64 = 1.95;
/// `SMOOTH_DATA_SMALL_DELTA_SHARE_NUMERATOR`.
const SMOOTH_DATA_SMALL_DELTA_SHARE_NUMERATOR: u64 = 97;
/// `SMOOTH_DATA_SMALL_DELTA_SHARE_DENOMINATOR`.
const SMOOTH_DATA_SMALL_DELTA_SHARE_DENOMINATOR: u64 = 100;

/// `fse_gate_check`: high conditional-to-symbol entropy means little
/// sequential structure, which favors FSE over an LZ graph for
/// low-cardinality num8 data.
pub(super) fn fse_gate_check(data: &[u8]) -> bool {
    let n = data.len();
    if n < 2 {
        return false;
    }
    let mut hist = [0usize; 256];
    for &byte in data {
        hist[usize::from(byte)] += 1;
    }
    let mut card = 0usize;
    let (mut val_min, mut val_max) = (255usize, 0usize);
    let mut max_freq = 0usize;
    let mut sym_to_idx = [0u8; 256];
    for symbol in 0..256usize {
        if hist[symbol] > 0 {
            sym_to_idx[symbol] = card as u8;
            card += 1;
            val_min = val_min.min(symbol);
            val_max = val_max.max(symbol);
            max_freq = max_freq.max(hist[symbol]);
        }
    }
    if card < 2 {
        return false;
    }
    // Above cardinality 8 require a dense value domain.
    if card > FSE_GATE_MAX_CARDINALITY {
        return false;
    }
    if card > 8 && card * 2 <= val_max - val_min + 1 {
        return false;
    }

    let mut h0 = 0.0f64;
    for &count in &hist {
        if count > 0 {
            let p = count as f64 / n as f64;
            h0 -= p * math::log2(p);
        }
    }
    if h0 < 0.001 {
        return false;
    }

    let mut trans = [0usize; FSE_GATE_MAX_CARDINALITY * FSE_GATE_MAX_CARDINALITY];
    for pair in data.windows(2) {
        trans[usize::from(sym_to_idx[usize::from(pair[0])]) * card
            + usize::from(sym_to_idx[usize::from(pair[1])])] += 1;
    }

    let n_pairs = (n - 1) as f64;
    let mut h_pair = 0.0f64;
    for &count in &trans[..card * card] {
        if count > 0 {
            let p = count as f64 / n_pairs;
            h_pair -= p * math::log2(p);
        }
    }

    // H1 = H(pair) - H0; H1/H0 > 0.95 <=> H(pair) > 1.95 * H0.
    if h_pair <= FSE_GATE_MIN_PAIR_TO_SYMBOL_ENTROPY_RATIO * h0 {
        return false;
    }

    // Moderate runs (mean run length roughly 10-17) are a dead zone where LZ
    // beats FSE; a dominant symbol with frequent sparse events forms motifs
    // LZ captures better.
    if card <= 8 && n >= 1000 {
        let mut off_diag = 0usize;
        for i in 0..card {
            for j in 0..card {
                if i != j {
                    off_diag += trans[i * card + j];
                }
            }
        }
        let np = n - 1;
        if card <= 6 && (off_diag as u64) * 10 <= np as u64 && (off_diag as u64) * 17 > np as u64 {
            return false;
        }
        if n >= 5000
            && (off_diag as u64) * 5 > np as u64
            && (val_max - val_min) > 100
            && (max_freq as u64) * FSE_GATE_DOMINANT_SHARE_DENOMINATOR
                >= (n as u64) * FSE_GATE_DOMINANT_SHARE_NUMERATOR
        {
            return false;
        }
    }
    true
}

/// `smooth_data_check`: temporal smoothness that makes `delta_int` effective
/// after `range_pack`.
fn smooth_data_check(bytes: &[u8], width: usize, range_u: u64) -> bool {
    let n = bytes.len() / width;
    if n < 1000 || range_u < 3 || !matches!(width, 2 | 4 | 8) {
        return false;
    }
    let threshold = (range_u / 4).max(3);
    let mut small_count = 0u64;
    let values = elements(bytes, width);
    for (previous, current) in values.clone().zip(values.skip(1)) {
        if current.abs_diff(previous) <= threshold {
            small_count += 1;
        }
    }
    small_count * SMOOTH_DATA_SMALL_DELTA_SHARE_DENOMINATOR
        >= (n as u64 - 1) * SMOOTH_DATA_SMALL_DELTA_SHARE_NUMERATOR
}

/// `static_terminal_graph`.
const fn terminal_graph(width: usize) -> GraphId {
    if width == 1 { ids::ZSTD } else { ids::FIELD_LZ }
}

/// `ZL_gcdImpl`.
const fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// `ZL_gcdVec`: 1 for fewer than two elements or only zeros.
pub(super) fn gcd_vec(bytes: &[u8], width: usize) -> u64 {
    if bytes.len() / width <= 1 {
        return 1;
    }
    let divisor = elements(bytes, width).fold(0, gcd);
    if divisor == 0 { 1 } else { divisor }
}

impl SelectFn for StaticCore {
    fn select(
        &self,
        cx: &mut SelectorCx<'_, '_, '_>,
        input: &Stream<'_>,
        _: &[GraphId],
    ) -> GraphId {
        core_select(
            cx.format_version(),
            cx.depth(),
            input.width(),
            input.bytes(),
        )
    }
}

/// `SI_transformer_static_core_select` on `bytes`, little-endian numbers of
/// `width` bytes, at graph depth `depth` under format version
/// `format_version`.
///
/// Ordered policy: trivial and depth exits, `delta_int`, `zigzag`,
/// `divide_by_gcd`, `range_pack`, the num8 FSE gate, then the width-specific
/// terminal graph. The order matters because several choices re-enter this
/// selector.
pub(super) fn core_select(format_version: u32, depth: u32, width: usize, bytes: &[u8]) -> GraphId {
    let analysis = analyze(bytes, width);
    let (up, down, dcard) = (analysis.up, analysis.down, analysis.delta_card);
    let (vmin, vmax) = (analysis.min, analysis.max);
    let (smin, smax) = (analysis.signed_min, analysis.signed_max);
    let n = bytes.len() / width;
    let half_width = half_width_threshold(width);
    if n == 0 {
        return ids::STORE;
    }
    if up == 0 && down == 0 {
        return if format_version >= 11 {
            ids::CONSTANT
        } else {
            ids::STORE
        };
    }
    if depth > MAX_STATIC_DEPTH {
        return terminal_graph(width);
    }

    // delta_int
    {
        let changes = up + down;
        if up == 0 || down == 0 {
            let range_u = vmax - vmin;
            if dcard <= 3 && (n as u64) * 2 < (changes as u64 + 1) * 3 {
                return ids::STATIC_DELTA;
            }
            if dcard <= 2 && range_u <= n as u64 && width >= 2 && changes >= 16 {
                return ids::STATIC_DELTA;
            }
        } else {
            let minority = up.min(down);
            let majority = up.max(down);
            // Of the n - 1 transitions, everything outside `majority` is
            // a repeated value or a minority-direction transition: allow
            // roughly one such exception per 256 elements.
            let non_dominant_transitions = (n - 1) - majority;
            let non_dominant_limit = (n + 200) / 256;
            if (minority as u64) * 1000 < changes as u64
                && non_dominant_transitions < non_dominant_limit
                && vmax >= half_width
            {
                return ids::STATIC_DELTA;
            }
            // Rare num8 wrap resets become sparse nonzero deltas.
            if width == 1
                && (minority as u64) * 20 < changes as u64
                && (changes as u64) * 4 < n as u64
                && changes >= 100
            {
                return ids::STATIC_DELTA;
            }
        }
    }

    // zigzag
    if width >= 2 && smin < 0 && smax >= 0 {
        let negative_extent = 0u64.wrapping_sub(smin as u64);
        let range_s = wide::i64_abs_diff_to_u64(smax, smin);
        // Require the negative extent to be at least 25% of the signed
        // range: the data meaningfully crosses zero.
        if range_s < half_width && negative_extent.wrapping_mul(4) >= range_s {
            return ids::STATIC_ZIGZAG;
        }
    }

    // divide_by_gcd: large common factors can expose range packing;
    // power-of-two factors often reflect float mantissa alignment, so
    // they need decisive narrowing.
    if format_version >= 16 && width >= 4 {
        let range_u = vmax - vmin;
        if range_u >= 512 {
            let g = gcd_vec(bytes, width);
            if g >= 256 {
                let gcd_ok = (g & (g - 1)) != 0 || (n >= 2000 && range_u / g < 65536);
                if gcd_ok {
                    return ids::STATIC_GCD;
                }
            }
        }
    }

    // range_pack
    {
        let range_u = vmax - vmin;
        if width >= 2 && range_u < 100 {
            // Smooth active or nearly monotonic streams benefit from a
            // second delta stage; sparse bidirectional jumps do not.
            let changes = up + down;
            let mostly_active = (changes as u64) * 2 > n as u64;
            let minority = up.min(down);
            let mostly_mono = (minority as u64) * 10 < up.max(down) as u64;
            if range_u >= 8
                && (mostly_active || (mostly_mono && changes >= 4))
                && smooth_data_check(bytes, width, range_u)
            {
                return ids::STATIC_RANGE_PACK_DELTA;
            }
            return ids::STATIC_RANGE_PACK;
        }
        if width >= 4 && range_u < half_width && vmax >= half_width {
            return ids::STATIC_RANGE_PACK;
        }
    }

    // num8 gates
    if width == 1 && fse_gate_check(bytes) {
        return ids::FSE;
    }
    terminal_graph(width)
}

/// `INDEX_GATE_SMALL_DELTA_SHARE_NUMERATOR`.
const INDEX_GATE_SMALL_DELTA_SHARE_NUMERATOR: u64 = 4;
/// `INDEX_GATE_SMALL_DELTA_SHARE_DENOMINATOR`.
const INDEX_GATE_SMALL_DELTA_SHARE_DENOMINATOR: u64 = 5;
/// `INDEX_GATE_STRONG_LOCALITY_RANGE_TO_MAD`.
const INDEX_GATE_STRONG_LOCALITY_RANGE_TO_MAD: f64 = 25.0;
/// `INDEX_GATE_NARROW_RANGE_TO_MAD`.
const INDEX_GATE_NARROW_RANGE_TO_MAD: f64 = 40.0;

impl SelectFn for StaticIndex {
    fn select(&self, _: &mut SelectorCx<'_, '_, '_>, input: &Stream<'_>, _: &[GraphId]) -> GraphId {
        if input.ty() != Type::Numeric {
            return ids::STATIC_CORE;
        }
        index_select(input.width(), input.bytes())
    }
}

/// `SI_transformer_static_index_select` on `bytes`, little-endian numbers of
/// `width` bytes.
///
/// Smooth tokenize index streams benefit from `delta_int` before entropy or
/// LZ. For num8, strong locality or extremely small absolute deltas
/// distinguish them from run-dominated and random-looking streams.
pub(super) fn index_select(width: usize, bytes: &[u8]) -> GraphId {
    let n = bytes.len() / width;

    if width == 1 && n >= 64 {
        // Single pass: mean absolute delta, range and transition count.
        let mut vmin = bytes[0];
        let mut vmax = bytes[0];
        let mut mad_sum = 0u64;
        let mut transitions = 0usize;
        for pair in bytes.windows(2) {
            let (b, a) = (pair[0], pair[1]);
            mad_sum += u64::from(a.abs_diff(b));
            transitions += usize::from(a != b);
            vmin = vmin.min(a);
            vmax = vmax.max(a);
        }
        let range_u = u64::from(vmax) - u64::from(vmin);
        let mad = mad_sum as f64 / (n - 1) as f64;

        if range_u >= 64
            && mad * INDEX_GATE_STRONG_LOCALITY_RANGE_TO_MAD < range_u as f64
            && ((transitions as u64) * 2 > n as u64 || mad < 0.5)
        {
            return ids::STATIC_DELTA;
        }

        // A concentrated small-delta majority catches right-skewed
        // streams missed by the mean; restricted to large, frequently
        // changing inputs so the entropy gain amortizes.
        if range_u >= 64 && n >= 16384 && (transitions as u64) * 4 > n as u64 {
            let small_threshold = range_u / 16;
            let small_count = bytes
                .windows(2)
                .filter(|pair| u64::from(pair[1].abs_diff(pair[0])) <= small_threshold)
                .count() as u64;
            if small_count * INDEX_GATE_SMALL_DELTA_SHARE_DENOMINATOR
                >= (n as u64 - 1) * INDEX_GATE_SMALL_DELTA_SHARE_NUMERATOR
            {
                return ids::STATIC_DELTA;
            }
        }
    }

    // Num16 indices need strong temporal locality and frequent changes;
    // narrower ranges require a tighter normalized MAD because the raw
    // indices are already compact.
    if width == 2 && n >= 10000 {
        let values: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&chunk| u16::from_le_bytes(chunk))
            .collect();
        let mut vmin = values[0];
        let mut vmax = values[0];
        let mut mad_sum = 0u64;
        let mut transitions = 0usize;
        for pair in values.windows(2) {
            let (b, a) = (pair[0], pair[1]);
            mad_sum += u64::from(a.abs_diff(b));
            transitions += usize::from(a != b);
            vmin = vmin.min(a);
            vmax = vmax.max(a);
        }
        let range16 = u64::from(vmax) - u64::from(vmin);
        let mad16 = mad_sum as f64 / (n - 1) as f64;

        if range16 >= 200
            && mad16 * INDEX_GATE_STRONG_LOCALITY_RANGE_TO_MAD < range16 as f64
            && (transitions as u64) * 2 > n as u64
            && (range16 >= 500 || mad16 * INDEX_GATE_NARROW_RANGE_TO_MAD < range16 as f64)
        {
            return ids::STATIC_DELTA;
        }
    }

    ids::STATIC_CORE
}

/// `TOKENIZE_GATE_DELTA_HASH_BITS`.
const DELTA_HASH_BITS: u32 = 12;
/// `TOKENIZE_GATE_DELTA_HASH_SIZE`.
const DELTA_HASH_SIZE: usize = 1 << DELTA_HASH_BITS;
/// `TOKENIZE_GATE_DELTA_HASH_MASK`.
const DELTA_HASH_MASK: u32 = (DELTA_HASH_SIZE as u32) - 1;
/// `TOKENIZE_GATE_DELTA_HASH_CARDINALITY_LIMIT`.
const DELTA_HASH_CARDINALITY_LIMIT: u64 = (DELTA_HASH_SIZE / 4) as u64;
/// `TOKENIZE_GATE_LOW_CARDINALITY_NUMERATOR`.
const LOW_CARDINALITY_NUMERATOR: u64 = 3;
/// `TOKENIZE_GATE_LOW_CARDINALITY_DENOMINATOR`.
const LOW_CARDINALITY_DENOMINATOR: u64 = 20;
/// `TOKENIZE_GATE_MAX_DELTA_CARDINALITY_RATIO_NUMERATOR`.
const MAX_DELTA_CARDINALITY_RATIO_NUMERATOR: u64 = 3;
/// `TOKENIZE_GATE_MAX_DELTA_CARDINALITY_RATIO_DENOMINATOR`.
const MAX_DELTA_CARDINALITY_RATIO_DENOMINATOR: u64 = 5;
/// `TOKENIZE_GATE_MAX_DELTA_OVERRIDE_RATIO_NUMERATOR`.
const MAX_DELTA_OVERRIDE_RATIO_NUMERATOR: u64 = 9;
/// `TOKENIZE_GATE_MAX_DELTA_OVERRIDE_RATIO_DENOMINATOR`.
const MAX_DELTA_OVERRIDE_RATIO_DENOMINATOR: u64 = 10;
/// `TOKENIZE_GATE_MAX_FLOAT64_CARDINALITY_RATIO_NUMERATOR`.
const MAX_FLOAT64_CARDINALITY_RATIO_NUMERATOR: u64 = 193;
/// `TOKENIZE_GATE_MAX_FLOAT64_CARDINALITY_RATIO_DENOMINATOR`.
const MAX_FLOAT64_CARDINALITY_RATIO_DENOMINATOR: u64 = 200;
/// `TOKENIZE_GATE_FLOAT64_SAMPLE_SIZE`.
const FLOAT64_SAMPLE_SIZE: usize = 200;

/// `TokenizeGateU16CardinalityInput`.
#[derive(Copy, Clone, Eq, PartialEq)]
enum U16Input {
    /// The values themselves.
    RawValues,
    /// The wrapping deltas between neighbors.
    WrappingDeltas,
}

/// `tokenize_gate_count_distinct_u16`: the exact number of distinct values
/// (the static gates need exact counts, unlike the model's estimates).
fn count_distinct_u16(data: &[u8], input: U16Input) -> u64 {
    let values = elements(data, 2).map(|value| value as u16);
    let mut bitmap = vec![0u64; 1024];
    let mut set = |value: u16| bitmap[usize::from(value) / 64] |= 1u64 << (value % 64);
    match input {
        U16Input::WrappingDeltas => {
            for (previous, current) in values.clone().zip(values.skip(1)) {
                set(current.wrapping_sub(previous));
            }
        }
        U16Input::RawValues => values.for_each(&mut set),
    }
    bitmap.iter().map(|word| u64::from(word.count_ones())).sum()
}

/// `TokenizeGateContext`.
struct Gate<'a> {
    data: &'a [u8],
    n: usize,
    width: usize,
    range_min: u64,
    range_u: u64,
    cardinality_scale: u64,
    card_estimate: u64,
    mean_abs_delta: Option<f64>,
    /// The 65536-bit scratch bitmap of the exact num16 refinements exists.
    has_u16_bitmap: bool,
}

impl Gate<'_> {
    /// `tokenize_gate_estimate_is_low_cardinality`: a calibrated boundary on
    /// the threshold-oriented estimate and the span-based scale.
    const fn is_low_cardinality(&self) -> bool {
        self.card_estimate * LOW_CARDINALITY_DENOMINATOR
            < self.cardinality_scale * LOW_CARDINALITY_NUMERATOR
    }

    /// `tokenize_gate_get_mean_abs_delta` (cached): the mean |delta|,
    /// accumulated in double.
    fn mean_abs_delta(&mut self) -> f64 {
        if let Some(value) = self.mean_abs_delta {
            return value;
        }
        let value = if self.n < 2 {
            0.0
        } else {
            let mut sum = 0.0f64;
            let values = elements(self.data, self.width);
            for (previous, current) in values.clone().zip(values.skip(1)) {
                sum += current.abs_diff(previous) as f64;
            }
            sum / (self.n - 1) as f64
        };
        self.mean_abs_delta = Some(value);
        value
    }

    /// `tokenize_gate_count_zeros`.
    fn count_zeros(&self) -> usize {
        elements(self.data, self.width)
            .filter(|&value| value == 0)
            .count()
    }

    /// `tokenize_gate_try_delta_override`: tries `delta_int` before tokenize
    /// when wide, oscillating data has high raw cardinality but a compact
    /// delta alphabet.
    fn try_delta_override(&self) -> Option<GraphId> {
        if self.is_low_cardinality() || self.width < 4 || self.n < 1000 {
            return None;
        }
        let mut up = 0usize;
        let mut down = 0usize;
        let mut keys = vec![0u64; DELTA_HASH_SIZE];
        let mut used = vec![false; DELTA_HASH_SIZE];
        let mut delta_card = 0u64;

        let values = elements(self.data, self.width);
        for (previous, current) in values.clone().zip(values.skip(1)) {
            if delta_card >= DELTA_HASH_CARDINALITY_LIMIT {
                break;
            }
            let delta = if self.width == 4 {
                u64::from((current as u32).wrapping_sub(previous as u32))
            } else {
                current.wrapping_sub(previous)
            };
            up += usize::from(current > previous);
            down += usize::from(current < previous);

            let hash = (delta.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - DELTA_HASH_BITS)) as u32;
            // Bound collision work: probe exhaustion omits the delta from
            // this calibrated estimate.
            for probe in 0..32u32 {
                let slot = (hash.wrapping_add(probe) & DELTA_HASH_MASK) as usize;
                if !used[slot] {
                    used[slot] = true;
                    keys[slot] = delta;
                    delta_card += 1;
                    break;
                }
                if keys[slot] == delta {
                    break;
                }
            }
        }

        // Reaching the cap may leave the direction counts incomplete, and
        // both override rules reject saturated delta cardinality.
        if delta_card >= DELTA_HASH_CARDINALITY_LIMIT {
            return None;
        }

        let minority = up.min(down);
        let changes = up + down;
        if self.range_u > 65536
            && delta_card * 5 > self.n as u64
            && (minority as u64) * 4 > self.n as u64
        {
            // Re-estimate raw cardinality to verify that delta_int reduces
            // it (relies on the estimator scanning the whole input).
            let card_high = estimate_cardinality_fixed(self.data, self.width, delta_card + 1)
                .map_or(0, |estimate| estimate.estimate);
            if delta_card * MAX_DELTA_OVERRIDE_RATIO_DENOMINATOR
                < card_high * MAX_DELTA_OVERRIDE_RATIO_NUMERATOR
            {
                return Some(ids::STATIC_DELTA_TOK);
            }
        }

        // For near-monotonic IDs or timestamps, delta_int creates a compact
        // step dictionary and tokenize exploits repeated step indices.
        let tokenize_delta_cardinality_limit = if self.width == 4 && self.n < 100_000 {
            256
        } else {
            DELTA_HASH_CARDINALITY_LIMIT
        };
        if delta_card > 4
            && delta_card < tokenize_delta_cardinality_limit
            && (minority as u64) * 50 < changes as u64
            && (changes as u64) * 10 >= (self.n as u64) * 9
            && delta_card * 2 < self.card_estimate
        {
            // On num32, small inputs and moderately broad step dictionaries
            // can make entropy-table overhead exceed its coding gain.
            let lz = self.width == 4
                && ((self.n < 5000 && delta_card >= 20) || (150..250).contains(&delta_card));
            return Some(if lz {
                ids::STATIC_DELTA_TOK_MONO_LZ
            } else {
                ids::STATIC_DELTA_TOK_MONO
            });
        }
        None
    }

    /// `tokenize_gate_try_float64_override`: the sorted dictionary of float64
    /// data has tiny inter-value deltas and its indices narrow to 2 bytes
    /// below 50000 distinct values. Likely IEEE754 data is detected by
    /// tightly clustered normal exponents in a 200-element sample.
    fn try_float64_override(&self) -> Option<GraphId> {
        if self.width != 8 || self.n < 10000 || self.is_low_cardinality() {
            return None;
        }
        let mut min_exp = 2047u16;
        let mut max_exp = 0u16;
        let mut valid = 0usize;
        for value in elements(self.data, 8).take(FLOAT64_SAMPLE_SIZE) {
            let exp = ((value >> 52) & 0x7FF) as u16;
            if exp == 0 || exp == 2047 {
                continue;
            }
            min_exp = min_exp.min(exp);
            max_exp = max_exp.max(exp);
            valid += 1;
        }
        if valid * 4 < FLOAT64_SAMPLE_SIZE * 3 {
            return None;
        }
        if min_exp < 100 || max_exp - min_exp >= 15 {
            return None;
        }

        // Require enough repetition for narrowed indices to compress:
        // calibration put beneficial samples at or below 0.960 and harmful
        // ones at or above 0.970; 0.965 preserves that margin.
        let card_hi = estimate_cardinality_fixed(self.data, self.width, 50001)
            .map_or(0, |estimate| estimate.estimate);
        if card_hi < 50000
            && card_hi * MAX_FLOAT64_CARDINALITY_RATIO_DENOMINATOR
                < (self.n as u64) * MAX_FLOAT64_CARDINALITY_RATIO_NUMERATOR
        {
            return Some(ids::STATIC_TOK_SORTED);
        }
        None
    }

    /// `tokenize_gate_try_low_cardinality`: `Some` when a tokenize pipeline
    /// is selected, `None` when tokenize is rejected and the caller should
    /// try the rescue.
    fn try_low_cardinality(&mut self) -> Option<GraphId> {
        // `card_estimate` is below the early-exit limit in this phase, so
        // the absolute cardinality thresholds below can rely on it.
        if self.width == 2 && self.card_estimate >= 1500 {
            return None;
        }

        // Sparse data (>50% zeros): static's range_pack already handles it.
        if self.range_min == 0 {
            let zero_count = self.count_zeros();
            // Sparse num16/num32 can still benefit at moderate cardinality
            // because sorted dictionaries compress well and indices narrow
            // to one byte.
            if (zero_count as u64) * 2 > self.n as u64
                && !((self.width == 4 || self.width == 2)
                    && self.card_estimate > 8
                    && self.card_estimate <= 256)
            {
                return None;
            }
        }

        // Allow a looser locality threshold when num16+ indices narrow to
        // num8.
        let mean_ad = self.mean_abs_delta();
        let delta_threshold = if self.width >= 2
            && self.card_estimate >= 50
            && self.card_estimate < 256
            && self.n >= 10000
        {
            2.0
        } else {
            0.7
        };
        if self.card_estimate as f64 / (mean_ad + 1.0) >= delta_threshold {
            return None;
        }

        // Sparse num64 data can retain byte patterns that field_lz exploits
        // but tokenization destroys.
        if self.width == 8
            && self.card_estimate > 256
            && (self.n as u64) < self.card_estimate.wrapping_mul(25)
        {
            if mean_ad > 1e14 {
                return None;
            }
            // Sorted tokenize also disrupts run-heavy data that oscillates
            // in both directions. Monotonic order remains compressible.
            let check_n = self.n.min(1000);
            let sample: Vec<u64> = elements(self.data, 8).take(check_n).collect();
            let ups = sample.windows(2).filter(|pair| pair[1] > pair[0]).count();
            let downs = sample.windows(2).filter(|pair| pair[1] < pair[0]).count();
            let changes = ups + downs;
            let min_dir = ups.min(downs);
            if min_dir * 4 > changes && changes * 7 < check_n {
                return None;
            }
        }

        // For num16, apply delta_int first when it produces a meaningfully
        // smaller dictionary and the resulting delta stream still passes the
        // tokenize density gate.
        if self.width == 2 && self.n >= 1000 && self.has_u16_bitmap {
            let delta_card = count_distinct_u16(self.data, U16Input::WrappingDeltas);
            if delta_card * MAX_DELTA_CARDINALITY_RATIO_DENOMINATOR
                < self.card_estimate * MAX_DELTA_CARDINALITY_RATIO_NUMERATOR
                && delta_card * LOW_CARDINALITY_DENOMINATOR
                    < (self.n as u64) * LOW_CARDINALITY_NUMERATOR
            {
                return Some(ids::STATIC_DELTA_TOK);
            }
        }

        // Without index-width narrowing, highly oscillating num16 data keeps
        // more useful structure in its raw representation.
        if self.width == 2 && self.card_estimate >= 256 && mean_ad * 4.0 > self.range_u as f64 {
            return None;
        }
        Some(ids::STATIC_TOK_SORTED)
    }

    /// `tokenize_gate_select_num16_rescue`: rescues slowly varying num16
    /// streams before the static core. Exact bitmap counts avoid misses from
    /// the low-resolution estimate.
    fn num16_rescue(&mut self) -> GraphId {
        if self.width == 2 && self.n >= 3000 && self.range_u >= 100 && self.has_u16_bitmap {
            let delta_card = count_distinct_u16(self.data, U16Input::WrappingDeltas);
            let rescue_limit = if self.n >= 10000 { 512 } else { 256 };
            if delta_card < rescue_limit {
                // The threshold-oriented estimate can substantially
                // underestimate cardinality in this branch.
                let exact_card = count_distinct_u16(self.data, U16Input::RawValues);

                // Tiered dc/card threshold: for n >= 10000 the delta_int
                // overhead is well amortized so dc/card < 0.6 is safe; for
                // smaller inputs use the tighter dc/card < 0.5.
                let dc_ok = if self.n >= 10000 {
                    delta_card * MAX_DELTA_CARDINALITY_RATIO_DENOMINATOR
                        < exact_card * MAX_DELTA_CARDINALITY_RATIO_NUMERATOR
                } else {
                    delta_card * 2 < exact_card
                };
                if dc_ok && exact_card > 30 {
                    return ids::STATIC_DELTA;
                }
            } else if self.n >= 10000 && self.range_u >= 500 && self.range_u < 3000 {
                // High delta cardinality does not imply large deltas: smooth
                // streams can still benefit from delta_int.
                let rescue_mad = self.mean_abs_delta();
                if rescue_mad < 35.0 {
                    // Density check: card/range > 0.5, to avoid sparse data
                    // where delta_int disrupts byte-level patterns.
                    let exact_card = count_distinct_u16(self.data, U16Input::RawValues);
                    if exact_card * 2 > self.range_u {
                        return ids::STATIC_DELTA;
                    }
                }
            }
        }
        ids::STATIC_CORE
    }
}

impl SelectFn for StaticFallback {
    fn select(&self, _: &mut SelectorCx<'_, '_, '_>, input: &Stream<'_>, _: &[GraphId]) -> GraphId {
        if input.ty() != Type::Numeric {
            return ids::STATIC_CORE;
        }
        fallback_select(input.width(), input.bytes())
    }
}

/// `SI_transformer_static_fallback_select` on `data`, little-endian numbers
/// of `width` bytes.
///
/// Tries the specialized high-cardinality overrides, then the low-cardinality
/// tokenize policy, and finally the num16 delta rescue.
pub(super) fn fallback_select(width: usize, data: &[u8]) -> GraphId {
    let n = data.len() / width;
    if n < 2 {
        return ids::STATIC_CORE;
    }

    let range = compute_unsigned_range(data, width);
    let range_u = range.max.wrapping_sub(range.min);
    // The calibrated density scale uses the value span, not the
    // inclusive domain size.
    let cardinality_scale = (n as u64).min(range_u);
    if cardinality_scale == 0 {
        return ids::STATIC_CORE;
    }

    // Threshold-oriented cardinality estimate: values at or above this
    // limit are evidence that the 15% gate failed.
    let cardinality_limit = ((cardinality_scale as f64) * 0.15) as u64 + 1;
    let card_estimate = estimate_cardinality_fixed(data, width, cardinality_limit)
        .map_or(0, |estimate| estimate.estimate);
    let mut gate = Gate {
        data,
        n,
        width,
        range_min: range.min,
        range_u,
        cardinality_scale,
        card_estimate,
        mean_abs_delta: None,
        has_u16_bitmap: false,
    };

    // Dense small-range num16 data can tokenize despite failing the 15%
    // gate: the sorted dictionary is tiny and indices narrow.
    if width == 2 && (100..300).contains(&range_u) && card_estimate >= 100 && n >= 2000 {
        return ids::STATIC_TOK_SORTED;
    }
    if let Some(result) = gate.try_delta_override() {
        return result;
    }
    if let Some(result) = gate.try_float64_override() {
        return result;
    }

    // Exact num16 refinements are optional when selector scratch is
    // unavailable; here it always is.
    if width == 2 && n >= 1000 {
        gate.has_u16_bitmap = true;
    }

    if !gate.is_low_cardinality() {
        // Slowly varying num8 signals benefit from repeated delta
        // stages. Require a large range to exclude tokenize index
        // streams that would recurse, and a low MAD to reject random or
        // coarsely quantized data.
        if width == 1 && n >= 2000 && range_u >= 200 {
            let mean_ad = gate.mean_abs_delta();
            if mean_ad < 25.0 {
                return ids::STATIC_DELTA;
            }
        }
    } else if let Some(result) = gate.try_low_cardinality() {
        return result;
    }

    gate.num16_rescue()
}
