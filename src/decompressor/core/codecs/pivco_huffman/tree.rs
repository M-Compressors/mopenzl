//! The PivCo-Huffman coding tree (`common_pivco_kernel.c`).
//!
//! The tree is a function of the Huffman weights alone. Symbols are ranked
//! canonically (descending weight, ascending symbol); runs of `2^k` symbols
//! of one code length collapse into a *flat leaf* decoded by a `k`-bit
//! lookup, and every other symbol is a constant leaf. The finished tree is
//! only rank-indexed arrays.

/// `ZL_PIVCO_MAX_SYMBOLS`.
pub(super) const MAX_SYMBOLS: usize = 256;
/// `ZL_PIVCO_MAX_TABLE_LOG`.
const MAX_TABLE_LOG: u32 = 12;
/// `ZL_PIVCO_MAX_WEIGHT`.
const MAX_WEIGHT: usize = MAX_TABLE_LOG as usize + 1;

/// Most leaves one weight bucket can hold: two remaining symbols and a flat
/// leaf for each of the other weights.
const MAX_RUNS: usize = 16;

/// A leaf under construction: a run of the symbols grouped by weight, of a
/// power-of-two length.
#[derive(Copy, Clone, Default)]
struct Run {
    start: usize,
    len: usize,
}

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
        let mut counts = [0usize; MAX_WEIGHT + 1];
        for &weight in weights {
            if u32::from(weight) > MAX_TABLE_LOG {
                return None;
            }
            sum += (1u32 << weight) >> 1;
            counts[usize::from(weight)] += 1;
        }
        if !sum.is_power_of_two() {
            return None;
        }
        let table_log = sum.trailing_zeros();
        if table_log > MAX_TABLE_LOG {
            return None;
        }
        let table_log = table_log as usize;

        // The symbols grouped by weight, ascending within a weight: every
        // leaf below is a contiguous run of this array.
        let mut next = [0usize; MAX_WEIGHT + 1];
        let mut taken = 0;
        for weight in 1..=MAX_WEIGHT {
            next[weight] = taken;
            taken += counts[weight];
        }
        let first = next;
        let mut grouped = [0u8; MAX_SYMBOLS];
        for (symbol, &weight) in weights.iter().enumerate() {
            if weight != 0 {
                let at = &mut next[usize::from(weight)];
                grouped[*at] = symbol as u8;
                *at += 1;
            }
        }

        // Leaves bucketed by weight. Collapse runs of equal-weight symbols
        // into flat leaves, shallowest weight first: peel the largest
        // power-of-two suffix while more than two remain. A flat leaf lands
        // in a bucket of larger weight, already processed, so it is never
        // flattened again and a bucket holds its own remaining symbols
        // first, then the flat leaves in the order they arrived.
        let mut buckets = [[Run::default(); MAX_RUNS]; MAX_WEIGHT + 1];
        let mut sizes = [0usize; MAX_WEIGHT + 1];
        for weight in (1..=MAX_WEIGHT).rev() {
            let mut len = counts[weight];
            while weight <= table_log && len > 2 {
                let bits = len.ilog2() as usize;
                let split = len - (1 << bits);
                let size = sizes.get_mut(weight + bits)?;
                *buckets[weight + bits].get_mut(*size)? = Run {
                    start: first[weight] + split,
                    len: 1 << bits,
                };
                *size += 1;
                len = split;
            }
            for single in 0..len {
                *buckets[weight].get_mut(sizes[weight])? = Run {
                    start: first[weight] + single,
                    len: 1,
                };
                sizes[weight] += 1;
            }
        }
        // The deepest non-empty bucket fixes the number of levels.
        let min_weight = (1..=MAX_WEIGHT).find(|&weight| sizes[weight] != 0)?;
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
            let weight = table_log + 1 - level;
            for run in &buckets[weight][..sizes[weight]] {
                let bits = run.len.ilog2() as usize;
                let total = level + bits;
                if rank + run.len > MAX_SYMBOLS || total > 16 {
                    return None;
                }
                for index in 0..run.len {
                    let word = (codeword << bits) + index as u32;
                    tree.symbols[rank + index] = grouped[run.start + index];
                    tree.depth[rank + index] = bits as u8;
                    tree.codeword[rank + index] = if total == 0 {
                        0
                    } else {
                        (word << (16 - total)) as u16
                    };
                }
                rank += run.len;
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
    use alloc::vec;
    use alloc::vec::Vec;

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

    /// Symbols, depths, codewords, levels and ranks of a tree.
    type Reference = (Vec<u8>, Vec<u8>, Vec<u16>, usize, usize);

    /// The tree of `weights` by the reference construction with owned leaf
    /// lists: (symbols, depths, codewords, levels, ranks).
    fn reference(weights: &[u8]) -> Option<Reference> {
        let sum: u32 = weights.iter().map(|&w| (1u32 << w) >> 1).sum();
        let table_log = sum.trailing_zeros() as usize;
        let mut buckets: Vec<Vec<Vec<u8>>> = vec![Vec::new(); 14];
        for (symbol, &weight) in weights.iter().enumerate() {
            if weight != 0 {
                buckets[usize::from(weight)].push(vec![symbol as u8]);
            }
        }
        for weight in (1..=table_log).rev() {
            while buckets[weight].len() > 2 {
                let count = buckets[weight].len();
                let bits = count.ilog2() as usize;
                let split = count - (1 << bits);
                let flat: Vec<u8> = buckets[weight].drain(split..).flatten().collect();
                buckets.get_mut(weight + bits)?.push(flat);
            }
        }
        let min_weight = (1..=13).find(|&weight| !buckets[weight].is_empty())?;
        let levels = (table_log + 2).checked_sub(min_weight)?;
        let (mut symbols, mut depths, mut words) =
            (vec![0u8; 256], vec![0u8; 256], vec![0u16; 256]);
        let (mut codeword, mut rank) = (0u32, 0usize);
        for level in 0..levels {
            for leaf in &buckets[table_log + 1 - level] {
                let bits = leaf.len().ilog2() as usize;
                let total = level + bits;
                if rank + leaf.len() > 256 || total > 16 {
                    return None;
                }
                for (index, &symbol) in leaf.iter().enumerate() {
                    let word = (codeword << bits) + index as u32;
                    symbols[rank + index] = symbol;
                    depths[rank + index] = bits as u8;
                    words[rank + index] = if total == 0 {
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
        Some((symbols, depths, words, levels, rank))
    }

    /// Weights of a random complete prefix code over `count` symbols.
    fn random_weights(seed: u64, count: usize) -> Vec<u8> {
        let mut state = seed | 1;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut lengths = vec![0u32];
        while lengths.len() < count {
            let at = (next() % lengths.len() as u64) as usize;
            if lengths[at] >= 11 {
                continue;
            }
            lengths[at] += 1;
            let again = lengths[at];
            lengths.push(again);
        }
        let longest = *lengths.iter().max().unwrap_or(&0);
        let mut weights = vec![0u8; 256];
        let mut slots: Vec<usize> = (0..256).collect();
        for length in lengths {
            let pick = (next() % slots.len() as u64) as usize;
            weights[slots.swap_remove(pick)] = (longest + 1 - length) as u8;
        }
        weights
    }

    #[test]
    fn construction_matches_the_owned_leaf_reference() {
        for seed in 1..200u64 {
            let count = 1 + (seed as usize * 7) % 256;
            let weights = random_weights(seed.wrapping_mul(0x9E37_79B9), count);
            let tree = Tree::new(&weights);
            let want = reference(&weights);
            assert_eq!(tree.is_some(), want.is_some(), "seed {seed}");
            let (Some(tree), Some((symbols, depths, words, levels, ranks))) = (tree, want) else {
                continue;
            };
            assert_eq!(
                (tree.levels(), tree.ranks()),
                (levels, ranks),
                "seed {seed}"
            );
            assert_eq!(&tree.symbols[..], &symbols[..], "seed {seed}");
            assert_eq!(&tree.depth[..], &depths[..], "seed {seed}");
            assert_eq!(&tree.codeword[..], &words[..], "seed {seed}");
        }
    }
}
