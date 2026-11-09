//! Huffman compression (`fse/compress/huf_compress.c`, `fse/huf.h`).
//!
//! [`build_ctable`] sorts the histogram, builds the tree and limits its
//! height exactly as `HUF_buildCTable` (the sort, including its quicksort
//! buckets, decides which of several equal-cost trees comes out, so it is
//! ported statement for statement). [`compress1x_using_ctable`] and
//! [`compress4x_using_ctable`] write Zstandard's Huffman streams.

use alloc::vec;
use alloc::vec::Vec;

use super::bits::BitWriter;
use super::{EntropyError, fse, highbit32};

/// `HUF_TABLELOG_MAX`.
pub(crate) const TABLELOG_MAX: u32 = 12;
/// `HUF_TABLELOG_DEFAULT`.
pub(crate) const TABLELOG_DEFAULT: u32 = 11;
/// `HUF_SYMBOLVALUE_MAX`.
pub(crate) const SYMBOLVALUE_MAX: u32 = 255;
/// `HUF_CTABLEBOUND`: room for a serialised table.
pub(crate) const CTABLE_BOUND: usize = 129;

/// `HUF_compressBound` (`HUF_COMPRESSBOUND`).
pub(crate) const fn compress_bound(size: usize) -> usize {
    CTABLE_BOUND + size + (size >> 8) + 8
}

/// A Huffman compression table (`HUF_CElt[]`): the code length and value of
/// every symbol up to the alphabet's maximum, and the table log.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HufCTable {
    table_log: u32,
    nb_bits: Vec<u8>,
    value: Vec<u16>,
}

impl HufCTable {
    /// The table log (`CTable[0]`): the length of the longest code.
    pub(crate) const fn table_log(&self) -> u32 {
        self.table_log
    }

    /// `HUF_getNbBitsFromCTable`: the code length of `symbol`, 0 when absent
    /// or beyond the table.
    pub(crate) fn nb_bits(&self, symbol: usize) -> u32 {
        self.nb_bits.get(symbol).map_or(0, |&bits| u32::from(bits))
    }
}

/// `HUF_optimalTableLog`.
pub(crate) fn optimal_table_log(max_table_log: u32, src_size: usize, max_symbol_value: u32) -> u32 {
    fse::optimal_table_log_internal(max_table_log, src_size, max_symbol_value, 1)
}

/// `nodeElt`.
#[derive(Clone, Copy, Debug, Default)]
struct NodeElt {
    count: u32,
    parent: u16,
    byte: u8,
    nb_bits: u8,
}

/// `HUF_CTABLE_WORKSPACE_SIZE_U32`: the node table size.
const NODE_TABLE_SIZE: usize = 2 * SYMBOLVALUE_MAX as usize + 2;
/// `STARTNODE`.
const START_NODE: i32 = SYMBOLVALUE_MAX as i32 + 1;
/// `RANK_POSITION_TABLE_SIZE`.
const RANK_POSITION_TABLE_SIZE: usize = 192;
/// `RANK_POSITION_MAX_COUNT_LOG`.
const RANK_POSITION_MAX_COUNT_LOG: u32 = 32;
/// `RANK_POSITION_LOG_BUCKETS_BEGIN`.
const RANK_POSITION_LOG_BUCKETS_BEGIN: u32 =
    RANK_POSITION_TABLE_SIZE as u32 - 1 - RANK_POSITION_MAX_COUNT_LOG - 1;
/// `RANK_POSITION_DISTINCT_COUNT_CUTOFF`. The reference's macro is not
/// parenthesised and expands to `158 + highbit32(158)`, which is 165 (its
/// comment says 166).
const RANK_POSITION_DISTINCT_COUNT_CUTOFF: u32 = RANK_POSITION_LOG_BUCKETS_BEGIN
    + (u32::BITS - 1 - RANK_POSITION_LOG_BUCKETS_BEGIN.leading_zeros());

/// `rankPos`.
#[derive(Clone, Copy, Debug, Default)]
struct RankPos {
    base: u16,
    curr: u16,
}

/// The node table with the reference's `huffNode[-1]` sentinel slot.
struct Nodes(Vec<NodeElt>);

impl Nodes {
    fn new() -> Self {
        Self(vec![NodeElt::default(); NODE_TABLE_SIZE])
    }

    const fn slot(index: i32) -> usize {
        (index + 1) as usize
    }

    fn at(&self, index: i32) -> &NodeElt {
        &self.0[Self::slot(index)]
    }

    fn at_mut(&mut self, index: i32) -> &mut NodeElt {
        &mut self.0[Self::slot(index)]
    }
}

/// `HUF_setMaxHeight`: enforces `target_nb_bits` on the sorted tree and
/// returns the resulting maximum height.
fn set_max_height(nodes: &mut Nodes, last_non_null: u32, target_nb_bits: u32) -> u32 {
    let largest_bits = u32::from(nodes.at(last_non_null as i32).nb_bits);
    if largest_bits <= target_nb_bits {
        return largest_bits;
    }
    let mut total_cost: i32 = 0;
    let base_cost: i32 = 1 << (largest_bits - target_nb_bits);
    let mut n = last_non_null as i32;

    while u32::from(nodes.at(n).nb_bits) > target_nb_bits {
        total_cost += base_cost - (1 << (largest_bits - u32::from(nodes.at(n).nb_bits)));
        nodes.at_mut(n).nb_bits = target_nb_bits as u8;
        n -= 1;
    }
    while u32::from(nodes.at(n).nb_bits) == target_nb_bits {
        n -= 1;
    }
    total_cost >>= largest_bits - target_nb_bits;

    const NO_SYMBOL: u32 = 0xF0F0_F0F0;
    let mut rank_last = [NO_SYMBOL; TABLELOG_MAX as usize + 2];
    {
        let mut current_nb_bits = target_nb_bits;
        let mut pos = n;
        while pos >= 0 {
            let bits = u32::from(nodes.at(pos).nb_bits);
            if bits < current_nb_bits {
                current_nb_bits = bits;
                rank_last[(target_nb_bits - current_nb_bits) as usize] = pos as u32;
            }
            pos -= 1;
        }
    }

    while total_cost > 0 {
        let mut n_bits_to_decrease = highbit32(total_cost as u32) + 1;
        while n_bits_to_decrease > 1 {
            let high_pos = rank_last[n_bits_to_decrease as usize];
            let low_pos = rank_last[n_bits_to_decrease as usize - 1];
            if high_pos == NO_SYMBOL {
                n_bits_to_decrease -= 1;
                continue;
            }
            if low_pos == NO_SYMBOL {
                break;
            }
            let high_total = nodes.at(high_pos as i32).count;
            let low_total = nodes.at(low_pos as i32).count.wrapping_mul(2);
            if high_total <= low_total {
                break;
            }
            n_bits_to_decrease -= 1;
        }
        while n_bits_to_decrease <= TABLELOG_MAX
            && rank_last[n_bits_to_decrease as usize] == NO_SYMBOL
        {
            n_bits_to_decrease += 1;
        }
        total_cost -= 1 << (n_bits_to_decrease - 1);
        nodes
            .at_mut(rank_last[n_bits_to_decrease as usize] as i32)
            .nb_bits += 1;
        if rank_last[n_bits_to_decrease as usize - 1] == NO_SYMBOL {
            rank_last[n_bits_to_decrease as usize - 1] = rank_last[n_bits_to_decrease as usize];
        }
        if rank_last[n_bits_to_decrease as usize] == 0 {
            rank_last[n_bits_to_decrease as usize] = NO_SYMBOL;
        } else {
            rank_last[n_bits_to_decrease as usize] -= 1;
            if u32::from(
                nodes
                    .at(rank_last[n_bits_to_decrease as usize] as i32)
                    .nb_bits,
            ) != target_nb_bits - n_bits_to_decrease
            {
                rank_last[n_bits_to_decrease as usize] = NO_SYMBOL;
            }
        }
    }

    while total_cost < 0 {
        if rank_last[1] == NO_SYMBOL {
            while u32::from(nodes.at(n).nb_bits) == target_nb_bits {
                n -= 1;
            }
            nodes.at_mut(n + 1).nb_bits -= 1;
            rank_last[1] = (n + 1) as u32;
            total_cost += 1;
            continue;
        }
        nodes.at_mut(rank_last[1] as i32 + 1).nb_bits -= 1;
        rank_last[1] += 1;
        total_cost += 1;
    }
    target_nb_bits
}

/// `HUF_getIndex`: the bucket of a count.
fn rank_index(count: u32) -> u32 {
    if count < RANK_POSITION_DISTINCT_COUNT_CUTOFF {
        count
    } else {
        highbit32(count) + RANK_POSITION_LOG_BUCKETS_BEGIN
    }
}

/// `HUF_insertionSort` (descending).
fn insertion_sort(arr: &mut [NodeElt], low: i32, high: i32) {
    let size = high - low + 1;
    let base = low as usize;
    for i in 1..size as usize {
        let key = arr[base + i];
        let mut j = i as i32 - 1;
        while j >= 0 && arr[base + j as usize].count < key.count {
            arr[base + j as usize + 1] = arr[base + j as usize];
            j -= 1;
        }
        arr[base + (j + 1) as usize] = key;
    }
}

/// `HUF_quickSortPartition`: the rightmost element is the pivot.
fn quick_sort_partition(arr: &mut [NodeElt], low: i32, high: i32) -> i32 {
    let pivot = arr[high as usize].count;
    let mut i = low - 1;
    let mut j = low;
    while j < high {
        if arr[j as usize].count > pivot {
            i += 1;
            arr.swap(i as usize, j as usize);
        }
        j += 1;
    }
    arr.swap((i + 1) as usize, high as usize);
    i + 1
}

/// `HUF_simpleQuickSort`.
fn simple_quick_sort(arr: &mut [NodeElt], mut low: i32, mut high: i32) {
    const INSERTION_SORT_THRESHOLD: i32 = 8;
    if high - low < INSERTION_SORT_THRESHOLD {
        insertion_sort(arr, low, high);
        return;
    }
    while low < high {
        let idx = quick_sort_partition(arr, low, high);
        if idx - low < high - idx {
            simple_quick_sort(arr, low, idx - 1);
            low = idx + 1;
        } else {
            simple_quick_sort(arr, idx + 1, high);
            high = idx - 1;
        }
    }
}

/// `HUF_sort`: orders the symbols `[0, max_symbol_value]` by decreasing
/// count into `nodes[0..]` (the table past the sentinel slot).
fn sort(nodes: &mut Nodes, count: &[u32], max_symbol_value: u32) {
    let max_symbol_value1 = max_symbol_value as usize + 1;
    let mut rank_position = [RankPos::default(); RANK_POSITION_TABLE_SIZE];
    for &c in &count[..max_symbol_value1] {
        let lower_rank = rank_index(c) as usize;
        rank_position[lower_rank].base += 1;
    }
    for n in (1..RANK_POSITION_TABLE_SIZE).rev() {
        rank_position[n - 1].base += rank_position[n].base;
        rank_position[n - 1].curr = rank_position[n - 1].base;
    }
    for (n, &c) in count[..max_symbol_value1].iter().enumerate() {
        let r = rank_index(c) as usize + 1;
        let pos = usize::from(rank_position[r].curr);
        rank_position[r].curr += 1;
        let node = nodes.at_mut(pos as i32);
        node.count = c;
        node.byte = n as u8;
    }
    for rank in
        &rank_position[RANK_POSITION_DISTINCT_COUNT_CUTOFF as usize..RANK_POSITION_TABLE_SIZE - 1]
    {
        let bucket_size = i32::from(rank.curr) - i32::from(rank.base);
        let start = usize::from(rank.base);
        if bucket_size > 1 {
            let bucket = &mut nodes.0[1 + start..1 + start + bucket_size as usize];
            simple_quick_sort(bucket, 0, bucket_size - 1);
        }
    }
}

/// `HUF_buildTree`: builds the unlimited-height tree over the sorted nodes
/// and returns the rank of the last non-null symbol.
fn build_tree(nodes: &mut Nodes, max_symbol_value: u32) -> i32 {
    let mut non_null_rank = max_symbol_value as i32;
    while nodes.at(non_null_rank).count == 0 {
        non_null_rank -= 1;
    }
    let mut low_s = non_null_rank;
    let mut node_nb = START_NODE;
    let node_root = node_nb + low_s - 1;
    let mut low_n = node_nb;
    nodes.at_mut(node_nb).count = nodes.at(low_s).count + nodes.at(low_s - 1).count;
    nodes.at_mut(low_s).parent = node_nb as u16;
    nodes.at_mut(low_s - 1).parent = node_nb as u16;
    node_nb += 1;
    low_s -= 2;
    for n in node_nb..=node_root {
        nodes.at_mut(n).count = 1 << 30;
    }
    nodes.at_mut(-1).count = 1 << 31;

    while node_nb <= node_root {
        let n1 = if nodes.at(low_s).count < nodes.at(low_n).count {
            low_s -= 1;
            low_s + 1
        } else {
            low_n += 1;
            low_n - 1
        };
        let n2 = if nodes.at(low_s).count < nodes.at(low_n).count {
            low_s -= 1;
            low_s + 1
        } else {
            low_n += 1;
            low_n - 1
        };
        nodes.at_mut(node_nb).count = nodes.at(n1).count + nodes.at(n2).count;
        nodes.at_mut(n1).parent = node_nb as u16;
        nodes.at_mut(n2).parent = node_nb as u16;
        node_nb += 1;
    }

    nodes.at_mut(node_root).nb_bits = 0;
    for n in (START_NODE..node_root).rev() {
        let parent = i32::from(nodes.at(n).parent);
        nodes.at_mut(n).nb_bits = nodes.at(parent).nb_bits + 1;
    }
    for n in 0..=non_null_rank {
        let parent = i32::from(nodes.at(n).parent);
        nodes.at_mut(n).nb_bits = nodes.at(parent).nb_bits + 1;
    }
    non_null_rank
}

/// `HUF_buildCTable`: builds the table of `count[0..=max_symbol_value]`
/// with codes of at most `max_nb_bits` bits (0 means the default). The
/// returned table's [`HufCTable::table_log`] is the actual maximum length.
///
/// # Errors
///
/// A symbol above 255, or a tree that does not fit the table.
pub(crate) fn build_ctable(
    count: &[u32],
    max_symbol_value: u32,
    mut max_nb_bits: u32,
) -> Result<HufCTable, EntropyError> {
    if max_nb_bits == 0 {
        max_nb_bits = TABLELOG_DEFAULT;
    }
    if max_symbol_value > SYMBOLVALUE_MAX {
        return Err(EntropyError::MaxSymbolValueTooLarge);
    }
    if count.len() <= max_symbol_value as usize {
        return Err(EntropyError::Generic);
    }
    let mut nodes = Nodes::new();
    sort(&mut nodes, count, max_symbol_value);
    let non_null_rank = build_tree(&mut nodes, max_symbol_value);
    max_nb_bits = set_max_height(&mut nodes, non_null_rank as u32, max_nb_bits);
    if max_nb_bits > TABLELOG_MAX {
        return Err(EntropyError::Generic);
    }

    // HUF_buildCTableFromTree
    let alphabet_size = max_symbol_value as usize + 1;
    let mut nb_per_rank = [0u16; TABLELOG_MAX as usize + 1];
    let mut val_per_rank = [0u16; TABLELOG_MAX as usize + 1];
    for n in 0..=non_null_rank {
        nb_per_rank[usize::from(nodes.at(n).nb_bits)] += 1;
    }
    let mut min = 0u16;
    for n in (1..=max_nb_bits as usize).rev() {
        val_per_rank[n] = min;
        min += nb_per_rank[n];
        min >>= 1;
    }
    let mut nb_bits = vec![0u8; alphabet_size];
    let mut value = vec![0u16; alphabet_size];
    for n in 0..alphabet_size {
        let node = nodes.at(n as i32);
        nb_bits[usize::from(node.byte)] = node.nb_bits;
    }
    for n in 0..alphabet_size {
        let rank = usize::from(nb_bits[n]);
        value[n] = val_per_rank[rank];
        val_per_rank[rank] += 1;
    }
    Ok(HufCTable {
        table_log: max_nb_bits,
        nb_bits,
        value,
    })
}

/// `HUF_estimateCompressedSize`: the bytes the symbols take, without the
/// table.
pub(crate) fn estimate_compressed_size(
    ctable: &HufCTable,
    count: &[u32],
    max_symbol_value: u32,
) -> usize {
    let mut nb_bits = 0usize;
    for (s, &c) in count[..=max_symbol_value as usize].iter().enumerate() {
        nb_bits = nb_bits.wrapping_add(ctable.nb_bits(s) as usize * c as usize);
    }
    nb_bits >> 3
}

/// `HIST_count_simple` over weights: the largest count, the largest symbol
/// and the counts.
fn count_weights(weights: &[u8], max_symbol_value: u32) -> (u32, u32, Vec<u32>) {
    let mut count = vec![0u32; max_symbol_value as usize + 1];
    for &weight in weights {
        count[usize::from(weight)] += 1;
    }
    let mut max = max_symbol_value;
    while count[max as usize] == 0 {
        max -= 1;
    }
    let largest = count[..=max as usize].iter().copied().max().unwrap_or(0);
    (largest, max, count)
}

/// `HUF_compressWeights` for `weights`: the byte count the reference
/// returns (0 not compressible, 1 a single repeated weight, otherwise the
/// size) and the FSE-coded bytes.
fn compress_weights(weights: &[u8], dst_size: usize) -> Result<(usize, Vec<u8>), EntropyError> {
    const MAX_FSE_TABLELOG_FOR_HUFF_HEADER: u32 = 6;
    let wt_size = weights.len();
    if wt_size <= 1 {
        return Ok((0, Vec::new()));
    }
    let (max_count, max_symbol_value, count) = count_weights(weights, TABLELOG_MAX);
    if max_count as usize == wt_size {
        return Ok((1, Vec::new()));
    }
    if max_count == 1 {
        return Ok((0, Vec::new()));
    }
    let table_log =
        fse::optimal_table_log(MAX_FSE_TABLELOG_FOR_HUFF_HEADER, wt_size, max_symbol_value);
    let mut norm = vec![0i16; TABLELOG_MAX as usize + 1];
    fse::normalize_count(
        &mut norm,
        table_log,
        &count,
        wt_size,
        max_symbol_value,
        false,
    )?;
    let mut out = fse::write_ncount(&norm, max_symbol_value, table_log)?;
    let ct = fse::CTable::build(&norm, max_symbol_value, table_log)?;
    let capacity = dst_size.saturating_sub(out.len());
    match fse::compress_using_ctable(weights, &ct, 2, capacity) {
        Some(bits) => {
            out.extend_from_slice(&bits);
            Ok((out.len(), out))
        }
        None => Ok((0, Vec::new())),
    }
}

/// `HUF_writeCTable`: serialises the code lengths of `ctable` (weights,
/// FSE-compressed when that is smaller than four bits each).
///
/// # Errors
///
/// A symbol above 255, a destination that is too small, or a table that
/// cannot be represented.
pub(crate) fn write_ctable(
    ctable: &HufCTable,
    max_symbol_value: u32,
    huf_log: u32,
    max_dst_size: usize,
) -> Result<Vec<u8>, EntropyError> {
    if max_symbol_value > SYMBOLVALUE_MAX {
        return Err(EntropyError::MaxSymbolValueTooLarge);
    }
    let max_symbol_value = max_symbol_value as usize;
    let mut bits_to_weight = [0u8; TABLELOG_MAX as usize + 1];
    for (n, weight) in bits_to_weight
        .iter_mut()
        .enumerate()
        .take(huf_log as usize + 1)
        .skip(1)
    {
        *weight = (huf_log as usize + 1 - n) as u8;
    }
    let mut huff_weight = vec![0u8; 256];
    for n in 0..max_symbol_value {
        huff_weight[n] = bits_to_weight[ctable.nb_bits(n) as usize];
    }
    if max_dst_size < 1 {
        return Err(EntropyError::DstSizeTooSmall);
    }
    let (h_size, compressed) =
        compress_weights(&huff_weight[..max_symbol_value], max_dst_size - 1)?;
    if h_size > 1 && h_size < max_symbol_value / 2 {
        let mut out = Vec::with_capacity(h_size + 1);
        out.push(h_size as u8);
        out.extend_from_slice(&compressed);
        return Ok(out);
    }
    if max_symbol_value > 256 - 128 {
        return Err(EntropyError::Generic);
    }
    if (max_symbol_value + 1).div_ceil(2) + 1 > max_dst_size {
        return Err(EntropyError::DstSizeTooSmall);
    }
    let mut out = Vec::with_capacity((max_symbol_value + 1).div_ceil(2) + 1);
    out.push((128 + (max_symbol_value - 1)) as u8);
    huff_weight[max_symbol_value] = 0;
    for n in (0..max_symbol_value).step_by(2) {
        out.push((huff_weight[n] << 4) + huff_weight[n + 1]);
    }
    Ok(out)
}

/// `HUF_compress1X_usingCTable`: one Huffman stream into a destination of
/// `dst_size` bytes; `None` where the reference returns 0.
pub(crate) fn compress1x_using_ctable(
    src: &[u8],
    ctable: &HufCTable,
    dst_size: usize,
) -> Option<Vec<u8>> {
    if dst_size < 8 {
        return None;
    }
    let mut bits = BitWriter::with_capacity(dst_size.min(src.len() + 16));
    for &symbol in src.iter().rev() {
        let symbol = usize::from(symbol);
        bits.add_bits(
            u64::from(ctable.value.get(symbol).copied().unwrap_or(0)),
            ctable.nb_bits(symbol),
        );
    }
    bits.close(dst_size)
}

/// `HUF_compress4X_usingCTable`: a 6-byte jump table of three little-endian
/// stream sizes, then four streams over quarters of the source; `None`
/// where the reference returns 0.
pub(crate) fn compress4x_using_ctable(
    src: &[u8],
    ctable: &HufCTable,
    dst_size: usize,
) -> Option<Vec<u8>> {
    if dst_size < 6 + 1 + 1 + 1 + 8 || src.len() < 12 {
        return None;
    }
    let segment_size = src.len().div_ceil(4);
    let mut out = vec![0u8; 6];
    let mut ip = 0usize;
    for segment in 0..4 {
        let end = if segment == 3 {
            src.len()
        } else {
            ip + segment_size
        };
        let stream = compress1x_using_ctable(&src[ip..end], ctable, dst_size - out.len())?;
        if stream.is_empty() || stream.len() > 65535 {
            return None;
        }
        if segment < 3 {
            out[2 * segment..2 * segment + 2].copy_from_slice(&(stream.len() as u16).to_le_bytes());
        }
        out.extend_from_slice(&stream);
        ip = end;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn histogram(src: &[u8]) -> (Vec<u32>, u32) {
        let mut count = vec![0u32; 256];
        for &b in src {
            count[usize::from(b)] += 1;
        }
        let max = count.iter().rposition(|&c| c != 0).unwrap_or(0) as u32;
        (count, max)
    }

    #[test]
    fn the_cutoff_matches_the_unparenthesised_macro() {
        assert_eq!(RANK_POSITION_LOG_BUCKETS_BEGIN, 158);
        assert_eq!(RANK_POSITION_DISTINCT_COUNT_CUTOFF, 165);
        assert_eq!(rank_index(164), 164);
        assert_eq!(rank_index(165), 165);
        assert_eq!(rank_index(256), 166);
    }

    #[test]
    fn compress_bound_adds_the_table_and_slop() {
        assert_eq!(compress_bound(0), 137);
        assert_eq!(compress_bound(256), 129 + 256 + 1 + 8);
    }

    #[test]
    fn tables_are_prefix_codes_within_the_log() {
        let src: Vec<u8> = (0..5000u32).map(|i| ((i * i / 7) % 40) as u8).collect();
        let (count, max) = histogram(&src);
        let log = optimal_table_log(TABLELOG_DEFAULT, src.len(), max);
        let table = build_ctable(&count, max, log).unwrap();
        assert!(table.table_log() <= log);
        assert_eq!(table.nb_bits.len(), max as usize + 1);
        let kraft: f64 = (0..=max as usize)
            .filter(|&s| table.nb_bits(s) != 0)
            .map(|s| 0.5f64.powi(table.nb_bits(s) as i32))
            .sum();
        assert!((kraft - 1.0).abs() < 1e-9, "{kraft}");
        for (s, &c) in count[..=max as usize].iter().enumerate() {
            assert_eq!(table.nb_bits(s) == 0, c == 0);
        }
        assert_eq!(table.nb_bits(1000), 0);
        assert!(estimate_compressed_size(&table, &count, max) > 0);
    }

    #[test]
    fn height_limiting_keeps_a_valid_code() {
        // Fibonacci-like counts force very long codes without a limit.
        let mut count = vec![0u32; 256];
        let (mut a, mut b) = (1u32, 1u32);
        for slot in count.iter_mut().take(30) {
            *slot = a;
            (a, b) = (b, a + b);
        }
        for limit in [6, 8, 11] {
            let table = build_ctable(&count, 29, limit).unwrap();
            assert!(table.table_log() <= limit);
            let kraft: f64 = (0..30).map(|s| 0.5f64.powi(table.nb_bits(s) as i32)).sum();
            assert!((kraft - 1.0).abs() < 1e-9, "limit {limit}: {kraft}");
        }
    }

    #[test]
    fn a_single_symbol_gets_a_one_bit_code() {
        let mut count = vec![0u32; 256];
        count[7] = 100;
        let table = build_ctable(&count, 7, 11).unwrap();
        assert_eq!(table.nb_bits(7), 1);
    }

    #[test]
    fn build_validates_its_arguments() {
        let count = vec![1u32; 4];
        assert_eq!(
            build_ctable(&count, 300, 11).unwrap_err(),
            EntropyError::MaxSymbolValueTooLarge
        );
        assert_eq!(
            build_ctable(&count, 9, 11).unwrap_err(),
            EntropyError::Generic
        );
        // Zero means the default log.
        assert!(build_ctable(&count, 3, 0).is_ok());
    }

    #[test]
    fn sorting_with_large_counts_uses_the_quicksort_buckets() {
        // Counts above the direct-bucket cutoff share log2 buckets that need
        // the quicksort, including ties.
        let mut count = vec![0u32; 256];
        for (s, slot) in count.iter_mut().enumerate().take(120) {
            *slot = 300 + ((s * 37) % 50) as u32;
        }
        let table = build_ctable(&count, 119, 11).unwrap();
        let kraft: f64 = (0..120).map(|s| 0.5f64.powi(table.nb_bits(s) as i32)).sum();
        assert!((kraft - 1.0).abs() < 1e-9);
    }

    #[test]
    fn streams_encode_with_one_and_four_segments() {
        let src: Vec<u8> = (0..3000u32)
            .map(|i| ((i * 31 + i / 5) % 13) as u8)
            .collect();
        let (count, max) = histogram(&src);
        let table = build_ctable(&count, max, optimal_table_log(11, src.len(), max)).unwrap();
        let one = compress1x_using_ctable(&src, &table, compress_bound(src.len())).unwrap();
        assert!(one.len() < src.len());
        let four = compress4x_using_ctable(&src, &table, compress_bound(src.len())).unwrap();
        assert!(four.len() > 6);
        let jump: usize = (0..3)
            .map(|i| usize::from(u16::from_le_bytes([four[2 * i], four[2 * i + 1]])))
            .sum();
        assert!(jump + 6 < four.len());
        // Failure modes.
        assert!(compress1x_using_ctable(&src, &table, 7).is_none());
        assert!(compress1x_using_ctable(&src, &table, 8).is_none());
        assert!(compress4x_using_ctable(&src[..11], &table, 4096).is_none());
        assert!(compress4x_using_ctable(&src, &table, 17).is_none());
        assert!(compress4x_using_ctable(&src, &table, 100).is_none());
    }

    #[test]
    fn serialised_tables_use_fse_when_it_pays_and_raw_nibbles_otherwise() {
        // Many symbols with few distinct code lengths: FSE-coded weights.
        let src: Vec<u8> = (0..20000u32).map(|i| ((i * 13) % 200) as u8).collect();
        let (count, _) = histogram(&src);
        let table = build_ctable(&count, 255, 11).unwrap();
        let wide = write_ctable(&table, 255, table.table_log(), CTABLE_BOUND);
        assert!(wide.is_err() || wide.unwrap().len() < 130);
        // A small alphabet falls back to four bits per weight.
        let small: Vec<u8> = b"abracadabra alakazam".to_vec();
        let (count, max) = histogram(&small);
        let table = build_ctable(&count, max, 11).unwrap();
        let header = write_ctable(&table, max, table.table_log(), CTABLE_BOUND).unwrap();
        assert!(header[0] >= 128 || usize::from(header[0]) + 1 == header.len());
        assert_eq!(
            write_ctable(&table, 300, 11, 10).unwrap_err(),
            EntropyError::MaxSymbolValueTooLarge
        );
        assert_eq!(
            write_ctable(&table, max, 11, 0).unwrap_err(),
            EntropyError::DstSizeTooSmall
        );
        assert_eq!(
            write_ctable(&table, max, 11, 1).unwrap_err(),
            EntropyError::DstSizeTooSmall
        );
    }

    #[test]
    fn weights_compress_only_when_they_vary() {
        assert_eq!(compress_weights(&[3], 100).unwrap().0, 0);
        assert_eq!(compress_weights(&[3, 3, 3, 3], 100).unwrap().0, 1);
        assert_eq!(compress_weights(&[1, 2, 3, 4], 100).unwrap().0, 0);
        let weights: Vec<u8> = (0..60).map(|i| 1 + (i % 3) as u8).collect();
        let (size, bytes) = compress_weights(&weights, 100).unwrap();
        assert!(size > 1 && bytes.len() == size);
        // Not enough room for the stream.
        assert_eq!(compress_weights(&weights, 3).unwrap().0, 0);
    }
}
