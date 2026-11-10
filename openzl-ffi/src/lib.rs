//! Rust bindings to the vendored OpenZL C library (v0.3.0).
//!
//! This crate is the differential-test and benchmark oracle for `mopenzl`.
//! The raw functions in [`sys`] mirror `shim/shim.c`, which flattens OpenZL's
//! by-value `ZL_Report` into a `size_t` result and an error code. The safe
//! wrappers own every buffer they hand to C.

#![deny(missing_docs)]

use core::ffi::c_int;

pub mod custom_codec;
pub mod enc_backends;
pub mod enc_dict;
pub mod enc_ml;
pub mod enc_numeric;
pub mod enc_structure;
pub mod enc_transformer;
pub mod enc_zstd;
pub mod frame_info;
pub mod graph;
pub mod lz;
pub mod numeric;
pub mod structure;

/// Raw C entry points of `shim/shim.c`.
pub mod sys {
    use core::ffi::{c_int, c_void};

    unsafe extern "C" {
        /// `ZL_compressBound`.
        pub fn mzl_compress_bound(src_size: usize) -> usize;
        /// `ZL_CCtx_compress` with global parameters; see the shim.
        pub fn mzl_compress(
            dst: *mut c_void,
            dst_capacity: usize,
            src: *const c_void,
            src_size: usize,
            level: c_int,
            format_version: c_int,
            content_checksum: c_int,
            compressed_checksum: c_int,
            chunk_size: usize,
            error: *mut c_int,
        ) -> usize;
        /// `ZL_getDecompressedSize`.
        pub fn mzl_decompressed_size(
            src: *const c_void,
            src_size: usize,
            error: *mut c_int,
        ) -> usize;
        /// `XXH3_64bits` as OpenZL vendors it.
        pub fn mzl_xxh3_64(src: *const c_void, src_size: usize) -> u64;
        /// `ZL_decompress`.
        pub fn mzl_decompress(
            dst: *mut c_void,
            dst_capacity: usize,
            src: *const c_void,
            src_size: usize,
            error: *mut c_int,
        ) -> usize;
    }
}

/// `ZL_TernaryParam` values accepted by the checksum parameters.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Ternary {
    /// Keep the library default (`ZL_TernaryParam_auto`, which enables).
    #[default]
    Auto,
    /// `ZL_TernaryParam_enable`.
    Enable,
    /// `ZL_TernaryParam_disable`.
    Disable,
}

impl Ternary {
    const fn raw(self) -> c_int {
        match self {
            Self::Auto => 0,
            Self::Enable => 1,
            Self::Disable => 2,
        }
    }
}

/// Global compression parameters passed to `ZL_CCtx_setParameter`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Params {
    /// `ZL_CParam_compressionLevel`; 0 keeps the default (6).
    pub level: i32,
    /// `ZL_CParam_formatVersion`.
    pub format_version: u32,
    /// `ZL_CParam_contentChecksum`.
    pub content_checksum: Ternary,
    /// `ZL_CParam_compressedChecksum`.
    pub compressed_checksum: Ternary,
    /// Serial segmenter chunk size, or 0 for the default single-chunk graph.
    pub chunk_size: usize,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            level: 0,
            format_version: 27,
            content_checksum: Ternary::Auto,
            compressed_checksum: Ternary::Auto,
            chunk_size: 0,
        }
    }
}

/// An OpenZL error code (`ZL_ErrorCode`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Error(pub i32);

/// `ZL_compressBound(src_size)`.
#[must_use]
pub fn compress_bound(src_size: usize) -> usize {
    // SAFETY: a pure arithmetic function.
    unsafe { sys::mzl_compress_bound(src_size) }
}

/// Compresses `src` with the C library.
///
/// # Errors
///
/// The OpenZL error code when compression fails.
pub fn compress(src: &[u8], params: Params) -> Result<Vec<u8>, Error> {
    let capacity = compress_bound(src.len());
    let mut dst = vec![0u8; capacity];
    let mut error = 0;
    // SAFETY: `dst` is writable for `capacity` bytes and `src` readable for
    // its length; the shim writes the error code through a valid pointer.
    let written = unsafe {
        sys::mzl_compress(
            dst.as_mut_ptr().cast(),
            capacity,
            src.as_ptr().cast(),
            src.len(),
            params.level,
            params.format_version as c_int,
            params.content_checksum.raw(),
            params.compressed_checksum.raw(),
            params.chunk_size,
            &mut error,
        )
    };
    if error != 0 {
        return Err(Error(error));
    }
    dst.truncate(written);
    Ok(dst)
}

/// `XXH3_64bits(src)`, the hash behind every OpenZL checksum.
#[must_use]
pub fn xxh3_64(src: &[u8]) -> u64 {
    // SAFETY: `src` is readable for its length.
    unsafe { sys::mzl_xxh3_64(src.as_ptr().cast(), src.len()) }
}

/// The decompressed size recorded in the frame header.
///
/// # Errors
///
/// The OpenZL error code when the header cannot be read.
pub fn decompressed_size(src: &[u8]) -> Result<usize, Error> {
    let mut error = 0;
    // SAFETY: `src` is readable for its length.
    let size = unsafe { sys::mzl_decompressed_size(src.as_ptr().cast(), src.len(), &mut error) };
    if error != 0 {
        return Err(Error(error));
    }
    Ok(size)
}

/// Decompresses one frame with the C library.
///
/// # Errors
///
/// The OpenZL error code when the frame is invalid.
pub fn decompress(src: &[u8]) -> Result<Vec<u8>, Error> {
    let capacity = decompressed_size(src)?;
    let mut dst = vec![0u8; capacity];
    let mut error = 0;
    // SAFETY: `dst` is writable for `capacity` bytes and `src` readable for
    // its length.
    let written = unsafe {
        sys::mzl_decompress(
            dst.as_mut_ptr().cast(),
            capacity,
            src.as_ptr().cast(),
            src.len(),
            &mut error,
        )
    };
    if error != 0 {
        return Err(Error(error));
    }
    dst.truncate(written);
    Ok(dst)
}

/// AddressSanitizer's default options for every binary that links the
/// oracle: `allocator_may_return_null=1`.
///
/// The reference decoder reserves whatever size a damaged frame declares
/// (`STREAM_reserve`) and reports a failed allocation as an error. Plain
/// `malloc` returns null for a request the host cannot back, so the
/// differential tests see that error; ASan aborts the process instead
/// unless this flag is set. `ASAN_OPTIONS` still overrides it, and Rust
/// allocations that fail keep aborting through `handle_alloc_error`.
#[unsafe(no_mangle)]
extern "C" fn __asan_default_options() -> *const core::ffi::c_char {
    c"allocator_may_return_null=1".as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_c_allocations_may_fail_without_aborting() {
        // SAFETY: the hook returns a pointer to a static C string.
        let options = unsafe { core::ffi::CStr::from_ptr(__asan_default_options()) };
        assert_eq!(options, c"allocator_may_return_null=1");
    }

    #[test]
    fn c_library_round_trips_its_own_frames() {
        let input: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        for chunk_size in [0, 32 << 10] {
            let params = Params {
                chunk_size,
                ..Params::default()
            };
            let frame = compress(&input, params).expect("compress");
            assert!(frame.len() < input.len());
            assert_eq!(decompress(&frame).expect("decompress"), input);
        }
    }

    #[test]
    fn invalid_frames_report_an_error_code() {
        assert!(decompress(b"not a frame").is_err());
    }
}
