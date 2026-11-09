//! Oracle for the partition-bitpack graph's partitioning search
//! (`shim/enc_numeric.c`).

use core::ffi::c_int;

unsafe extern "C" {
    fn mzl_pb_fixed_partition(
        data: *const u8,
        num_elts: usize,
        elt_width: usize,
        optimal: c_int,
        starts: *mut u64,
        bit_cost: *mut u64,
        max_symbol_value: *mut u64,
        bitpack_bits: *mut usize,
        bitpack_cost: *mut usize,
    ) -> usize;
}

/// The result of `PB_fixedPartition`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixedPartition {
    /// The first value of every partition; empty when none was found.
    pub starts: Vec<u64>,
    /// The cost of the partitioning in bits.
    pub bit_cost: u64,
    /// The largest value the histogram covers.
    pub max_symbol_value: u64,
    /// The bits a plain bit-packing of the values needs per element.
    pub bitpack_bits: usize,
    /// The bits a plain bit-packing needs in total.
    pub bitpack_cost: usize,
}

/// Runs the reference's `PB_fixedPartition` over `data` (2 or 4-byte
/// little-endian numbers).
///
/// # Panics
///
/// Panics when `width` is not 2 or 4 or `data` is not a whole number of
/// elements.
#[must_use]
pub fn pb_fixed_partition(data: &[u8], width: usize, optimal: bool) -> FixedPartition {
    assert!(matches!(width, 2 | 4) && data.len().is_multiple_of(width));
    let mut starts = vec![0u64; 256];
    let (mut bit_cost, mut max_symbol_value) = (0u64, 0u64);
    let (mut bitpack_bits, mut bitpack_cost) = (0usize, 0usize);
    // SAFETY: `data` and `starts` outlive the call and are sized as declared
    // (`starts` holds the 256 entries the search can return); the other
    // arguments are valid out-pointers.
    let count = unsafe {
        mzl_pb_fixed_partition(
            data.as_ptr(),
            data.len() / width,
            width,
            c_int::from(optimal),
            starts.as_mut_ptr(),
            &raw mut bit_cost,
            &raw mut max_symbol_value,
            &raw mut bitpack_bits,
            &raw mut bitpack_cost,
        )
    };
    starts.truncate(count);
    FixedPartition {
        starts,
        bit_cost,
        max_symbol_value,
        bitpack_bits,
        bitpack_cost,
    }
}
