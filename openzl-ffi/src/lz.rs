//! Test helpers of the LZ family (`shim/lz.c`).

mod sys {
    use core::ffi::c_void;

    unsafe extern "C" {
        pub fn mzl_lz4_decompress_safe(
            src: *const c_void,
            src_size: usize,
            dst: *mut c_void,
            dst_capacity: usize,
        ) -> i64;
    }
}

/// Decodes the LZ4 block `src` with `LZ4_decompress_safe` into a buffer of
/// `capacity` bytes: the decoded bytes, or `None` when C rejects the block.
#[must_use]
pub fn lz4_decompress_safe(src: &[u8], capacity: usize) -> Option<Vec<u8>> {
    let mut dst = vec![0u8; capacity];
    // SAFETY: `src` is readable for its length and `dst` is writable for
    // `capacity` bytes.
    let written = unsafe {
        sys::mzl_lz4_decompress_safe(
            src.as_ptr().cast(),
            src.len(),
            dst.as_mut_ptr().cast(),
            capacity,
        )
    };
    let written = usize::try_from(written).ok()?;
    dst.truncate(written);
    Some(dst)
}
