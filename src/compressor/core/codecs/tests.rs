//! Tests of the ported node encoders through a one-node graph.

use alloc::borrow::Cow;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use crate::compressor::config::EncoderConfig;
use crate::compressor::core::Params;
use crate::compressor::core::data::{Bytes, Stream, Type};
use crate::compressor::core::error::{ExecError, NodeError};
use crate::compressor::core::exec::{Exec, Resources};
use crate::compressor::core::params::{LocalParams, RefParam};
use crate::compressor::core::registry::{
    GraphDef, GraphId, GraphKind, NodeDef, NodeId, Registry, gid, nodes,
};
use crate::compressor::core::sink::VecSink;
use crate::compressor::core::zstd::ZstdNode;
use crate::{Backend, Level};

/// Runs `node` (with `params` if any) on `input` and stores every output.
fn run_node(
    node: u32,
    params: Option<LocalParams>,
    input: Stream<'_>,
    config: EncoderConfig,
) -> Result<Vec<u8>, ExecError> {
    let spec = nodes::standard(node).unwrap();
    let mut registry = Registry::new(1, gid::STORE);
    let node_id = match params {
        Some(params) => registry.add_node(NodeDef {
            #[cfg(feature = "custom-codec")]
            custom: None,
            spec: crate::compressor::core::registry::Callback::Static(spec),
            params: Some(params),
            dict_id: None,
            dict_index: None,
        }),
        None => NodeId::std(node),
    };
    let outcomes = spec.singletons.len() + spec.variable.len();
    let graph = registry.add_graph(GraphDef {
        name: Cow::Borrowed("one node"),
        kind: GraphKind::Static,
        inputs: Cow::Owned(vec![15]),
        last_variable: false,
        custom_graphs: Cow::Owned(vec![gid::SERIAL_STORE; outcomes]),
        custom_nodes: Cow::Owned(vec![node_id]),
        params: LocalParams::default(),
    });
    let p = Params::from(&config);
    let mut zstd = ZstdNode::new(p.level, Backend::default());
    let resources = Resources {
        zstd: &mut zstd,
        simd: Backend::default().0,
    };
    let mut out = Vec::new();
    let mut sink = VecSink::new(&mut out);
    Exec::new(&registry, p, resources, None, vec![input], &mut sink).compress_frame(graph, None)?;
    Ok(out)
}

fn serial(bytes: &[u8]) -> Stream<'_> {
    Stream::serial(bytes)
}

fn ints(pairs: &[(i32, i32)]) -> LocalParams {
    LocalParams {
        ints: Cow::Owned(pairs.to_vec()),
        ..LocalParams::default()
    }
}

fn node_error(result: Result<Vec<u8>, ExecError>) -> NodeError {
    match result.unwrap_err() {
        ExecError::Node { source, .. } => source,
        other => panic!("not a node error: {other:?}"),
    }
}

fn text(len: usize) -> Vec<u8> {
    (0..len).map(|i| b"openzl "[i % 7]).collect()
}

#[test]
fn numeric_conversions_validate_widths_and_swap_big_endian() {
    let data = text(64);
    // The conversions never shrink the data: keep their output.
    let config = EncoderConfig::default().with_store_on_expansion(false);
    // 61 bytes are not whole 4-byte integers.
    assert!(matches!(
        node_error(run_node(21, None, serial(&data[..61]), config)),
        NodeError::InvalidInput(_)
    ));
    // A struct of width 3 cannot become numeric.
    let records = Stream::new(Type::Struct, 3, Bytes::Borrowed(&data[..63]));
    assert!(matches!(
        node_error(run_node(17, None, records, config)),
        NodeError::InvalidInput(_)
    ));
    // Big-endian conversion swaps each element: the stored bytes differ.
    let le = run_node(21, None, serial(&data), config).unwrap();
    let be = run_node(24, None, serial(&data), config).unwrap();
    assert_ne!(le, be);
    assert_eq!(le.len(), be.len());
    let struct_be = Stream::new(Type::Struct, 4, Bytes::Borrowed(&data));
    let struct_le = Stream::new(Type::Struct, 4, Bytes::Borrowed(&data));
    assert_ne!(
        run_node(18, None, struct_be, config).unwrap(),
        run_node(17, None, struct_le, config).unwrap()
    );
}

#[test]
fn serial_to_struct_needs_a_positive_dividing_token_size() {
    let data = text(60);
    let config = EncoderConfig::default();
    assert!(matches!(
        node_error(run_node(13, None, serial(&data), config)),
        NodeError::InvalidParam(_)
    ));
    for bad in [0, -3] {
        assert!(matches!(
            node_error(run_node(13, Some(ints(&[(1, bad)])), serial(&data), config)),
            NodeError::InvalidParam(_)
        ));
    }
    assert!(matches!(
        node_error(run_node(13, Some(ints(&[(1, 7)])), serial(&data), config)),
        NodeError::InvalidInput(_)
    ));
    assert!(run_node(13, Some(ints(&[(1, 6)])), serial(&data), config).is_ok());
}

#[test]
fn set_string_lens_takes_its_lengths_from_a_reference_parameter() {
    let data = text(30);
    let config = EncoderConfig::default();
    let with = |lens: Vec<u32>| LocalParams {
        refs: vec![(521, RefParam(Arc::new(lens)))],
        ..LocalParams::default()
    };
    let ok = run_node(57, Some(with(vec![10, 20])), serial(&data), config);
    assert!(
        ok.is_err(),
        "the string output has no serial successor: {ok:?}"
    );
    // The node itself succeeds; the one-node harness cannot store strings, so
    // exercise the failures, which come first.
    assert!(matches!(
        node_error(run_node(
            57,
            Some(with(vec![10, 21])),
            serial(&data),
            config
        )),
        NodeError::InvalidParam(_)
    ));
    assert!(matches!(
        node_error(run_node(57, None, serial(&data), config)),
        NodeError::InvalidParam(_)
    ));
    let wrong_type = LocalParams {
        refs: vec![(521, RefParam(Arc::new(7u8)))],
        ..LocalParams::default()
    };
    assert!(matches!(
        node_error(run_node(57, Some(wrong_type), serial(&data), config)),
        NodeError::InvalidParam(_)
    ));
    let parser = LocalParams {
        copies: vec![(520, Arc::from(&b"fn"[..]))],
        ..LocalParams::default()
    };
    assert!(matches!(
        node_error(run_node(57, Some(parser), serial(&data), config)),
        NodeError::UnsupportedParam(_)
    ));
}

#[test]
fn zstd_node_applies_parameters_and_refuses_what_the_binding_refuses() {
    let data = text(5000);
    let config = EncoderConfig::default();
    let plain = run_node(62, None, serial(&data), config).unwrap();
    // Level 100 overrides the global level.
    let level_19 = run_node(62, Some(ints(&[(100, 19)])), serial(&data), config).unwrap();
    let global_19 = run_node(
        62,
        None,
        serial(&data),
        config.with_level(Level::try_from(19).unwrap()),
    )
    .unwrap();
    assert_eq!(level_19, global_19);
    // Match-finder, long-distance and flag parameters are accepted.
    for pair in [
        (101, 15),
        (102, 12),
        (103, 12),
        (104, 3),
        (105, 4),
        (106, 32),
        (107, 3),
        (107, 0),
        (160, 1),
        (161, 12),
        (201, 1),
        (202, 0),
        (1002, 2),
        (1011, 1),
    ] {
        assert!(
            run_node(62, Some(ints(&[pair])), serial(&data), config).is_ok(),
            "{pair:?}"
        );
    }
    assert!(plain.len() > 4);
    // The format and the content size flag cannot be modified.
    for pair in [(10, 0), (200, 1)] {
        assert!(matches!(
            node_error(run_node(62, Some(ints(&[pair])), serial(&data), config)),
            NodeError::InvalidParam(_)
        ));
    }
    // Memory caps, ranges and unknown IDs fail the node.
    for pair in [
        (101, 29),
        (102, 28),
        (103, 28),
        (161, 28),
        (107, 99),
        (101, -1),
        (104, 99),
        (12345, 0),
        (400, 1),
    ] {
        assert!(
            matches!(
                node_error(run_node(62, Some(ints(&[pair])), serial(&data), config)),
                NodeError::InvalidParam(_)
            ),
            "{pair:?}"
        );
    }
    // A decompression level of 1 stores literals raw.
    // (The byte-level comparison with the reference is in
    // `tests/encode_zstd.rs`.)
    assert!(run_node(62, None, serial(&data), config.with_decompression_level(1)).is_ok());
    // Keep the shared imports in use.
    let _ = GraphId::ILLEGAL;
    let _ = GraphKind::Store;
}
