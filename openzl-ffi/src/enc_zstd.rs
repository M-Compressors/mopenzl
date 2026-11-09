//! Test oracle of the Zstandard node (`shim/enc_zstd.c`).

mod sys {
    use core::ffi::c_void;

    unsafe extern "C" {
        pub fn mzl_zstd_node(
            src: *const c_void,
            n_elts: usize,
            width: usize,
            level: i32,
            decompression_level: i32,
            params: *const i32,
            n_params: usize,
            dst: *mut c_void,
            dst_capacity: usize,
        ) -> i64;
    }
}

/// The Zstandard node's output stream (`EI_zstdWithCCtx`): the element width
/// as a varint, then the magicless frame, for `input` read as elements of
/// `width` bytes. `ints` are the node's local integer parameters in order.
///
/// `None` when the reference fails (a refused parameter, a zstd error).
///
/// # Panics
///
/// When `width` is zero or does not divide `input.len()`.
#[must_use]
pub fn zstd_node(
    input: &[u8],
    width: usize,
    level: i32,
    decompression_level: i32,
    ints: &[(i32, i32)],
) -> Option<Vec<u8>> {
    assert!(width > 0 && input.len().is_multiple_of(width));
    let n_elts = input.len() / width;
    let params: Vec<i32> = ints.iter().flat_map(|&(id, value)| [id, value]).collect();
    // `ZSTD_compressBound`, with the reference's extra room for block
    // headers and the width varint.
    let capacity = input.len() + (input.len() >> 8) + 128 + 3 * n_elts + 10;
    let mut dst = vec![0u8; capacity];
    // SAFETY: `input` is readable for `n_elts * width` bytes, `params` for
    // `ints.len()` pairs and `dst` is writable for `capacity` bytes.
    let written = unsafe {
        sys::mzl_zstd_node(
            input.as_ptr().cast(),
            n_elts,
            width,
            level,
            decompression_level,
            params.as_ptr(),
            ints.len(),
            dst.as_mut_ptr().cast(),
            capacity,
        )
    };
    let written = usize::try_from(written).ok()?;
    dst.truncate(written);
    Some(dst)
}
