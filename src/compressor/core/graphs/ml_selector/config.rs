//! The serialized ML selector configuration
//! (`MLSelector_serializeMLSelectorConfig`,
//! `MLSelector_deserializeMLSelectorConfig` in `ml_selector_graph.c`): a CBOR
//! map `{"model": 0, "runtimeConfig": {...}}` whose runtime configuration is a
//! GBT model.
//!
//! The reader accepts what the reference's reader accepts, including models
//! that would not pass `GBTModel_validate` and model types other than GBT
//! (which load but fail when the graph runs), and ignores unknown keys.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

use thiserror::Error;

use super::features::FeatureGen;
use super::gbt::{Forest, Model, Node, Tree};
use crate::shared::cbor::{self, DecoderConfig, Item};

/// `ZL_MLSelectorModelType`: the only model is `ZL_GBT` (0).
const MODEL_GBT: i64 = 0;

/// Why a configuration could not be read or written.
#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub(crate) enum ConfigError {
    /// The CBOR is malformed.
    #[error("malformed CBOR: {0}")]
    Cbor(#[from] cbor::Error),
    /// The CBOR is well formed but not a configuration.
    #[error("invalid ML selector configuration: {0}")]
    Invalid(&'static str),
}

/// `ZL_MLSelectorConfig`: the model a graph selects with.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Config {
    /// `ZL_GBT`.
    Gbt(Model),
    /// A model type this library does not know: it loads, then fails when the
    /// graph runs.
    Unsupported,
}

/// `A1C_Map_get_cstr` + a type check, failing like `A1C_TRY_EXTRACT_*`.
fn field<'i, 'a>(map: &'i Item<'a>, key: &str) -> Result<&'i Item<'a>, ConfigError> {
    map.map_get_str(key)
        .ok_or(ConfigError::Invalid("missing or mistyped field"))
}

fn int_field(map: &Item<'_>, key: &str) -> Result<i64, ConfigError> {
    field(map, key)?
        .as_int64()
        .ok_or(ConfigError::Invalid("expected an integer"))
}

fn array_field<'i, 'a>(map: &'i Item<'a>, key: &str) -> Result<&'i [Item<'a>], ConfigError> {
    field(map, key)?
        .as_array()
        .ok_or(ConfigError::Invalid("expected an array"))
}

fn expect_map<'i, 'a>(item: &'i Item<'a>) -> Result<&'i Item<'a>, ConfigError> {
    item.as_map()
        .map(|_| item)
        .ok_or(ConfigError::Invalid("expected a map"))
}

/// `GBTModel_deserializeNode`.
fn read_node(item: &Item<'_>) -> Result<Node, ConfigError> {
    let map = expect_map(item)?;
    Ok(Node {
        feature_idx: int_field(map, "featureIdx")? as i32,
        value: field(map, "value")?
            .as_float64()
            .ok_or(ConfigError::Invalid("expected a double"))? as f32,
        left: int_field(map, "leftChildIdx")? as usize,
        right: int_field(map, "rightChildIdx")? as usize,
        missing: int_field(map, "missingChildIdx")? as usize,
    })
}

/// Reads `{count_key: n, items_key: [...]}` and checks the array has `n`
/// elements, as the reference's three nested readers do.
fn read_counted<'i, 'a>(
    item: &'i Item<'a>,
    count_key: &str,
    items_key: &str,
    mismatch: &'static str,
) -> Result<&'i [Item<'a>], ConfigError> {
    let map = expect_map(item)?;
    let count = int_field(map, count_key)? as usize;
    let items = array_field(map, items_key)?;
    if items.len() != count {
        return Err(ConfigError::Invalid(mismatch));
    }
    Ok(items)
}

/// `GBTModel_deserializeTree`.
fn read_tree(item: &Item<'_>) -> Result<Tree, ConfigError> {
    read_counted(item, "numNodes", "nodes", "numNodes doesn't match nodes")?
        .iter()
        .map(read_node)
        .collect()
}

/// `GBTModel_deserializeForest`.
fn read_forest(item: &Item<'_>) -> Result<Forest, ConfigError> {
    read_counted(item, "numTrees", "trees", "numTrees doesn't match trees")?
        .iter()
        .map(read_tree)
        .collect()
}

/// `GBTModel_deserialize`.
fn read_model(root: &Item<'_>) -> Result<Model, ConfigError> {
    let root = expect_map(root)?;
    let predictor = field(root, "predictor")?;
    let forests = read_counted(
        predictor,
        "numForests",
        "forests",
        "numForests doesn't match forests",
    )?
    .iter()
    .map(read_forest)
    .collect::<Result<Vec<_>, _>>()?;
    let generator = int_field(root, "featureGenerator")?;
    let generator = FeatureGen::from_id(generator as i32).ok_or(ConfigError::Invalid(
        "must use a standard feature generator",
    ))?;
    let nb_successors = int_field(root, "nbSuccessors")? as usize;
    let nb_features = int_field(root, "nbFeatures")? as usize;
    let labels = array_field(root, "featureLabels")?;
    if labels.len() != nb_features {
        return Err(ConfigError::Invalid(
            "nbFeatures doesn't match featureLabels",
        ));
    }
    let feature_labels = labels
        .iter()
        .map(|label| {
            label
                .as_text_bytes()
                .map(<[u8]>::to_vec)
                .ok_or(ConfigError::Invalid("expected a string"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Model {
        forests,
        generator,
        nb_successors,
        feature_labels,
    })
}

/// `MLSelector_deserializeMLSelectorConfig` (and so `ZL_MLSel_materialize`).
///
/// # Errors
///
/// Malformed CBOR, a missing or mistyped field, a count that does not match
/// its array, or an unknown feature generator.
pub(crate) fn deserialize(bytes: &[u8]) -> Result<Config, ConfigError> {
    let root = cbor::decode(bytes, &DecoderConfig::STRICT)?;
    let root = expect_map(&root)?;
    let model = int_field(root, "model")?;
    if model != MODEL_GBT {
        return Ok(Config::Unsupported);
    }
    let runtime = root
        .map_get_str("runtimeConfig")
        .ok_or(ConfigError::Invalid("missing runtimeConfig"))?;
    Ok(Config::Gbt(read_model(runtime)?))
}

fn int(value: usize) -> Item<'static> {
    Item::Int(value as i64)
}

fn entry(key: &'static str, value: Item<'static>) -> (Item<'static>, Item<'static>) {
    (Item::text(key), value)
}

fn write_node(node: &Node) -> Item<'static> {
    Item::map([
        entry("featureIdx", Item::Int(i64::from(node.feature_idx))),
        entry("value", Item::Float64(f64::from(node.value))),
        entry("leftChildIdx", int(node.left)),
        entry("rightChildIdx", int(node.right)),
        entry("missingChildIdx", int(node.missing)),
    ])
}

fn write_tree(tree: &Tree) -> Item<'static> {
    Item::map([
        entry("numNodes", int(tree.len())),
        entry("nodes", Item::array(tree.iter().map(write_node))),
    ])
}

fn write_forest(forest: &Forest) -> Item<'static> {
    Item::map([
        entry("numTrees", int(forest.len())),
        entry("trees", Item::array(forest.iter().map(write_tree))),
    ])
}

/// `MLSelector_serializeMLSelectorConfig` for a GBT model: the CBOR blob a
/// graph carries as its MParam.
///
/// # Errors
///
/// A label that is not UTF-8 cannot be written as text by the reference
/// either; none arises from [`Model`]s built by this crate.
pub(crate) fn serialize(model: &Model) -> Result<Vec<u8>, ConfigError> {
    let labels = model
        .feature_labels
        .iter()
        .map(|label| String::from_utf8(label.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ConfigError::Invalid("feature label is not UTF-8"))?;
    let runtime = Item::map([
        entry(
            "predictor",
            Item::map([
                entry("numForests", int(model.forests.len())),
                entry(
                    "forests",
                    Item::array(model.forests.iter().map(write_forest)),
                ),
            ]),
        ),
        entry(
            "featureGenerator",
            Item::Int(i64::from(model.generator.id())),
        ),
        entry("nbSuccessors", int(model.nb_successors)),
        entry("nbFeatures", int(model.feature_labels.len())),
        entry(
            "featureLabels",
            Item::array(
                labels
                    .into_iter()
                    .map(|label| Item::Text(Cow::Owned(label.into_bytes()))),
            ),
        ),
    ]);
    let root = Item::map([
        entry("model", Item::Int(MODEL_GBT)),
        entry("runtimeConfig", runtime),
    ]);
    Ok(cbor::encode_to_vec(&root)?)
}
