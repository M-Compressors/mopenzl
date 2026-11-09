//! [`BundleStore`]: the decoder's source of dictionary bundles
//! (`ZL_FatBundleDictLoader`).

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::core::{Materialized, check_unique, materialize_decode};
use super::{BundleId, DictBundle, DictError, DictId};
use crate::shared::dictwire::{MAX_BUNDLES_PER_STORE, MAX_DICTS_PER_BUNDLE};
use crate::shared::sha256;

/// A bundle whose dictionaries are materialized.
#[derive(Debug)]
pub(crate) struct StoredBundle {
    /// The dictionaries in bundle order.
    pub(crate) dicts: Vec<Arc<Materialized>>,
}

/// A dictionary shared by the bundles that list it.
#[derive(Debug)]
struct Entry {
    hash: [u8; 32],
    dict: Arc<Materialized>,
}

/// The bundles a decompressor can resolve frames against.
///
/// Inserting a bundle materializes its dictionaries once (a Zstandard
/// `DDict`); every decompressor given the store shares them. A dictionary
/// listed by several bundles is materialized once, and holding the same
/// bundle twice is a no-op, as in the reference's fat-bundle loader.
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "zstd")]
/// # {
/// use mopenzl::dict::{BundleStore, Dict, DictBundle};
///
/// let bundle = DictBundle::try_from(vec![Dict::zstd(b"shared history", 3)?])?;
/// let mut store = BundleStore::default();
/// assert_eq!(store.insert(&bundle)?, bundle.id());
/// assert!(store.contains(&bundle.id()));
/// assert_eq!(store.len(), 1);
/// # }
/// # Ok::<(), mopenzl::dict::DictError>(())
/// ```
#[derive(Debug, Default)]
pub struct BundleStore {
    bundles: BTreeMap<BundleId, Arc<StoredBundle>>,
    dicts: BTreeMap<DictId, Entry>,
}

impl BundleStore {
    /// Materializes and stores `bundle`, returning its ID.
    ///
    /// # Errors
    ///
    /// [`DictError::StoreFull`] past 128 bundles or 65 535 dictionaries,
    /// [`DictError::ConflictingId`] when a dictionary reuses the ID of a
    /// different content, [`DictError::NoMaterializer`] for a dictionary that
    /// is not a standard Zstandard one (or without the `zstd` feature) and
    /// [`DictError::Materialization`] when the codec rejects its content. The
    /// store is unchanged on error.
    pub fn insert(&mut self, bundle: &DictBundle) -> Result<BundleId, DictError> {
        if self.bundles.contains_key(&bundle.id) {
            return Ok(bundle.id);
        }
        if self.bundles.len() >= MAX_BUNDLES_PER_STORE {
            return Err(DictError::StoreFull);
        }
        check_unique(&bundle.dicts)?;
        let mut staged: BTreeMap<DictId, Entry> = BTreeMap::new();
        let mut list = Vec::with_capacity(bundle.dicts.len());
        for dict in &bundle.dicts {
            let hash = sha256::digest(dict.content());
            let known = staged.get(&dict.id).or_else(|| self.dicts.get(&dict.id));
            if let Some(entry) = known {
                if entry.hash != hash {
                    return Err(DictError::ConflictingId(dict.id));
                }
                list.push(Arc::clone(&entry.dict));
                continue;
            }
            if self.dicts.len() + staged.len() >= MAX_DICTS_PER_BUNDLE {
                return Err(DictError::StoreFull);
            }
            let materialized = Arc::new(materialize_decode(dict)?);
            list.push(Arc::clone(&materialized));
            staged.insert(
                dict.id,
                Entry {
                    hash,
                    dict: materialized,
                },
            );
        }
        self.dicts.append(&mut staged);
        self.bundles
            .insert(bundle.id, Arc::new(StoredBundle { dicts: list }));
        Ok(bundle.id)
    }

    /// Whether the store holds the bundle `id`.
    #[must_use]
    pub fn contains(&self, id: &BundleId) -> bool {
        self.bundles.contains_key(id)
    }

    /// The number of bundles.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bundles.len()
    }

    /// Whether the store holds no bundle.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bundles.is_empty()
    }

    /// The materialized bundle `id`.
    pub(crate) fn get(&self, id: &BundleId) -> Option<&Arc<StoredBundle>> {
        self.bundles.get(id)
    }
}

impl TryFrom<&DictBundle> for BundleStore {
    type Error = DictError;

    /// A store holding `bundle`.
    ///
    /// # Errors
    ///
    /// As [`BundleStore::insert`].
    fn try_from(bundle: &DictBundle) -> Result<Self, Self::Error> {
        let mut store = Self::default();
        store.insert(bundle)?;
        Ok(store)
    }
}

impl TryFrom<&[u8]> for BundleStore {
    type Error = DictError;

    /// A store holding the fat bundle `bytes`.
    ///
    /// # Errors
    ///
    /// Parsing errors of [`DictBundle`] and the errors of
    /// [`BundleStore::insert`].
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Self::try_from(&DictBundle::try_from(bytes)?)
    }
}
