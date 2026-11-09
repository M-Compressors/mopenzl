//! The dictionary bundle of a compressor (`CDictMgr`, dict/cdictmgr.c, and
//! `CGraph_resolveDictIndices`, cgraph.c).
//!
//! `ZL_Compressor_loadDictBundle` loads one fat bundle per compressor. Each
//! dictionary is materialized by the materializer of a node that references
//! it by ID; only the Zstandard node (codec 22) has one, so a dictionary no
//! such node references cannot be loaded. After the graphs are complete,
//! every node that names a dictionary is resolved to its index in the
//! bundle; a node whose dictionary is missing fails the build.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::{NodeDef, Registry};
use crate::dict::{BundleId, Dict, DictBundle, DictError, DictId};
use crate::shared::sha256;

/// `ZL_StandardTransformID_zstd`, the only codec with a materializer.
const ZSTD_CODEC: u32 = 22;

/// A dictionary prepared for encoding.
#[derive(Debug)]
pub(crate) enum Prepared {
    /// A Zstandard `ZSTD_CDict`.
    #[cfg(feature = "zstd")]
    Zstd(mzstd::dictionary::PreparedDictionary),
}

/// The loaded bundle: its identity and the materialized dictionaries.
#[derive(Debug)]
pub(crate) struct DictState {
    /// The bundle ID written to the frame header.
    pub(crate) id: BundleId,
    /// Dictionary IDs in bundle order.
    pub(crate) ids: Vec<DictId>,
    /// The materialized dictionary of each index.
    pub(crate) prepared: Vec<Arc<Prepared>>,
}

impl DictState {
    /// The dictionary at `index`.
    pub(crate) fn prepared(&self, index: u32) -> Option<&Prepared> {
        self.prepared
            .get(index as usize)
            .map(|prepared| &**prepared)
    }
}

/// Why loading or resolving failed.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum LoadError {
    /// A bundle is already loaded.
    AlreadyLoaded,
    /// The bundle or one of its dictionaries was rejected.
    Dict(DictError),
    /// A node names a dictionary the bundle does not hold.
    NotFound(DictId),
}

impl From<DictError> for LoadError {
    fn from(error: DictError) -> Self {
        Self::Dict(error)
    }
}

/// `EIZSTD_materializeCDict`: `ZSTD_createCDict_advanced` with the level's
/// parameters for the dictionary's size.
#[cfg(feature = "zstd")]
fn materialize(dict: &Dict) -> Result<Arc<Prepared>, DictError> {
    use alloc::string::ToString;
    let parsed = crate::shared::dictwire::parse_zstd_content(dict.content()).ok_or_else(|| {
        DictError::Materialization("failed to parse trained zstd dict content".into())
    })?;
    // `ZSTD_getCParams` clamps the level into range.
    let level = parsed
        .level
        .clamp(mzstd::Level::MIN.get(), mzstd::Level::MAX.get());
    // The clamped level is always valid.
    let level = mzstd::Level::try_from(level).unwrap_or_default();
    let dictionary = mzstd::dictionary::DictionaryBuilder::new(parsed.raw)
        .with_content(mzstd::dictionary::DictionaryContent::Auto)
        .with_level_parameters(level)
        .build()
        .map_err(|error| DictError::Materialization(error.to_string()))?;
    Ok(Arc::new(Prepared::Zstd(dictionary)))
}

/// Without the `zstd` feature no node has a materializer.
#[cfg(not(feature = "zstd"))]
fn materialize(dict: &Dict) -> Result<Arc<Prepared>, DictError> {
    Err(DictError::NoMaterializer {
        codec: dict.codec(),
    })
}

impl Registry {
    /// `ZL_Compressor_loadDictBundle`.
    ///
    /// Every dictionary must be referenced by a Zstandard node already
    /// parameterized with its ID, as `CDictMgr_findMaterializer` requires.
    pub(crate) fn load_bundle(&mut self, bundle: &DictBundle) -> Result<(), LoadError> {
        if self.dicts.is_some() {
            return Err(LoadError::AlreadyLoaded);
        }
        let mut prepared: Vec<Arc<Prepared>> = Vec::with_capacity(bundle.dicts().len());
        let mut cache: BTreeMap<DictId, ([u8; 32], Arc<Prepared>)> = BTreeMap::new();
        for dict in bundle.dicts() {
            let hash = sha256::digest(dict.content());
            if let Some((known, shared)) = cache.get(&dict.id()) {
                if *known != hash {
                    return Err(DictError::ConflictingId(dict.id()).into());
                }
                prepared.push(Arc::clone(shared));
                continue;
            }
            let referenced = !dict.codec().is_custom()
                && self.nodes.iter().any(|node: &NodeDef| {
                    node.dict_id == Some(dict.id()) && node.spec.codec == ZSTD_CODEC
                });
            if !referenced {
                return Err(DictError::NoMaterializer {
                    codec: dict.codec(),
                }
                .into());
            }
            let materialized = materialize(dict)?;
            cache.insert(dict.id(), (hash, Arc::clone(&materialized)));
            prepared.push(materialized);
        }
        self.dicts = Some(DictState {
            id: bundle.id(),
            ids: bundle.dicts().iter().map(Dict::id).collect(),
            prepared,
        });
        Ok(())
    }

    /// `CGraph_resolveDictIndices`: gives every node that names a dictionary
    /// its index in the bundle.
    pub(crate) fn resolve_dict_indices(&mut self) -> Result<(), LoadError> {
        let ids: &[DictId] = self.dicts.as_ref().map_or(&[], |state| &state.ids);
        for node in &mut self.nodes {
            let Some(id) = node.dict_id else {
                node.dict_index = None;
                continue;
            };
            let index = ids
                .iter()
                .position(|candidate| *candidate == id)
                .ok_or(LoadError::NotFound(id))?;
            node.dict_index = Some(index as u32);
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "zstd"))]
mod tests {
    use alloc::vec;

    use super::super::{gid, nodes};
    use super::*;
    use crate::dict::Dict;

    fn zstd_node(registry: &mut Registry, dict: Option<&Dict>) {
        registry.add_node(NodeDef {
            spec: nodes::standard(62).unwrap(),
            params: None,
            dict_id: dict.map(Dict::id),
            dict_index: None,
        });
    }

    fn bundle(dicts: Vec<Dict>) -> DictBundle {
        DictBundle::try_from(dicts).unwrap()
    }

    #[test]
    fn dictionaries_load_once_and_resolve_to_their_index() {
        let (a, b) = (
            Dict::zstd(b"first", 3).unwrap(),
            Dict::zstd(b"second", 3).unwrap(),
        );
        let mut registry = Registry::new(1, gid::STORE);
        zstd_node(&mut registry, Some(&b));
        zstd_node(&mut registry, None);
        zstd_node(&mut registry, Some(&a));
        let both = bundle(vec![a.clone(), b.clone()]);
        registry.load_bundle(&both).unwrap();
        assert_eq!(registry.load_bundle(&both), Err(LoadError::AlreadyLoaded));
        registry.resolve_dict_indices().unwrap();
        let indices: Vec<_> = registry.nodes.iter().map(|node| node.dict_index).collect();
        assert_eq!(indices, [Some(1), None, Some(0)]);
        let state = registry.dicts.as_ref().unwrap();
        assert_eq!(state.id, both.id());
        assert!(state.prepared(1).is_some());
        assert!(state.prepared(2).is_none());
    }

    #[test]
    fn a_repeated_dictionary_is_prepared_once_and_a_conflict_is_refused() {
        let a = Dict::zstd(b"first", 3).unwrap();
        let mut registry = Registry::new(1, gid::STORE);
        zstd_node(&mut registry, Some(&a));
        registry
            .load_bundle(&bundle(vec![a.clone(), a.clone()]))
            .unwrap();
        let state = registry.dicts.as_ref().unwrap();
        assert!(Arc::ptr_eq(&state.prepared[0], &state.prepared[1]));

        let clash = Dict::zstd(b"other", 3).unwrap().with_id(a.id());
        let mut registry = Registry::new(1, gid::STORE);
        zstd_node(&mut registry, Some(&a));
        assert_eq!(
            registry.load_bundle(&bundle(vec![a.clone(), clash])),
            Err(LoadError::Dict(DictError::ConflictingId(a.id())))
        );
    }

    #[test]
    fn contents_the_codec_rejects_fail_to_materialize() {
        let mut broken = 0xEC30_A437u32.to_le_bytes().to_vec();
        broken.extend_from_slice(&[1, 0, 0, 0, 0xff, 0xff]);
        for dict in [
            Dict::zstd(&broken, 3).unwrap(),
            Dict::new(crate::dict::DictCodec::ZSTD, b"no envelope".to_vec()).unwrap(),
        ] {
            let mut registry = Registry::new(1, gid::STORE);
            zstd_node(&mut registry, Some(&dict));
            assert!(matches!(
                registry.load_bundle(&bundle(vec![dict])),
                Err(LoadError::Dict(DictError::Materialization(_)))
            ));
        }
        // Levels outside the range are clamped, not refused.
        for level in [i32::MIN, i32::MAX] {
            let dict = Dict::zstd(b"history", level).unwrap();
            let mut registry = Registry::new(1, gid::STORE);
            zstd_node(&mut registry, Some(&dict));
            registry.load_bundle(&bundle(vec![dict])).unwrap();
        }
    }

    #[test]
    fn unreferenced_custom_and_missing_dictionaries_are_errors() {
        let a = Dict::zstd(b"first", 3).unwrap();
        let mut registry = Registry::new(1, gid::STORE);
        assert!(matches!(
            registry.load_bundle(&bundle(vec![a.clone()])),
            Err(LoadError::Dict(DictError::NoMaterializer { .. }))
        ));
        // A custom-codec dictionary never matches a standard node.
        let custom = Dict::new(crate::dict::DictCodec::from((22, true)), b"x".to_vec()).unwrap();
        zstd_node(&mut registry, Some(&custom));
        assert!(registry.load_bundle(&bundle(vec![custom])).is_err());
        // A node whose dictionary is not in the (empty) bundle.
        let mut registry = Registry::new(1, gid::STORE);
        zstd_node(&mut registry, Some(&a));
        assert_eq!(
            registry.resolve_dict_indices(),
            Err(LoadError::NotFound(a.id()))
        );
        registry.load_bundle(&bundle(vec![])).unwrap();
        assert_eq!(
            registry.resolve_dict_indices(),
            Err(LoadError::NotFound(a.id()))
        );
    }
}
