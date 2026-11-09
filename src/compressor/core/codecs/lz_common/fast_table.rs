//! The single-candidate hash table of `common/fast_table.{h,c}`.

use alloc::vec;
use alloc::vec::Vec;

use super::hash::hash_ptr;

/// A hash table without collision resolution: `2^log` 32-bit positions,
/// keyed by the first `min_match` bytes at a position.
#[derive(Debug)]
pub(crate) struct FastTable {
    table: Vec<u32>,
    log: u32,
    min_match: u32,
}

impl FastTable {
    /// `ZS_FastTable_init`: a zeroed table of `2^log` entries.
    pub(crate) fn new(log: u32, min_match: u32) -> Self {
        Self {
            table: vec![0; 1usize << log],
            log,
            min_match,
        }
    }

    /// The hash of the bytes at `data[at..]`.
    #[inline]
    pub(crate) fn hash(&self, data: &[u8], at: usize) -> usize {
        hash_ptr(data, at, self.log, self.min_match)
    }

    /// `ZS_FastTable_getAndUpdateHC`: the entry at an already computed
    /// `hash`, replaced by `value`.
    #[inline]
    pub(crate) fn get_and_update_hash(&mut self, hash: usize, value: u32) -> u32 {
        core::mem::replace(&mut self.table[hash], value)
    }

    /// `ZS_FastTable_getAndUpdateT`.
    #[cfg(feature = "lz")]
    #[inline]
    pub(crate) fn get_and_update(&mut self, data: &[u8], at: usize, value: u32) -> u32 {
        let hash = self.hash(data, at);
        self.get_and_update_hash(hash, value)
    }

    /// `ZS_FastTable_getAndConditionalUpdateT`: the entry is replaced only
    /// when `condition` holds.
    #[cfg(feature = "lz")]
    #[inline]
    pub(crate) fn get_and_conditional_update(
        &mut self,
        data: &[u8],
        at: usize,
        value: u32,
        condition: bool,
    ) -> u32 {
        let hash = self.hash(data, at);
        let old = self.table[hash];
        if condition {
            self.table[hash] = value;
        }
        old
    }

    /// `ZS_FastTable_putT`.
    #[inline]
    pub(crate) fn put(&mut self, data: &[u8], at: usize, value: u32) {
        let hash = self.hash(data, at);
        self.table[hash] = value;
    }
}

#[cfg(all(test, feature = "lz"))]
mod tests {
    use super::*;

    #[test]
    fn entries_start_empty_and_return_the_previous_value() {
        let data = *b"abcdefghabcdefgh";
        let mut table = FastTable::new(10, 4);
        assert_eq!(table.get_and_update(&data, 0, 5), 0);
        assert_eq!(table.get_and_update(&data, 8, 9), 5);
        table.put(&data, 0, 11);
        assert_eq!(table.get_and_update(&data, 8, 1), 11);
        let hash = table.hash(&data, 0);
        assert_eq!(table.get_and_update_hash(hash, 2), 1);
    }

    #[test]
    fn conditional_updates_keep_the_entry_when_the_condition_fails() {
        let data = *b"abcdefgh";
        let mut table = FastTable::new(8, 4);
        assert_eq!(table.get_and_conditional_update(&data, 0, 3, false), 0);
        assert_eq!(table.get_and_conditional_update(&data, 0, 3, true), 0);
        assert_eq!(table.get_and_conditional_update(&data, 0, 4, false), 3);
        assert_eq!(table.get_and_update(&data, 0, 6), 3);
    }
}
