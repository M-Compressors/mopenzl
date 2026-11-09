//! The standard graph table (`GR_standardGraphs`, graph_registry.c).
//!
//! Rows are in ID order with the reference's numbering. Static graphs are a
//! node plus successors; selectors, function graphs and segmenters point at
//! the implementations in [`graphs`](super::super::graphs). A package that
//! ports a graph replaces the `unported_*` stub of its row with the real
//! implementation; the row's name, port masks and custom graphs already
//! match the reference.

use alloc::borrow::Cow;

use super::{Callback, GraphDef, GraphId, GraphKind, NodeId};
use crate::compressor::core::graphs as g;
use crate::compressor::core::params::LocalParams;

const SERIAL: u8 = 1;
const STRUCT: u8 = 2;
const NUMERIC: u8 = 4;
const STRING: u8 = 8;
const ANY: u8 = 15;

macro_rules! graph_def {
    ($name:expr, $kind:expr, [$($mask:expr),+], $var:expr, [$($graph:expr),*], [$($node:expr),*], [$($int:expr),*]) => {
        GraphDef {
            name: Cow::Borrowed($name),
            kind: $kind,
            inputs: Cow::Borrowed(&[$($mask),+]),
            last_variable: $var,
            custom_graphs: Cow::Borrowed(&[$(GraphId::std($graph)),*]),
            custom_nodes: Cow::Borrowed(&[$(NodeId::std($node)),*]),
            params: LocalParams {
                ints: Cow::Borrowed(&[$($int),*]),
                copies: alloc::vec::Vec::new(),
                refs: alloc::vec::Vec::new(),
            },
        }
    };
}

/// A node followed by its successors.
macro_rules! node_graph {
    ($name:expr, $mask:expr, $node:expr, [$($succ:expr),*]) => {
        graph_def!($name, GraphKind::Static, [$mask], false, [$($succ),*], [$node], [])
    };
}

macro_rules! selector {
    ($name:expr, $mask:expr, $f:expr) => {
        graph_def!(
            $name,
            GraphKind::Selector(Callback::Static($f)),
            [$mask],
            false,
            [],
            [],
            []
        )
    };
}

macro_rules! function {
    ($name:expr, $mask:expr, $var:expr, $f:expr) => {
        graph_def!(
            $name,
            GraphKind::Function(Callback::Static($f)),
            [$mask],
            $var,
            [],
            [],
            []
        )
    };
}

/// A segmenter taking one serial input and handing chunks to `succ`.
macro_rules! segmenter {
    ($name:expr, $mask:expr, $f:expr, [$($succ:expr),*]) => {
        graph_def!($name, GraphKind::Segmenter(Callback::Static($f)), [$mask], false, [$($succ),*], [], [])
    };
}

/// Every standard graph, indexed by `ZL_GraphID`.
pub(crate) static STANDARD_GRAPHS: [GraphDef; 91] = [
    /* 0 */ GraphDef::ILLEGAL,
    /* 1 */
    graph_def!(
        "!zl.private.serial_store",
        GraphKind::Store,
        [SERIAL],
        false,
        [],
        [],
        []
    ),
    /* 2 */ function!("!zl.store", ANY, true, &g::STORE),
    /* 3 */ function!("!zl.fse", SERIAL, false, &g::FSE),
    /* 4 */
    function!("!zl.huffman", SERIAL | STRUCT | NUMERIC, false, &g::HUFFMAN),
    /* 5 */
    function!(
        "!zl.huffman_huf0",
        SERIAL | STRUCT | NUMERIC,
        false,
        &g::HUFFMAN_HUF0
    ),
    /* 6 */
    function!(
        "!zl.huffman_pivco",
        SERIAL | STRUCT | NUMERIC,
        false,
        &g::HUFFMAN_PIVCO
    ),
    /* 7 */
    function!("!zl.entropy", SERIAL | STRUCT | NUMERIC, false, &g::ENTROPY),
    /* 8 */
    selector!("!zl.constant", SERIAL | STRUCT | NUMERIC, &g::CONSTANT),
    /* 9 */ node_graph!("!zl.zstd", SERIAL, 62, [1]),
    /* 10 */ selector!("!zl.bitpack", SERIAL | NUMERIC, &g::BITPACK),
    /* 11 */ node_graph!("!zl.flatpack", SERIAL, 65, [1, 1]),
    /* 12 */
    function!("!zl.field_lz", STRUCT | NUMERIC, false, &g::FIELD_LZ),
    /* 13 */ function!("!zl.compress_generic", ANY, true, &g::COMPRESS_GENERIC),
    /* 14 */
    selector!(
        "!zl.select_generic_lz_backend",
        SERIAL,
        &g::SELECT_GENERIC_LZ
    ),
    /* 15 */ segmenter!("!zl.segmenter_numeric", NUMERIC, &g::SEGMENT_NUMERIC, []),
    /* 16 */ selector!("!zl.select_numeric", NUMERIC, &g::SELECT_NUMERIC),
    /* 17 */ function!("!zl.ml_selector", NUMERIC, false, &g::ML_SELECTOR),
    /* 18 */ function!("!zl.cluster", ANY, true, &g::CLUSTERING),
    /* 19 */ function!("!zl.try_parse_int", STRING, false, &g::TRY_PARSE_INT),
    /* 20 */ function!("!zl.sddl", SERIAL, false, &g::SDDL),
    /* 21 */ segmenter!("!zl.sddl2", SERIAL, &g::SDDL2, []),
    /* 22 */ node_graph!("!zl.lz4", SERIAL, 86, [1]),
    /* 23 */
    function!(
        "!zl.partition_bitpack",
        NUMERIC,
        false,
        &g::PARTITION_BITPACK
    ),
    /* 24 */
    graph_def!(
        "!zl.segment_num8_from_serial",
        GraphKind::Segmenter(Callback::Static(&g::SEGMENT_NUM_FROM_SERIAL)),
        [SERIAL],
        false,
        [65],
        [],
        [(301, 1)]
    ),
    /* 25 */
    graph_def!(
        "!zl.segment_num16_from_serial",
        GraphKind::Segmenter(Callback::Static(&g::SEGMENT_NUM_FROM_SERIAL)),
        [SERIAL],
        false,
        [66],
        [],
        [(301, 2)]
    ),
    /* 26 */
    graph_def!(
        "!zl.segment_num32_from_serial",
        GraphKind::Segmenter(Callback::Static(&g::SEGMENT_NUM_FROM_SERIAL)),
        [SERIAL],
        false,
        [67],
        [],
        [(301, 4)]
    ),
    /* 27 */
    graph_def!(
        "!zl.segment_num64_from_serial",
        GraphKind::Segmenter(Callback::Static(&g::SEGMENT_NUM_FROM_SERIAL)),
        [SERIAL],
        false,
        [68],
        [],
        [(301, 8)]
    ),
    /* 28 */ function!("!zl.lz", SERIAL, false, &g::LZ),
    /* 29 */ segmenter!("!zl.segment_serial", SERIAL, &g::SEGMENT_SERIAL, [13]),
    /* 30 */
    function!(
        "!zl.transformer_numeric",
        NUMERIC,
        true,
        &g::TRANSFORMER_NUMERIC
    ),
    /* 31 */ selector!("!zl.brute_force", ANY, &g::BRUTE_FORCE),
    /* 32 */ selector!("!zl.private.store1", ANY, &g::STORE1),
    /* 33 */ node_graph!("!zl.private.string_store", STRING, 26, [1, 1]),
    /* 34 */ selector!("!zl.private.compress2", ANY, &g::COMPRESS1),
    /* 35 */ selector!("!zl.private.serial_compress", SERIAL, &g::SERIAL_COMPRESS),
    /* 36 */ selector!("!zl.private.struct_compress", STRUCT, &g::STRUCT_COMPRESS),
    /* 37 */
    selector!(
        "!zl.private.numeric_compress",
        NUMERIC,
        &g::NUMERIC_COMPRESS
    ),
    /* 38 */ selector!("!zl.private.string_compress", STRING, &g::STRING_COMPRESS),
    /* 39 */ node_graph!("!zl.private.string_separate_compress", STRING, 26, [35, 37]),
    /* 40 */ node_graph!("!zl.private.bitpack_serial", SERIAL, 63, [1]),
    /* 41 */ node_graph!("!zl.private.bitpack_int", NUMERIC, 64, [1]),
    /* 42 */ node_graph!("!zl.private.constant_serial", SERIAL, 70, [1]),
    /* 43 */ node_graph!("!zl.private.constant_fixed", STRUCT, 71, [1]),
    /* 44 */ node_graph!("!zl.private.fse_ncount", NUMERIC, 61, [1]),
    /* 45 */
    function!(
        "!zl.private.field_lz_literals",
        STRUCT,
        false,
        &g::FIELD_LZ_LITERALS
    ),
    /* 46 */
    selector!(
        "!zl.private.field_lz_literals_channel",
        SERIAL,
        &g::FIELD_LZ_LITERALS_CHANNEL
    ),
    /* 47 */ node_graph!("!zl.private.delta_huffman", SERIAL, 19, [50]),
    /* 48 */ node_graph!("!zl.private.delta_flatpack", SERIAL, 19, [51]),
    /* 49 */ node_graph!("!zl.private.delta_zstd", SERIAL, 19, [52]),
    /* 50 */ node_graph!("!zl.private.delta_huffman_internal", NUMERIC, 2, [4]),
    /* 51 */ node_graph!("!zl.private.flatpack_internal", NUMERIC, 2, [11]),
    /* 52 */ node_graph!("!zl.private.zstd_internal", NUMERIC, 2, [9]),
    /* 53 */ node_graph!("!zl.private.delta_field_lz", NUMERIC, 2, [12]),
    /* 54 */ node_graph!("!zl.private.range_pack", NUMERIC, 28, [12]),
    /* 55 */ node_graph!("!zl.private.range_pack_zstd", NUMERIC, 28, [9]),
    /* 56 */ node_graph!("!zl.private.tokenize_delta_field_lz", NUMERIC, 72, [53, 12]),
    /* 57 */
    function!("!zl.private.split_serial", SERIAL, false, &g::SPLIT),
    /* 58 */
    function!("!zl.private.split_struct", STRUCT, false, &g::SPLIT),
    /* 59 */
    function!("!zl.private.split_numeric", NUMERIC, false, &g::SPLIT),
    /* 60 */
    function!("!zl.private.split_string", STRING, false, &g::SPLIT),
    /* 61 */
    function!("!zl.private.sddl2_chunk", SERIAL, false, &g::SDDL2_CHUNK),
    /* 62 */ function!("!zl.n_to_n", ANY, true, &g::N_TO_N),
    /* 63 */
    function!(
        "!zl.private.merge_sorted_selector",
        NUMERIC,
        false,
        &g::MERGE_SORTED_GRAPH
    ),
    /* 64 */
    function!(
        "!zl.private.transpose_split_selector",
        STRUCT,
        false,
        &g::TRANSPOSE_SPLIT_SELECTOR
    ),
    /* 65 */ node_graph!("!zl.private.interpret_num8_compress", SERIAL, 19, [37]),
    /* 66 */ node_graph!("!zl.private.interpret_num16_compress", SERIAL, 20, [37]),
    /* 67 */ node_graph!("!zl.private.interpret_num32_compress", SERIAL, 21, [37]),
    /* 68 */ node_graph!("!zl.private.interpret_num64_compress", SERIAL, 22, [37]),
    /* 69 */
    function!(
        "!zl.compress_small_lengths",
        NUMERIC,
        false,
        &g::COMPRESS_SMALL_LENGTHS
    ),
    /* 70 */ node_graph!("!zl.private.transformer_delta_int", NUMERIC, 2, [90]),
    /* 71 */ node_graph!("!zl.private.transformer_range_pack", NUMERIC, 28, [90]),
    /* 72 */ node_graph!("!zl.private.transformer_divide_by_gcd", NUMERIC, 31, [90]),
    /* 73 */
    node_graph!(
        "!zl.private.transformer_tokenize_numeric",
        NUMERIC,
        41,
        [90, 90]
    ),
    /* 74 */
    node_graph!(
        "!zl.private.transformer_tokenize_numeric_sorted",
        NUMERIC,
        72,
        [90, 90]
    ),
    /* 75 */ node_graph!("!zl.private.transformer_sparse_num", NUMERIC, 54, [90, 90]),
    /* 76 */
    selector!(
        "!zl.private.transformer_static_core",
        NUMERIC,
        &g::STATIC_CORE
    ),
    /* 77 */ node_graph!("!zl.private.transformer_static_delta", NUMERIC, 2, [76]),
    /* 78 */
    node_graph!(
        "!zl.private.transformer_static_range_pack",
        NUMERIC,
        28,
        [76]
    ),
    /* 79 */
    node_graph!(
        "!zl.private.transformer_static_range_pack_delta",
        NUMERIC,
        28,
        [77]
    ),
    /* 80 */ node_graph!("!zl.private.transformer_static_zigzag", NUMERIC, 4, [76]),
    /* 81 */ node_graph!("!zl.private.transformer_static_gcd", NUMERIC, 31, [78]),
    /* 82 */
    selector!(
        "!zl.private.transformer_static_index",
        NUMERIC,
        &g::STATIC_INDEX
    ),
    /* 83 */
    node_graph!(
        "!zl.private.transformer_static_tok_sorted",
        NUMERIC,
        72,
        [77, 82]
    ),
    /* 84 */ node_graph!("!zl.private.transformer_static_delta_tok", NUMERIC, 2, [83]),
    /* 85 */
    node_graph!(
        "!zl.private.transformer_static_tok_mono",
        NUMERIC,
        72,
        [9, 7]
    ),
    /* 86 */
    node_graph!(
        "!zl.private.transformer_static_delta_tok_mono",
        NUMERIC,
        2,
        [85]
    ),
    /* 87 */
    node_graph!(
        "!zl.private.transformer_static_tok_mono_lz",
        NUMERIC,
        72,
        [9, 9]
    ),
    /* 88 */
    node_graph!(
        "!zl.private.transformer_static_delta_tok_mono_lz",
        NUMERIC,
        2,
        [87]
    ),
    /* 89 */
    selector!(
        "!zl.private.transformer_static_fallback",
        NUMERIC,
        &g::STATIC_FALLBACK
    ),
    /* 90 */
    selector!(
        "!zl.private.transformer_numeric1",
        NUMERIC,
        &g::TRANSFORMER_NUMERIC1
    ),
];
