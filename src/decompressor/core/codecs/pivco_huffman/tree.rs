//! The PivCo-Huffman coding tree (`common_pivco_kernel.c`).
//!
//! The tree is a function of the Huffman weights alone. Symbols are ranked
//! canonically (descending weight, ascending symbol); runs of `2^k` symbols
//! of one code length collapse into a *flat leaf* decoded by a `k`-bit
//! lookup, and every other symbol is a constant leaf. The finished tree is
//! only rank-indexed arrays.

use alloc::vec::Vec;

/// `ZL_PIVCO_MAX_SYMBOLS`.
pub(super) const MAX_SYMBOLS: usize = 256;
/// `ZL_PIVCO_MAX_TABLE_LOG`.
const MAX_TABLE_LOG: u32 = 12;
/// `ZL_PIVCO_MAX_WEIGHT`.
const MAX_WEIGHT: usize = MAX_TABLE_LOG as usize + 1;

/// A leaf under construction: its symbols, a power-of-two count of them.
type Leaf = Vec<u8>;

/// The tree of one weight table.
#[derive(Debug)]
pub(super) struct Tree {
    /// Rank to symbol; a flat leaf's symbols are a contiguous slice.
    symbols: [u8; MAX_SYMBOLS],
    /// Rank to the flat depth (`log2` of the symbol count) of its leaf.
    depth: [u8; MAX_SYMBOLS],
    /// Rank to its canonical codeword, left-justified into 16 bits.
    codeword: [u16; MAX_SYMBOLS],
    /// Levels with at least one leaf.
    levels: usize,
    /// Present symbols.
    ranks: usize,
}

impl Tree {
    /// Builds the tree of `weights` (`ZL_PivCoHuffmanTree_build`), or `None`
    /// when they are not a complete prefix code: at most 256 weights of at
    /// most 12 whose `sum(2^(w-1))` is a power of two of log at most 12
    /// (`ZL_PivCoHuffman_computeTableLog`).
    pub(super) fn new(weights: &[u8]) -> Option<Self> {
        if weights.is_empty() || weights.len() > MAX_SYMBOLS {
            return None;
        }
        let mut sum = 0u32;
        for &weight in weights {
            if u32::from(weight) > MAX_TABLE_LOG {
                return None;
            }
            sum += (1u32 << weight) >> 1;
        }
        if !sum.is_power_of_two() {
            return None;
        }
        let table_log = sum.trailing_zeros();
        if table_log > MAX_TABLE_LOG {
            return None;
        }
        let table_log = table_log as usize;

        // Buckets of leaves by weight: single symbols in ascending order.
        let mut buckets: [Vec<Leaf>; MAX_WEIGHT + 1] = Default::default();
        for (symbol, &weight) in weights.iter().enumerate() {
            if weight != 0 {
                buckets[usize::from(weight)].push(alloc::vec![symbol as u8]);
            }
        }
        // Collapse runs of equal-weight leaves into flat leaves, shallowest
        // weight first: peel the largest power-of-two suffix while more
        // than two remain. A flat leaf lands in a bucket of larger weight,
        // already processed, so it is never flattened again.
        for weight in (1..=table_log).rev() {
            while buckets[weight].len() > 2 {
                let count = buckets[weight].len();
                let bits = count.ilog2() as usize;
                let split = count - (1 << bits);
                let flat: Leaf = buckets[weight]
                    .drain(split..)
                    .flat_map(|leaf| leaf.into_iter())
                    .collect();
                buckets.get_mut(weight + bits)?.push(flat);
            }
        }
        // The deepest non-empty bucket fixes the number of levels.
        let min_weight = (1..=MAX_WEIGHT).find(|&weight| !buckets[weight].is_empty())?;
        let levels = (table_log + 2).checked_sub(min_weight)?;

        let mut tree = Self {
            symbols: [0; MAX_SYMBOLS],
            depth: [0; MAX_SYMBOLS],
            codeword: [0; MAX_SYMBOLS],
            levels,
            ranks: 0,
        };
        // Canonical codewords: consecutive per leaf, doubling per level.
        let mut codeword = 0u32;
        let mut rank = 0usize;
        for level in 0..levels {
            for leaf in &buckets[table_log + 1 - level] {
                let bits = leaf.len().ilog2() as usize;
                let total = level + bits;
                if rank + leaf.len() > MAX_SYMBOLS || total > 16 {
                    return None;
                }
                for (index, &symbol) in leaf.iter().enumerate() {
                    let word = (codeword << bits) + index as u32;
                    tree.symbols[rank + index] = symbol;
                    tree.depth[rank + index] = bits as u8;
                    tree.codeword[rank + index] = if total == 0 {
                        0
                    } else {
                        (word << (16 - total)) as u16
                    };
                }
                rank += leaf.len();
                codeword += 1;
            }
            codeword <<= 1;
        }
        tree.ranks = rank;
        (rank != 0).then_some(tree)
    }

    /// Number of levels with a leaf.
    pub(super) const fn levels(&self) -> usize {
        self.levels
    }

    /// Number of present symbols.
    pub(super) const fn ranks(&self) -> usize {
        self.ranks
    }

    /// The flat depth of the leaf starting at `first`.
    pub(super) const fn depth(&self, first: usize) -> usize {
        self.depth[first] as usize
    }

    /// Whether `[first, end)` is exactly one leaf.
    pub(super) const fn is_leaf(&self, first: usize, end: usize) -> bool {
        1usize << self.depth[first] == end - first
    }

    /// Whether `[first, end)` is a single-symbol leaf.
    pub(super) const fn is_constant(&self, first: usize, end: usize) -> bool {
        self.is_leaf(first, end) && self.depth[first] == 0
    }

    /// The symbols of the leaf starting at `first`, in flat order.
    pub(super) fn leaf(&self, first: usize) -> &[u8] {
        &self.symbols[first..first + (1 << self.depth[first])]
    }

    /// The first rank of `[first, end)` whose codeword has bit `level` set:
    /// the boundary between the node's left and right children. `None` when
    /// the range does not split there.
    pub(super) fn split(&self, level: usize, first: usize, end: usize) -> Option<usize> {
        let mask = 0x8000u16.checked_shr(level as u32)?;
        (first + 1..end).find(|&rank| self.codeword[rank] & mask != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_must_form_a_complete_code() {
        assert!(Tree::new(&[]).is_none());
        assert!(Tree::new(&[0; 257]).is_none());
        assert!(Tree::new(&[13, 0]).is_none());
        assert!(Tree::new(&[0, 0]).is_none());
        assert!(Tree::new(&[1, 1, 1]).is_none());
        assert!(Tree::new(&[2, 2, 2, 1]).is_none());
        // One present symbol is a constant root.
        let single = Tree::new(&[0, 1]).unwrap();
        assert_eq!((single.ranks(), single.levels()), (1, 1));
        assert!(single.is_constant(0, 1));
        assert_eq!(single.leaf(0), [1]);
    }

    #[test]
    fn equal_weights_collapse_into_flat_leaves() {
        // Four symbols of one code length: a depth-2 flat root.
        let flat = Tree::new(&[1, 1, 1, 1]).unwrap();
        assert_eq!((flat.ranks(), flat.levels(), flat.depth(0)), (4, 1, 2));
        assert!(flat.is_leaf(0, 4));
        assert!(!flat.is_constant(0, 4));
        assert_eq!(flat.leaf(0), [0, 1, 2, 3]);
        // Two symbols stay single leaves under a binary split.
        let pair = Tree::new(&[1, 1]).unwrap();
        assert_eq!((pair.levels(), pair.depth(0), pair.depth(1)), (2, 0, 0));
        assert_eq!(pair.split(0, 0, 2), Some(1));
        assert_eq!(pair.split(0, 0, 1), None);
        assert_eq!(pair.split(40, 0, 2), None);
        assert!(pair.is_constant(0, 1) && pair.is_constant(1, 2));
        // Weights 3,2,2: one 1-bit and two 2-bit symbols.
        let mixed = Tree::new(&[3, 2, 2]).unwrap();
        assert_eq!(mixed.leaf(0), [0]);
        assert_eq!(mixed.split(0, 0, 3), Some(1));
        assert_eq!(mixed.split(1, 1, 3), Some(2));
    }

    #[test]
    fn flat_leaves_move_to_a_shallower_level() {
        // Four 3-bit codes (weight 1) and two 2-bit codes (weight 2) of an
        // 8-cell table: the four collapse into a flat leaf one level above
        // the pair, and take the lowest ranks.
        let tree = Tree::new(&[1, 1, 1, 1, 2, 2]).unwrap();
        assert_eq!((tree.ranks(), tree.levels()), (6, 3));
        assert_eq!(tree.leaf(0), [0, 1, 2, 3]);
        assert_eq!(tree.depth(0), 2);
        assert_eq!(tree.leaf(4), [4]);
        assert_eq!(tree.split(0, 0, 6), Some(4));
    }

    #[test]
    fn flattening_peels_the_highest_symbols_of_a_bucket() {
        // Six 3-bit codes and one 2-bit code: of the six weight-1 symbols the
        // largest power of two (four) at the end of the bucket collapse.
        let tree = Tree::new(&[1, 1, 1, 1, 1, 1, 2]).unwrap();
        assert_eq!((tree.ranks(), tree.levels()), (7, 4));
        assert_eq!(tree.leaf(0), [2, 3, 4, 5]);
        assert_eq!(tree.leaf(4), [6]);
        assert_eq!(tree.leaf(5), [0]);
        assert_eq!(tree.leaf(6), [1]);
    }
}
