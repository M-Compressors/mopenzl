//! The 16-bit "large" Huffman of `huffman_struct_v2`
//! (`codecs/entropy/encode_huffman_kernel.c`).
//!
//! Zstandard's Huffman is limited to bytes and 128 KB blocks, so OpenZL
//! wrote its own for 16-bit symbols with code lengths up to 20. The table is
//! built from a histogram ([`build_ctable`]); [`encode_using_ctable`] and
//! [`encode_using_ctable_x4`] append length-prefixed bit streams to a
//! destination of limited size.

use alloc::vec;
use alloc::vec::Vec;

use super::bits::BitWriter;
use super::{EntropyError, highbit32};

/// `ZS_kLargeHuffmanMaxTableLog`.
const MAX_TABLE_LOG: i32 = 20;

/// `ZS_Huf16CElt`: the code value and length of one symbol.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Huf16CElt {
    /// The code (the reference names this field `symbol`).
    pub(crate) value: u16,
    /// The code length in bits, 0 for an absent symbol.
    pub(crate) nb_bits: u16,
}

/// `ZS_NodeElt`.
#[derive(Clone, Copy, Debug, Default)]
struct NodeElt {
    count: u32,
    parent: u32,
    symbol: u16,
    nb_bits: u16,
}

/// The node table with the reference's `huffNode[-1]` sentinel slot.
struct Nodes(Vec<NodeElt>);

impl Nodes {
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

/// `ZS_largeHuffmanSetMaxHeight`.
fn set_max_height(nodes: &mut Nodes, last_non_null: i32, max_nb_bits: i32) -> i32 {
    let largest_bits = i32::from(nodes.at(last_non_null).nb_bits);
    if largest_bits <= max_nb_bits {
        return largest_bits;
    }
    let mut total_cost: i32 = 0;
    let base_cost: i32 = 1 << (largest_bits - max_nb_bits);
    let mut node = last_non_null;
    while i32::from(nodes.at(node).nb_bits) > max_nb_bits {
        total_cost += base_cost - (1 << (largest_bits - i32::from(nodes.at(node).nb_bits)));
        nodes.at_mut(node).nb_bits = max_nb_bits as u16;
        node -= 1;
    }
    while i32::from(nodes.at(node).nb_bits) == max_nb_bits {
        node -= 1;
    }
    total_cost >>= largest_bits - max_nb_bits;

    const NO_SYMBOL: u32 = 0xF0F0_F0F0;
    let mut rank_last = [NO_SYMBOL; MAX_TABLE_LOG as usize + 2];
    {
        let mut current_nb_bits = max_nb_bits;
        let mut pos = node;
        while pos >= 0 {
            let bits = i32::from(nodes.at(pos).nb_bits);
            if bits < current_nb_bits {
                current_nb_bits = bits;
                rank_last[(max_nb_bits - current_nb_bits) as usize] = pos as u32;
            }
            pos -= 1;
        }
    }
    while total_cost > 0 {
        let mut n_bits_to_decrease = highbit32(total_cost as u32) as usize + 1;
        while n_bits_to_decrease > 1 && rank_last[n_bits_to_decrease] == NO_SYMBOL {
            n_bits_to_decrease -= 1;
        }
        while n_bits_to_decrease > 1 {
            let high_pos = rank_last[n_bits_to_decrease];
            let low_pos = rank_last[n_bits_to_decrease - 1];
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
        while n_bits_to_decrease <= MAX_TABLE_LOG as usize
            && rank_last[n_bits_to_decrease] == NO_SYMBOL
        {
            n_bits_to_decrease += 1;
        }
        total_cost -= 1 << (n_bits_to_decrease - 1);
        if rank_last[n_bits_to_decrease - 1] == NO_SYMBOL {
            rank_last[n_bits_to_decrease - 1] = rank_last[n_bits_to_decrease];
        }
        nodes.at_mut(rank_last[n_bits_to_decrease] as i32).nb_bits += 1;
        if rank_last[n_bits_to_decrease] == 0 {
            rank_last[n_bits_to_decrease] = NO_SYMBOL;
        } else {
            rank_last[n_bits_to_decrease] -= 1;
            if i32::from(nodes.at(rank_last[n_bits_to_decrease] as i32).nb_bits)
                != max_nb_bits - n_bits_to_decrease as i32
            {
                rank_last[n_bits_to_decrease] = NO_SYMBOL;
            }
        }
    }
    while total_cost < 0 {
        if rank_last[1] == NO_SYMBOL {
            while i32::from(nodes.at(node).nb_bits) == max_nb_bits {
                node -= 1;
            }
            nodes.at_mut(node + 1).nb_bits -= 1;
            rank_last[1] = (node + 1) as u32;
            total_cost += 1;
            continue;
        }
        nodes.at_mut(rank_last[1] as i32 + 1).nb_bits -= 1;
        rank_last[1] += 1;
        total_cost += 1;
    }
    max_nb_bits
}

/// `ZS_largeHuffmanBuildTree`.
fn build_tree(nodes: &mut Nodes, max_symbol_value: u16) -> i32 {
    let mut non_null_rank = i32::from(max_symbol_value);
    while nodes.at(non_null_rank).count == 0 {
        non_null_rank -= 1;
    }
    let start_node = i32::from(max_symbol_value) + 1;
    let mut node_nb = start_node;
    let mut low_singleton = non_null_rank;
    let mut low_node = node_nb;
    let node_root = node_nb + low_singleton - 1;

    nodes.at_mut(node_nb).count = nodes
        .at(low_singleton)
        .count
        .wrapping_add(nodes.at(low_singleton - 1).count);
    nodes.at_mut(low_singleton).parent = node_nb as u32;
    nodes.at_mut(low_singleton - 1).parent = node_nb as u32;
    node_nb += 1;
    low_singleton -= 2;
    for n in node_nb..=node_root {
        nodes.at_mut(n).count = 1 << 30;
    }
    nodes.at_mut(-1).count = 1 << 31;

    while node_nb <= node_root {
        let n1 = if nodes.at(low_singleton).count < nodes.at(low_node).count {
            low_singleton -= 1;
            low_singleton + 1
        } else {
            low_node += 1;
            low_node - 1
        };
        let n2 = if nodes.at(low_singleton).count < nodes.at(low_node).count {
            low_singleton -= 1;
            low_singleton + 1
        } else {
            low_node += 1;
            low_node - 1
        };
        nodes.at_mut(node_nb).count = nodes.at(n1).count.wrapping_add(nodes.at(n2).count);
        nodes.at_mut(n1).parent = node_nb as u32;
        nodes.at_mut(n2).parent = node_nb as u32;
        node_nb += 1;
    }

    nodes.at_mut(node_root).nb_bits = 0;
    for n in (start_node..node_root).rev() {
        let parent = nodes.at(n).parent as i32;
        nodes.at_mut(n).nb_bits = nodes.at(parent).nb_bits + 1;
    }
    for n in 0..=non_null_rank {
        let parent = nodes.at(n).parent as i32;
        nodes.at_mut(n).nb_bits = nodes.at(parent).nb_bits + 1;
    }
    non_null_rank
}

/// `ZS_largeHuffmanBuildCTable`: the table of `histogram[0..=max_symbol_value]`
/// with codes of at most `max_nb_bits` bits (0 means 20), and the table log
/// it ended up with.
///
/// The reference sorts with an MSD radix partition that is stable, so equal
/// counts keep their symbol order; a stable descending sort gives the same
/// order.
///
/// # Errors
///
/// A tree taller than 20 bits.
pub(crate) fn build_ctable(
    histogram: &[u32],
    max_symbol_value: u16,
    mut max_nb_bits: i32,
) -> Result<(Vec<Huf16CElt>, i32), EntropyError> {
    let alphabet_size = usize::from(max_symbol_value) + 1;
    if histogram.len() < alphabet_size {
        return Err(EntropyError::Generic);
    }
    let mut nodes = Nodes(vec![
        NodeElt::default();
        2 * usize::from(max_symbol_value) + 3
    ]);
    if max_nb_bits == 0 {
        max_nb_bits = MAX_TABLE_LOG;
    }
    {
        let alphabet_size_bound = (u64::from(max_symbol_value) + 1)
            .next_power_of_two()
            .trailing_zeros() as i32;
        max_nb_bits = max_nb_bits.min(alphabet_size_bound + 3);
    }

    // ZS_largeHuffmanSort
    for (s, &count) in histogram[..alphabet_size].iter().enumerate() {
        let node = nodes.at_mut(s as i32);
        node.count = count;
        node.symbol = s as u16;
    }
    nodes.0[1..1 + alphabet_size].sort_by_key(|node| core::cmp::Reverse(node.count));

    let non_null_rank = build_tree(&mut nodes, max_symbol_value);
    max_nb_bits = set_max_height(&mut nodes, non_null_rank, max_nb_bits);
    if max_nb_bits > MAX_TABLE_LOG {
        return Err(EntropyError::Generic);
    }

    // ZS_largeHuffmanBuildCTableFromTree
    let mut nb_per_rank = vec![0u16; 1usize << max_nb_bits];
    let mut val_per_rank = vec![0u16; 1usize << max_nb_bits];
    for n in 0..=non_null_rank {
        nb_per_rank[usize::from(nodes.at(n).nb_bits)] += 1;
    }
    let mut min = 0u16;
    for n in (1..=max_nb_bits as usize).rev() {
        val_per_rank[n] = min;
        min = min.wrapping_add(nb_per_rank[n]);
        min >>= 1;
    }
    let mut ctable = vec![Huf16CElt::default(); alphabet_size];
    for n in 0..alphabet_size {
        let node = nodes.at(n as i32);
        ctable[usize::from(node.symbol)].nb_bits = node.nb_bits;
    }
    for elt in &mut ctable {
        let rank = usize::from(elt.nb_bits);
        elt.value = val_per_rank[rank];
        val_per_rank[rank] = val_per_rank[rank].wrapping_add(1);
    }
    Ok((ctable, max_nb_bits))
}

/// The room left in a `ZL_WC`: appends fail like the reference's cursor
/// checks when a block does not fit.
fn encode_block(
    out: &mut Vec<u8>,
    avail: usize,
    src: &[u16],
    ctable: &[Huf16CElt],
) -> Result<(), EntropyError> {
    if avail < 8 {
        return Err(EntropyError::Generic);
    }
    out.extend_from_slice(&(src.len() as u32).to_le_bytes());
    let size_pos = out.len();
    out.extend_from_slice(&[0; 4]);
    let mut bits = BitWriter::with_capacity(avail);
    for &symbol in src.iter().rev() {
        let elt = ctable[usize::from(symbol)];
        bits.add_bits(u64::from(elt.value), u32::from(elt.nb_bits));
    }
    let stream = bits.close(avail - 8).ok_or(EntropyError::Generic)?;
    out.extend_from_slice(&stream);
    out[size_pos..size_pos + 4].copy_from_slice(&(stream.len() as u32).to_le_bytes());
    Ok(())
}

/// `ZS_largeHuffmanEncodeUsingCTable`: appends `[size: u32][stream size: u32]`
/// and one stream to `out`, which may grow by at most `avail` bytes.
///
/// # Errors
///
/// Less than 8 bytes of room, or a stream that does not fit.
pub(crate) fn encode_using_ctable(
    out: &mut Vec<u8>,
    avail: usize,
    src: &[u16],
    ctable: &[Huf16CElt],
) -> Result<(), EntropyError> {
    encode_block(out, avail, src, ctable)
}

/// `ZS_largeHuffmanEncodeUsingCTableX4`: four such blocks over quarters of
/// `src` (the first three are `size / 4 + 1` symbols).
///
/// # Errors
///
/// Any block that does not fit in the `avail` bytes left.
pub(crate) fn encode_using_ctable_x4(
    out: &mut Vec<u8>,
    avail: usize,
    src: &[u16],
    ctable: &[Huf16CElt],
) -> Result<(), EntropyError> {
    let start = out.len();
    let max_chunk_size = src.len() / 4 + 1;
    let mut off = 0usize;
    for _ in 0..4 {
        let chunk_size = max_chunk_size.min(src.len() - off);
        let used = out.len() - start;
        let left = avail.checked_sub(used).ok_or(EntropyError::Generic)?;
        encode_block(out, left, &src[off..off + chunk_size], ctable)?;
        off += chunk_size;
    }
    debug_assert_eq!(off, src.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn histogram(src: &[u16], max: usize) -> Vec<u32> {
        let mut count = vec![0u32; max + 1];
        for &s in src {
            count[usize::from(s)] += 1;
        }
        count
    }

    #[test]
    fn tables_are_prefix_codes() {
        let src: Vec<u16> = (0..6000u32).map(|i| ((i * i) % 300) as u16).collect();
        let count = histogram(&src, 299);
        let (table, log) = build_ctable(&count, 299, 0).unwrap();
        assert!(log <= MAX_TABLE_LOG);
        let kraft: f64 = table
            .iter()
            .filter(|elt| elt.nb_bits != 0)
            .map(|elt| 0.5f64.powi(i32::from(elt.nb_bits)))
            .sum();
        assert!((kraft - 1.0).abs() < 1e-9, "{kraft}");
    }

    #[test]
    fn the_code_length_limit_is_enforced() {
        let mut count = vec![0u32; 30];
        let (mut a, mut b) = (1u32, 1u32);
        for slot in count.iter_mut() {
            *slot = a;
            (a, b) = (b, a.saturating_add(b));
        }
        for limit in [8, 12, 16] {
            let (table, log) = build_ctable(&count, 29, limit).unwrap();
            assert!(log <= limit);
            let kraft: f64 = table
                .iter()
                .map(|e| 0.5f64.powi(i32::from(e.nb_bits)))
                .sum();
            assert!((kraft - 1.0).abs() < 1e-9, "limit {limit}: {kraft}");
        }
        // The default limit is shrunk to the alphabet's log plus three.
        let (_, log) = build_ctable(&count[..4], 3, 0).unwrap();
        assert!(log <= 5);
        assert!(build_ctable(&count[..4], 9, 0).is_err());
    }

    #[test]
    fn blocks_carry_their_sizes() {
        let src: Vec<u16> = (0..2000u32).map(|i| ((i * 7) % 90) as u16).collect();
        let count = histogram(&src, 89);
        let (table, _) = build_ctable(&count, 89, 0).unwrap();
        let mut out = Vec::new();
        encode_using_ctable(&mut out, 2 * src.len() + 32, &src, &table).unwrap();
        assert_eq!(u32::from_le_bytes(out[..4].try_into().unwrap()), 2000);
        let stream = u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize;
        assert_eq!(out.len(), 8 + stream);
        let mut four = Vec::new();
        encode_using_ctable_x4(&mut four, 2 * src.len() + 32, &src, &table).unwrap();
        let mut at = 0usize;
        let mut total = 0usize;
        for _ in 0..4 {
            total += u32::from_le_bytes(four[at..at + 4].try_into().unwrap()) as usize;
            at += 8 + u32::from_le_bytes(four[at + 4..at + 8].try_into().unwrap()) as usize;
        }
        assert_eq!((total, at), (2000, four.len()));
    }

    #[test]
    fn blocks_that_do_not_fit_fail() {
        let src: Vec<u16> = (0..64u16).collect();
        let count = histogram(&src, 63);
        let (table, _) = build_ctable(&count, 63, 0).unwrap();
        let mut out = Vec::new();
        assert_eq!(
            encode_using_ctable(&mut out, 7, &src, &table),
            Err(EntropyError::Generic)
        );
        assert!(encode_using_ctable(&mut out, 20, &src, &table).is_err());
        assert!(encode_using_ctable_x4(&mut Vec::new(), 40, &src, &table).is_err());
    }
}
