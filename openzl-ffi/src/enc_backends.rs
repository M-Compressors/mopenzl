//! Test helpers of the encoder backends package (`shim/enc_backends.c`): the
//! C LZ4 block compressors behind OpenZL's `lz4` node.

use core::ffi::c_void;

mod sys {
    use core::ffi::{c_int, c_void};

    unsafe extern "C" {
        pub fn mzl_lz4_compress_fast(
            src: *const c_void,
            src_size: usize,
            dst: *mut c_void,
            dst_capacity: usize,
            acceleration: c_int,
        ) -> c_int;
        pub fn mzl_lz4_compress_hc(
            src: *const c_void,
            src_size: usize,
            dst: *mut c_void,
            dst_capacity: usize,
            level: c_int,
        ) -> c_int;
        pub fn mzl_lz4_compress_bound(size: c_int) -> c_int;
    }
}

/// The source pointer handed to C for `src`.
///
/// The dangling pointer of an empty slice (address 1) is not usable:
/// `LZ4HC_compress_optimal` derives `src + size - 12` before it looks at the
/// size, which wraps around for that address and starts the parser on
/// unmapped memory. Any real address keeps the bound below the start.
fn source(src: &[u8]) -> *const c_void {
    static EMPTY: u8 = 0;
    if src.is_empty() {
        core::ptr::from_ref(&EMPTY).cast()
    } else {
        src.as_ptr().cast()
    }
}

/// `LZ4_compressBound` (0 above `LZ4_MAX_INPUT_SIZE`).
#[must_use]
pub fn lz4_compress_bound(size: i32) -> i32 {
    // SAFETY: a pure function of its argument.
    unsafe { sys::mzl_lz4_compress_bound(size) }
}

/// `LZ4_compress_fast` of `src` into a buffer of `LZ4_compressBound`
/// bytes: the block, or `None` when C returns 0 or fails.
#[must_use]
pub fn lz4_compress_fast(src: &[u8], acceleration: i32) -> Option<Vec<u8>> {
    let capacity = usize::try_from(lz4_compress_bound(i32::try_from(src.len()).ok()?)).ok()?;
    let mut dst = vec![0u8; capacity];
    // SAFETY: `source(src)` is readable for `src.len()` bytes and `dst`
    // writable for `capacity` bytes.
    let written = unsafe {
        sys::mzl_lz4_compress_fast(
            source(src),
            src.len(),
            dst.as_mut_ptr().cast(),
            capacity,
            acceleration,
        )
    };
    dst.truncate(usize::try_from(written).ok().filter(|&n| n > 0)?);
    Some(dst)
}

/// `LZ4_compress_HC` of `src` into a buffer of `LZ4_compressBound` bytes:
/// the block, or `None` when C returns 0 or fails.
#[must_use]
pub fn lz4_compress_hc(src: &[u8], level: i32) -> Option<Vec<u8>> {
    let capacity = usize::try_from(lz4_compress_bound(i32::try_from(src.len()).ok()?)).ok()?;
    let mut dst = vec![0u8; capacity];
    // SAFETY: `source(src)` is readable for `src.len()` bytes and `dst`
    // writable for `capacity` bytes.
    let written = unsafe {
        sys::mzl_lz4_compress_hc(
            source(src),
            src.len(),
            dst.as_mut_ptr().cast(),
            capacity,
            level,
        )
    };
    dst.truncate(usize::try_from(written).ok().filter(|&n| n > 0)?);
    Some(dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_inputs_compress_to_the_empty_block_at_every_level() {
        // Every HC level, the optimal parser (10-12) included, must see a
        // real source address: the empty slice's dangling one crashed it.
        for level in 1..=12 {
            assert_eq!(lz4_compress_hc(&[], level), Some(vec![0]), "level {level}");
        }
        for acceleration in [1, 2, 65_537] {
            assert_eq!(lz4_compress_fast(&[], acceleration), Some(vec![0]));
        }
    }

    #[test]
    fn non_empty_inputs_keep_their_own_address() {
        let data = [7u8; 3];
        assert_eq!(source(&data), data.as_ptr().cast());
        assert_ne!(source(&[]), core::ptr::dangling::<c_void>());
    }
}
