//! Test helpers of the encoder backends package (`shim/enc_backends.c`): the
//! C LZ4 block compressors behind OpenZL's `lz4` node.

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
    // SAFETY: `src` is readable for its length and `dst` writable for
    // `capacity` bytes.
    let written = unsafe {
        sys::mzl_lz4_compress_fast(
            src.as_ptr().cast(),
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
    // SAFETY: `src` is readable for its length and `dst` writable for
    // `capacity` bytes.
    let written = unsafe {
        sys::mzl_lz4_compress_hc(
            src.as_ptr().cast(),
            src.len(),
            dst.as_mut_ptr().cast(),
            capacity,
            level,
        )
    };
    dst.truncate(usize::try_from(written).ok().filter(|&n| n > 0)?);
    Some(dst)
}
