//! The clustering graph (`ZL_GRAPH_CLUSTERING`, graph 18): groups its inputs
//! by an integer tag and sends each group, concatenated, to a successor.
//!
//! C source: `compress/graphs/generic_clustering_graph.c`
//! (`graph_compressClustered`, `ZL_Clustering_deserializeClusteringConfig`).
//!
//! # Inputs and configuration
//!
//! Every input carries the integer metadata [`TAG_METADATA_ID`] (0): its tag.
//! The configuration, a CBOR blob in the graph's reference parameter
//! [`CONFIG_PARAM_ID`] (315), lists clusters (a type and element width, the
//! tags that belong, a successor index and a clustering-codec index) and
//! per-type defaults. Inputs whose `(type, width, tag)` matches no cluster
//! form one new cluster per distinct tag, handled by the default of their
//! type, or `compress_generic` and the type's concat node without one.
//!
//! ```text
//! configured clusters (config order) | unconfigured clusters (first appearance)
//!   one edge  -> successor
//!   n edges   -> clustering node (concat) -> [sizes -> field_lz,] data -> successor
//! ```
//!
//! The graph's custom graphs are the successors, its custom nodes the
//! clustering codecs. Nodes run cluster by cluster, so the node (and stream)
//! order, hence the frame bytes, follow the cluster order above.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use thiserror::Error;

use super::super::data::{Type, type_mask};
use super::super::error::ExecError;
use super::super::exec::{Edge, GraphCx};
use super::super::params::LocalParams;
use super::super::registry::{GraphFn, GraphId, NodeId, gid};
use crate::shared::cbor::{self, DecoderConfig, Item};

/// `ZL_CLUSTERING_TAG_METADATA_ID`.
pub(crate) const TAG_METADATA_ID: i32 = crate::graph::params::clustering::TAG_METADATA_ID;
/// `ZL_GENERIC_CLUSTERING_CONFIG_ID`.
pub(crate) const CONFIG_PARAM_ID: i32 = crate::graph::params::clustering::CONFIG;
/// `ZL_runtimeNodeInputLimit(ZL_MAX_FORMAT_VERSION)`: most clusters a
/// configuration may list.
const MAX_CLUSTERS: usize = 2048;

/// `ZL_NODE_CONCAT_SERIAL`, `_STRUCT`, `_NUMERIC`, `_STRING`.
const CONCAT_SERIAL: NodeId = NodeId::std(33);
const CONCAT_NUM: NodeId = NodeId::std(34);
const CONCAT_STRUCT: NodeId = NodeId::std(35);
const CONCAT_STRING: NodeId = NodeId::std(36);

/// Why a clustering configuration could not be read or written.
#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub(crate) enum ConfigError {
    /// The CBOR is malformed.
    #[error("malformed CBOR: {0}")]
    Cbor(#[from] cbor::Error),
    /// The CBOR is well formed but not a clustering configuration.
    #[error("invalid clustering configuration: {0}")]
    Invalid(&'static str),
}

/// `ZL_ClusteringConfig_TypeSuccessor`: where inputs of a type and width go.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct TypeSuccessor {
    /// A `ZL_Type` bit (1 serial, 2 struct, 4 numeric, 8 string).
    pub(crate) ty: u32,
    /// The element width (0 for strings).
    pub(crate) elt_width: usize,
    /// Index into the graph's custom graphs.
    pub(crate) successor: usize,
    /// Index into the graph's custom nodes.
    pub(crate) codec: usize,
}

/// `ZL_ClusteringConfig_Cluster`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Cluster {
    /// Type, width and successors of the cluster.
    pub(crate) type_successor: TypeSuccessor,
    /// The tags that belong to the cluster.
    pub(crate) member_tags: Vec<i32>,
}

/// `ZL_ClusteringConfig`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Config {
    /// The configured clusters.
    pub(crate) clusters: Vec<Cluster>,
    /// Defaults for inputs whose tag is not configured.
    pub(crate) type_defaults: Vec<TypeSuccessor>,
}

fn invalid(message: &'static str) -> ConfigError {
    ConfigError::Invalid(message)
}

fn int_field(map: &Item<'_>, key: &str) -> Result<i64, ConfigError> {
    map.map_get_str(key)
        .and_then(Item::as_int64)
        .ok_or(invalid("missing or mistyped integer field"))
}

fn map_field<'i, 'a>(map: &'i Item<'a>, key: &str) -> Result<&'i Item<'a>, ConfigError> {
    map.map_get_str(key)
        .filter(|item| item.as_map().is_some())
        .ok_or(invalid("missing or mistyped map field"))
}

fn array_field<'i, 'a>(map: &'i Item<'a>, key: &str) -> Result<&'i [Item<'a>], ConfigError> {
    map.map_get_str(key)
        .and_then(Item::as_array)
        .ok_or(invalid("missing or mistyped array field"))
}

/// `cbor_deserializeTypeSuccessor`.
fn read_type_successor(map: &Item<'_>) -> Result<TypeSuccessor, ConfigError> {
    if map.as_map().is_none() {
        return Err(invalid("expected a map"));
    }
    Ok(TypeSuccessor {
        ty: int_field(map, "type")? as u32,
        elt_width: int_field(map, "eltWidth")? as usize,
        successor: int_field(map, "successorIdx")? as usize,
        codec: int_field(map, "clusteringCodecIdx")? as usize,
    })
}

/// `ZL_Clustering_deserializeClusteringConfig`.
///
/// # Errors
///
/// Malformed CBOR, a missing or mistyped field, more than 2048 clusters, or a
/// member tag above `i32::MAX`.
pub(crate) fn deserialize(bytes: &[u8]) -> Result<Config, ConfigError> {
    let root = cbor::decode(bytes, &DecoderConfig::STRICT)?;
    if root.as_map().is_none() {
        return Err(invalid("expected a map"));
    }
    let clusters = array_field(&root, "clusters")?;
    if clusters.len() > MAX_CLUSTERS {
        return Err(invalid("too many clusters"));
    }
    let mut config = Config::default();
    for cluster in clusters {
        if cluster.as_map().is_none() {
            return Err(invalid("expected a map"));
        }
        let type_successor = read_type_successor(map_field(cluster, "typeSuccessor")?)?;
        let member_tags = array_field(cluster, "memberTags")?
            .iter()
            .map(|tag| {
                let tag = tag.as_int64().ok_or(invalid("expected an integer"))?;
                if tag > i64::from(i32::MAX) {
                    return Err(invalid("member tag out of range"));
                }
                Ok(tag as i32)
            })
            .collect::<Result<Vec<_>, _>>()?;
        config.clusters.push(Cluster {
            type_successor,
            member_tags,
        });
    }
    for default in array_field(&root, "typeDefaults")? {
        config.type_defaults.push(read_type_successor(default)?);
    }
    Ok(config)
}

fn type_successor_item(ts: &TypeSuccessor) -> Item<'static> {
    Item::map([
        (Item::text("type"), Item::Int(i64::from(ts.ty))),
        (Item::text("eltWidth"), Item::Int(ts.elt_width as i64)),
        (Item::text("successorIdx"), Item::Int(ts.successor as i64)),
        (Item::text("clusteringCodecIdx"), Item::Int(ts.codec as i64)),
    ])
}

/// `ZL_Clustering_serializeClusteringConfig`: the CBOR blob of a
/// configuration.
///
/// # Errors
///
/// None in practice: every configuration is representable.
pub(crate) fn serialize(config: &Config) -> Result<Vec<u8>, ConfigError> {
    let clusters = config.clusters.iter().map(|cluster| {
        Item::map([
            (
                Item::text("typeSuccessor"),
                type_successor_item(&cluster.type_successor),
            ),
            (
                Item::text("memberTags"),
                Item::array(
                    cluster
                        .member_tags
                        .iter()
                        .map(|&tag| Item::Int(i64::from(tag))),
                ),
            ),
        ])
    });
    let root = Item::map([
        (Item::text("clusters"), Item::array(clusters)),
        (
            Item::text("typeDefaults"),
            Item::array(config.type_defaults.iter().map(type_successor_item)),
        ),
    ]);
    Ok(cbor::encode_to_vec(&root)?)
}

/// `TypeWidth`: the type and element width inputs are clustered by.
type TypeWidth = (u32, usize);

/// What `graph_compressClustered` knows about one cluster (`ClusterInfo`).
#[derive(Copy, Clone, Debug)]
struct ClusterInfo {
    n_edges: usize,
    successor: GraphId,
    node: NodeId,
}

/// `ZL_Input_eltWidth`: 0 for strings.
fn elt_width(ty: Type, width: usize) -> usize {
    if ty == Type::String { 0 } else { width }
}

/// `getTagForEdge`: `(type, width, tag)` of an input, which must carry the tag
/// metadata.
fn tag_of(cx: &GraphCx<'_, '_, '_>, edge: Edge) -> Result<(TypeWidth, i32), ExecError> {
    let tag = cx
        .int_metadata(edge, TAG_METADATA_ID)?
        .ok_or(ExecError::Parameter("clustering input has no tag"))?;
    let stream = cx.data(edge)?;
    let type_width = (
        u32::from(type_mask(stream.ty())),
        elt_width(stream.ty(), stream.width()),
    );
    Ok((type_width, tag))
}

/// `getDefaultClusteringCodec`.
fn default_codec(
    (ty, width): TypeWidth,
    defaults: &BTreeMap<TypeWidth, TypeSuccessor>,
    nodes: &[NodeId],
) -> Result<NodeId, ExecError> {
    match defaults.get(&(ty, width)) {
        None => match ty {
            1 => Ok(CONCAT_SERIAL),
            4 => Ok(CONCAT_NUM),
            2 => Ok(CONCAT_STRUCT),
            8 => Ok(CONCAT_STRING),
            _ => Err(ExecError::Parameter("invalid type for an unconfigured tag")),
        },
        Some(entry) => nodes.get(entry.codec).copied().ok_or(ExecError::Parameter(
            "cluster codec index out of range for an unconfigured tag",
        )),
    }
}

/// `getDefaultSuccessor`.
fn default_successor(
    key: TypeWidth,
    defaults: &BTreeMap<TypeWidth, TypeSuccessor>,
    successors: &[GraphId],
) -> Result<GraphId, ExecError> {
    match defaults.get(&key) {
        None => Ok(gid::COMPRESS_GENERIC),
        Some(entry) => successors
            .get(entry.successor)
            .copied()
            .ok_or(ExecError::Parameter(
                "successor index out of range for an unconfigured tag",
            )),
    }
}

/// `graph_getClusteringConfig`: the configuration parameter, as a reference
/// (bytes in a `Vec<u8>`) or a copy.
fn config_of(params: &LocalParams) -> Result<Config, ExecError> {
    let blob = match params.reference(CONFIG_PARAM_ID) {
        Some(object) => object
            .downcast_ref::<Vec<u8>>()
            .map(Vec::as_slice)
            .ok_or(ExecError::Parameter("clustering config is not bytes"))?,
        None => params
            .copy(CONFIG_PARAM_ID)
            .ok_or(ExecError::Parameter("clustering config parameter missing"))?,
    };
    deserialize(blob).map_err(|_| ExecError::Parameter("invalid clustering config"))
}

/// `graph_compressClustered`.
#[derive(Debug)]
pub(crate) struct Clustering;

/// See [`Clustering`].
pub(crate) static CLUSTERING: Clustering = Clustering;

impl GraphFn for Clustering {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let config = config_of(cx.eff.params)?;
        let successors = cx.eff.custom_graphs;
        let nodes = cx.eff.custom_nodes;

        // `validateClusteredConfig`.
        let out_of_range = |ts: &TypeSuccessor| ts.successor >= successors.len();
        if config
            .clusters
            .iter()
            .any(|c| out_of_range(&c.type_successor))
            || config.type_defaults.iter().any(out_of_range)
        {
            return Err(ExecError::Parameter("successor index out of range"));
        }

        // `setClusterInfosConfigured`.
        let mut tag_to_cluster: BTreeMap<(TypeWidth, i32), usize> = BTreeMap::new();
        let mut infos: Vec<ClusterInfo> = Vec::with_capacity(inputs.len() + config.clusters.len());
        for (index, cluster) in config.clusters.iter().enumerate() {
            let ts = &cluster.type_successor;
            let key: TypeWidth = (ts.ty, ts.elt_width);
            for &tag in &cluster.member_tags {
                // A tag may belong to one cluster of a type and width only.
                if tag_to_cluster.insert((key, tag), index).is_some() {
                    return Err(ExecError::Parameter("a tag appears in two clusters"));
                }
            }
            let successor = *successors.get(ts.successor).ok_or(ExecError::Parameter(
                "successor index out of range for a cluster",
            ))?;
            let node = *nodes.get(ts.codec).ok_or(ExecError::Parameter(
                "cluster codec index out of range for a cluster",
            ))?;
            infos.push(ClusterInfo {
                n_edges: 0,
                successor,
                node,
            });
        }
        for &input in inputs {
            if let Some(&index) = tag_to_cluster.get(&tag_of(cx, input)?) {
                infos[index].n_edges += 1;
            }
        }
        let n_configured = infos.len();

        // The per-type defaults; a type and width may be listed once.
        let mut defaults: BTreeMap<TypeWidth, TypeSuccessor> = BTreeMap::new();
        for default in &config.type_defaults {
            if defaults
                .insert((default.ty, default.elt_width), *default)
                .is_some()
            {
                return Err(ExecError::Parameter("duplicate type default"));
            }
        }

        // `setClusterInfosUnconfigured_byTag`.
        for &input in inputs {
            let (type_width, tag) = tag_of(cx, input)?;
            let index = infos.len();
            match tag_to_cluster.entry((type_width, tag)) {
                alloc::collections::btree_map::Entry::Occupied(entry) => {
                    let index = *entry.get();
                    if index >= n_configured {
                        infos[index].n_edges += 1;
                    }
                }
                alloc::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(index);
                    infos.push(ClusterInfo {
                        n_edges: 1,
                        node: default_codec(type_width, &defaults, nodes)?,
                        successor: default_successor(type_width, &defaults, successors)?,
                    });
                }
            }
        }

        // Group the edges by cluster, in input order.
        let mut clusters: Vec<Vec<Edge>> = infos
            .iter()
            .map(|info| Vec::with_capacity(info.n_edges))
            .collect();
        for &input in inputs {
            let index = *tag_to_cluster
                .get(&tag_of(cx, input)?)
                .ok_or(ExecError::Parameter("clustering input without a cluster"))?;
            clusters[index].push(input);
        }

        // `sendClustersToSuccessors`.
        for (info, edges) in infos.iter().zip(&clusters) {
            match edges.len() {
                0 => {}
                1 => cx.set_destination(&edges[..1], info.successor, None)?,
                _ => {
                    let outputs = cx.run_node(info.node, edges, None)?;
                    // The first output of a concat is the sizes.
                    let data_index = usize::from(outputs.len() == 2);
                    if data_index == 1 {
                        cx.set_destination(&outputs[..1], gid::FIELD_LZ, None)?;
                    }
                    let data = outputs
                        .get(data_index)
                        .ok_or(ExecError::Parameter("clustering node produced no output"))?;
                    cx.set_destination(core::slice::from_ref(data), info.successor, None)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
