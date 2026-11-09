//! Typed configurations of the ML selector and clustering graphs.
//!
//! Both graphs read a CBOR configuration the reference's training tools
//! produce. [`MlSelectorConfig`] and [`ClusteringConfig`] wrap it: they parse
//! a blob (`TryFrom<&[u8]>`), can be built in code, and give the blob back, so
//! a configuration trained elsewhere and one assembled here behave the same.
//!
//! # Examples
//!
//! A binary ML selector that always picks the second successor, and a
//! clustering of two numeric tags:
//!
//! ```
//! use mopenzl::DataType;
//! use mopenzl::graph::{ClusteringConfig, GbtModel, GbtNode, MlSelectorConfig};
//!
//! // One forest of one tree: a lone leaf scoring +1 means "class 1".
//! let model = GbtModel {
//!     forests: vec![vec![vec![GbtNode::leaf(1.0)]]],
//!     successors: 2,
//!     features: vec!["nbElts".to_string()],
//! };
//! let config = MlSelectorConfig::try_from(&model)?;
//! assert_eq!(MlSelectorConfig::try_from(config.as_ref())?, config);
//!
//! let clustering = ClusteringConfig::default().with_cluster(DataType::Numeric, 4, 0, 0, [1, 2]);
//! let blob = Vec::from(&clustering);
//! assert_eq!(ClusteringConfig::try_from(&blob[..])?, clustering);
//! # Ok::<(), mopenzl::graph::ConfigError>(())
//! ```

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;

use thiserror::Error;

use crate::DataType;
use crate::compressor::core::data::type_mask;
use crate::compressor::core::graphs::clustering as cluster;
use crate::compressor::core::graphs::ml_selector as ml;

/// Why a configuration was rejected.
#[derive(Error, Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum ConfigError {
    /// The blob is not valid CBOR.
    #[error("malformed configuration: {0}")]
    Malformed(String),
    /// The blob is valid CBOR but not a configuration of this graph.
    #[error("invalid configuration: {0}")]
    Invalid(String),
    /// The model fails the reference's `GBTModel_validate`.
    #[error("invalid model: {0}")]
    Model(String),
}

impl From<ml::ConfigError> for ConfigError {
    fn from(error: ml::ConfigError) -> Self {
        match error {
            ml::ConfigError::Cbor(_) => Self::Malformed(error.to_string()),
            ml::ConfigError::Invalid(_) => Self::Invalid(error.to_string()),
        }
    }
}

impl From<cluster::ConfigError> for ConfigError {
    fn from(error: cluster::ConfigError) -> Self {
        match error {
            cluster::ConfigError::Cbor(_) => Self::Malformed(error.to_string()),
            cluster::ConfigError::Invalid(_) => Self::Invalid(error.to_string()),
        }
    }
}

/// One node of a gradient-boosted decision tree.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct GbtNode {
    /// The feature the node splits on, as an index into the model's
    /// [`features`](GbtModel::features); `-1` for a leaf.
    pub feature: i32,
    /// The split threshold (a feature strictly below it goes left), or the
    /// score of a leaf.
    pub value: f32,
    /// Index of the left child in the tree; it must come after this node.
    pub left: usize,
    /// Index of the right child.
    pub right: usize,
    /// Index of the child taken when the feature is missing or NaN.
    pub missing: usize,
}

impl GbtNode {
    /// A leaf scoring `value`.
    #[must_use]
    pub const fn leaf(value: f32) -> Self {
        Self {
            feature: -1,
            value,
            left: 0,
            right: 0,
            missing: 0,
        }
    }

    /// A split of `feature` at `threshold` with the three children.
    #[must_use]
    pub const fn split(
        feature: i32,
        threshold: f32,
        left: usize,
        right: usize,
        missing: usize,
    ) -> Self {
        Self {
            feature,
            value: threshold,
            left,
            right,
            missing,
        }
    }
}

/// A gradient-boosted-tree classifier over statistics of a numeric stream.
///
/// With one forest the model is binary (two successors; class 1 when the
/// summed leaves are not negative). Otherwise there is one forest per
/// successor and the forest with the highest score wins.
///
/// The statistics (`features`) are those of the reference's integer feature
/// generator: `nbElts`, `eltWidth`, `cardinality`, `cardinality_upper`,
/// `cardinality_lower`, `range_size`, `mean`, `variance`, `stddev`,
/// `skewness` and `kurtosis`. A name outside this list reads as missing.
#[derive(Clone, Debug, PartialEq)]
pub struct GbtModel {
    /// `forests[forest][tree]` is the node list of a tree; node 0 is its root.
    pub forests: Vec<Vec<Vec<GbtNode>>>,
    /// How many successors the model selects among.
    pub successors: usize,
    /// The feature names the nodes' feature indices refer to.
    pub features: Vec<String>,
}

impl GbtModel {
    fn to_core(&self) -> ml::Model {
        ml::Model {
            forests: self
                .forests
                .iter()
                .map(|forest| {
                    forest
                        .iter()
                        .map(|tree| {
                            tree.iter()
                                .map(|node| ml::Node {
                                    feature_idx: node.feature,
                                    value: node.value,
                                    left: node.left,
                                    right: node.right,
                                    missing: node.missing,
                                })
                                .collect()
                        })
                        .collect()
                })
                .collect(),
            generator: ml::FeatureGen::Integer,
            nb_successors: self.successors,
            feature_labels: self
                .features
                .iter()
                .map(|name| name.as_bytes().to_vec())
                .collect(),
        }
    }
}

/// A serialized ML selector configuration: the CBOR blob of a model, checked
/// to load.
///
/// The blob is the graph's MParam. It is decoded once, when the config is
/// built; handing it to a [`GraphBuilder`](super::GraphBuilder) shares the
/// decoded model.
#[derive(Clone)]
pub struct MlSelectorConfig {
    blob: Arc<[u8]>,
    parsed: Arc<ml::Config>,
}

impl core::fmt::Debug for MlSelectorConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MlSelectorConfig")
            .field("bytes", &self.blob.len())
            .finish_non_exhaustive()
    }
}

impl PartialEq for MlSelectorConfig {
    fn eq(&self, other: &Self) -> bool {
        self.blob == other.blob
    }
}

impl TryFrom<&[u8]> for MlSelectorConfig {
    type Error = ConfigError;

    /// Reads a CBOR configuration as the reference's materializer does:
    /// the model is not validated, so it may fail when the graph runs.
    ///
    /// # Errors
    ///
    /// Malformed CBOR, a missing or mistyped field, a count that does not
    /// match its array, or an unknown feature generator.
    fn try_from(blob: &[u8]) -> Result<Self, ConfigError> {
        let parsed = ml::deserialize(blob)?;
        Ok(Self {
            blob: Arc::from(blob),
            parsed: Arc::new(parsed),
        })
    }
}

impl TryFrom<&GbtModel> for MlSelectorConfig {
    type Error = ConfigError;

    /// Validates a model as `ZL_MLSelector_registerGraph` does and serializes
    /// it.
    ///
    /// # Errors
    ///
    /// A model with no forest, a forest count that does not fit the number of
    /// successors, a feature index outside the features, a NaN or infinite
    /// threshold, or a child that does not come after its parent or lies
    /// outside the tree.
    fn try_from(model: &GbtModel) -> Result<Self, ConfigError> {
        let core = model.to_core();
        core.validate()
            .map_err(|message| ConfigError::Model(message.to_string()))?;
        let blob = ml::serialize(&core)?;
        Self::try_from(&blob[..])
    }
}

impl AsRef<[u8]> for MlSelectorConfig {
    /// The CBOR blob.
    fn as_ref(&self) -> &[u8] {
        &self.blob
    }
}

impl MlSelectorConfig {
    /// `ZL_Compressor_buildUntrainedMLSelector`'s model: every forest is one
    /// leaf scoring -1, so the selector always picks the first successor
    /// (class 0), for `successors` successors.
    ///
    /// # Errors
    ///
    /// Fewer than 2 successors, which the reference rejects.
    pub fn untrained(successors: usize) -> Result<Self, ConfigError> {
        let forests = if successors == 2 { 1 } else { successors };
        Self::try_from(&GbtModel {
            forests: (0..forests)
                .map(|_| vec![vec![GbtNode::leaf(-1.0)]])
                .collect(),
            successors,
            features: vec!["placeholder".to_string()],
        })
    }

    /// The decoded configuration, shared with the graph that runs it.
    pub(crate) fn materialized(&self) -> Arc<dyn Any + Send + Sync> {
        Arc::clone(&self.parsed) as Arc<dyn Any + Send + Sync>
    }
}

/// Where inputs of one type and width go, in a [`ClusteringConfig`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Target {
    ty: DataType,
    width: usize,
    successor: usize,
    codec: usize,
}

impl Target {
    fn to_core(self) -> cluster::TypeSuccessor {
        cluster::TypeSuccessor {
            ty: u32::from(type_mask(self.ty)),
            elt_width: self.width,
            successor: self.successor,
            codec: self.codec,
        }
    }
}

/// How the clustering graph groups its inputs.
///
/// Inputs are matched by `(type, element width, tag)`. The width of a serial
/// input is 1 and that of a string input 0. A cluster sends its members, one
/// concatenated stream, to a successor; an input whose tag no cluster lists
/// forms its own cluster, handled by the default of its type or by
/// `compress_generic`.
///
/// `successor` and `codec` are indices into the successors and clustering
/// codecs given to [`GraphBuilder::clustering`](super::GraphBuilder::clustering).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClusteringConfig {
    clusters: Vec<(Target, Vec<i32>)>,
    defaults: Vec<Target>,
}

impl ClusteringConfig {
    /// Adds a cluster of the inputs of `ty` and `width` tagged with one of
    /// `tags`.
    #[must_use]
    pub fn with_cluster(
        mut self,
        ty: DataType,
        width: usize,
        successor: usize,
        codec: usize,
        tags: impl Into<Vec<i32>>,
    ) -> Self {
        let target = Target {
            ty,
            width,
            successor,
            codec,
        };
        self.clusters.push((target, tags.into()));
        self
    }

    /// Adds the default for untagged-by-config inputs of `ty` and `width`.
    #[must_use]
    pub fn with_type_default(
        mut self,
        ty: DataType,
        width: usize,
        successor: usize,
        codec: usize,
    ) -> Self {
        self.defaults.push(Target {
            ty,
            width,
            successor,
            codec,
        });
        self
    }

    fn to_core(&self) -> cluster::Config {
        cluster::Config {
            clusters: self
                .clusters
                .iter()
                .map(|(target, tags)| cluster::Cluster {
                    type_successor: target.to_core(),
                    member_tags: tags.clone(),
                })
                .collect(),
            type_defaults: self
                .defaults
                .iter()
                .map(|target| target.to_core())
                .collect(),
        }
    }

    /// The highest successor and codec indices the configuration uses, as
    /// `(successors needed, codecs needed)`.
    pub(crate) fn needs(&self) -> (usize, usize) {
        self.clusters
            .iter()
            .map(|(target, _)| target)
            .chain(&self.defaults)
            .fold((0, 0), |(s, c), target| {
                (s.max(target.successor + 1), c.max(target.codec + 1))
            })
    }
}

impl From<&ClusteringConfig> for Vec<u8> {
    /// The CBOR blob (`ZL_Clustering_serializeClusteringConfig`).
    fn from(config: &ClusteringConfig) -> Self {
        // Serializing a configuration cannot fail: every value is encodable.
        cluster::serialize(&config.to_core()).unwrap_or_default()
    }
}

impl TryFrom<&[u8]> for ClusteringConfig {
    type Error = ConfigError;

    /// Reads a CBOR configuration.
    ///
    /// # Errors
    ///
    /// Malformed CBOR, a missing or mistyped field, more than 2048 clusters,
    /// a member tag above `i32::MAX`, or a cluster type other than the four
    /// stream types.
    fn try_from(blob: &[u8]) -> Result<Self, ConfigError> {
        let core = cluster::deserialize(blob)?;
        let target = |ts: &cluster::TypeSuccessor| {
            let ty = match ts.ty {
                1 => DataType::Serial,
                2 => DataType::Struct,
                4 => DataType::Numeric,
                8 => DataType::String,
                _ => return Err(ConfigError::Invalid("unknown stream type".to_string())),
            };
            Ok(Target {
                ty,
                width: ts.elt_width,
                successor: ts.successor,
                codec: ts.codec,
            })
        };
        Ok(Self {
            clusters: core
                .clusters
                .iter()
                .map(|c| Ok((target(&c.type_successor)?, c.member_tags.clone())))
                .collect::<Result<_, ConfigError>>()?,
            defaults: core
                .type_defaults
                .iter()
                .map(target)
                .collect::<Result<_, _>>()?,
        })
    }
}
