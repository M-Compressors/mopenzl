//! C oracle for a custom serial XOR codec with a one-byte private header.

unsafe extern "C" {
    fn mzl_custom_codec(
        decoding: i32,
        id: u32,
        version: i32,
        src: *const u8,
        size: usize,
        dst: *mut u8,
        cap: usize,
    ) -> usize;
}

/// Encodes or decodes a custom XOR frame. `output_size` bounds the destination.
/// Returns `None` when registration, compression or decompression fails.
pub fn transform(
    decoding: bool,
    id: u32,
    version: i32,
    input: &[u8],
    output_size: usize,
) -> Option<Vec<u8>> {
    let mut output = vec![0; output_size];
    // SAFETY: input is readable for its length, output is writable for its
    // length, and the C shim retains neither buffer after returning.
    let written = unsafe {
        mzl_custom_codec(
            i32::from(decoding),
            id,
            version,
            input.as_ptr(),
            input.len(),
            output.as_mut_ptr(),
            output.len(),
        )
    };
    if written > output.len() {
        return None;
    }
    output.truncate(written);
    Some(output)
}
