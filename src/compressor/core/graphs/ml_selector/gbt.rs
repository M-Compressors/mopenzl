//! The gradient-boosted-tree predictor of the ML selector
//! (`compress/selectors/ml/gbt.c`, `gbt.h`).
//!
//! A [`Model`] holds forests of trees. With one forest the model is a binary
//! classifier (class 1 when the summed leaves are not negative), otherwise
//! every forest scores one class and the highest score wins (the first one on
//! ties). Scores accumulate in `f32`, tree by tree, in the reference's order.
//!
//! The reference trusts the nodes of a deserialized model and, for a malformed
//! tree, reads out of bounds or loops forever; here every such tree makes the
//! prediction fail instead.

use alloc::vec::Vec;

use super::features::{FeatureGen, LabeledFeature};

/// `GBTPredictor_Node`: a split on `feature_idx` (`-1` for a leaf, whose
/// `value` is the score).
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Node {
    /// Index into the feature vector; `-1` marks a leaf.
    pub(crate) feature_idx: i32,
    /// The split threshold, or the leaf score.
    pub(crate) value: f32,
    /// Child taken when the feature is below the threshold.
    pub(crate) left: usize,
    /// Child taken otherwise.
    pub(crate) right: usize,
    /// Child taken when the feature is missing or NaN.
    pub(crate) missing: usize,
}

/// `GBTPredictor_Tree`.
pub(crate) type Tree = Vec<Node>;

/// `GBTPredictor_Forest`.
pub(crate) type Forest = Vec<Tree>;

/// Why a prediction failed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum PredictError {
    /// A tree the reference would read out of bounds or loop on: a node
    /// without children, a child that does not advance, a child out of
    /// range, or a feature index below -1.
    MalformedTree,
    /// The predicted class is not one of the successors.
    ClassOutOfRange,
}

/// `GBTModel`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Model {
    /// The forests (`GBTPredictor`).
    pub(crate) forests: Vec<Forest>,
    /// The feature generator.
    pub(crate) generator: FeatureGen,
    /// The number of successors the model selects among.
    pub(crate) nb_successors: usize,
    /// The label of every feature, in the order the trees index them.
    pub(crate) feature_labels: Vec<Vec<u8>>,
}

/// `GBTPredictor_Tree_evaluate`.
fn evaluate_tree(tree: &[Node], features: &[f32]) -> Result<f32, PredictError> {
    let mut node = 0usize;
    loop {
        let current = tree.get(node).ok_or(PredictError::MalformedTree)?;
        if current.feature_idx == -1 {
            return Ok(current.value);
        }
        let next = if i64::from(current.feature_idx) >= features.len() as i64 {
            current.missing
        } else {
            let index =
                usize::try_from(current.feature_idx).map_err(|_| PredictError::MalformedTree)?;
            let feature = features[index];
            if feature.is_nan() {
                current.missing
            } else if feature < current.value {
                current.left
            } else {
                current.right
            }
        };
        if next <= node {
            return Err(PredictError::MalformedTree);
        }
        node = next;
    }
}

/// `GBTPredictor_Forest_evaluate`.
fn evaluate_forest(forest: &[Tree], features: &[f32]) -> Result<f32, PredictError> {
    let mut value = 0f32;
    for tree in forest {
        value += evaluate_tree(tree, features)?;
    }
    Ok(value)
}

impl Model {
    /// `GBTPredictor_predict`: the class of a feature vector.
    pub(crate) fn predict_class(&self, features: &[f32]) -> Result<usize, PredictError> {
        let mut best = 0usize;
        let mut best_value = f32::NEG_INFINITY;
        for (index, forest) in self.forests.iter().enumerate() {
            let value = evaluate_forest(forest, features)?;
            if value > best_value {
                best_value = value;
                best = index;
            }
        }
        if self.forests.len() == 1 {
            // `best_value` is never NaN: a NaN score never beats -inf.
            return Ok(usize::from(best_value >= 0.0));
        }
        Ok(best)
    }

    /// `GBTModel_predictInd` on already generated features: maps each of the
    /// model's labels to the generated value of the same label (NaN when the
    /// generator has none) and predicts.
    pub(crate) fn predict(&self, generated: &[LabeledFeature]) -> Result<usize, PredictError> {
        let features: Vec<f32> = self
            .feature_labels
            .iter()
            .map(|label| {
                // `strcmp`: the label ends at its first NUL.
                let label = label.split(|&byte| byte == 0).next().unwrap_or_default();
                generated
                    .iter()
                    .rev()
                    .find(|feature| feature.label.as_bytes() == label)
                    .map_or(f32::NAN, |feature| feature.value)
            })
            .collect();
        let class = self.predict_class(&features)?;
        if class >= self.nb_successors {
            return Err(PredictError::ClassOutOfRange);
        }
        Ok(class)
    }

    /// `GBTModel_validate`: the checks the typed registration applies. A
    /// model read from a serialized blob is not validated by the reference.
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.forests.is_empty() {
            return Err("GBTModel's forests is null");
        }
        if self.forests.len() == 1 {
            if self.nb_successors != 2 {
                return Err(
                    "There is only one forest(binary classification), but number of successors is not 2",
                );
            }
        } else if self.forests.len() != self.nb_successors {
            return Err(
                "Multiclass classification. Number of forests not equal to number of successors",
            );
        }
        let nb_features = self.feature_labels.len();
        for forest in &self.forests {
            for tree in forest {
                if tree.is_empty() {
                    return Err("GBTModel's tree is null");
                }
                validate_tree(tree, nb_features)?;
            }
        }
        Ok(())
    }
}

/// `GBTPredictor_validate_tree`.
fn validate_tree(tree: &[Node], nb_features: usize) -> Result<(), &'static str> {
    for (index, node) in tree.iter().enumerate() {
        if i64::from(node.feature_idx) >= nb_features as i64 {
            return Err("Feature index is out of bounds");
        }
        if node.feature_idx < -1 {
            return Err("Feature index is out of bounds");
        }
        if node.feature_idx == -1 {
            continue;
        }
        if node.value.is_nan() {
            return Err("Node value is nan");
        }
        if node.value.is_infinite() {
            return Err("Node value is positive or negative infinity");
        }
        if index >= node.left {
            return Err("Left child index is less than current node index");
        }
        if index >= node.right {
            return Err("Right child index is less than current node index");
        }
        if index >= node.missing {
            return Err("Missing child index is less than current node index");
        }
        if node.left >= tree.len() {
            return Err("Left child index is out of bounds");
        }
        if node.right >= tree.len() {
            return Err("Right child index is out of bounds");
        }
        if node.missing >= tree.len() {
            return Err("Missing child index is out of bounds");
        }
    }
    Ok(())
}
