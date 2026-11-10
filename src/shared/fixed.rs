//! Fixed-size tables on the heap.

use alloc::boxed::Box;
use alloc::vec;

/// A boxed `[value; N]`, allocated directly on the heap (`vec!` reaches
/// `alloc_zeroed` for a zero `value`) rather than built on the stack and
/// moved. A length in the type turns index checks into comparisons against
/// a constant.
pub(crate) fn fixed_table<T: Copy, const N: usize>(value: T) -> Box<[T; N]> {
    match vec![value; N].into_boxed_slice().try_into() {
        Ok(table) => table,
        // Unreachable: the slice has exactly `N` elements.
        Err(_) => Box::new([value; N]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_hold_n_copies_of_the_value() {
        let table: Box<[u16; 4096]> = fixed_table(7);
        assert!(table.iter().all(|&cell| cell == 7));
        let empty: Box<[u32; 0]> = fixed_table(0);
        assert!(empty.is_empty());
    }
}
