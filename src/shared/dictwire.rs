//! Wire formats of dictionaries and dictionary bundles
//! (`dict/dict_constants.h`, `dict/dict.c`, `dict/bundle.c`) and of the
//! Zstandard dictionary content (`codecs/zstd/common_zstd.c`).
//!
//! ```text
//! dictionary  = LE32 0x4944CCDA | id (32) | LE32 materializing codec
//!               | custom flag (1) | LE32 content size | content
//! bundle info = LE32 0x4942CCDA | bundle id (32) | fat flag (1)
//!               | LE32 dictionary count (<= 65535) | dictionary ids (32 each)
//! fat bundle  = bundle info | dictionary * count
//! zstd content = LE32 1 | LE32 compression level | raw dictionary
//! ```
//!
//! A bundle ID is the SHA-256 of the concatenated dictionary IDs; a
//! dictionary ID defaults to the SHA-256 of its content. Parsing follows the
//! reference: bytes after the last dictionary of a fat bundle are ignored.

use alloc::vec::Vec;

use thiserror::Error;

use super::sha256;

/// `ZL_DICT_MAGIC`.
pub(crate) const DICT_MAGIC: u32 = 0x4944_CCDA;
/// `ZL_DICT_HEADER_SIZE`: magic, ID, codec, custom flag and content size.
pub(crate) const DICT_HEADER_SIZE: usize = 4 + 32 + 4 + 1 + 4;
/// `ZL_BUNDLEINFO_MAGIC`.
pub(crate) const BUNDLE_MAGIC: u32 = 0x4942_CCDA;
/// `ZL_BUNDLE_HEADER_SIZE`: magic, ID, fat flag and dictionary count.
pub(crate) const BUNDLE_HEADER_SIZE: usize = 4 + 32 + 1 + 4;
/// `ZL_UNIQUE_ID_SIZE`.
pub(crate) const ID_SIZE: usize = 32;
/// `ZL_MAX_DICTS_PER_BUNDLE`.
pub(crate) const MAX_DICTS_PER_BUNDLE: usize = 0xFFFF;
/// `ZL_MAX_BUNDLES_PER_FATBUNDLE_LOADER`.
#[cfg(feature = "decompression")]
pub(crate) const MAX_BUNDLES_PER_STORE: usize = 128;
/// `ZL_TRAINED_ZSTD_CONTENT_VERSION`.
pub(crate) const ZSTD_CONTENT_VERSION: u32 = 1;
/// `ZL_TRAINED_ZSTD_CONTENT_HEADER_SIZE`: version and level.
pub(crate) const ZSTD_CONTENT_HEADER_SIZE: usize = 8;

/// A 32-byte identifier (`ZL_UniqueID`).
pub(crate) type Id = [u8; ID_SIZE];

/// Why bytes are not a dictionary, a bundle or a Zstandard dictionary.
#[derive(Error, Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum WireError {
    /// Fewer bytes than the fixed header of the structure.
    #[error("the {0} is too small")]
    TooSmall(&'static str),
    /// The leading magic number is wrong.
    #[error("the {0} has an invalid magic number")]
    BadMagic(&'static str),
    /// The declared payload extends past the bytes.
    #[error("the {0} is truncated")]
    Truncated(&'static str),
    /// A bundle declaring more dictionaries than the format allows.
    #[error("{0} dictionaries exceed the limit of 65535 per bundle")]
    TooManyDicts(usize),
    /// A content larger than a 32-bit size field holds.
    #[error("a dictionary content is limited to 4 GiB - 1")]
    ContentTooLarge,
    /// A bundle that is not marked as holding its dictionaries.
    #[error("the bundle is not a fat bundle")]
    NotFat,
    /// A packed dictionary whose ID differs from the bundle's list.
    #[error("dictionary {0} does not match the ID the bundle declares")]
    IdMismatch(usize),
}

/// `LE32` at `at`, which the callers have bounds-checked.
fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// The 32 bytes at `at`, which the callers have bounds-checked.
fn id_at(bytes: &[u8], at: usize) -> Id {
    let mut id = [0u8; ID_SIZE];
    id.copy_from_slice(&bytes[at..at + ID_SIZE]);
    id
}

/// A packed dictionary read in place (`ZL_ParsedDict`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedDict<'a> {
    /// The dictionary ID.
    pub(crate) id: Id,
    /// The codec that materializes it.
    pub(crate) codec: u32,
    /// Whether that codec is a custom one.
    pub(crate) custom: bool,
    /// The content.
    pub(crate) content: &'a [u8],
    /// Bytes the packed dictionary occupies: header plus content.
    pub(crate) packed_size: usize,
}

/// `ZL_Dict_parse`: the dictionary at the front of `bytes`.
pub(crate) fn parse_dict(bytes: &[u8]) -> Result<ParsedDict<'_>, WireError> {
    if bytes.len() < DICT_HEADER_SIZE {
        return Err(WireError::TooSmall("dictionary"));
    }
    if le32(bytes, 0) != DICT_MAGIC {
        return Err(WireError::BadMagic("dictionary"));
    }
    let id = id_at(bytes, 4);
    let codec = le32(bytes, 36);
    let custom = bytes[40] != 0;
    let size = le32(bytes, 41) as usize;
    let packed_size = DICT_HEADER_SIZE
        .checked_add(size)
        .filter(|&total| total <= bytes.len())
        .ok_or(WireError::Truncated("dictionary"))?;
    Ok(ParsedDict {
        id,
        codec,
        custom,
        content: &bytes[DICT_HEADER_SIZE..packed_size],
        packed_size,
    })
}

/// `Dict_pack`: appends a packed dictionary to `out`. An all-zero `id`
/// stands for "derive it" and is replaced by the SHA-256 of `content`.
pub(crate) fn pack_dict(
    out: &mut Vec<u8>,
    id: &Id,
    codec: u32,
    custom: bool,
    content: &[u8],
) -> Result<(), WireError> {
    let size = u32::try_from(content.len()).map_err(|_| WireError::ContentTooLarge)?;
    let id = if *id == [0; ID_SIZE] {
        sha256::digest(content)
    } else {
        *id
    };
    out.reserve(DICT_HEADER_SIZE + content.len());
    out.extend_from_slice(&DICT_MAGIC.to_le_bytes());
    out.extend_from_slice(&id);
    out.extend_from_slice(&codec.to_le_bytes());
    out.push(u8::from(custom));
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(content);
    Ok(())
}

/// A bundle info block read in place (`ZL_BundleInfo`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedBundle<'a> {
    /// The bundle ID as stored.
    pub(crate) id: Id,
    /// Whether the dictionaries follow the info.
    pub(crate) fat: bool,
    /// The dictionary IDs, 32 bytes each.
    pub(crate) dict_ids: &'a [u8],
    /// Bytes the info block occupies.
    pub(crate) packed_size: usize,
}

impl ParsedBundle<'_> {
    /// The number of dictionaries.
    pub(crate) const fn len(&self) -> usize {
        self.dict_ids.len() / ID_SIZE
    }

    /// The ID of dictionary `index`.
    pub(crate) fn dict_id(&self, index: usize) -> Id {
        id_at(self.dict_ids, index * ID_SIZE)
    }
}

/// `ZL_BundleInfo_parse`: the bundle info at the front of `bytes`.
pub(crate) fn parse_bundle_info(bytes: &[u8]) -> Result<ParsedBundle<'_>, WireError> {
    if bytes.len() < BUNDLE_HEADER_SIZE {
        return Err(WireError::TooSmall("bundle"));
    }
    if le32(bytes, 0) != BUNDLE_MAGIC {
        return Err(WireError::BadMagic("bundle"));
    }
    let id = id_at(bytes, 4);
    let fat = bytes[36] != 0;
    let count = le32(bytes, 37) as usize;
    if count > MAX_DICTS_PER_BUNDLE {
        return Err(WireError::TooManyDicts(count));
    }
    let packed_size = BUNDLE_HEADER_SIZE + count * ID_SIZE;
    if bytes.len() < packed_size {
        return Err(WireError::Truncated("bundle"));
    }
    Ok(ParsedBundle {
        id,
        fat,
        dict_ids: &bytes[BUNDLE_HEADER_SIZE..packed_size],
        packed_size,
    })
}

/// `ZL_DictBundle_genBundleID`: the SHA-256 of the dictionary IDs in order.
pub(crate) fn bundle_id<'a>(dict_ids: impl IntoIterator<Item = &'a Id>) -> Id {
    let joined: Vec<u8> = dict_ids.into_iter().flatten().copied().collect();
    sha256::digest(&joined)
}

/// A fat bundle read in place.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedFat<'a> {
    /// The bundle ID as stored.
    pub(crate) id: Id,
    /// The dictionaries in order, each checked against the declared IDs.
    pub(crate) dicts: Vec<ParsedDict<'a>>,
}

/// The fat bundle at the front of `bytes` (`CDictMgr_loadFatBundle` and
/// `ZL_FatBundleDictLoader_loadFatBundle`): the info, then one dictionary
/// per declared ID, each with its ID checked. The stored bundle ID is not
/// recomputed, as in the reference.
pub(crate) fn parse_fat_bundle(bytes: &[u8]) -> Result<ParsedFat<'_>, WireError> {
    let info = parse_bundle_info(bytes)?;
    if !info.fat {
        return Err(WireError::NotFat);
    }
    let mut rest = &bytes[info.packed_size..];
    let mut dicts = Vec::with_capacity(info.len());
    for index in 0..info.len() {
        let dict = parse_dict(rest)?;
        if dict.id != info.dict_id(index) {
            return Err(WireError::IdMismatch(index));
        }
        rest = &rest[dict.packed_size..];
        dicts.push(dict);
    }
    Ok(ParsedFat { id: info.id, dicts })
}

/// `ZL_DictBundle_packFatBundle`: a fat bundle of the packed `dicts`; the
/// bundle ID is derived from their IDs.
pub(crate) fn pack_fat_bundle(dicts: &[ParsedDict<'_>]) -> Result<Vec<u8>, WireError> {
    if dicts.len() > MAX_DICTS_PER_BUNDLE {
        return Err(WireError::TooManyDicts(dicts.len()));
    }
    let id = bundle_id(dicts.iter().map(|dict| &dict.id));
    let total = BUNDLE_HEADER_SIZE
        + dicts
            .iter()
            .map(|dict| ID_SIZE + DICT_HEADER_SIZE + dict.content.len())
            .sum::<usize>();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&BUNDLE_MAGIC.to_le_bytes());
    out.extend_from_slice(&id);
    out.push(1);
    out.extend_from_slice(&(dicts.len() as u32).to_le_bytes());
    for dict in dicts {
        out.extend_from_slice(&dict.id);
    }
    for dict in dicts {
        pack_dict(&mut out, &dict.id, dict.codec, dict.custom, dict.content)?;
    }
    Ok(out)
}

/// The content of a Zstandard dictionary (`ZL_TrainedZstdContentParsed`).
#[cfg(feature = "zstd")]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ZstdContent<'a> {
    /// The level the dictionary is prepared for.
    pub(crate) level: i32,
    /// The dictionary in Zstandard's own format, or raw content.
    pub(crate) raw: &'a [u8],
}

/// `ZL_TrainedZstdContent_parse`; `None` for the reference's `false`.
#[cfg(feature = "zstd")]
pub(crate) fn parse_zstd_content(bytes: &[u8]) -> Option<ZstdContent<'_>> {
    if bytes.len() < ZSTD_CONTENT_HEADER_SIZE || le32(bytes, 0) != ZSTD_CONTENT_VERSION {
        return None;
    }
    Some(ZstdContent {
        level: le32(bytes, 4) as i32,
        raw: &bytes[ZSTD_CONTENT_HEADER_SIZE..],
    })
}

/// `ZL_TrainedZstdContent_pack`.
pub(crate) fn pack_zstd_content(level: i32, raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ZSTD_CONTENT_HEADER_SIZE + raw.len());
    out.extend_from_slice(&ZSTD_CONTENT_VERSION.to_le_bytes());
    out.extend_from_slice(&level.to_le_bytes());
    out.extend_from_slice(raw);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(content: &[u8], codec: u32) -> Vec<u8> {
        let mut out = Vec::new();
        pack_dict(&mut out, &[0; ID_SIZE], codec, false, content).unwrap();
        out
    }

    #[test]
    fn a_packed_dictionary_parses_back() {
        let bytes = sample(b"content", 22);
        let parsed = parse_dict(&bytes).unwrap();
        assert_eq!(parsed.id, sha256::digest(b"content"));
        assert_eq!((parsed.codec, parsed.custom), (22, false));
        assert_eq!(parsed.content, b"content");
        assert_eq!(parsed.packed_size, DICT_HEADER_SIZE + 7);
        // Bytes after the dictionary are not part of it.
        let longer = [&bytes[..], b"tail"].concat();
        assert_eq!(parse_dict(&longer).unwrap(), parsed);
        // An explicit ID is kept; a custom flag round trips.
        let mut explicit = Vec::new();
        pack_dict(&mut explicit, &[7; ID_SIZE], 9, true, b"").unwrap();
        let parsed = parse_dict(&explicit).unwrap();
        assert_eq!((parsed.id, parsed.codec, parsed.custom), ([7; 32], 9, true));
        assert!(parsed.content.is_empty());
    }

    #[test]
    fn malformed_dictionaries_are_rejected() {
        let bytes = sample(b"content", 22);
        assert_eq!(
            parse_dict(&bytes[..DICT_HEADER_SIZE - 1]),
            Err(WireError::TooSmall("dictionary"))
        );
        let mut bad = bytes.clone();
        bad[0] ^= 1;
        assert_eq!(parse_dict(&bad), Err(WireError::BadMagic("dictionary")));
        assert_eq!(
            parse_dict(&bytes[..bytes.len() - 1]),
            Err(WireError::Truncated("dictionary"))
        );
        // A size field near 4 GiB must not overflow the addition.
        let mut huge = bytes.clone();
        huge[41..45].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse_dict(&huge), Err(WireError::Truncated("dictionary")));
    }

    #[test]
    fn bundles_hash_their_dictionary_ids() {
        let a = sample(b"first", 22);
        let b = sample(b"second", 22);
        let dicts = [parse_dict(&a).unwrap(), parse_dict(&b).unwrap()];
        let fat = pack_fat_bundle(&dicts).unwrap();
        let info = parse_bundle_info(&fat).unwrap();
        assert!(info.fat);
        assert_eq!(info.len(), 2);
        assert_eq!(info.dict_id(1), dicts[1].id);
        let expected = bundle_id([&dicts[0].id, &dicts[1].id]);
        assert_eq!(info.id, expected);
        assert_eq!(info.packed_size, BUNDLE_HEADER_SIZE + 64);
        let parsed = parse_fat_bundle(&fat).unwrap();
        assert_eq!(parsed.id, expected);
        assert_eq!(parsed.dicts, dicts);
        // Trailing bytes are ignored, as in the reference.
        let longer = [&fat[..], &[0xAA; 9][..]].concat();
        assert_eq!(parse_fat_bundle(&longer).unwrap(), parsed);
        // An empty bundle is the digest of nothing.
        let empty = pack_fat_bundle(&[]).unwrap();
        assert_eq!(parse_bundle_info(&empty).unwrap().id, sha256::digest(b""));
    }

    #[test]
    fn malformed_bundles_are_rejected() {
        let a = sample(b"first", 22);
        let fat = pack_fat_bundle(&[parse_dict(&a).unwrap()]).unwrap();
        assert_eq!(
            parse_fat_bundle(&fat[..BUNDLE_HEADER_SIZE - 1]),
            Err(WireError::TooSmall("bundle"))
        );
        let mut bad = fat.clone();
        bad[3] ^= 1;
        assert_eq!(parse_fat_bundle(&bad), Err(WireError::BadMagic("bundle")));
        let mut thin = fat.clone();
        thin[36] = 0;
        assert_eq!(parse_fat_bundle(&thin), Err(WireError::NotFat));
        let mut many = fat.clone();
        many[37..41].copy_from_slice(&0x1_0000u32.to_le_bytes());
        assert_eq!(
            parse_bundle_info(&many),
            Err(WireError::TooManyDicts(0x1_0000))
        );
        // The ID array is cut short, then a dictionary is.
        assert_eq!(
            parse_bundle_info(&fat[..BUNDLE_HEADER_SIZE + 31]),
            Err(WireError::Truncated("bundle"))
        );
        assert_eq!(
            parse_fat_bundle(&fat[..fat.len() - 1]),
            Err(WireError::Truncated("dictionary"))
        );
        assert_eq!(
            parse_fat_bundle(&fat[..BUNDLE_HEADER_SIZE + 32]),
            Err(WireError::TooSmall("dictionary"))
        );
        // The declared ID differs from the packed dictionary's.
        let mut mismatch = fat.clone();
        mismatch[BUNDLE_HEADER_SIZE] ^= 1;
        assert_eq!(parse_fat_bundle(&mismatch), Err(WireError::IdMismatch(0)));
        // More dictionaries than the limit cannot be packed.
        let dict = parse_dict(&a).unwrap();
        let too_many = alloc::vec![dict; MAX_DICTS_PER_BUNDLE + 1];
        assert_eq!(
            pack_fat_bundle(&too_many),
            Err(WireError::TooManyDicts(MAX_DICTS_PER_BUNDLE + 1))
        );
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_content_carries_a_level_and_the_raw_dictionary() {
        let packed = pack_zstd_content(-7, b"raw");
        assert_eq!(&packed[..4], &1u32.to_le_bytes());
        let parsed = parse_zstd_content(&packed).unwrap();
        assert_eq!((parsed.level, parsed.raw), (-7, &b"raw"[..]));
        assert_eq!(parse_zstd_content(&packed[..7]), None);
        let mut other = packed.clone();
        other[0] = 2;
        assert_eq!(parse_zstd_content(&other), None);
        // A header alone is an empty dictionary.
        assert_eq!(parse_zstd_content(&packed[..8]).unwrap().raw, b"");
    }

    #[test]
    fn errors_describe_themselves() {
        use alloc::string::ToString;
        for error in [
            WireError::TooSmall("bundle"),
            WireError::BadMagic("bundle"),
            WireError::Truncated("bundle"),
            WireError::TooManyDicts(70_000),
            WireError::ContentTooLarge,
            WireError::NotFat,
            WireError::IdMismatch(3),
        ] {
            assert!(!error.to_string().is_empty());
        }
    }

    #[test]
    fn packing_agrees_with_the_reference_library() {
        let content = b"some dictionary content ".repeat(9);
        for (codec, custom) in [(22, false), (0, true), (u32::MAX, false)] {
            let mut ours = Vec::new();
            pack_dict(&mut ours, &[0; ID_SIZE], codec, custom, &content).unwrap();
            assert_eq!(
                ours,
                openzl_ffi::enc_dict::pack_dict(None, codec, custom, &content)
            );
        }
        let mut explicit = Vec::new();
        pack_dict(&mut explicit, &[9; ID_SIZE], 22, false, b"x").unwrap();
        assert_eq!(
            explicit,
            openzl_ffi::enc_dict::pack_dict(Some([9; 32]), 22, false, b"x")
        );
        let a = sample(b"first", 22);
        let b = sample(&content, 22);
        let fat = pack_fat_bundle(&[parse_dict(&a).unwrap(), parse_dict(&b).unwrap()]).unwrap();
        assert_eq!(fat, openzl_ffi::enc_dict::pack_fat_bundle(&[&a, &b]));
        assert_eq!(
            pack_zstd_content(5, &content),
            openzl_ffi::enc_dict::pack_zstd_content(5, &content)
        );
    }
}
