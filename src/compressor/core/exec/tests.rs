//! Tests of the graph engine against hand-built registries.

use alloc::borrow::Cow;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::compressor::config::EncoderConfig;
use crate::compressor::core::codecs;
#[cfg(all(feature = "zstd", feature = "conversion"))]
use crate::compressor::core::data::Bytes;
use crate::compressor::core::params::RefParam;
#[cfg(all(feature = "zstd", feature = "conversion"))]
use crate::compressor::core::registry::NodeDef;
use crate::compressor::core::registry::{Callback, GraphDef, GraphFn, SelectFn, nodes};
#[cfg(feature = "zstd")]
use crate::compressor::core::sink::SliceSink;
use crate::compressor::core::sink::VecSink;
use crate::{Backend, FormatVersion};

fn params(version: u32) -> Params {
    let config =
        EncoderConfig::default().with_format_version(FormatVersion::try_from(version).unwrap());
    Params::from(&config)
}

fn run_with(
    reg: &Registry,
    start: GraphId,
    params: Params,
    inputs: Vec<Stream<'_>>,
    sink: &mut dyn Sink,
) -> Result<(), ExecError> {
    #[cfg(feature = "zstd")]
    let mut zstd = ZstdNode::new(params.level, Backend::default());
    let resources = Resources {
        #[cfg(feature = "zstd")]
        zstd: &mut zstd,
        simd: Backend::default().0,
        #[cfg(not(feature = "zstd"))]
        marker: core::marker::PhantomData,
    };
    let mut exec = Exec::new(reg, params, resources, None, inputs, sink);
    exec.compress_frame(start, None)
}

fn run(
    reg: &Registry,
    start: GraphId,
    params: Params,
    inputs: Vec<Stream<'_>>,
) -> Result<Vec<u8>, ExecError> {
    let mut out = Vec::new();
    run_with(reg, start, params, inputs, &mut VecSink::new(&mut out))?;
    Ok(out)
}

fn text(len: usize) -> Vec<u8> {
    (0..len).map(|i| b"openzl "[i % 7]).collect()
}

fn registry() -> Registry {
    Registry::new(1, gid::COMPRESS_GENERIC)
}

fn user_graph(
    kind: GraphKind,
    inputs: &'static [u8],
    variable: bool,
    graphs: Vec<GraphId>,
) -> GraphDef {
    GraphDef {
        name: Cow::Borrowed("test"),
        kind,
        inputs: Cow::Borrowed(inputs),
        last_variable: variable,
        custom_graphs: Cow::Owned(graphs),
        custom_nodes: Cow::Borrowed(&[]),
        params: LocalParams::default(),
    }
}

/// A function graph made of a closure-like function pointer.
struct Probe(for<'g, 'e, 'a> fn(&mut GraphCx<'g, 'e, 'a>, &[Edge]) -> Result<(), ExecError>);

impl GraphFn for Probe {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        (self.0)(cx, inputs)
    }
}

fn function(reg: &mut Registry, probe: Probe, mask: u8, graphs: Vec<GraphId>) -> GraphId {
    let callback: Arc<dyn GraphFn> = Arc::new(probe);
    reg.add_graph(user_graph(
        GraphKind::Function(Callback::Shared(callback)),
        mask_slice(mask),
        false,
        graphs,
    ))
}

fn mask_slice(mask: u8) -> &'static [u8] {
    match mask {
        1 => &[1],
        2 => &[2],
        4 => &[4],
        8 => &[8],
        _ => &[15],
    }
}

fn serial(bytes: &[u8]) -> Stream<'_> {
    Stream::serial(bytes)
}

#[test]
fn failing_stub_encoders_name_their_node() {
    let spec = nodes::standard(2).unwrap();
    let params = params(27);
    #[cfg(feature = "zstd")]
    let mut zstd = ZstdNode::new(6, Backend::default());
    let mut cx = NodeCx::new(
        spec,
        None,
        &params,
        #[cfg(feature = "zstd")]
        &mut zstd,
    );
    assert_eq!(
        codecs::DISABLED.encode(&mut cx, &[]),
        Err(NodeError::Disabled("delta_int"))
    );
    assert_eq!(
        codecs::LEGACY.encode(&mut cx, &[]),
        Err(NodeError::Legacy("delta_int"))
    );
    assert!(alloc::format!("{:?}", codecs::DISABLED).contains("Disabled"));
}

#[test]
fn node_contexts_enforce_the_output_contract() {
    let spec = nodes::standard(26).unwrap(); // separate_string_components: serial, numeric
    let params = params(27);
    #[cfg(feature = "zstd")]
    let mut zstd = ZstdNode::new(6, Backend::default());
    #[cfg(feature = "zstd")]
    {
        fn make<'x>(
            spec: &'static crate::compressor::core::registry::NodeSpec,
            params: &'x Params,
            zstd: &'x mut ZstdNode,
        ) -> NodeCx<'x, 'static> {
            NodeCx::new(spec, None, params, zstd)
        }
        let mut cx = make(spec, &params, &mut zstd);
        assert_eq!(cx.spec().name, "separate_string_components");
        assert_eq!(cx.level(), 6);
        assert_eq!(cx.decompression_level(), 3);
        assert!(cx.ints().is_empty());
        assert_eq!(cx.int(1), None);
        assert!(cx.copy_param(1).is_none() && cx.ref_param(1).is_none());
        assert_eq!(cx.emit(5, 1, vec![]), Err(NodeError::OutcomeOutOfRange));
        assert!(cx.emit(1, 3, vec![0; 3]).is_err(), "numeric width 3");
        assert!(cx.emit(0, 2, vec![0; 2]).is_err(), "serial width 2");
        assert_eq!(cx.emit(0, 1, vec![1]), Ok(0));
        assert_eq!(cx.emit(0, 1, vec![1]), Err(NodeError::OutputTwice));
        assert_eq!(cx.send_header(b"h"), Ok(()));
        assert_eq!(cx.send_header(b"h"), Err(NodeError::Header));
        let source = Stream::serial(b"abcd");
        assert!(
            cx.reference(1, &source, 3, 1, 0).is_err(),
            "numeric width 3"
        );
        let mut cx2 = make(spec, &params, &mut zstd);
        assert!(
            cx2.reference(0, &source, 1, 9, 0).is_err(),
            "outside the source"
        );
        assert!(
            cx2.reference(0, &source, usize::MAX, 2, 0).is_err(),
            "overflow"
        );
        assert!(cx2.reference_strings(0, &source, vec![9]).is_err());
        assert_eq!(cx2.reference(0, &source, 1, 2, 1), Ok(0));
        let outputs = cx2.finish();
        assert_eq!(outputs.err(), Some(NodeError::OutputNotCommitted));
        let mut cx3 = make(spec, &params, &mut zstd);
        cx3.emit(0, 1, vec![1]).unwrap();
        cx3.emit(1, 1, vec![2]).unwrap();
        let done = cx3.finish().unwrap();
        assert_eq!(done.singles.len(), 2);
        assert!(done.variable.is_empty() && done.header.is_none());
    }
}

#[test]
fn variable_outputs_follow_the_singletons_in_creation_order() {
    // transpose_split has no singletons and one variable outcome.
    let spec = nodes::standard(3).unwrap();
    let params = params(27);
    #[cfg(feature = "zstd")]
    let mut zstd = ZstdNode::new(6, Backend::default());
    let mut cx = NodeCx::new(
        spec,
        None,
        &params,
        #[cfg(feature = "zstd")]
        &mut zstd,
    );
    assert_eq!(cx.emit(0, 1, vec![1]), Ok(0));
    assert_eq!(cx.emit(0, 1, vec![2]), Ok(1));
    let outputs = cx.finish().unwrap();
    assert!(outputs.singles.is_empty());
    assert_eq!(outputs.variable.len(), 2);
}

#[cfg(all(feature = "zstd", feature = "conversion"))]
#[test]
fn legacy_nodes_and_graph_stubs_fail_with_their_own_errors() {
    let data = text(100);
    let mut builder = registry();
    // A deprecated node.
    let legacy = builder.add_graph(GraphDef {
        custom_nodes: Cow::Owned(vec![NodeId::std(76)]),
        custom_graphs: Cow::Owned(vec![gid::SERIAL_STORE]),
        ..user_graph(GraphKind::Static, &[1], false, vec![])
    });
    let error = run(&builder, legacy, params(27), vec![serial(&data)]).unwrap_err();
    assert_eq!(error, ExecError::NodeVersion("rolz_deprecated"));
    // The stubs of an unported function graph, selector and segmenter.
    use crate::compressor::core::graphs as g;
    use crate::compressor::core::registry::Callback;
    for (kind, expected) in [
        (
            GraphKind::Function(Callback::Static(&g::UNPORTED_FUNCTION)),
            ExecError::Unported,
        ),
        (
            GraphKind::Selector(Callback::Static(&g::UNPORTED_SELECTOR)),
            ExecError::GraphInvalid,
        ),
        (
            GraphKind::Segmenter(Callback::Static(&g::UNPORTED_SEGMENTER)),
            ExecError::Unported,
        ),
    ] {
        let graph = builder.add_graph(user_graph(kind, &[1], false, vec![]));
        let error = run(&builder, graph, params(27), vec![serial(&data)]).unwrap_err();
        assert_eq!(error, expected);
    }
}

#[cfg(all(feature = "zstd", feature = "conversion"))]
#[test]
fn segmenters_check_where_and_what_they_run_on() {
    let reg = registry();
    let data = text(100_000);
    let numeric = Stream::new(Type::Numeric, 1, Bytes::Borrowed(&data));
    // A serial segmenter cannot convert a numeric input.
    assert_eq!(
        run(&reg, gid::SEGMENT_SERIAL, params(27), vec![numeric.clone()]).unwrap_err(),
        ExecError::SegmenterConversion
    );
    // A segmenter after a node ran is out of place.
    let mut registry = registry();
    let after_node = registry.add_graph(GraphDef {
        custom_nodes: Cow::Owned(vec![NodeId::std(19)]),
        custom_graphs: Cow::Owned(vec![GraphId::std(15)]),
        ..user_graph(GraphKind::Static, &[1], false, vec![])
    });
    assert_eq!(
        run(&registry, after_node, params(27), vec![serial(&data)]).unwrap_err(),
        ExecError::SegmenterPlacement
    );
    // The numeric segmenter works on a numeric start.
    let frame = run(
        &reg,
        GraphId::std(15),
        params(27),
        vec![Stream::new(Type::Numeric, 2, Bytes::Borrowed(&data[..400]))],
    );
    assert!(frame.is_ok(), "numeric_compress reaches field_lz");
    // Bad serial segmenter parameters.
    let mut custom = registry.add_graph(user_graph(GraphKind::Illegal, &[1], false, vec![]));
    let _ = &mut custom;
    for (ints, graphs, expected) in [
        (vec![(7700, 0)], vec![gid::STORE], "zero"),
        (vec![(7700, -5)], vec![gid::STORE], "negative"),
        (vec![], vec![], "no successor"),
    ] {
        let rp = RuntimeParams {
            params: Some(LocalParams {
                ints: Cow::Owned(ints),
                ..LocalParams::default()
            }),
            custom_graphs: Some(graphs),
            custom_nodes: None,
        };
        let mut out = Vec::new();
        #[cfg(feature = "zstd")]
        let mut zstd = ZstdNode::new(6, Backend::default());
        let resources = Resources {
            #[cfg(feature = "zstd")]
            zstd: &mut zstd,
            simd: Backend::default().0,
            #[cfg(not(feature = "zstd"))]
            marker: core::marker::PhantomData,
        };
        let mut sink = VecSink::new(&mut out);
        let mut exec = Exec::new(
            &reg,
            params(27),
            resources,
            None,
            vec![serial(&data)],
            &mut sink,
        );
        let error = exec
            .compress_frame(gid::SEGMENT_SERIAL, Some(&rp))
            .unwrap_err();
        assert!(
            matches!(error, ExecError::Parameter(_)),
            "{expected}: {error:?}"
        );
    }
    // num_from_serial: bad width and a chunk smaller than the width.
    for ints in [vec![(301, 3)], vec![(301, 8), (300, 4)]] {
        let rp = RuntimeParams {
            params: Some(LocalParams {
                ints: Cow::Owned(ints),
                ..LocalParams::default()
            }),
            custom_graphs: Some(vec![gid::STORE]),
            custom_nodes: None,
        };
        let mut out = Vec::new();
        let mut zstd = ZstdNode::new(6, Backend::default());
        let resources = Resources {
            zstd: &mut zstd,
            simd: Backend::default().0,
        };
        let mut sink = VecSink::new(&mut out);
        let mut exec = Exec::new(
            &reg,
            params(27),
            resources,
            None,
            vec![serial(&data)],
            &mut sink,
        );
        let error = exec
            .compress_frame(GraphId::std(26), Some(&rp))
            .unwrap_err();
        assert!(matches!(error, ExecError::Parameter(_)), "{error:?}");
    }
    // No successor for num_from_serial either.
    let rp = RuntimeParams {
        custom_graphs: Some(vec![]),
        ..RuntimeParams::default()
    };
    let mut out = Vec::new();
    let mut zstd = ZstdNode::new(6, Backend::default());
    let resources = Resources {
        zstd: &mut zstd,
        simd: Backend::default().0,
    };
    let mut sink = VecSink::new(&mut out);
    let mut exec = Exec::new(
        &reg,
        params(27),
        resources,
        None,
        vec![serial(&data)],
        &mut sink,
    );
    assert!(exec.compress_frame(GraphId::std(26), Some(&rp)).is_err());
}

#[cfg(all(feature = "zstd", feature = "conversion"))]
#[test]
fn nodes_reject_wrong_inputs_versions_and_dictionaries() {
    let data = text(100);
    let mut reg = registry();
    let make = |reg: &mut Registry, node: u32, mask: u8| {
        reg.add_graph(GraphDef {
            custom_nodes: Cow::Owned(vec![NodeId::std(node)]),
            custom_graphs: Cow::Owned(vec![gid::SERIAL_STORE; 2]),
            ..user_graph(GraphKind::Static, mask_slice(mask), false, vec![])
        })
    };
    // separate_string_components takes strings: a serial stream mismatches.
    let wrong_type = make(&mut reg, 26, 15);
    assert!(matches!(
        run(&reg, wrong_type, params(27), vec![serial(&data)]).unwrap_err(),
        ExecError::NodeInputType("separate_string_components")
    ));
    // A node newer than the format version.
    let newer = make(&mut reg, 50, 4);
    let numbers = Stream::new(Type::Numeric, 4, Bytes::from(vec![0u8; 100]));
    assert_eq!(
        run(&reg, newer, params(21), vec![numbers]).unwrap_err(),
        ExecError::NodeVersion("sentinel_byte")
    );
    // A node that needs a dictionary below v25.
    let spec = nodes::standard(62).unwrap();
    let with_dict = reg.add_node(NodeDef {
        #[cfg(feature = "custom-codec")]
        custom: None,
        spec: crate::compressor::core::registry::Callback::Static(spec),
        params: None,
        dict_id: None,
        dict_index: Some(0),
    });
    let graph = reg.add_graph(GraphDef {
        custom_nodes: Cow::Owned(vec![with_dict]),
        custom_graphs: Cow::Owned(vec![gid::SERIAL_STORE]),
        ..user_graph(GraphKind::Static, &[1], false, vec![])
    });
    assert_eq!(
        run(&reg, graph, params(24), vec![serial(&data)]).unwrap_err(),
        ExecError::NodeVersion("zstd")
    );
    // Two inputs for a one-input node, run from a function graph.
    let two = function(
        &mut reg,
        Probe(|cx, edges| {
            cx.run_node(NodeId::std(62), &[edges[0], edges[0]], None)
                .map(drop)
        }),
        15,
        vec![],
    );
    assert!(matches!(
        run(&reg, two, params(27), vec![serial(&data)]).unwrap_err(),
        ExecError::NodeInputCount("zstd")
    ));
    // An unknown node.
    let unknown = function(
        &mut reg,
        Probe(|cx, edges| cx.run_node(NodeId::std(9999), edges, None).map(drop)),
        15,
        vec![],
    );
    assert_eq!(
        run(&reg, unknown, params(27), vec![serial(&data)]).unwrap_err(),
        ExecError::NodeInvalid
    );
}

#[cfg(feature = "zstd")]
#[test]
fn graph_functions_reject_misuse_of_edges_and_destinations() {
    let data = text(100);
    let mut reg = registry();
    let cases: [(Probe, ExecError); 6] = [
        // Running a node on an edge that was already consumed.
        (
            Probe(|cx, edges| {
                cx.run_node(NodeId::std(62), edges, None)?;
                cx.run_node(NodeId::std(62), edges, None).map(drop)
            }),
            ExecError::SuccessorAlreadySet,
        ),
        // Two destinations for one edge fail the invocation.
        (
            Probe(|cx, edges| {
                cx.set_destination(edges, gid::STORE, None)?;
                cx.set_destination(edges, gid::STORE, None)
            }),
            ExecError::SuccessorAlreadySet,
        ),
        // A destination graph that does not exist.
        (
            Probe(|cx, edges| cx.set_destination(edges, GraphId::std(999), None)),
            ExecError::GraphInvalid,
        ),
        // No edge to send.
        (
            Probe(|cx, _| cx.set_destination(&[], gid::STORE, None)),
            ExecError::SuccessorInputs,
        ),
        // Too many edges for a one-input graph.
        (
            Probe(|cx, edges| {
                let outputs = cx.run_node(NodeId::std(62), edges, None)?;
                cx.set_destination(&[outputs[0], Edge(0)], gid::SERIAL_STORE, None)
            }),
            ExecError::SuccessorInputs,
        ),
        // An edge nobody handles.
        (Probe(|_, _| Ok(())), ExecError::SuccessorMissing),
    ];
    for (probe, expected) in cases {
        let graph = function(&mut reg, probe, 15, vec![]);
        assert_eq!(
            run(&reg, graph, params(27), vec![serial(&data)]).unwrap_err(),
            expected
        );
    }
    // Invalid edge handles.
    let invalid = function(
        &mut reg,
        Probe(|cx, _| {
            assert!(cx.data(Edge(77)).is_err());
            assert!(cx.edge_rtsid(Edge(77)).is_err());
            assert!(cx.set_metadata(Edge(77), 1, 1).is_err());
            assert!(cx.run_node(NodeId::std(62), &[Edge(77)], None).is_err());
            assert!(cx.set_destination(&[Edge(77)], gid::STORE, None).is_err());
            Err(ExecError::User("probe done"))
        }),
        15,
        vec![],
    );
    assert_eq!(
        run(&reg, invalid, params(27), vec![serial(&data)]).unwrap_err(),
        ExecError::User("probe done")
    );
    // Metadata travels with the stream.
    let metadata = function(
        &mut reg,
        Probe(|cx, edges| {
            cx.set_metadata(edges[0], 4, 9)?;
            assert_eq!(cx.data(edges[0])?.metadata(), [(4, 9)]);
            assert!(cx.is_node_supported(NodeId::std(62)));
            assert!(!cx.is_node_supported(NodeId::std(9999)));
            cx.set_destination(edges, gid::ZSTD, None)
        }),
        15,
        vec![],
    );
    assert!(run(&reg, metadata, params(27), vec![serial(&data)]).is_ok());
}

struct Choose(GraphId, fn(&mut SelectorCx<'_, '_, '_>));

impl SelectFn for Choose {
    fn select(&self, cx: &mut SelectorCx<'_, '_, '_>, _: &Stream<'_>, _: &[GraphId]) -> GraphId {
        (self.1)(cx);
        self.0
    }
}

#[cfg(feature = "zstd")]
#[test]
fn selectors_see_parameters_and_can_try_graphs() {
    let data = text(5000);
    let mut reg = registry();
    let probe: Arc<dyn SelectFn> = Arc::new(Choose(gid::ZSTD, |cx| {
        assert_eq!((cx.level(), cx.format_version(), cx.depth()), (6, 27, 1));
        assert!(cx.is_node_supported(NodeId::std(62)));
        assert_eq!(cx.input0_mask(gid::ZSTD), Some(1));
        assert_eq!(cx.input0_mask(GraphId::std(999)), None);
        let input = Stream::serial(b"0123456789abcdef");
        assert!(cx.try_graph(&input, gid::STORE).is_ok());
        assert!(cx.try_graph(&input, GraphId::std(999)).is_err());
        let mut local = LocalParams::default();
        local.ints.to_mut().push((100, 3));
        cx.set_successor_params(local);
    }));
    let selector = reg.add_graph(user_graph(
        GraphKind::Selector(Callback::Shared(probe)),
        &[1],
        false,
        vec![],
    ));
    let wrapped = reg.add_graph(user_graph(
        GraphKind::Function(Callback::Static(
            &crate::compressor::core::graphs::COMPRESS_GENERIC,
        )),
        &[15],
        true,
        vec![],
    ));
    let _ = wrapped;
    let frame = run(&reg, selector, params(27), vec![serial(&data)]).unwrap();
    assert!(!frame.is_empty());
    // The selector's local parameters reached zstd (level 3 here).
    let level3 = run(
        &registry(),
        gid::ZSTD,
        {
            let mut p = params(27);
            p.level = 3;
            p
        },
        vec![serial(&data)],
    )
    .unwrap();
    assert_eq!(frame, level3);
}

struct Loop;

impl SelectFn for Loop {
    fn select(&self, _: &mut SelectorCx<'_, '_, '_>, _: &Stream<'_>, _: &[GraphId]) -> GraphId {
        GraphId {
            index: crate::compressor::core::registry::FIRST_CUSTOM_GRAPH,
            owner: 1,
        }
    }
}

#[cfg(feature = "zstd")]
#[test]
fn runaway_recursion_is_cut_at_the_depth_limit() {
    let mut reg = registry();
    let callback: Arc<dyn SelectFn> = Arc::new(Loop);
    let looping = reg.add_graph(user_graph(
        GraphKind::Selector(Callback::Shared(callback)),
        &[1],
        false,
        vec![],
    ));
    // The recursion is a thousand graphs deep: give it room in debug builds.
    let outcome = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn_scoped(scope, || {
                let data = text(100);
                run(&reg, looping, params(27), vec![serial(&data)])
            })
            .unwrap()
            .join()
            .unwrap()
    });
    assert_eq!(outcome.unwrap_err(), ExecError::DepthExceeded);
}

#[cfg(feature = "zstd")]
#[test]
fn full_destinations_store_or_fail() {
    let reg = registry();
    let data = text(4000);
    let mut buffer = [0u8; 12];
    let mut sink = SliceSink::new(&mut buffer);
    assert_eq!(
        run_with(&reg, gid::ZSTD, params(27), vec![serial(&data)], &mut sink).unwrap_err(),
        ExecError::Full
    );
    // A frame header that fits but not the chunk.
    for size in [20usize, 40, 70] {
        let mut buffer = vec![0u8; size];
        let mut sink = SliceSink::new(&mut buffer);
        let result = run_with(&reg, gid::ZSTD, params(27), vec![serial(&data)], &mut sink);
        assert_eq!(result.unwrap_err(), ExecError::Full, "capacity {size}");
        let mut sink = SliceSink::new(&mut buffer);
        let mut no_store = params(27);
        no_store.store_on_expansion = false;
        assert!(run_with(&reg, gid::ZSTD, no_store, vec![serial(&data)], &mut sink).is_err());
    }
}

#[cfg(feature = "zstd")]
#[test]
fn trial_compression_reports_sizes_and_errors() {
    let reg = registry();
    let data = text(4000);
    let mut out = Vec::new();
    let mut zstd = ZstdNode::new(6, Backend::default());
    let resources = Resources {
        zstd: &mut zstd,
        simd: Backend::default().0,
    };
    let mut sink = VecSink::new(&mut out);
    let mut exec = Exec::new(
        &reg,
        params(27),
        resources,
        None,
        vec![serial(&data)],
        &mut sink,
    );
    let stored = exec.try_graph(&[serial(&data)], gid::STORE, None).unwrap();
    let compressed = exec.try_graph(&[serial(&data)], gid::ZSTD, None).unwrap();
    assert!(compressed < stored);
    assert!(exec.try_graph(&[], gid::STORE, None).is_err());
    assert!(
        exec.try_graph(&[serial(&data)], GraphId::std(999), None)
            .is_err()
    );
    assert!(exec.node_supported(NodeId::std(62)) && !exec.node_supported(NodeId::std(0)));
}

#[test]
fn implicit_conversions_follow_the_preference_order() {
    assert_eq!(
        implicit_conversion(Type::Numeric, 2 | 1),
        Some(nid::CONVERT_NUM_TO_STRUCT_LE)
    );
    assert_eq!(
        implicit_conversion(Type::Struct, 1),
        Some(nid::CONVERT_STRUCT_TO_SERIAL)
    );
    assert_eq!(
        implicit_conversion(Type::Numeric, 1),
        Some(nid::CONVERT_NUM_TO_SERIAL_LE)
    );
    assert_eq!(implicit_conversion(Type::Serial, 4), None);
    assert_eq!(implicit_conversion(Type::String, 1), None);
    assert!(conversion_compatible(Type::Numeric, 1));
    assert!(conversion_compatible(Type::Serial, 1));
    assert!(!conversion_compatible(Type::String, 4));
}

// ---- MParam and metadata accessors (ML selector and clustering package) ----

#[cfg(feature = "zstd")]
#[test]
fn graphs_see_their_mparam_and_the_metadata_of_their_inputs() {
    let data = text(200);
    let mut reg = registry();
    let probe: Arc<dyn GraphFn> = Arc::new(Probe(|cx, edges| {
        assert_eq!(
            cx.mparam().and_then(|object| object.downcast_ref::<u32>()),
            Some(&42)
        );
        cx.set_metadata(edges[0], 3, -7)?;
        assert_eq!(cx.int_metadata(edges[0], 3)?, Some(-7));
        assert_eq!(cx.int_metadata(edges[0], 4)?, None);
        assert!(cx.int_metadata(Edge(9), 3).is_err());
        cx.set_destination(edges, gid::ZSTD, None)
    }));
    let with_mparam = reg.add_graph(GraphDef {
        params: LocalParams {
            refs: vec![(MPARAM_REF_ID, RefParam(Arc::new(42u32)))],
            ..LocalParams::default()
        },
        ..user_graph(
            GraphKind::Function(Callback::Shared(probe)),
            mask_slice(15),
            false,
            vec![],
        )
    });
    assert!(run(&reg, with_mparam, params(27), vec![serial(&data)]).is_ok());

    // Without an MParam there is nothing to read.
    let without = function(
        &mut reg,
        Probe(|cx, edges| {
            assert!(cx.mparam().is_none());
            cx.set_destination(edges, gid::ZSTD, None)
        }),
        15,
        vec![],
    );
    assert!(run(&reg, without, params(27), vec![serial(&data)]).is_ok());
}
