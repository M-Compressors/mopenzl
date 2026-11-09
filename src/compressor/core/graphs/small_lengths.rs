//! The `compress_small_lengths` graph (`compress/graphs/small_lengths_graph.c`,
//! `ZL_compressSmallLengthsGraph`): compresses the overflow lengths of the
//! `lz` graph, mostly tiny values with a few large ones.

use super::super::error::ExecError;
use super::super::exec::{Edge, GraphCx};
use super::super::params::LocalParams;
use super::super::registry::{GraphFn, GraphId, NodeId, RuntimeParams};

/// `ZL_NODE_SENTINEL_BYTE`.
const NODE_SENTINEL_BYTE: NodeId = NodeId::std(50);
/// `ZL_NODE_PARTITION`.
const NODE_PARTITION: NodeId = NodeId::std(48);
/// `ZL_NODE_QUANTIZE_LENGTHS`.
const NODE_QUANTIZE_LENGTHS: NodeId = NodeId::std(44);

const GRAPH_STORE: GraphId = GraphId::std(2);
const GRAPH_FSE: GraphId = GraphId::std(3);
const GRAPH_HUFFMAN: GraphId = GraphId::std(4);
const GRAPH_COMPRESS_GENERIC: GraphId = GraphId::std(13);
const GRAPH_PARTITION_BITPACK: GraphId = GraphId::std(23);

/// `kInputStoreThreshold`.
const INPUT_STORE_THRESHOLD: usize = 50;
/// `kLargeStoreThreshold`.
const LARGE_STORE_THRESHOLD: usize = 100;

/// `ZL_compressSmallLengthsGraph` (`ZL_GRAPH_COMPRESS_SMALL_LENGTHS`).
///
/// Fewer than 50 values are stored. Byte values go to Huffman. Wider values
/// are split by `sentinel_byte` into a byte stream (Huffman) and the large
/// values, which are stored (fewer than 100), partitioned and bit-packed
/// (16-bit), quantized (32-bit) or compressed generically. Formats without
/// the sentinel node compress generically.
#[derive(Debug)]
pub(crate) struct CompressSmallLengths;

/// See [`CompressSmallLengths`].
pub(crate) static COMPRESS_SMALL_LENGTHS: CompressSmallLengths = CompressSmallLengths;

/// The graph's own local parameters, forwarded to Huffman.
fn forwarded(cx: &GraphCx<'_, '_, '_>) -> RuntimeParams {
    RuntimeParams {
        params: Some(LocalParams::clone(cx.eff.params)),
        ..RuntimeParams::default()
    }
}

impl GraphFn for CompressSmallLengths {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let [edge] = *inputs else {
            return Err(ExecError::Parameter(
                "compress_small_lengths takes exactly one input",
            ));
        };
        let (n_elts, width) = {
            let input = cx.data(edge)?;
            (input.n_elts(), input.width())
        };
        if n_elts < INPUT_STORE_THRESHOLD {
            return cx.set_destination(&[edge], GRAPH_STORE, None);
        }
        if width == 1 {
            // 1-byte values go directly to Huffman.
            let params = forwarded(cx);
            return cx.set_destination(&[edge], GRAPH_HUFFMAN, Some(params));
        }
        if !cx.is_node_supported(NODE_SENTINEL_BYTE) {
            // Older format versions fall back to generic compression.
            return cx.set_destination(&[edge], GRAPH_COMPRESS_GENERIC, None);
        }
        // Values below 255 go into the small stream; the others push 255 into
        // it and go into the large stream.
        let edges = cx.run_node(NODE_SENTINEL_BYTE, &[edge], None)?;
        let [small, large] = edges[..] else {
            return Err(ExecError::GraphInvalid);
        };
        let params = forwarded(cx);
        cx.set_destination(&[small], GRAPH_HUFFMAN, Some(params))?;
        let (large_elts, large_width) = {
            let data = cx.data(large)?;
            (data.n_elts(), data.width())
        };
        if large_elts < LARGE_STORE_THRESHOLD {
            cx.set_destination(&[large], GRAPH_STORE, None)?;
        } else if large_width == 2 && cx.is_node_supported(NODE_PARTITION) {
            cx.set_destination(&[large], GRAPH_PARTITION_BITPACK, None)?;
        } else if large_width == 4 {
            let quantized = cx.run_node(NODE_QUANTIZE_LENGTHS, &[large], None)?;
            let [codes, extra_bits] = quantized[..] else {
                return Err(ExecError::GraphInvalid);
            };
            cx.set_destination(&[codes], GRAPH_FSE, None)?;
            cx.set_destination(&[extra_bits], GRAPH_STORE, None)?;
        } else {
            cx.set_destination(&[large], GRAPH_COMPRESS_GENERIC, None)?;
        }
        Ok(())
    }
}
