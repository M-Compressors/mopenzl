//! Unit tests of the ML selector: the feature generator and the predictor
//! against the reference, the configuration reader and writer, and the
//! predictor's handling of malformed trees.

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use openzl_ffi::enc_ml;

use super::features::{FeatureGen, LabeledFeature, integer_features};
use super::gbt::{Model, Node, PredictError};
use super::{Config, ConfigError, deserialize, serialize};
use crate::shared::cbor::{self, Item};

/// xorshift64*.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn leaf(value: f32) -> Node {
    Node {
        feature_idx: -1,
        value,
        left: 0,
        right: 0,
        missing: 0,
    }
}

fn split(feature_idx: i32, value: f32, left: usize, right: usize, missing: usize) -> Node {
    Node {
        feature_idx,
        value,
        left,
        right,
        missing,
    }
}

fn model(forests: Vec<Vec<Vec<Node>>>, nb_successors: usize, labels: &[&str]) -> Model {
    Model {
        forests,
        generator: FeatureGen::Integer,
        nb_successors,
        feature_labels: labels.iter().map(|l| l.as_bytes().to_vec()).collect(),
    }
}

const LABELS: [&str; 11] = [
    "nbElts",
    "eltWidth",
    "cardinality",
    "cardinality_upper",
    "cardinality_lower",
    "range_size",
    "mean",
    "variance",
    "stddev",
    "skewness",
    "kurtosis",
];

fn elements(rng: &mut Rng, width: usize, count: usize, shape: u64) -> Vec<u8> {
    let max = if width == 8 {
        u64::MAX
    } else {
        (1u64 << (width * 8)) - 1
    };
    (0..count)
        .flat_map(|i| {
            let value = match shape {
                0 => 0,
                1 => max,
                2 => rng.next() & max,
                3 => (rng.next() & max) % 7,
                4 => (i as u64).wrapping_mul(3) & max,
                _ => {
                    if i % 2 == 0 {
                        0
                    } else {
                        max
                    }
                }
            };
            value.to_le_bytes()[..width].to_vec()
        })
        .collect()
}

fn same(ours: f32, theirs: f32) -> bool {
    (ours.is_nan() && theirs.is_nan()) || ours.to_bits() == theirs.to_bits()
}

#[test]
fn features_match_the_reference_bit_for_bit() {
    let mut rng = Rng(0x0123_4567_89AB_CDEF);
    let counts: &[usize] = if cfg!(miri) {
        &[0, 1, 3, 20]
    } else {
        &[0, 1, 2, 3, 4, 5, 17, 100, 1000, 5000]
    };
    for width in [1usize, 2, 4, 8] {
        for &count in counts {
            for shape in 0..6 {
                let bytes = elements(&mut rng, width, count, shape);
                let ours = integer_features(&bytes, width);
                let theirs = enc_ml::ml_features(&bytes, width).expect("reference features");
                assert_eq!(ours.len(), 11);
                for (n, feature) in ours.iter().enumerate() {
                    assert_eq!(feature.label, LABELS[n]);
                    assert!(
                        same(feature.value, theirs[n]),
                        "w{width} n{count} shape{shape} {}: {} vs {}",
                        feature.label,
                        feature.value,
                        theirs[n]
                    );
                }
            }
        }
    }
}

#[test]
fn feature_generator_ids_follow_the_reference() {
    assert_eq!(FeatureGen::from_id(0), Some(FeatureGen::Integer));
    assert_eq!(FeatureGen::from_id(1), None);
    assert_eq!(FeatureGen::from_id(-1), None);
    assert_eq!(FeatureGen::Integer.id(), 0);
    let generated = FeatureGen::Integer.generate(&[1, 2, 3], 1);
    assert_eq!(generated.len(), 11);
    assert_eq!(
        generated[0],
        LabeledFeature {
            label: "nbElts",
            value: 3.0
        }
    );
}

/// A random valid tree: every inner node's children come later.
fn random_tree(rng: &mut Rng, nb_features: u64) -> Vec<Node> {
    let size = 1 + 2 * rng.below(4) as usize;
    let mut tree = Vec::new();
    for index in 0..size {
        let remaining = size - index - 1;
        if remaining < 2 || rng.below(4) == 0 {
            tree.push(leaf((rng.below(2000) as f32 - 1000.0) / 100.0));
            continue;
        }
        let child = |rng: &mut Rng| index + 1 + rng.below(remaining as u64) as usize;
        let feature = if rng.below(6) == 0 {
            nb_features as i32 + 3
        } else {
            rng.below(nb_features) as i32
        };
        let value = match rng.below(4) {
            0 => rng.below(10) as f32,
            1 => rng.below(2000) as f32,
            2 => rng.below(100_000) as f32 / 7.0,
            _ => rng.below(1 << 30) as f32,
        };
        tree.push(split(feature, value, child(rng), child(rng), child(rng)));
    }
    // Make the last node a leaf so every chain terminates.
    if let Some(last) = tree.last_mut() {
        *last = leaf(0.5);
    }
    tree
}

#[test]
fn predictions_match_the_reference_on_random_models() {
    let mut rng = Rng(0xFEED_FACE_CAFE_BEEF);
    let rounds = if cfg!(miri) { 4 } else { 150 };
    for round in 0..rounds {
        let binary = rng.below(3) == 0;
        let forests_count = if binary { 1 } else { 3 + rng.below(3) as usize };
        let nb_labels = 1 + rng.below(5) as usize;
        let mut labels: Vec<&str> = (0..nb_labels)
            .map(|_| LABELS[rng.below(11) as usize])
            .collect();
        if rng.below(3) == 0 {
            labels.push("unknown");
        }
        let forests: Vec<Vec<Vec<Node>>> = (0..forests_count)
            .map(|_| {
                (0..1 + rng.below(3))
                    .map(|_| random_tree(&mut rng, labels.len() as u64))
                    .collect()
            })
            .collect();
        let nb_successors = if binary { 2 } else { forests_count };
        let model = model(forests, nb_successors, &labels);
        let blob = serialize(&model).unwrap();
        for width in [1usize, 2, 4, 8] {
            for shape in 0..6 {
                let count = [0usize, 1, 5, 64][rng.below(4) as usize];
                let bytes = elements(&mut rng, width, count, shape);
                let ours = model.predict(&model.generator.generate(&bytes, width));
                let theirs = enc_ml::ml_predict(&blob, &bytes, width);
                assert_eq!(ours.ok(), theirs, "round {round} w{width} shape{shape}");
            }
        }
    }
}

#[test]
fn a_class_past_the_successors_is_an_error() {
    let model = model(vec![vec![vec![leaf(1.0)]]], 1, &["nbElts"]);
    let features = [LabeledFeature {
        label: "nbElts",
        value: 3.0,
    }];
    assert_eq!(model.predict(&features), Err(PredictError::ClassOutOfRange));
    assert_eq!(
        enc_ml::ml_predict(&serialize(&model).unwrap(), &[1, 2, 3], 1),
        None
    );
}

#[test]
fn multiclass_ties_go_to_the_first_forest_and_nan_never_wins() {
    let forest = |score: f32| vec![vec![leaf(score)]];
    let tie = model(vec![forest(1.0), forest(1.0), forest(0.5)], 3, &["a"]);
    assert_eq!(tie.predict_class(&[0.0]), Ok(0));
    let later = model(vec![forest(1.0), forest(2.0), forest(2.0)], 3, &["a"]);
    assert_eq!(later.predict_class(&[0.0]), Ok(1));
    // A NaN score never beats -inf, so forest 0 stays the best.
    let nan = model(vec![forest(f32::NAN), forest(-1.0)], 2, &["a"]);
    assert_eq!(nan.predict_class(&[0.0]), Ok(1));
    // Binary: class 1 for a score of exactly zero, 0 for a NaN.
    let zero = model(vec![forest(0.0)], 2, &["a"]);
    assert_eq!(zero.predict_class(&[0.0]), Ok(1));
    let nan_binary = model(vec![forest(f32::NAN)], 2, &["a"]);
    assert_eq!(nan_binary.predict_class(&[0.0]), Ok(0));
    let negative = model(vec![forest(-0.5)], 2, &["a"]);
    assert_eq!(negative.predict_class(&[0.0]), Ok(0));
    // No forests: class 0.
    assert_eq!(model(vec![], 2, &[]).predict_class(&[]), Ok(0));
}

#[test]
fn trees_route_by_threshold_missing_and_nan_features() {
    let tree = vec![split(0, 5.0, 1, 2, 3), leaf(-1.0), leaf(1.0), leaf(7.0)];
    let class =
        |features: &[f32]| model(vec![vec![tree.clone()]], 2, &["a"]).predict_class(features);
    assert_eq!(class(&[4.0]), Ok(0));
    assert_eq!(class(&[5.0]), Ok(1));
    assert_eq!(class(&[f32::NAN]), Ok(1));
    // A feature index past the vector takes the missing child.
    let far = vec![split(4, 5.0, 1, 2, 2), leaf(-1.0), leaf(1.0)];
    assert_eq!(
        model(vec![vec![far]], 2, &["a"]).predict_class(&[0.0]),
        Ok(1)
    );
}

#[test]
fn malformed_trees_are_errors_not_loops() {
    let check = |tree: Vec<Node>| model(vec![vec![tree]], 2, &["a"]).predict_class(&[1.0]);
    assert_eq!(check(vec![]), Err(PredictError::MalformedTree));
    assert_eq!(
        check(vec![split(0, 0.0, 0, 0, 0)]),
        Err(PredictError::MalformedTree)
    );
    assert_eq!(
        check(vec![split(0, 5.0, 3, 3, 3)]),
        Err(PredictError::MalformedTree)
    );
    assert_eq!(
        check(vec![split(-5, 5.0, 1, 1, 1), leaf(1.0)]),
        Err(PredictError::MalformedTree)
    );
    assert_eq!(
        check(vec![split(0, 5.0, 1, 1, 1), split(0, 5.0, 0, 0, 0)]),
        Err(PredictError::MalformedTree)
    );
}

#[test]
fn validation_reports_the_reference_messages() {
    let ok_tree = vec![split(0, 1.0, 1, 1, 1), leaf(0.0)];
    let invalid = |forests: Vec<Vec<Vec<Node>>>, successors, labels: &[&str]| {
        model(forests, successors, labels).validate().unwrap_err()
    };
    assert!(invalid(vec![], 2, &["a"]).contains("forests"));
    assert!(invalid(vec![vec![ok_tree.clone()]], 3, &["a"]).contains("only one forest"));
    assert!(invalid(vec![vec![ok_tree.clone()]; 2], 3, &["a"]).contains("Multiclass"));
    assert!(invalid(vec![vec![vec![]]], 2, &["a"]).contains("tree is null"));
    let bad_node = |node: Node| invalid(vec![vec![vec![node, leaf(0.0)]]], 2, &["a"]);
    assert!(bad_node(split(1, 1.0, 1, 1, 1)).contains("Feature index"));
    assert!(bad_node(split(-2, 1.0, 1, 1, 1)).contains("Feature index"));
    assert!(bad_node(split(0, f32::NAN, 1, 1, 1)).contains("nan"));
    assert!(bad_node(split(0, f32::NEG_INFINITY, 1, 1, 1)).contains("infinity"));
    assert!(bad_node(split(0, 1.0, 0, 1, 1)).contains("Left child index is less"));
    assert!(bad_node(split(0, 1.0, 1, 0, 1)).contains("Right child index is less"));
    assert!(bad_node(split(0, 1.0, 1, 1, 0)).contains("Missing child index is less"));
    assert!(bad_node(split(0, 1.0, 5, 1, 1)).contains("Left child index is out"));
    assert!(bad_node(split(0, 1.0, 1, 5, 1)).contains("Right child index is out"));
    assert!(bad_node(split(0, 1.0, 1, 1, 5)).contains("Missing child index is out"));
    assert!(model(vec![vec![ok_tree]], 2, &["a"]).validate().is_ok());
}

#[test]
fn labels_match_up_to_the_first_nul_and_the_last_duplicate_wins() {
    let tree = vec![split(0, 5.0, 1, 2, 2), leaf(-1.0), leaf(1.0)];
    let mut m = model(vec![vec![tree]], 2, &["x"]);
    m.feature_labels = vec![b"nbElts\0ignored".to_vec()];
    let big = [LabeledFeature {
        label: "nbElts",
        value: 9.0,
    }];
    assert_eq!(m.predict(&big), Ok(1));
    let twice = [
        LabeledFeature {
            label: "nbElts",
            value: 9.0,
        },
        LabeledFeature {
            label: "nbElts",
            value: 1.0,
        },
    ];
    assert_eq!(m.predict(&twice), Ok(0));
}

fn entry(key: &'static str, value: Item<'static>) -> (Item<'static>, Item<'static>) {
    (Item::text(key), value)
}

#[test]
fn configurations_round_trip_through_cbor() {
    let original = model(
        vec![
            vec![vec![split(0, 5.5, 1, 2, 2), leaf(-1.0), leaf(1.0)]],
            vec![vec![leaf(0.25)], vec![leaf(0.5)]],
            vec![vec![leaf(0.0)]],
        ],
        3,
        &["nbElts", "mean"],
    );
    let blob = serialize(&original).unwrap();
    assert_eq!(deserialize(&blob), Ok(Config::Gbt(original.clone())));
    // Unknown keys are ignored and key order does not matter.
    let item = cbor::decode(&blob, &cbor::DecoderConfig::STRICT).unwrap();
    let mut pairs = item.as_map().unwrap().to_vec();
    pairs.reverse();
    pairs.push(entry("extra", Item::Null));
    let reordered = cbor::encode_to_vec(&Item::map(pairs)).unwrap();
    assert_eq!(deserialize(&reordered), Ok(Config::Gbt(original)));
}

#[test]
fn unreadable_configurations_are_rejected_like_the_reference() {
    let valid = serialize(&model(vec![vec![vec![leaf(1.0)]]], 2, &["a"])).unwrap();
    let item = cbor::decode(&valid, &cbor::DecoderConfig::STRICT).unwrap();
    let runtime = item.map_get_str("runtimeConfig").unwrap().clone();
    let with = |runtime_pairs: Vec<(Item<'static>, Item<'static>)>| {
        let root = Item::map([
            entry("model", Item::Int(0)),
            entry("runtimeConfig", Item::map(runtime_pairs)),
        ]);
        cbor::encode_to_vec(&root).unwrap()
    };
    let pairs: Vec<(Item<'static>, Item<'static>)> = runtime
        .as_map()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone().into_owned(), v.clone().into_owned()))
        .collect();
    // Dropping any field fails.
    for dropped in 0..pairs.len() {
        let mut rest = pairs.clone();
        rest.remove(dropped);
        assert!(deserialize(&with(rest)).is_err(), "field {dropped}");
    }
    // Replacing a field by a value of the wrong type fails.
    for (index, replacement) in [
        (0, Item::Int(1)),
        (1, Item::text("zero")),
        (2, Item::Float64(2.0)),
        (3, Item::Float64(1.0)),
        (4, Item::Int(0)),
    ] {
        let mut changed = pairs.clone();
        changed[index].1 = replacement;
        assert!(deserialize(&with(changed)).is_err(), "field {index}");
    }
    // Garbage, the wrong root type and a missing runtime configuration.
    for blob in [
        vec![],
        vec![0xFF],
        cbor::encode_to_vec(&Item::Int(3)).unwrap(),
        cbor::encode_to_vec(&Item::map([])).unwrap(),
        cbor::encode_to_vec(&Item::map([entry("model", Item::Int(0))])).unwrap(),
        cbor::encode_to_vec(&Item::map([entry("model", Item::text("gbt"))])).unwrap(),
    ] {
        assert!(deserialize(&blob).is_err(), "{blob:02x?}");
    }
    // A model type other than GBT loads and is flagged unsupported.
    let other = cbor::encode_to_vec(&Item::map([entry("model", Item::Int(9))])).unwrap();
    assert_eq!(deserialize(&other), Ok(Config::Unsupported));
    let error = deserialize(&[0xFF]).unwrap_err();
    assert!(matches!(error, ConfigError::Cbor(_)));
    assert!(error.to_string().contains("CBOR"));
}

#[test]
fn nested_counts_must_match_their_arrays() {
    let node = |value: f64| {
        Item::map([
            entry("featureIdx", Item::Int(-1)),
            entry("value", Item::Float64(value)),
            entry("leftChildIdx", Item::Int(0)),
            entry("rightChildIdx", Item::Int(0)),
            entry("missingChildIdx", Item::Int(0)),
        ])
    };
    let build = |forests: i64, trees: i64, nodes: i64| {
        let tree = Item::map([
            entry("numNodes", Item::Int(nodes)),
            entry("nodes", Item::array([node(1.0)])),
        ]);
        let forest = Item::map([
            entry("numTrees", Item::Int(trees)),
            entry("trees", Item::array([tree])),
        ]);
        let runtime = Item::map([
            entry(
                "predictor",
                Item::map([
                    entry("numForests", Item::Int(forests)),
                    entry("forests", Item::array([forest])),
                ]),
            ),
            entry("featureGenerator", Item::Int(0)),
            entry("nbSuccessors", Item::Int(2)),
            entry("nbFeatures", Item::Int(1)),
            entry("featureLabels", Item::array([Item::text("a")])),
        ]);
        cbor::encode_to_vec(&Item::map([
            entry("model", Item::Int(0)),
            entry("runtimeConfig", runtime),
        ]))
        .unwrap()
    };
    assert!(deserialize(&build(1, 1, 1)).is_ok());
    assert!(deserialize(&build(2, 1, 1)).is_err());
    assert!(deserialize(&build(1, 2, 1)).is_err());
    assert!(deserialize(&build(1, 1, 3)).is_err());
    assert!(deserialize(&build(-1, 1, 1)).is_err());
}

#[test]
fn serializing_rejects_labels_that_are_not_text() {
    let mut bad = model(vec![vec![vec![leaf(1.0)]]], 2, &["a"]);
    bad.feature_labels = vec![vec![0xFF]];
    assert!(matches!(serialize(&bad), Err(ConfigError::Invalid(_))));
}
