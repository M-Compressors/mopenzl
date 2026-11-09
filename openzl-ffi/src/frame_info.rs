//! Test helper of the frame header (`shim/frame_info.c`): the
//! `ZL_FrameInfo_*` getters in one call.

use core::ffi::{c_int, c_void};

use crate::Error;

mod sys {
    use super::{c_int, c_void};

    unsafe extern "C" {
        pub fn mzl_frame_info_create(src: *const c_void, src_size: usize) -> *mut c_void;
        pub fn mzl_frame_info_free(info: *mut c_void);
        pub fn mzl_frame_info_version(info: *const c_void, error: *mut c_int) -> usize;
        pub fn mzl_frame_info_num_outputs(info: *const c_void, error: *mut c_int) -> usize;
        pub fn mzl_frame_info_output_type(
            info: *const c_void,
            id: c_int,
            error: *mut c_int,
        ) -> usize;
        pub fn mzl_frame_info_decompressed_size(
            info: *const c_void,
            id: c_int,
            error: *mut c_int,
        ) -> usize;
        pub fn mzl_frame_info_num_elts(info: *const c_void, id: c_int, error: *mut c_int) -> usize;
        pub fn mzl_frame_info_comment(
            info: *const c_void,
            dst: *mut c_void,
            capacity: usize,
            error: *mut c_int,
        ) -> usize;
        pub fn mzl_frame_info_bundle_id(info: *const c_void, dst: *mut u8) -> c_int;
    }
}

/// One output of a frame, as `ZL_FrameInfo` reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Output {
    /// `ZL_Type`: 1 serial, 2 struct, 4 numeric, 8 string.
    pub ty: usize,
    /// `ZL_FrameInfo_getDecompressedSize`.
    pub size: usize,
    /// `ZL_FrameInfo_getNumElts`, or `None` where C rejects the type.
    pub num_elts: Option<usize>,
}

/// What the `ZL_FrameInfo_*` getters say about a frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameInfo {
    /// `ZL_FrameInfo_getFormatVersion`.
    pub version: usize,
    /// Every output, in order.
    pub outputs: Vec<Output>,
    /// `ZL_FrameInfo_getComment`; empty when there is none.
    pub comment: Vec<u8>,
    /// `ZL_FrameInfo_getBundleID`.
    pub bundle_id: Option<[u8; 32]>,
}

struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from `mzl_frame_info_create`, is non-null
        // and is freed exactly once, here.
        unsafe { sys::mzl_frame_info_free(self.0) }
    }
}

/// Parses the header of `src` with `ZL_FrameInfo_create` and reads every
/// getter.
///
/// # Errors
///
/// The OpenZL error code of the first getter that fails; `Error(-1)` when
/// the header cannot be created.
pub fn frame_info(src: &[u8]) -> Result<FrameInfo, Error> {
    // SAFETY: `src` is readable for its length.
    let raw = unsafe { sys::mzl_frame_info_create(src.as_ptr().cast(), src.len()) };
    if raw.is_null() {
        return Err(Error(-1));
    }
    let handle = Handle(raw);
    let info = handle.0.cast_const();
    let mut error = 0;
    let checked = |value: usize, error: c_int| {
        if error == 0 {
            Ok(value)
        } else {
            Err(Error(error))
        }
    };
    // SAFETY (all calls below): `info` is a live `ZL_FrameInfo` until
    // `handle` drops, and `error` is writable.
    let version = checked(
        unsafe { sys::mzl_frame_info_version(info, &mut error) },
        error,
    )?;
    let count = checked(
        unsafe { sys::mzl_frame_info_num_outputs(info, &mut error) },
        error,
    )?;
    let mut outputs = Vec::with_capacity(count);
    for id in 0..count as c_int {
        let ty = checked(
            unsafe { sys::mzl_frame_info_output_type(info, id, &mut error) },
            error,
        )?;
        let size = checked(
            unsafe { sys::mzl_frame_info_decompressed_size(info, id, &mut error) },
            error,
        )?;
        let num_elts = checked(
            unsafe { sys::mzl_frame_info_num_elts(info, id, &mut error) },
            error,
        )
        .ok();
        outputs.push(Output { ty, size, num_elts });
    }
    let comment = if version >= 22 {
        let size = checked(
            unsafe { sys::mzl_frame_info_comment(info, core::ptr::null_mut(), 0, &mut error) },
            error,
        )?;
        let mut comment = vec![0u8; size];
        checked(
            unsafe {
                sys::mzl_frame_info_comment(info, comment.as_mut_ptr().cast(), size, &mut error)
            },
            error,
        )?;
        comment
    } else {
        Vec::new()
    };
    let mut id = [0u8; 32];
    // SAFETY: `id` is writable for the 32 bytes the shim copies.
    let bundle_id =
        (unsafe { sys::mzl_frame_info_bundle_id(info, id.as_mut_ptr()) } != 0).then_some(id);
    Ok(FrameInfo {
        version,
        outputs,
        comment,
        bundle_id,
    })
}
