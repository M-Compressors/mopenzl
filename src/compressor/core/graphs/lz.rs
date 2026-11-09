//! The `lz` graph (`codecs/lz/encode_lz_binding.c`, `EI_lzDynGraph`,
//! `ZL_GRAPH_LZ`, graph 28): runs the `lz` node and sends its four streams
//! (literals, offsets, literal lengths, match lengths) to entropy coders.

use super::super::error::ExecError;
use super::super::exec::{Edge, GraphCx};
use super::super::registry::{GraphFn, GraphId, NodeId};
use super::entropy::set_entropy_destination;

/// `ZL_LzParam_compressionLevel`.
const LZ_PARAM_COMPRESSION_LEVEL: i32 = crate::graph::params::lz::COMPRESSION_LEVEL;
/// `ZL_LzParam_literalsGraphIdx`.
const LITERALS_GRAPH_IDX: i32 = crate::graph::params::lz::LITERALS_GRAPH_IDX;
/// `ZL_LzParam_offsetsGraphIdx`.
const OFFSETS_GRAPH_IDX: i32 = crate::graph::params::lz::OFFSETS_GRAPH_IDX;
/// `ZL_LzParam_muxedBytesGraphIdx`.
const MUXED_BYTES_GRAPH_IDX: i32 = crate::graph::params::lz::MUXED_BYTES_GRAPH_IDX;
/// `ZL_LzParam_overflowLengthsGraphIdx`.
const OVERFLOW_LENGTHS_GRAPH_IDX: i32 = crate::graph::params::lz::OVERFLOW_LENGTHS_GRAPH_IDX;
/// `ZL_LzParam_muxLengthsGraphIdx`.
const MUX_LENGTHS_GRAPH_IDX: i32 = crate::graph::params::lz::MUX_LENGTHS_GRAPH_IDX;
/// `ZL_LzParam_minGainForEntropyBytes`.
const MIN_GAIN_BYTES: i32 = crate::graph::params::lz::MIN_GAIN_FOR_ENTROPY_BYTES;
/// `ZL_LzParam_minGainForEntropyPct`.
const MIN_GAIN_PCT: i32 = crate::graph::params::lz::MIN_GAIN_FOR_ENTROPY_PCT;

/// `ZL_HUFFMAN_PIVCO_MIN_FORMAT_VERSION`.
const HUFFMAN_PIVCO_MIN_FORMAT_VERSION: u32 = 27;

const NODE_LZ: NodeId = NodeId::std(52);
const NODE_MUX_LENGTHS: NodeId = NodeId::std(53);

const GRAPH_STORE: GraphId = GraphId::std(2);
const GRAPH_HUFFMAN: GraphId = GraphId::std(4);
const GRAPH_HUFFMAN_PIVCO: GraphId = GraphId::std(6);
const GRAPH_BITPACK: GraphId = GraphId::std(10);
const GRAPH_PARTITION_BITPACK: GraphId = GraphId::std(23);
const GRAPH_COMPRESS_SMALL_LENGTHS: GraphId = GraphId::std(69);

/// `EI_lzDynGraph`.
///
/// Local parameters (forwarded to the `lz` node): the node's own, plus the
/// graph indices `1000..=1004` selecting custom graphs for the literals,
/// offsets, muxed bytes, overflow lengths and for all lengths together, and
/// the minimum Huffman gain `10000` (bytes) and `10001` (percent).
#[derive(Debug)]
pub(crate) struct LzGraph;

/// See [`LzGraph`].
pub(crate) static LZ: LzGraph = LzGraph;

/// `getIntParamOrDefault`.
fn int_param(cx: &GraphCx<'_, '_, '_>, key: i32, default: i32) -> i32 {
    cx.eff.params.int(key).unwrap_or(default)
}

/// `getGraph`: custom graph number `param`, else `default`; an index out of
/// range is `ZL_GRAPH_ILLEGAL`.
fn graph_param(cx: &GraphCx<'_, '_, '_>, key: i32, default: GraphId) -> GraphId {
    match cx.eff.params.int(key) {
        None => default,
        Some(index) => usize::try_from(index)
            .ok()
            .and_then(|index| cx.eff.custom_graphs.get(index))
            .copied()
            .unwrap_or(GraphId::ILLEGAL),
    }
}

/// `setEntropyDestinationOrOverride`: an invalid override index keeps the
/// default graph.
fn set_entropy_or_override(
    cx: &mut GraphCx<'_, '_, '_>,
    edge: Edge,
    key: i32,
    entropy_graph: GraphId,
    min_gain_bytes: i32,
    min_gain_pct: i32,
) -> Result<(), ExecError> {
    let over = graph_param(cx, key, GraphId::ILLEGAL);
    let graph = if over == GraphId::ILLEGAL {
        entropy_graph
    } else {
        over
    };
    set_entropy_destination(cx, edge, graph, min_gain_bytes, min_gain_pct)
}

/// `huffmanGraph`.
const fn huffman_graph(format_version: u32, level: i32, src_size: usize) -> GraphId {
    if level < 0 {
        GRAPH_STORE
    } else if format_version >= HUFFMAN_PIVCO_MIN_FORMAT_VERSION && src_size >= (1 << 14) {
        // PivCo decodes significantly faster on larger inputs.
        GRAPH_HUFFMAN_PIVCO
    } else {
        GRAPH_HUFFMAN
    }
}

/// `guessMuxedEntropySize`: an optimistic estimate of the compressed muxed
/// lengths.
fn guess_muxed_entropy_size(num_sequences: usize, min_gain_bytes: i32, min_gain_pct: i32) -> usize {
    let mut add = usize::try_from(min_gain_bytes).unwrap_or(0);
    if min_gain_pct > 0 {
        add = add.max(num_sequences.wrapping_mul(min_gain_pct as usize) / 100);
    }
    let encoded_size = num_sequences.wrapping_mul(6) / 10;
    let header_size = 75;
    header_size + encoded_size + add
}

impl GraphFn for LzGraph {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let [input] = *inputs else {
            return Err(ExecError::Parameter("lz takes exactly one input"));
        };
        let input_size = cx.data(input)?.content_size();
        // Huffman must save at least this much to be considered.
        let min_gain_bytes = int_param(cx, MIN_GAIN_BYTES, (input_size / 200) as i32 + 50);
        let min_gain_pct = int_param(cx, MIN_GAIN_PCT, -1);
        let format_version = cx.ex.p.version;

        // Run the LZ node, forwarding the graph's parameters.
        let params = cx.eff.params.clone();
        let streams = cx.run_node(NODE_LZ, &[input], Some(&params))?;
        let [literals, offsets, literal_lengths, match_lengths] = streams[..] else {
            return Err(ExecError::GraphInvalid);
        };

        let level = int_param(cx, LZ_PARAM_COMPRESSION_LEVEL, cx.ex.p.level);

        // Offsets up to 13 bits go straight to bitpack.
        let offsets_graph = graph_param(
            cx,
            OFFSETS_GRAPH_IDX,
            if input_size <= (1 << 13) {
                GRAPH_BITPACK
            } else {
                GRAPH_PARTITION_BITPACK
            },
        );
        let mux_lengths_graph = graph_param(cx, MUX_LENGTHS_GRAPH_IDX, GraphId::ILLEGAL);

        let literals_size = cx.data(literals)?.content_size();
        set_entropy_or_override(
            cx,
            literals,
            LITERALS_GRAPH_IDX,
            huffman_graph(format_version, level, literals_size),
            min_gain_bytes,
            min_gain_pct,
        )?;
        cx.set_destination(&[offsets], offsets_graph, None)?;

        let mux_inputs = [literal_lengths, match_lengths];
        if mux_lengths_graph != GraphId::ILLEGAL {
            cx.set_destination(&mux_inputs, mux_lengths_graph, None)?;
        } else {
            // `mux_lengths` computes the split point and minimum match length.
            let muxed = cx.run_node(NODE_MUX_LENGTHS, &mux_inputs, None)?;
            let [bytes, overflow] = muxed[..] else {
                return Err(ExecError::GraphInvalid);
            };
            let muxed_store_size = cx.data(bytes)?.content_size();
            let guess = guess_muxed_entropy_size(muxed_store_size, min_gain_bytes, min_gain_pct);
            // Skip Huffman when it is unlikely to pay for itself.
            let bytes_graph = if guess < muxed_store_size {
                huffman_graph(format_version, level, muxed_store_size)
            } else {
                GRAPH_STORE
            };
            set_entropy_or_override(
                cx,
                bytes,
                MUXED_BYTES_GRAPH_IDX,
                bytes_graph,
                min_gain_bytes,
                min_gain_pct,
            )?;
            set_entropy_or_override(
                cx,
                overflow,
                OVERFLOW_LENGTHS_GRAPH_IDX,
                GRAPH_COMPRESS_SMALL_LENGTHS,
                min_gain_bytes,
                min_gain_pct,
            )?;
        }
        Ok(())
    }
}
