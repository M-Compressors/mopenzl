//! The `field_lz` graphs (`codecs/lz/encode_lz_binding.c`,
//! `codecs/lz/encode_field_lz_literals_selector.c`): the `field_lz` graph
//! (`EI_fieldLzDynGraph`, graph 12), its private literals graph
//! (`EI_fieldLzLiteralsDynGraph`) and the per-channel literals selector
//! (`SI_fieldLzLiteralsChannelSelector`).

use alloc::borrow::Cow;

use super::super::data::{Stream, Type};
use super::super::error::ExecError;
use super::super::exec::{Edge, GraphCx, SelectorCx};
use super::super::params::LocalParams;
use super::super::registry::{GraphFn, GraphId, NodeId, SelectFn};
use crate::shared::stats::data_stats::DataStatsU8;

/// `ZL_FIELD_LZ_COMPRESSION_LEVEL_OVERRIDE_PID`.
const COMPRESSION_LEVEL_OVERRIDE_PID: i32 =
    crate::graph::params::field_lz::COMPRESSION_LEVEL_OVERRIDE;
/// `FIELDLZ_NUM_SUCCESSORS`.
const NUM_SUCCESSORS: usize = 5;

const NODE_TRANSPOSE_SPLIT: NodeId = NodeId::std(3);
const NODE_FIELD_LZ: NodeId = NodeId::std(9);
const NODE_CONVERT_NUM_TO_TOKEN: NodeId = NodeId::std(11);
const NODE_INTERPRET_TOKEN_AS_LE: NodeId = NodeId::std(17);
const NODE_QUANTIZE_OFFSETS: NodeId = NodeId::std(43);
const NODE_QUANTIZE_LENGTHS: NodeId = NodeId::std(44);
const NODE_TRANSPOSE_SPLIT2_DEPRECATED: NodeId = NodeId::std(83);
const NODE_TRANSPOSE_SPLIT4_DEPRECATED: NodeId = NodeId::std(84);
const NODE_TRANSPOSE_SPLIT8_DEPRECATED: NodeId = NodeId::std(85);
/// `ZL_NODE_ILLEGAL`.
const NODE_ILLEGAL: NodeId = NodeId::std(0);

const GRAPH_STORE: GraphId = GraphId::std(2);
const GRAPH_FSE: GraphId = GraphId::std(3);
const GRAPH_HUFFMAN: GraphId = GraphId::std(4);
const GRAPH_ZSTD: GraphId = GraphId::std(9);
const GRAPH_BITPACK: GraphId = GraphId::std(10);
const GRAPH_FLATPACK: GraphId = GraphId::std(11);
const GRAPH_FIELD_LZ_LITERALS: GraphId = GraphId::std(45);
const GRAPH_FIELD_LZ_LITERALS_CHANNEL: GraphId = GraphId::std(46);
const GRAPH_CONSTANT_SERIAL: GraphId = GraphId::std(42);
const GRAPH_DELTA_HUFFMAN: GraphId = GraphId::std(47);
const GRAPH_DELTA_FLATPACK: GraphId = GraphId::std(48);
const GRAPH_DELTA_ZSTD: GraphId = GraphId::std(49);

/// `EI_fieldLzDynGraph` (`ZL_GRAPH_FIELD_LZ`): runs the `field_lz` node and
/// sends its five streams (literals, tokens, offsets, extra literal lengths,
/// extra match lengths) to their entropy graphs.
///
/// Widths other than 2, 4 and 8 go to Zstandard. Numeric input is first
/// reinterpreted as structs. Streams under the `minStreamSize` parameter are
/// stored, and local int parameter `i` (0 to 4) replaces stream `i`'s graph
/// with custom graph number `value`.
#[derive(Debug)]
pub(crate) struct FieldLzGraph;

/// `EI_fieldLzLiteralsDynGraph` (the private `field_lz_literals` graph):
/// transposes wide literals into byte channels and sends each to the
/// channel selector.
#[derive(Debug)]
pub(crate) struct FieldLzLiteralsGraph;

/// `SI_fieldLzLiteralsChannelSelector`
/// (`ZS2_transposedLiteralStreamSelector_impl`): picks the coder of one
/// literal byte channel from statistics and trial compressions.
#[derive(Debug)]
pub(crate) struct FieldLzLiteralsChannelSelector;

/// See [`FieldLzGraph`].
pub(crate) static FIELD_LZ: FieldLzGraph = FieldLzGraph;
/// See [`FieldLzLiteralsGraph`].
pub(crate) static FIELD_LZ_LITERALS: FieldLzLiteralsGraph = FieldLzLiteralsGraph;
/// See [`FieldLzLiteralsChannelSelector`].
pub(crate) static FIELD_LZ_LITERALS_CHANNEL: FieldLzLiteralsChannelSelector =
    FieldLzLiteralsChannelSelector;

/// `tokensDynGraph`.
fn tokens_dyn_graph(cx: &mut GraphCx<'_, '_, '_>, tokens: Edge) -> Result<(), ExecError> {
    if cx.ex.p.decompression_level <= 1 || cx.data(tokens)?.n_elts() <= 128 {
        let streams = cx.run_node(NODE_INTERPRET_TOKEN_AS_LE, &[tokens], None)?;
        let [numbers] = streams[..] else {
            return Err(ExecError::GraphInvalid);
        };
        cx.set_destination(&[numbers], GRAPH_BITPACK, None)
    } else {
        cx.set_destination(&[tokens], GRAPH_HUFFMAN, None)
    }
}

/// `quantizeDynGraph`.
fn quantize_dyn_graph(
    cx: &mut GraphCx<'_, '_, '_>,
    stream: Edge,
    quantize_node: NodeId,
) -> Result<(), ExecError> {
    let streams = cx.run_node(quantize_node, &[stream], None)?;
    let [codes, extra_bits] = streams[..] else {
        return Err(ExecError::GraphInvalid);
    };
    let codes_graph = if cx.ex.p.decompression_level <= 1 {
        GRAPH_BITPACK
    } else {
        GRAPH_FSE
    };
    cx.set_destination(&[codes], codes_graph, None)?;
    cx.set_destination(&[extra_bits], GRAPH_STORE, None)
}

impl GraphFn for FieldLzGraph {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let [mut input] = *inputs else {
            return Err(ExecError::Parameter("field_lz takes exactly one input"));
        };
        let (ty, width) = {
            let data = cx.data(input)?;
            (data.ty(), data.width())
        };
        // Zstandard for unsupported widths.
        if !matches!(width, 2 | 4 | 8) {
            return cx.set_destination(&[input], GRAPH_ZSTD, None);
        }
        // Convert to struct.
        if ty == Type::Numeric {
            let streams = cx.run_node(NODE_CONVERT_NUM_TO_TOKEN, &[input], None)?;
            let [token] = streams[..] else {
                return Err(ExecError::GraphInvalid);
            };
            input = token;
        }
        let override_level = cx.eff.params.int(COMPRESSION_LEVEL_OVERRIDE_PID);
        let streams = match override_level {
            Some(level) => {
                let params = LocalParams {
                    ints: Cow::Owned(alloc::vec![(COMPRESSION_LEVEL_OVERRIDE_PID, level)]),
                    ..LocalParams::default()
                };
                cx.run_node(NODE_FIELD_LZ, &[input], Some(&params))?
            }
            None => cx.run_node(NODE_FIELD_LZ, &[input], None)?,
        };
        let streams: [Edge; NUM_SUCCESSORS] = streams[..]
            .try_into()
            .map_err(|_| ExecError::GraphInvalid)?;

        // Allow overriding each successor with a custom graph, and store the
        // streams below the configured limit.
        let mut set = [false; NUM_SUCCESSORS];
        let custom_graphs = cx.eff.custom_graphs;
        let limit = usize::try_from(cx.ex.p.min_stream_size).unwrap_or(0);
        for (index, &stream) in streams.iter().enumerate() {
            if cx.data(stream)?.content_size() < limit {
                cx.set_destination(&[stream], GRAPH_STORE, None)?;
                set[index] = true;
                continue;
            }
            if let Some(value) = cx.eff.params.int(index as i32) {
                let graph = usize::try_from(value)
                    .ok()
                    .and_then(|value| custom_graphs.get(value))
                    .ok_or(ExecError::Parameter("invalid node parameter"))?;
                cx.set_destination(&[stream], *graph, None)?;
                set[index] = true;
            }
        }

        let [
            literals,
            tokens,
            offsets,
            extra_literal_lengths,
            extra_match_lengths,
        ] = streams;
        if !set[0] {
            cx.set_destination(&[literals], GRAPH_FIELD_LZ_LITERALS, None)?;
        }
        if !set[1] {
            tokens_dyn_graph(cx, tokens)?;
        }
        if !set[2] {
            quantize_dyn_graph(cx, offsets, NODE_QUANTIZE_OFFSETS)?;
        }
        if !set[3] {
            quantize_dyn_graph(cx, extra_literal_lengths, NODE_QUANTIZE_LENGTHS)?;
        }
        if !set[4] {
            quantize_dyn_graph(cx, extra_match_lengths, NODE_QUANTIZE_LENGTHS)?;
        }
        Ok(())
    }
}

/// `ZL_Graph_getTransposeSplitNode`.
fn transpose_split_node(cx: &GraphCx<'_, '_, '_>, width: usize) -> NodeId {
    if cx.is_node_supported(NODE_TRANSPOSE_SPLIT) {
        return NODE_TRANSPOSE_SPLIT;
    }
    match width {
        2 => NODE_TRANSPOSE_SPLIT2_DEPRECATED,
        4 => NODE_TRANSPOSE_SPLIT4_DEPRECATED,
        8 => NODE_TRANSPOSE_SPLIT8_DEPRECATED,
        _ => NODE_ILLEGAL,
    }
}

impl GraphFn for FieldLzLiteralsGraph {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let [literals] = *inputs else {
            return Err(ExecError::Parameter(
                "field_lz_literals takes exactly one input",
            ));
        };
        // Always transposing is the reference's choice; see its TODO.
        let width = cx.data(literals)?.width();
        if width == 1 {
            return cx.set_destination(&[literals], GRAPH_FIELD_LZ_LITERALS_CHANNEL, None);
        }
        let transpose = transpose_split_node(cx, width);
        let streams = cx.run_node(transpose, &[literals], None)?;
        if streams.len() != width {
            return Err(ExecError::GraphInvalid);
        }
        for &stream in &streams {
            cx.set_destination(&[stream], GRAPH_FIELD_LZ_LITERALS_CHANNEL, None)?;
        }
        Ok(())
    }
}

/// `reportToSize`: the size of a trial compression, `(size_t)-1` if it failed.
fn trial_size(cx: &mut SelectorCx<'_, '_, '_>, input: &Stream<'_>, graph: GraphId) -> usize {
    cx.try_graph(input, graph).unwrap_or(usize::MAX)
}

/// `gainSize`: `size` grown by `gain` percent, with the reference's
/// `size_t` wraparound.
const fn gain_size(size: usize, gain: usize) -> usize {
    let grown = size.wrapping_add(size.wrapping_mul(gain) / 100);
    if size > grown { size } else { grown }
}

/// `ZL_Selector_isConstantSupported`.
const fn constant_supported(cx: &SelectorCx<'_, '_, '_>) -> bool {
    cx.format_version() >= 11
}

/// Index of the first minimum (strictly smaller wins, as in the reference).
fn best_graph(graphs: &[GraphId], sizes: &[usize]) -> GraphId {
    let mut best = 0;
    for index in 1..graphs.len() {
        if sizes[index] < sizes[best] {
            best = index;
        }
    }
    graphs[best]
}

/// `ZL_fastTransposedLiteralStreamSelector` (decompression level 1).
fn fast_selector(cx: &mut SelectorCx<'_, '_, '_>, input: &Stream<'_>) -> GraphId {
    let input_size = input.n_elts();
    let mut stats = DataStatsU8::new(input.bytes());
    let graphs = [
        GRAPH_STORE,
        GRAPH_CONSTANT_SERIAL,
        GRAPH_BITPACK,
        GRAPH_FLATPACK,
        GRAPH_DELTA_FLATPACK,
    ];
    let gain = [0usize, 0, 5, 15, 25];
    let mut sizes = [
        input_size,
        stats.constant_size(),
        stats.bitpacked_size(),
        stats.flatpacked_size(),
        0,
    ];
    for index in 0..graphs.len() {
        if sizes[index] == 0 {
            sizes[index] = trial_size(cx, input, graphs[index]);
        }
        sizes[index] = gain_size(sizes[index], gain[index]);
    }
    if !constant_supported(cx) {
        sizes[1] = usize::MAX;
    }
    best_graph(&graphs, &sizes)
}

impl SelectFn for FieldLzLiteralsChannelSelector {
    fn select(
        &self,
        cx: &mut SelectorCx<'_, '_, '_>,
        input: &Stream<'_>,
        _: &[GraphId],
    ) -> GraphId {
        if cx.ex.p.decompression_level == 1 {
            return fast_selector(cx, input);
        }
        let input_size = input.n_elts();
        let level = cx.level();
        const DELTA_GAIN: usize = 4;
        const HUFF_GAIN: usize = 2;
        const ZSTD_GAIN: usize = 4;

        let mut stats = DataStatsU8::new(input.bytes());
        if stats.cardinality() == 1 {
            if input_size > 1 && constant_supported(cx) {
                return GRAPH_CONSTANT_SERIAL;
            }
            if input_size > 20 {
                return GRAPH_HUFFMAN;
            }
        }
        if input_size < 200 {
            return GRAPH_STORE;
        }

        // Whether to delta before Huffman.
        let (huff_size, delta_huff_size) = if level <= 3 {
            (
                stats.estimate_huffman_size_fast(false),
                stats.estimate_huffman_size_fast(true),
            )
        } else if level < 7 {
            (stats.huffman_size(), stats.delta_huffman_size())
        } else {
            (
                trial_size(cx, input, GRAPH_HUFFMAN),
                trial_size(cx, input, GRAPH_DELTA_HUFFMAN),
            )
        };
        let delta = gain_size(delta_huff_size, DELTA_GAIN) < huff_size;
        let best_huff_size = if delta { delta_huff_size } else { huff_size };

        // Without enough ratio, don't compress at all.
        if gain_size(best_huff_size, HUFF_GAIN) >= input_size {
            return GRAPH_STORE;
        }

        let mut graphs = [GRAPH_STORE, GRAPH_HUFFMAN, GRAPH_ZSTD];
        if delta {
            graphs[1] = GRAPH_DELTA_HUFFMAN;
            graphs[2] = GRAPH_DELTA_ZSTD;
        }
        let gain = [0, HUFF_GAIN, ZSTD_GAIN];
        let mut sizes = [input_size, best_huff_size, 0];
        for index in 0..graphs.len() {
            if sizes[index] == 0 {
                sizes[index] = trial_size(cx, input, graphs[index]);
            }
            sizes[index] = gain_size(sizes[index], gain[index]);
        }
        best_graph(&graphs, &sizes)
    }
}
