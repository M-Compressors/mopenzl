//! Unit tests of the clustering configuration and its defaults.

use alloc::borrow::Cow;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::compressor::core::params::RefParam;

fn type_successor(ty: u32, width: usize, successor: usize, codec: usize) -> TypeSuccessor {
    TypeSuccessor {
        ty,
        elt_width: width,
        successor,
        codec,
    }
}

fn sample() -> Config {
    Config {
        clusters: vec![
            Cluster {
                type_successor: type_successor(4, 4, 0, 2),
                member_tags: vec![1, -2, i32::MAX],
            },
            Cluster {
                type_successor: type_successor(8, 0, 1, 3),
                member_tags: vec![],
            },
        ],
        type_defaults: vec![type_successor(1, 1, 1, 0)],
    }
}

fn entry(key: &'static str, value: Item<'static>) -> (Item<'static>, Item<'static>) {
    (Item::text(key), value)
}

fn ts_item(ty: i64, width: i64, successor: i64, codec: i64) -> Item<'static> {
    Item::map([
        entry("type", Item::Int(ty)),
        entry("eltWidth", Item::Int(width)),
        entry("successorIdx", Item::Int(successor)),
        entry("clusteringCodecIdx", Item::Int(codec)),
    ])
}

fn cluster_item(tags: Vec<Item<'static>>) -> Item<'static> {
    Item::map([
        entry("typeSuccessor", ts_item(4, 4, 0, 0)),
        entry("memberTags", Item::array(tags)),
    ])
}

fn blob(clusters: Vec<Item<'static>>, defaults: Vec<Item<'static>>) -> Vec<u8> {
    cbor::encode_to_vec(&Item::map([
        entry("clusters", Item::array(clusters)),
        entry("typeDefaults", Item::array(defaults)),
    ]))
    .unwrap()
}

#[test]
fn configurations_round_trip_through_cbor() {
    let config = sample();
    let bytes = serialize(&config).unwrap();
    assert_eq!(deserialize(&bytes), Ok(config));
    let empty = Config::default();
    assert_eq!(deserialize(&serialize(&empty).unwrap()), Ok(empty));
}

#[test]
fn unreadable_configurations_are_rejected() {
    let good = || cluster_item(vec![Item::Int(1)]);
    assert!(deserialize(&blob(vec![good()], vec![ts_item(1, 1, 0, 0)])).is_ok());
    // CBOR errors carry their source.
    let error = deserialize(&[0xFF]).unwrap_err();
    assert!(matches!(error, ConfigError::Cbor(_)));
    assert!(alloc::string::ToString::to_string(&error).contains("CBOR"));
    // Wrong root, missing arrays, wrong element types.
    let bad_roots = [
        cbor::encode_to_vec(&Item::Int(1)).unwrap(),
        cbor::encode_to_vec(&Item::map([])).unwrap(),
        cbor::encode_to_vec(&Item::map([entry("clusters", Item::array([]))])).unwrap(),
        cbor::encode_to_vec(&Item::map([
            entry("clusters", Item::Int(1)),
            entry("typeDefaults", Item::array([])),
        ]))
        .unwrap(),
        blob(vec![Item::Int(1)], vec![]),
        blob(vec![], vec![Item::Int(1)]),
        blob(vec![Item::map([])], vec![]),
        blob(
            vec![Item::map([
                entry("typeSuccessor", Item::Int(3)),
                entry("memberTags", Item::array([])),
            ])],
            vec![],
        ),
        blob(
            vec![Item::map([entry("typeSuccessor", ts_item(4, 4, 0, 0))])],
            vec![],
        ),
        blob(vec![cluster_item(vec![Item::text("x")])], vec![]),
        // A member tag above INT32_MAX.
        blob(
            vec![cluster_item(vec![Item::Int(i64::from(i32::MAX) + 1)])],
            vec![],
        ),
        // A type successor missing a field.
        blob(vec![], vec![Item::map([entry("type", Item::Int(1))])]),
    ];
    for bad in bad_roots {
        assert!(deserialize(&bad).is_err(), "{bad:02x?}");
    }
    // More clusters than the format allows inputs.
    let many = vec![good(); MAX_CLUSTERS + 1];
    assert_eq!(
        deserialize(&blob(many, vec![])),
        Err(ConfigError::Invalid("too many clusters"))
    );
    assert!(deserialize(&blob(vec![good(); MAX_CLUSTERS], vec![])).is_ok());
    // Tags below INT32_MIN truncate, as the reference's `(int)` cast does.
    let low = deserialize(&blob(
        vec![cluster_item(vec![Item::Int(i64::from(i32::MIN) - 1)])],
        vec![],
    ))
    .unwrap();
    assert_eq!(low.clusters[0].member_tags, [i32::MAX]);
}

#[test]
fn the_config_parameter_is_read_as_a_reference_or_a_copy() {
    let bytes = serialize(&sample()).unwrap();
    let copied = LocalParams {
        copies: vec![(CONFIG_PARAM_ID, Arc::from(&bytes[..]))],
        ..LocalParams::EMPTY
    };
    assert_eq!(config_of(&copied), Ok(sample()));
    let referenced = LocalParams {
        refs: vec![(CONFIG_PARAM_ID, RefParam(Arc::new(bytes.clone())))],
        ..LocalParams::EMPTY
    };
    assert_eq!(config_of(&referenced), Ok(sample()));
    let wrong = LocalParams {
        refs: vec![(CONFIG_PARAM_ID, RefParam(Arc::new(7u32)))],
        ..LocalParams::EMPTY
    };
    assert!(config_of(&wrong).is_err());
    assert!(config_of(&LocalParams::EMPTY).is_err());
    let garbage = LocalParams {
        copies: vec![(CONFIG_PARAM_ID, Arc::from(&[0xFFu8][..]))],
        ints: Cow::Borrowed(&[]),
        refs: vec![],
    };
    assert!(config_of(&garbage).is_err());
}

#[test]
fn unconfigured_inputs_fall_back_to_concat_and_the_generic_graph() {
    let none = BTreeMap::new();
    let nodes = [NodeId::std(100), NodeId::std(101)];
    let successors = [GraphId::std(2), GraphId::std(9)];
    for (ty, expected) in [
        (1, CONCAT_SERIAL),
        (2, CONCAT_STRUCT),
        (4, CONCAT_NUM),
        (8, CONCAT_STRING),
    ] {
        assert_eq!(default_codec((ty, 1), &none, &nodes), Ok(expected));
        assert_eq!(
            default_successor((ty, 1), &none, &successors),
            Ok(gid::COMPRESS_GENERIC)
        );
    }
    assert!(default_codec((3, 1), &none, &nodes).is_err());
    // A default picks its own codec and successor, with range checks.
    let mut defaults = BTreeMap::new();
    defaults.insert((4, 4), type_successor(4, 4, 1, 1));
    defaults.insert((4, 8), type_successor(4, 8, 5, 5));
    assert_eq!(default_codec((4, 4), &defaults, &nodes), Ok(nodes[1]));
    assert_eq!(
        default_successor((4, 4), &defaults, &successors),
        Ok(successors[1])
    );
    assert!(default_codec((4, 8), &defaults, &nodes).is_err());
    assert!(default_successor((4, 8), &defaults, &successors).is_err());
    // Other widths of the same type are unaffected by a default.
    assert_eq!(default_codec((4, 2), &defaults, &nodes), Ok(CONCAT_NUM));
}

#[test]
fn string_inputs_are_keyed_by_width_zero() {
    assert_eq!(elt_width(Type::String, 1), 0);
    assert_eq!(elt_width(Type::Struct, 6), 6);
    assert_eq!(elt_width(Type::Serial, 1), 1);
}
