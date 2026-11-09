//! Test oracle of dictionary bundles (`shim/enc_dict.c`): SHA-256, the
//! wire-format packers, a trained-dictionary builder, compression through
//! graphs whose nodes carry dictionary IDs, and decompression with a
//! fat-bundle dict loader.

use core::ffi::{c_int, c_void};

use crate::graph::{self, Output};

mod sys {
    use core::ffi::{c_int, c_void};

    use crate::graph::sys::Decoded;

    unsafe extern "C" {
        pub fn mzl_sha256(data: *const c_void, size: usize, out: *mut u8);
        pub fn mzl_dict_pack(
            id: *const u8,
            codec: u32,
            custom: c_int,
            content: *const c_void,
            content_size: usize,
            dst: *mut c_void,
            dst_capacity: usize,
        ) -> usize;
        pub fn mzl_fat_pack(
            dicts: *const *const c_void,
            sizes: *const usize,
            n: usize,
            dst: *mut c_void,
            dst_capacity: usize,
        ) -> usize;
        pub fn mzl_zstd_content_pack(
            level: c_int,
            raw: *const c_void,
            raw_size: usize,
            dst: *mut c_void,
            dst_capacity: usize,
        ) -> usize;
        pub fn mzl_zdict_train(
            dict: *mut c_void,
            dict_capacity: usize,
            samples: *const c_void,
            sizes: *const usize,
            n: u32,
        ) -> usize;
        pub fn mzl_compress_dict(
            dst: *mut c_void,
            dst_capacity: usize,
            src: *const c_void,
            src_size: usize,
            bundle: *const c_void,
            bundle_size: usize,
            program: *const u32,
            program_len: usize,
            cparams: *const c_int,
            n_cparams: usize,
            error: *mut c_int,
        ) -> usize;
        pub fn mzl_decode_typed_dict(
            src: *const c_void,
            src_size: usize,
            bundles: *const *const c_void,
            bundle_sizes: *const usize,
            n_bundles: usize,
            error: *mut c_int,
        ) -> *mut Decoded;
    }
}

/// `ZL_UniqueID_computeSHA256`.
#[must_use]
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    // SAFETY: `data` is readable for its length and `out` holds 32 bytes.
    unsafe { sys::mzl_sha256(data.as_ptr().cast(), data.len(), out.as_mut_ptr()) };
    out
}

/// `Dict_pack`: a packed dictionary; `id` `None` derives it from the
/// content.
///
/// # Panics
///
/// When the reference refuses the content size.
#[must_use]
pub fn pack_dict(id: Option<[u8; 32]>, codec: u32, custom: bool, content: &[u8]) -> Vec<u8> {
    let mut dst = vec![0u8; 45 + content.len()];
    // SAFETY: `content` is readable for its length, `id` points at 32 bytes
    // when set, and `dst` is writable for its length.
    let written = unsafe {
        sys::mzl_dict_pack(
            id.as_ref().map_or(core::ptr::null(), |id| id.as_ptr()),
            codec,
            c_int::from(custom),
            content.as_ptr().cast(),
            content.len(),
            dst.as_mut_ptr().cast(),
            dst.len(),
        )
    };
    assert_ne!(written, 0, "Dict_pack failed");
    dst.truncate(written);
    dst
}

/// `ZL_DictBundle_packFatBundle` over packed dictionaries.
///
/// # Panics
///
/// When the reference refuses the dictionaries.
#[must_use]
pub fn pack_fat_bundle(dicts: &[&[u8]]) -> Vec<u8> {
    let pointers: Vec<*const c_void> = dicts.iter().map(|dict| dict.as_ptr().cast()).collect();
    let sizes: Vec<usize> = dicts.iter().map(|dict| dict.len()).collect();
    let capacity = 41 + dicts.iter().map(|dict| 32 + dict.len()).sum::<usize>();
    let mut dst = vec![0u8; capacity];
    // SAFETY: every pointer is readable for its size and `dst` is writable
    // for `capacity` bytes.
    let written = unsafe {
        sys::mzl_fat_pack(
            pointers.as_ptr(),
            sizes.as_ptr(),
            dicts.len(),
            dst.as_mut_ptr().cast(),
            capacity,
        )
    };
    assert_ne!(written, 0, "packFatBundle failed");
    dst.truncate(written);
    dst
}

/// `ZL_TrainedZstdContent_pack`.
///
/// # Panics
///
/// Never in practice: the buffer is sized for the content.
#[must_use]
pub fn pack_zstd_content(level: i32, raw: &[u8]) -> Vec<u8> {
    let mut dst = vec![0u8; 8 + raw.len()];
    // SAFETY: `raw` is readable for its length and `dst` is writable for its.
    let written = unsafe {
        sys::mzl_zstd_content_pack(
            level,
            raw.as_ptr().cast(),
            raw.len(),
            dst.as_mut_ptr().cast(),
            dst.len(),
        )
    };
    assert_eq!(written, dst.len());
    dst
}

/// `ZDICT_trainFromBuffer` over `samples`: a Zstandard-format dictionary of
/// at most `capacity` bytes, or `None` when training fails.
#[must_use]
pub fn train_zstd_dictionary(samples: &[&[u8]], capacity: usize) -> Option<Vec<u8>> {
    let flat: Vec<u8> = samples
        .iter()
        .flat_map(|sample| sample.iter().copied())
        .collect();
    let sizes: Vec<usize> = samples.iter().map(|sample| sample.len()).collect();
    let mut dict = vec![0u8; capacity];
    // SAFETY: `flat` holds the concatenated samples whose lengths `sizes`
    // lists, and `dict` is writable for `capacity` bytes.
    let written = unsafe {
        sys::mzl_zdict_train(
            dict.as_mut_ptr().cast(),
            capacity,
            flat.as_ptr().cast(),
            sizes.as_ptr(),
            u32::try_from(samples.len()).ok()?,
        )
    };
    if written == 0 {
        return None;
    }
    dict.truncate(written);
    Some(dict)
}

/// A compression graph for [`compress`]: standard graphs and nodes that may
/// carry a dictionary ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DictGraph {
    /// A registered graph, by `ZL_GraphID`.
    Id(u32),
    /// Node `node` parameterized by integer parameters `ints` and the
    /// dictionary `dict`, whose outputs go to `successors`.
    Node {
        /// The node ID.
        node: u32,
        /// Local integer parameters.
        ints: Vec<(i32, i32)>,
        /// The dictionary ID the node references.
        dict: Option<[u8; 32]>,
        /// One graph per output.
        successors: Vec<DictGraph>,
    },
}

impl DictGraph {
    /// `ZL_GRAPH_STORE`.
    pub const STORE: Self = Self::Id(graph::GRAPH_STORE);

    fn encode(&self, program: &mut Vec<u32>) {
        match self {
            Self::Id(id) => program.extend([0, *id]),
            Self::Node {
                node,
                ints,
                dict,
                successors,
            } => {
                program.extend([1, *node, ints.len() as u32]);
                for &(id, value) in ints {
                    program.extend([id as u32, value as u32]);
                }
                match dict {
                    Some(id) => {
                        program.push(1);
                        program.extend(
                            id.as_chunks::<4>()
                                .0
                                .iter()
                                .map(|word| u32::from_le_bytes(*word)),
                        );
                    }
                    None => program.push(0),
                }
                program.push(successors.len() as u32);
                for successor in successors {
                    successor.encode(program);
                }
            }
        }
    }
}

/// Compresses the serial `input` through `graph` after loading the fat
/// bundle `bundle` (if any) into the compressor, as the reference's flow
/// does: nodes first, then `ZL_Compressor_loadDictBundle`, then the starting
/// graph. `cparams` are `ZL_CParam` pairs; the format version defaults to 27.
///
/// # Errors
///
/// The OpenZL error code of the failing step, or -1 for a graph the shim
/// could not build.
pub fn compress(
    input: &[u8],
    graph: &DictGraph,
    bundle: Option<&[u8]>,
    cparams: &[(i32, i32)],
) -> Result<Vec<u8>, crate::Error> {
    let mut program = Vec::new();
    graph.encode(&mut program);
    let mut flat: Vec<c_int> = Vec::new();
    if !cparams
        .iter()
        .any(|&(id, _)| id == graph::cparam::FORMAT_VERSION)
    {
        flat.extend([graph::cparam::FORMAT_VERSION, 27]);
    }
    flat.extend(cparams.iter().flat_map(|&(id, value)| [id, value]));
    let capacity = crate::compress_bound(input.len()) + 4096 + bundle.map_or(0, <[u8]>::len);
    let mut dst = vec![0u8; capacity];
    let mut error = 0;
    let (bundle_ptr, bundle_len) = bundle.map_or((core::ptr::null(), 0), |bundle| {
        (bundle.as_ptr().cast::<c_void>(), bundle.len())
    });
    // An empty slice's dangling pointer is not a valid source to lend.
    let source = if input.is_empty() {
        [0u8; 1].as_ptr()
    } else {
        input.as_ptr()
    };
    // SAFETY: `input`, `bundle`, `program` and `flat` are readable for their
    // lengths and `dst` is writable for `capacity` bytes.
    let written = unsafe {
        sys::mzl_compress_dict(
            dst.as_mut_ptr().cast(),
            capacity,
            source.cast(),
            input.len(),
            bundle_ptr,
            bundle_len,
            program.as_ptr(),
            program.len(),
            flat.as_ptr(),
            flat.len() / 2,
            &mut error,
        )
    };
    if error != 0 {
        return Err(crate::Error(error));
    }
    dst.truncate(written);
    Ok(dst)
}

/// Decodes every output of `src` with a fat-bundle dict loader holding
/// `bundles`.
///
/// # Errors
///
/// The OpenZL error code: a bundle the loader rejects, or a frame that
/// fails to decode (including a missing bundle).
pub fn decompress(src: &[u8], bundles: &[&[u8]]) -> Result<Vec<Output>, crate::Error> {
    let pointers: Vec<*const c_void> = bundles
        .iter()
        .map(|bundle| bundle.as_ptr().cast())
        .collect();
    let sizes: Vec<usize> = bundles.iter().map(|bundle| bundle.len()).collect();
    let mut error = 0;
    // SAFETY: `src` and every bundle are readable for their lengths.
    let decoded = unsafe {
        sys::mzl_decode_typed_dict(
            src.as_ptr().cast(),
            src.len(),
            pointers.as_ptr(),
            sizes.as_ptr(),
            bundles.len(),
            &mut error,
        )
    };
    if decoded.is_null() {
        return Err(crate::Error(if error == 0 { 1 } else { error }));
    }
    // SAFETY: `decoded` is a live handle; every index is below its count and
    // each pointer is readable for the size reported with it until the
    // handle is freed.
    let outputs = unsafe {
        let count = graph::sys::mzl_decoded_count(decoded);
        let outputs = (0..count)
            .map(|i| {
                let kind = graph::sys::mzl_decoded_type(decoded, i);
                let bytes_len = graph::sys::mzl_decoded_bytes(decoded, i);
                let ptr = graph::sys::mzl_decoded_ptr(decoded, i).cast::<u8>();
                let bytes = if bytes_len == 0 {
                    Vec::new()
                } else {
                    core::slice::from_raw_parts(ptr, bytes_len).to_vec()
                };
                Output {
                    kind,
                    width: graph::sys::mzl_decoded_width(decoded, i),
                    bytes,
                    elts: graph::sys::mzl_decoded_elts(decoded, i),
                    lens: Vec::new(),
                }
            })
            .collect();
        graph::sys::mzl_decoded_free(decoded);
        outputs
    };
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_published_vector() {
        let digest = sha256(b"abc");
        assert_eq!(digest[..4], [0xba, 0x78, 0x16, 0xbf]);
    }

    #[test]
    fn a_trained_dictionary_starts_with_the_zstd_magic() {
        let samples: Vec<Vec<u8>> = (0..400u32)
            .map(|i| {
                format!(
                    "{{\"id\":{i},\"name\":\"user{}\",\"city\":\"Oslo\"}}",
                    i * 7
                )
                .into_bytes()
            })
            .collect();
        let refs: Vec<&[u8]> = samples.iter().map(Vec::as_slice).collect();
        let dict = train_zstd_dictionary(&refs, 2048).expect("training succeeds");
        assert_eq!(dict[..4], 0xEC30_A437u32.to_le_bytes());
        assert!(train_zstd_dictionary(&[], 2048).is_none());
    }
}
