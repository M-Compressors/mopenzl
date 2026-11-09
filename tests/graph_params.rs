//! The parameter IDs of `graph::params` keep the reference's values.
#![cfg(feature = "compression")]

use mopenzl::graph::CLUSTERING_TAG_METADATA_ID;
use mopenzl::graph::params::{
    bitunpack, clustering, conversion, dedup, dispatch, divide_by, entropy, field_lz, lz, lz4,
    mux_lengths, partition, sddl, sddl2, segmenters, sentinel, sparse_num, split, split_by_struct,
    tokenize, zstd,
};

#[test]
fn ids_match_the_c_headers() {
    let expected = [
        (zstd::COMPRESSION_LEVEL, 100),
        (zstd::STRATEGY, 107),
        (zstd::TARGET_C_BLOCK_SIZE, 130),
        (zstd::FORMAT, 10),
        (zstd::CONTENT_SIZE_FLAG, 200),
        (lz::COMPRESSION_LEVEL, 1),
        (lz::ACCELERATION, 100),
        (lz::SEARCH_LOG, 106),
        (lz::LITERALS_GRAPH_IDX, 1000),
        (lz::MUX_LENGTHS_GRAPH_IDX, 1004),
        (lz::MIN_GAIN_FOR_ENTROPY_PCT, 10001),
        (lz::MIN_MATCH_LENGTH_METADATA_ID, 77),
        (lz4::COMPRESSION_LEVEL_OVERRIDE, 0),
        (field_lz::COMPRESSION_LEVEL_OVERRIDE, 181),
        (field_lz::EXTRA_MATCH_LENGTHS_GRAPH_OVERRIDE_INDEX, 4),
        (entropy::MIN_GAIN_BYTES, 66),
        (entropy::MIN_GAIN_PCT, 80),
        (divide_by::DIVIDE_BY, 112),
        (bitunpack::NUM_BITS, 1),
        (partition::PRESET, 120),
        (partition::CUSTOM, 121),
        (partition::BITPACK_OPTIMAL, 0),
        (sparse_num::DOMINANT_VALUE, 132),
        (sentinel::INDICES, 130),
        (sentinel::VALUE, 131),
        (tokenize::SORT, 0),
        (split::BYRANGE_MIN_SEGMENT_SIZE, 324),
        (split::CHANNEL_ID, 867),
        (split_by_struct::FIELD_SIZES, 387),
        (dispatch::CHANNEL_ID, 83),
        (dispatch::STRING_NUM_OUTPUTS, 47),
        (dispatch::STRING_INDICES, 48),
        (mux_lengths::SPLIT_POINT, 0),
        (mux_lengths::MATCH_LENGTH_BIAS, 1),
        (conversion::SERIAL_TO_STRUCT_SIZE, 1),
        (dedup::TRUST_IDENTICAL, 9438),
        (segmenters::SERIAL_CHUNK_BYTE_SIZE, 7700),
        (segmenters::NUM_FROM_SERIAL_CHUNK_BYTE_SIZE, 300),
        (segmenters::NUM_FROM_SERIAL_ELT_WIDTH, 301),
        (sddl::DESCRIPTION, 522),
        (sddl2::BYTECODE, 7685),
        (sddl2::CHUNK_BYTE_SIZE, 7686),
        (clustering::TAG_METADATA_ID, 0),
        (clustering::CONFIG, 315),
    ];
    for (id, value) in expected {
        assert_eq!(id, value);
    }
    assert_eq!(CLUSTERING_TAG_METADATA_ID, clustering::TAG_METADATA_ID);
}

#[cfg(all(feature = "zstd", feature = "decompression"))]
#[test]
fn named_parameters_reach_the_nodes() {
    use mopenzl::graph::{GraphBuilder, LocalParams, StandardGraph, StandardNode};
    use mopenzl::{Compressor, Decompressor, EncoderConfig};

    let input = b"named parameters reach the node. ".repeat(400);
    let size = |level: i32| {
        let mut builder = GraphBuilder::new();
        let node = builder
            .parameterize_node(
                StandardNode::Zstd,
                LocalParams::default().with_int(zstd::COMPRESSION_LEVEL, level),
            )
            .unwrap();
        let start = builder
            .static_graph(node, &[StandardGraph::Store.into()])
            .unwrap();
        let mut compressor = Compressor::builder(EncoderConfig::default())
            .with_graph(builder.build(start).unwrap())
            .build()
            .unwrap();
        let frame = compressor.compress(&input).unwrap();
        assert_eq!(Decompressor::default().decompress(&frame).unwrap(), input);
        frame.len()
    };
    assert_ne!(size(1), size(19));
}

/// A graph derived from an ML selector keeps the selector's model when its
/// local parameters are replaced (`ZL_Compressor_parameterizeGraph` keeps
/// the MParam), so it still compresses like the original.
#[cfg(all(feature = "decompression", feature = "zstd", feature = "conversion"))]
#[test]
fn reparameterized_ml_selectors_keep_their_model() {
    use mopenzl::graph::{GraphBuilder, GraphParams, LocalParams, StandardGraph};
    use mopenzl::input::{Input, Numbers};
    use mopenzl::{Compressor, Decompressor, EncoderConfig};

    let bytes: Vec<u8> = (0..4000u32).flat_map(u32::to_le_bytes).collect();
    let numbers = Numbers::try_from((&bytes[..], 4)).unwrap();
    let compress = |derive: bool| {
        let mut builder = GraphBuilder::default();
        let successors = [StandardGraph::Store.into(), StandardGraph::Zstd.into()];
        let mut selector = builder.untrained_ml_selector(&successors).unwrap();
        if derive {
            let params = GraphParams::default().with_params(LocalParams::default().with_int(1, 1));
            selector = builder.parameterize_graph(selector, params).unwrap();
            // Deriving again replaces the stored model with itself.
            selector = builder
                .parameterize_graph(selector, GraphParams::default())
                .unwrap();
        }
        let graph = builder.build(selector).unwrap();
        let mut compressor = Compressor::builder(EncoderConfig::default())
            .with_graph(graph)
            .build()
            .unwrap();
        compressor.compress_inputs(&[Input::from(numbers)]).unwrap()
    };
    let derived = compress(true);
    assert_eq!(derived, compress(false));
    let outputs = Decompressor::default().decompress_typed(&derived).unwrap();
    assert_eq!(outputs[0].content(), bytes);
}
