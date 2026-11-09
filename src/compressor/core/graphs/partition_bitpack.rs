//! The `partition_bitpack` graph (`EI_partitionBitpackDynGraph`,
//! `encode_partition_bitpack.c`), graph 23.
//!
//! For 16-bit and 32-bit numbers it looks for the partitioning of the value
//! range that minimizes the bits spent, then runs the partition node with it,
//! sending the bucket indices to `bitpack` and the offsets to `store`.
//! Inputs under ten elements, or a partitioning that saves less than one
//! percent, go to `store`; a trivial partitioning goes to `bitpack` when
//! that narrows the numbers.
//!
//! The values are first summarized in a histogram of buckets: 16 values each
//! below 2^16, then one bucket per power of two. The cost model is
//! `24 + count * ceil(log2(bucket size))` bits per partition plus
//! `ceil(log2(partitions))` bits per element. The default mode (3-element
//! sampling of the histogram, greedy splitting of the partition with the
//! best gain, then iterative improvement for 10 000 elements or more) is
//! ported statement for statement. With the graph's int parameter 0 equal to
//! 1 the reference samples every element but its histogram size stays 1 (it
//! only widens the histogram when sampling), which this port reproduces; the
//! dynamic program of that mode is ported too.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::super::codecs::numeric::custom_params_bytes;
use super::super::error::ExecError;
use super::super::exec::{Edge, GraphCx};
use super::super::params::LocalParams;
use super::super::registry::{GraphFn, GraphId, NodeId};
use crate::shared::numeric::elements;

/// `ZL_GRAPH_PARTITION_BITPACK_OPTIMAL_PID`.
const OPTIMAL_PID: i32 = crate::graph::params::partition::BITPACK_OPTIMAL;
/// `ZL_TernaryParam_enable`.
const TERNARY_ENABLE: i32 = 1;
/// `ZL_PARTITION_CUSTOM_PID`.
const PARTITION_CUSTOM_PID: i32 = 121;
/// `ZL_NODE_PARTITION`.
const PARTITION_NODE: NodeId = NodeId::std(48);
/// `ZL_GRAPH_STORE`.
const STORE: GraphId = GraphId::std(2);
/// `ZL_GRAPH_BITPACK`.
const BITPACK: GraphId = GraphId::std(10);

const PB_OVERHEAD_BITS: u64 = 24;
const PB_MAX_PARTITIONS: usize = 256;
const PB_MIN_GAIN_PCT: usize = 1;
const PB_PRECISION_LOSS: u32 = 4;
const PB_LINEAR_THRESHOLD_LOG: u32 = 16;
const PB_LINEAR_THRESHOLD: u32 = 1 << PB_LINEAR_THRESHOLD_LOG;
const PB_LINEAR_BUCKETS: u32 = PB_LINEAR_THRESHOLD >> PB_PRECISION_LOSS;
/// `ZL_PARTITION_MAX_PARTITION_SIZE_FOR_UNROLL4`.
const MAX_SIZE_16: u64 = 1 << 14;
/// `ZL_PARTITION_MAX_PARTITION_SIZE_FOR_UNROLL2`.
const MAX_SIZE_32: u64 = 1 << 28;

/// `partition_bitpack`.
#[derive(Debug)]
pub(crate) struct PartitionBitpack;

/// See [`PartitionBitpack`].
pub(crate) static PARTITION_BITPACK: PartitionBitpack = PartitionBitpack;

/// `ZL_nextPow2`: the smallest `n` with `2^n >= bound`.
const fn next_pow2(bound: u64) -> u32 {
    if bound <= 1 {
        0
    } else {
        64 - (bound - 1).leading_zeros()
    }
}

/// `ZL_isPow2`, which also accepts zero.
const fn is_pow2(value: u64) -> bool {
    value & value.wrapping_sub(1) == 0
}

/// `PB_histCapacity`.
const fn hist_capacity(value_bits: u32) -> u32 {
    if value_bits <= PB_LINEAR_THRESHOLD_LOG {
        1 << (value_bits - PB_PRECISION_LOSS)
    } else {
        PB_LINEAR_BUCKETS + value_bits - PB_LINEAR_THRESHOLD_LOG
    }
}

/// `PB_idx2bucket`.
const fn idx2bucket(idx: u32) -> u32 {
    if idx < PB_LINEAR_THRESHOLD {
        idx >> PB_PRECISION_LOSS
    } else {
        PB_LINEAR_BUCKETS + (31 - idx.leading_zeros()) - PB_LINEAR_THRESHOLD_LOG
    }
}

/// `PB_buildCumBases`: the first value of every bucket, plus the end of the
/// last.
fn build_cum_bases(value_bits: u32) -> Vec<u64> {
    let capacity = hist_capacity(value_bits);
    let linear = capacity.min(PB_LINEAR_BUCKETS);
    let mut bases = vec![0u64; capacity as usize + 1];
    for (i, base) in bases.iter_mut().enumerate().take(linear as usize + 1) {
        *base = (i as u64) << PB_PRECISION_LOSS;
    }
    let mut i = 1;
    while linear + i <= capacity {
        bases[(linear + i) as usize] = 1u64 << (PB_LINEAR_THRESHOLD_LOG + i);
        i += 1;
    }
    bases
}

/// `PB_CumHist`: the cumulative histogram and the bucket boundaries.
struct CumHist {
    /// Elements before each bucket, `hist_size + 1` entries.
    cum_hist: Vec<u32>,
    /// First value of each bucket.
    cum_bases: Vec<u64>,
    hist_size: u32,
    max_bucket_size: u64,
}

impl CumHist {
    /// The elements of the histogram.
    fn total(&self) -> u32 {
        self.cum_hist[self.hist_size as usize]
    }

    /// The number of values from bucket `b` up to bucket `e`.
    fn bucket_size(&self, b: u32, e: u32) -> u64 {
        self.cum_bases[e as usize] - self.cum_bases[b as usize]
    }

    /// `PB_isLegalPartition`.
    fn is_legal(&self, b: u32, e: u32) -> bool {
        b < e && self.bucket_size(b, e) <= self.max_bucket_size
    }

    /// `PB_singlePartitionCost`.
    fn single_cost(&self, b: u32, e: u32) -> u64 {
        let count = self.cum_hist[e as usize].wrapping_sub(self.cum_hist[b as usize]);
        let off_bits = next_pow2(self.bucket_size(b, e));
        PB_OVERHEAD_BITS + u64::from(count) * u64::from(off_bits)
    }

    /// `PB_partitionEnd`.
    fn end_of(&self, partitions: &[u32], n: usize, i: usize) -> u32 {
        if i + 1 == n {
            self.hist_size
        } else {
            partitions[i + 1]
        }
    }
}

/// `PB_buildCumHist`: the cumulative histogram of `bytes` (elements of
/// `width` bytes) and the largest value it can represent.
fn build_cum_hist(bytes: &[u8], width: usize, optimal: bool) -> (CumHist, u32) {
    let value_bits = 8 * width as u32;
    let capacity = hist_capacity(value_bits);
    let cum_bases = build_cum_bases(value_bits);
    let skip: u32 = if optimal { 1 } else { 3 };
    let mut min_bucket = capacity - 1;
    let mut max_bucket = 0u32;
    let mut hist = vec![0u32; capacity as usize + 1];
    for value in elements(bytes, width).step_by(skip as usize) {
        hist[idx2bucket(value as u32) as usize] += 1;
    }
    let n_elts = bytes.len() / width;
    if skip > 1 && n_elts > 0 {
        for value in elements(bytes, width) {
            let bucket = idx2bucket(value as u32);
            min_bucket = min_bucket.min(bucket);
            max_bucket = max_bucket.max(bucket);
        }
        hist[min_bucket as usize] += u32::from(hist[min_bucket as usize] == 0);
        hist[max_bucket as usize] += u32::from(hist[max_bucket as usize] == 0);
    }
    let hist_size = max_bucket + 1;
    let mut sum = 0u32;
    for count in hist.iter_mut().take(hist_size as usize) {
        let scaled = count.wrapping_mul(skip);
        *count = sum;
        sum = sum.wrapping_add(scaled);
    }
    hist[hist_size as usize] = sum;
    hist.truncate(hist_size as usize + 1);
    let max_seen = (cum_bases[hist_size as usize] - 1) as u32;
    let cum = CumHist {
        cum_hist: hist,
        cum_bases,
        hist_size,
        max_bucket_size: if width == 2 { MAX_SIZE_16 } else { MAX_SIZE_32 },
    };
    (cum, max_seen)
}

/// `PB_fixedBucketCost`: the bits of a partitioning, `u64::MAX` when a
/// partition is illegal.
fn fixed_bucket_cost(ch: &CumHist, partitions: &[u32], total_partitions: usize) -> u64 {
    let bucket_bits = next_pow2(total_partitions as u64);
    let mut cost = u64::from(ch.total()) * u64::from(bucket_bits);
    for (i, &b) in partitions.iter().enumerate() {
        let e = ch.end_of(partitions, partitions.len(), i);
        if !ch.is_legal(b, e) {
            return u64::MAX;
        }
        let count = ch.cum_hist[e as usize].wrapping_sub(ch.cum_hist[b as usize]);
        let off_bits = next_pow2(ch.bucket_size(b, e));
        cost += PB_OVERHEAD_BITS + u64::from(count) * u64::from(off_bits);
    }
    cost
}

/// `PB_dpBucketCost`.
fn dp_bucket_cost(ch: &CumHist, fixed_cost: u32, b: u32, e: u32) -> u64 {
    if !ch.is_legal(b, e) {
        return u64::MAX;
    }
    let count = ch.cum_hist[e as usize].wrapping_sub(ch.cum_hist[b as usize]);
    let size = ch.bucket_size(b, e);
    u64::from(count) * u64::from(fixed_cost.wrapping_add(next_pow2(size)))
}

/// `PB_dpPartition`: the optimal partitioning into at most `num_buckets`
/// partitions, or an empty vector.
fn dp_partition(ch: &CumHist, num_buckets: usize) -> Vec<u32> {
    let fixed_cost = next_pow2(num_buckets as u64);
    let n = ch.hist_size as usize;
    let b = num_buckets.min(n);
    let (b1, n1) = (b + 1, n + 1);
    let mut opt = vec![u64::MAX; n1 * b1];
    let mut begin = vec![u32::MAX; n1 * b1];
    opt[0] = 0;
    begin[0] = 0;
    for e in 1..n1 {
        let max_possible = e.min(b);
        for k in 1..=max_possible {
            let max_size = e - (k - 1);
            let mut size = 1;
            while size <= max_size {
                let start = e - size;
                if ch.cum_bases[e] - ch.cum_bases[start] > ch.max_bucket_size {
                    break;
                }
                let prev = opt[n1 * (k - 1) + start];
                if prev != u64::MAX {
                    let old = opt[n1 * k + e];
                    let end_cost = dp_bucket_cost(ch, fixed_cost, start as u32, e as u32);
                    if end_cost != u64::MAX {
                        let new = prev.wrapping_add(end_cost);
                        if new < old {
                            opt[n1 * k + e] = new;
                            begin[n1 * k + e] = start as u32;
                        }
                    }
                }
                size = if e == n { size + 1 } else { size * 2 };
            }
        }
    }
    let mut out = vec![0u32; b];
    let (mut e, mut pos) = (n, b);
    while e > 0 {
        let start = begin[n1 * pos + e];
        if start == u32::MAX || pos == 0 {
            return Vec::new();
        }
        pos -= 1;
        out[pos] = start;
        e = start as usize;
    }
    out
}

/// `PB_GreedyOpt`: a partitioning being refined.
struct Greedy<'h> {
    ch: &'h CumHist,
    partitions: Vec<u32>,
    target: usize,
}

impl Greedy<'_> {
    /// `PB_splitGain`.
    fn split_gain(&self, begin: u32, end: u32) -> i64 {
        let old = self.ch.single_cost(begin, end);
        let size = end - begin;
        let new_size = 1u32 << (next_pow2(u64::from(size)) - 1);
        let mid = begin + new_size;
        let left = self.ch.single_cost(begin, mid);
        let right = self.ch.single_cost(mid, end);
        old as i64 - left.wrapping_add(right) as i64
    }

    /// `PB_partitionSplitGain`.
    fn partition_split_gain(&self, begin: u32, end: u32) -> i64 {
        if !self.ch.is_legal(begin, end) {
            return i64::MAX;
        }
        if end - begin <= 1 {
            return i64::MIN;
        }
        self.split_gain(begin, end)
    }

    /// `PB_dividePartitions`: splits the illegal or most profitable partition
    /// until `target` partitions exist.
    fn divide(&mut self) {
        let mut gains = [i64::MIN; PB_MAX_PARTITIONS];
        let n = self.partitions.len();
        for (i, gain) in gains.iter_mut().enumerate().take(n) {
            let (b, e) = (self.partitions[i], self.ch.end_of(&self.partitions, n, i));
            *gain = self.partition_split_gain(b, e);
        }
        while self.partitions.len() < self.target {
            let n = self.partitions.len();
            let mut best_gain = i64::MIN;
            let mut best_idx = None;
            for (i, &gain) in gains.iter().enumerate().take(n) {
                if gain == i64::MAX {
                    best_idx = Some(i);
                    break;
                }
                if gain > best_gain {
                    best_gain = gain;
                    best_idx = Some(i);
                }
            }
            let Some(best) = best_idx else { break };
            let best_begin = self.partitions[best];
            let best_end = self.ch.end_of(&self.partitions, n, best);
            if best_end - best_begin <= 1 {
                break;
            }
            // PB_dividePartitionAt: the first half is the largest power of
            // two below the size.
            let new_size = 1u32 << (next_pow2(u64::from(best_end - best_begin)) - 1);
            self.partitions.insert(best + 1, best_begin + new_size);
            let n = self.partitions.len();
            for j in ((best + 2)..n).rev() {
                gains[j] = gains[j - 1];
            }
            for j in [best, best + 1] {
                if j < n {
                    let (b, e) = (self.partitions[j], self.ch.end_of(&self.partitions, n, j));
                    gains[j] = self.partition_split_gain(b, e);
                }
            }
        }
    }

    /// `PB_growPartitions`: the boundaries after growing partition `idx` to
    /// the next power of two, written to `out`; returns the end of the
    /// cascade.
    fn grow(&self, idx: usize, out: &mut [u32]) -> usize {
        let n = self.partitions.len();
        if idx + 1 == n {
            return idx;
        }
        let begin = self.partitions[idx];
        let end = self.partitions[idx + 1];
        let size = end.wrapping_sub(begin);
        let new_size = 1u32 << (next_pow2(u64::from(size)) + 1);
        out[idx + 1] = begin.wrapping_add(new_size);
        let mut j = idx + 1;
        while j + 1 < n {
            let begin = out[j];
            let end = self.partitions[j + 1];
            let size = if end > begin { end - begin } else { 1 };
            let alt = 1u32 << next_pow2(u64::from(size));
            let new_end = begin.wrapping_add(new_size.max(alt));
            if new_end == end {
                return j + 1;
            }
            out[j + 1] = new_end;
            j += 1;
        }
        n
    }

    /// `PB_shrinkPartitions`.
    fn shrink(&self, idx: usize, out: &mut [u32]) -> usize {
        let n = self.partitions.len();
        if idx + 1 == n {
            return idx;
        }
        let begin = self.partitions[idx];
        let end = self.partitions[idx + 1];
        let size = end.wrapping_sub(begin);
        if size <= 1 {
            return idx;
        }
        let new_size = 1u32 << (next_pow2(u64::from(size)) - 1);
        out[idx + 1] = begin.wrapping_add(new_size);
        let mut j = idx + 1;
        while j + 1 < n {
            let begin = out[j];
            let end = self.partitions[j + 1];
            let size = end.wrapping_sub(begin);
            if is_pow2(u64::from(size)) {
                return j + 1;
            }
            let new_end = begin.wrapping_add(1u32 << (next_pow2(u64::from(size)) - 1));
            if new_end == end {
                return j + 1;
            }
            out[j + 1] = new_end;
            j += 1;
        }
        n
    }

    /// `PB_modifiedRangeCost`.
    fn modified_range_cost(&self, out: &[u32], from: usize, to: usize) -> u64 {
        let n = self.partitions.len();
        let mut cost = 0u64;
        for i in from..to {
            let b = if i == from {
                self.partitions[i]
            } else {
                out[i]
            };
            let e = if i + 1 == n {
                self.ch.hist_size
            } else if i + 1 < to {
                out[i + 1]
            } else {
                self.partitions[i + 1]
            };
            if e > self.ch.hist_size || !self.ch.is_legal(b, e) {
                return u64::MAX;
            }
            cost += self.ch.single_cost(b, e);
        }
        cost
    }

    /// `PB_tryMutation`: keeps the mutation in `out` when it is cheaper.
    fn try_mutation(
        &mut self,
        out: &[u32],
        part_cost: &mut [u64],
        current: &mut u64,
        idx: usize,
        cascade_end: usize,
    ) -> bool {
        let n = self.partitions.len();
        let new = self.modified_range_cost(out, idx, cascade_end);
        if new == u64::MAX {
            return false;
        }
        let old: u64 = part_cost[idx..cascade_end].iter().sum();
        if new >= old {
            return false;
        }
        let copied = (idx + 1)..cascade_end.min(n);
        if !copied.is_empty() {
            self.partitions[copied.clone()].copy_from_slice(&out[copied]);
        }
        *current = current.wrapping_sub(old).wrapping_add(new);
        for (j, cost) in part_cost.iter_mut().enumerate().take(cascade_end).skip(idx) {
            let b = self.partitions[j];
            let e = self.ch.end_of(&self.partitions, n, j);
            *cost = self.ch.single_cost(b, e);
        }
        true
    }

    /// `PB_iterativeImprovement`: grows or shrinks boundaries to powers of
    /// two until no change helps.
    fn improve(&mut self) {
        let n = self.partitions.len();
        if n < 2 {
            return;
        }
        let mut part_cost = [0u64; PB_MAX_PARTITIONS];
        let mut sum = 0u64;
        for (i, cost) in part_cost.iter_mut().enumerate().take(n) {
            let (b, e) = (self.partitions[i], self.ch.end_of(&self.partitions, n, i));
            if !self.ch.is_legal(b, e) {
                return;
            }
            *cost = self.ch.single_cost(b, e);
            sum += *cost;
        }
        let bucket_bits = next_pow2(self.target as u64);
        let base = u64::from(self.ch.total()) * u64::from(bucket_bits);
        let mut current = base + sum;
        let mut scratch = [0u32; PB_MAX_PARTITIONS + 1];
        loop {
            let start_cost = current;
            for idx in 0..n - 1 {
                let cascade_end = self.grow(idx, &mut scratch);
                if self.try_mutation(&scratch, &mut part_cost, &mut current, idx, cascade_end) {
                    continue;
                }
                let cascade_end = self.shrink(idx, &mut scratch);
                self.try_mutation(&scratch, &mut part_cost, &mut current, idx, cascade_end);
            }
            if current == start_cost {
                break;
            }
        }
    }

    /// `PB_partitionsAreLegal`.
    fn all_legal(&self) -> bool {
        let n = self.partitions.len();
        (0..n).all(|i| {
            self.ch
                .is_legal(self.partitions[i], self.ch.end_of(&self.partitions, n, i))
        })
    }
}

/// `PB_greedyOptimize` without a prefix.
fn greedy_optimize(ch: &CumHist, target: usize) -> Vec<u32> {
    let mut greedy = Greedy {
        ch,
        partitions: vec![0],
        target,
    };
    greedy.divide();
    if ch.total() >= 10_000 {
        greedy.improve();
    }
    if greedy.all_legal() {
        greedy.partitions
    } else {
        Vec::new()
    }
}

/// `PB_PartitionResult`.
struct PartitionResult {
    /// First value of every partition, empty for no result.
    partitions: Vec<u64>,
    bit_cost: u64,
    max_symbol_value: u64,
    bitpack_bits: u32,
    bitpack_cost: u64,
}

/// `PB_fixedPartition`: tries 16 and then 32 partitions.
fn fixed_partition(bytes: &[u8], width: usize, optimal: bool) -> PartitionResult {
    let (ch, max_seen) = build_cum_hist(bytes, width, optimal);
    let bitpack_bits = next_pow2(u64::from(max_seen) + 1);
    let bitpack_cost = u64::from(bitpack_bits) * u64::from(ch.total());
    let mut result = PartitionResult {
        partitions: Vec::new(),
        bit_cost: 0,
        max_symbol_value: 0,
        bitpack_bits,
        bitpack_cost,
    };
    let mut best: Vec<u32> = Vec::new();
    let mut best_cost = u64::MAX;
    for partitions in [16usize, 32] {
        let trial = if optimal {
            dp_partition(&ch, partitions)
        } else {
            greedy_optimize(&ch, partitions)
        };
        if trial.is_empty() {
            continue;
        }
        let cost = fixed_bucket_cost(&ch, &trial, trial.len());
        if cost == u64::MAX {
            continue;
        }
        if cost + cost / 64 < best_cost {
            best_cost = cost;
            best = trial;
        } else {
            break;
        }
    }
    if best.is_empty() {
        return result;
    }
    result.partitions = best.iter().map(|&b| ch.cum_bases[b as usize]).collect();
    result.bit_cost = best_cost;
    result.max_symbol_value = ch.cum_bases[ch.hist_size as usize] - 1;
    result
}

/// What the graph decided for its input.
#[derive(Debug, Eq, PartialEq)]
enum Plan {
    /// Send the input to `store`.
    Store,
    /// Send the input to `bitpack`.
    Bitpack,
    /// Run the partition node with `(start, sizes)`.
    Partition(u64, Vec<u64>),
}

/// The decision of `EI_partitionBitpackDynGraph` for an input.
///
/// # Errors
///
/// An element width other than 2 or 4.
fn plan(bytes: &[u8], width: usize, optimal: bool) -> Result<Plan, ExecError> {
    if width != 2 && width != 4 {
        return Err(ExecError::Parameter(
            "Only 2-byte and 4-byte numeric values are accepted",
        ));
    }
    let n_elts = bytes.len() / width;
    if n_elts < 10 {
        return Ok(Plan::Store);
    }
    let result = fixed_partition(bytes, width, optimal);
    if result.partitions.is_empty() {
        return Ok(Plan::Store);
    }
    if result.partitions.len() == 1 || result.bit_cost >= result.bitpack_cost {
        return Ok(if (result.bitpack_bits as usize) < 8 * width {
            Plan::Bitpack
        } else {
            Plan::Store
        });
    }
    let max_byte_cost = (width * (100 - PB_MIN_GAIN_PCT) * n_elts) / 100;
    if (result.bit_cost / 8) as usize >= max_byte_cost {
        return Ok(Plan::Store);
    }
    let count = result.partitions.len();
    let sizes = (0..count)
        .map(|i| {
            let begin = result.partitions[i];
            let end = if i + 1 == count {
                result.max_symbol_value + 1
            } else {
                result.partitions[i + 1]
            };
            end - begin
        })
        .collect();
    Ok(Plan::Partition(result.partitions[0], sizes))
}

impl GraphFn for PartitionBitpack {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let input = inputs[0];
        let optimal = cx.eff.params.int(OPTIMAL_PID) == Some(TERNARY_ENABLE);
        let plan = {
            let stream = cx.data(input)?;
            plan(stream.bytes(), stream.width(), optimal)?
        };
        match plan {
            Plan::Store => cx.set_destination(&[input], STORE, None),
            Plan::Bitpack => cx.set_destination(&[input], BITPACK, None),
            Plan::Partition(start, sizes) => {
                let params = LocalParams {
                    copies: vec![(
                        PARTITION_CUSTOM_PID,
                        Arc::from(custom_params_bytes(start, &sizes)),
                    )],
                    ..LocalParams::default()
                };
                let outputs = cx.run_node(PARTITION_NODE, &[input], Some(&params))?;
                debug_assert_eq!(outputs.len(), 2);
                cx.set_destination(&[outputs[0]], BITPACK, None)?;
                cx.set_destination(&[outputs[1]], STORE, None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::codecs::numeric::PartitionParams;
    use super::*;
    use alloc::boxed::Box;

    fn bytes16(values: &[u16]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn bucket_arithmetic_matches_the_reference() {
        assert_eq!(hist_capacity(16), 4096);
        assert_eq!(hist_capacity(32), 4112);
        assert_eq!(
            [0, 15, 16, 65_535, 65_536, 1 << 20, u32::MAX].map(idx2bucket),
            [0, 0, 1, 4095, 4096, 4100, 4111]
        );
        let bases = build_cum_bases(32);
        assert_eq!(
            (bases[0], bases[1], bases[4096], bases[4097], bases[4112]),
            (0, 16, 65_536, 1 << 17, 1 << 32)
        );
        assert_eq!(*build_cum_bases(16).last().unwrap(), 65_536);
        assert_eq!(
            [0, 1, 2, 3, 4, 5, 1 << 20].map(next_pow2),
            [0, 0, 1, 2, 2, 3, 20]
        );
        assert!(is_pow2(0) && is_pow2(8) && !is_pow2(6));
    }

    #[test]
    fn small_and_unsupported_inputs_are_stored_or_rejected() {
        assert_eq!(plan(&bytes16(&[1; 9]), 2, false), Ok(Plan::Store));
        assert!(plan(&[0; 10], 1, false).is_err());
        assert!(plan(&[0; 80], 8, false).is_err());
    }

    #[test]
    fn narrow_values_are_bitpacked() {
        // Uniform 10-bit values: one useful partition at best, so bitpack.
        let values: Vec<u16> = (0..2000u32).map(|i| (i * 7919 % 1024) as u16).collect();
        let decision = plan(&bytes16(&values), 2, false).unwrap();
        assert!(
            matches!(decision, Plan::Bitpack | Plan::Partition(..)),
            "{decision:?}"
        );
        // All bits used and no structure: stored.
        let wide: Vec<u16> = (0..2000u32).map(|i| (i * 40_503 % 65_536) as u16).collect();
        assert_eq!(plan(&bytes16(&wide), 2, false), Ok(Plan::Store));
    }

    #[test]
    fn skewed_values_are_partitioned_with_valid_parameters() {
        // Mostly tiny values with a few huge ones.
        let values: Vec<u16> = (0..3000u32)
            .map(|i| {
                if i % 50 == 0 {
                    60_000 + (i % 100) as u16
                } else {
                    (i % 5) as u16
                }
            })
            .collect();
        for optimal in [false, true] {
            if let Ok(Plan::Partition(start, sizes)) = plan(&bytes16(&values), 2, optimal) {
                let params = PartitionParams { start, sizes };
                assert!(params.is_valid());
            }
        }
        match plan(&bytes16(&values), 2, false).unwrap() {
            Plan::Partition(start, sizes) => {
                assert_eq!(start, 0);
                assert!(sizes.len() >= 2 && sizes.len() <= 32);
            }
            other => panic!("expected a partitioning, got {other:?}"),
        }
    }

    #[test]
    fn the_dynamic_program_finds_a_legal_partitioning() {
        let values: Vec<u16> = (0..4000u32).map(|i| ((i * i) % 3000) as u16).collect();
        let bytes = bytes16(&values);
        let (ch, _) = build_cum_hist(&bytes, 2, true);
        // The reference's histogram stays one bucket wide in this mode.
        assert_eq!(ch.hist_size, 1);
        let parts = dp_partition(&ch, 16);
        assert_eq!(parts, [0]);
        let (ch, _) = build_cum_hist(&bytes, 2, false);
        assert!(ch.hist_size > 1);
        let parts = dp_partition(&ch, 16);
        assert!(!parts.is_empty());
        assert!(fixed_bucket_cost(&ch, &parts, parts.len()) < u64::MAX);
    }

    #[test]
    fn improvement_only_lowers_the_cost() {
        let values: Vec<u16> = (0..20_000u32).map(|i| ((i * i) % 5000) as u16).collect();
        let (ch, _) = build_cum_hist(&bytes16(&values), 2, false);
        let mut greedy = Greedy {
            ch: &ch,
            partitions: vec![0],
            target: 16,
        };
        greedy.divide();
        let before = fixed_bucket_cost(&ch, &greedy.partitions, 16);
        greedy.improve();
        let after = fixed_bucket_cost(&ch, &greedy.partitions, 16);
        assert!(after <= before);
        assert!(greedy.all_legal());
    }

    /// The reference's `PB_fixedPartition`, compiled from its source, finds
    /// the same partitioning, cost and bit-packing figures.
    #[test]
    #[cfg(not(miri))]
    fn the_partitioning_search_matches_the_reference() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut inputs: Vec<(Vec<u8>, usize)> = Vec::new();
        for width in [2usize, 4] {
            let max = u64::MAX >> (64 - 8 * width);
            for count in [10usize, 11, 100, 3000, 20_000] {
                let generators: [Box<dyn FnMut(u64, u64) -> u64>; 5] = [
                    Box::new(|_, r| r & max),
                    Box::new(|i, _| i % 1000),
                    Box::new(move |i, r| if i % 50 == 0 { max - r % 100 } else { r % 5 }),
                    Box::new(move |_, r| (r & max) >> ((r >> 58) % (8 * width as u64))),
                    Box::new(|i, _| (i * i) % 5000),
                ];
                for mut generator in generators {
                    let bytes: Vec<u8> = (0..count as u64)
                        .flat_map(|i| {
                            let r = next();
                            generator(i, r).to_le_bytes()[..width].to_vec()
                        })
                        .collect();
                    inputs.push((bytes, width));
                }
            }
            inputs.push((alloc::vec![7u8; 400 * width], width));
        }
        let mut found = 0;
        for (bytes, width) in &inputs {
            for optimal in [false, true] {
                let ours = fixed_partition(bytes, *width, optimal);
                let theirs = openzl_ffi::enc_numeric::pb_fixed_partition(bytes, *width, optimal);
                let context = alloc::format!("width {width} len {} optimal {optimal}", bytes.len());
                assert_eq!(ours.partitions, theirs.starts, "{context}");
                assert_eq!(ours.bitpack_bits as usize, theirs.bitpack_bits, "{context}");
                assert_eq!(ours.bitpack_cost as usize, theirs.bitpack_cost, "{context}");
                if !ours.partitions.is_empty() {
                    found += 1;
                    assert_eq!(ours.bit_cost, theirs.bit_cost, "{context}");
                    assert_eq!(ours.max_symbol_value, theirs.max_symbol_value, "{context}");
                }
            }
        }
        assert!(found > 20, "only {found} partitionings were compared");
    }
}
