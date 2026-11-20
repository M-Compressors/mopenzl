//! Test helpers of the encoder backends package (`shim/enc_backends.c`): the
//! C LZ4 block compressors behind OpenZL's `lz4` node, and the bit-packing
//! kernel behind the `bitunpack` decoder.

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
        pub fn mzl_bitpack_encode(
            dst: *mut c_void,
            dst_capacity: usize,
            src: *const c_void,
            nb_elts: usize,
            elt_width: usize,
            nb_bits: c_int,
        ) -> usize;
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

/// `ZS_bitpackEncode`: the `bits` low bits of each little-endian `width`-byte
/// number of `numbers`, packed least significant bit first, exactly as the
/// `bitunpack` decoder's kernel writes them, including for numbers wider
/// than `bits`. `None` for a width other than 1, 2, 4 or 8, more bits than
/// the width holds, or a length that is not a whole number of numbers.
#[must_use]
pub fn bitpack_encode(numbers: &[u8], width: usize, bits: u32) -> Option<Vec<u8>> {
    if !matches!(width, 1 | 2 | 4 | 8) || bits as usize > 8 * width {
        return None;
    }
    if !numbers.len().is_multiple_of(width) {
        return None;
    }
    let count = numbers.len() / width;
    // The kernel reads 32- and 64-bit numbers through typed pointers.
    let mut aligned = vec![0u64; numbers.len().div_ceil(8).max(1)];
    for (word, bytes) in aligned.iter_mut().zip(numbers.chunks(8)) {
        let mut padded = [0u8; 8];
        padded[..bytes.len()].copy_from_slice(bytes);
        *word = u64::from_ne_bytes(padded);
    }
    let size = (count * bits as usize).div_ceil(8);
    let mut dst = vec![0u8; size.max(1)];
    // SAFETY: `aligned` holds `count` numbers of `width` bytes at 8-byte
    // alignment and `dst` is writable for `size` bytes, the kernel's bound;
    // `bits` is at most 64, which the `c_int` holds.
    let written = unsafe {
        sys::mzl_bitpack_encode(
            dst.as_mut_ptr().cast(),
            size,
            aligned.as_ptr().cast(),
            count,
            width,
            bits as core::ffi::c_int,
        )
    };
    if written != size {
        return None;
    }
    dst.truncate(size);
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

    #[test]
    fn bitpack_kernel_packs_valid_and_rejects_invalid_arguments() {
        // 3 bits each: 1, 2, 3, 4 -> 0b1001_1101_0001.
        assert_eq!(bitpack_encode(&[1, 2, 3, 4], 1, 3), Some(vec![0xd1, 0x08]));
        assert_eq!(bitpack_encode(&[], 4, 7), Some(vec![]));
        assert_eq!(bitpack_encode(&[1, 2, 3], 3, 8), None);
        assert_eq!(bitpack_encode(&[1, 2], 1, 9), None);
        assert_eq!(bitpack_encode(&[1, 2, 3], 2, 8), None);
    }
}
