//! The private half of [`crate::dict`]: error mapping from the wire layer,
//! the uniqueness rule the reference applies to dictionary IDs, and the
//! decoder-side materialization of dictionaries (`ZL_DictLoader_materialize`
//! with the standard Zstandard materializer, `DIZSTD_materializeDDict`).

use alloc::string::ToString;

use super::DictError;
#[cfg(feature = "decompression")]
use super::{Dict, DictId};
use crate::shared::dictwire::WireError;
#[cfg(feature = "decompression")]
use crate::shared::sha256;

impl From<WireError> for DictError {
    fn from(error: WireError) -> Self {
        match error {
            WireError::TooManyDicts(count) => Self::TooManyDicts { count },
            WireError::ContentTooLarge => Self::ContentTooLarge,
            other => Self::Malformed(other.to_string()),
        }
    }
}

/// Two dictionaries with one ID must have one content
/// ("Two different materialized objects cannot have the same ID").
#[cfg(feature = "decompression")]
pub(crate) fn check_unique(dicts: &[Dict]) -> Result<(), DictError> {
    let mut seen: alloc::collections::BTreeMap<DictId, [u8; 32]> =
        alloc::collections::BTreeMap::new();
    for dict in dicts {
        let hash = sha256::digest(dict.content());
        if *seen.entry(dict.id()).or_insert(hash) != hash {
            return Err(DictError::ConflictingId(dict.id()));
        }
    }
    Ok(())
}

/// A dictionary prepared for decoding.
#[cfg(feature = "decompression")]
#[derive(Debug)]
pub(crate) enum Materialized {
    /// A Zstandard `ZSTD_DDict`.
    #[cfg(feature = "zstd")]
    Zstd(mzstd::dictionary::DecodeDictionary),
}

/// Materializes `dict` the way the decoder's standard dict loader does: only
/// the standard Zstandard codec has a materializer.
#[cfg(feature = "decompression")]
pub(crate) fn materialize_decode(dict: &Dict) -> Result<Materialized, DictError> {
    #[cfg(feature = "zstd")]
    if dict.codec() == super::DictCodec::ZSTD {
        let content =
            crate::shared::dictwire::parse_zstd_content(dict.content()).ok_or_else(|| {
                DictError::Materialization("failed to parse trained zstd dict content".into())
            })?;
        let ddict = mzstd::dictionary::DecodeDictionary::new(
            content.raw,
            mzstd::dictionary::DictionaryContent::Auto,
        )
        .map_err(|error| DictError::Materialization(error.to_string()))?;
        return Ok(Materialized::Zstd(ddict));
    }
    Err(DictError::NoMaterializer {
        codec: dict.codec(),
    })
}
