//! The entropy graphs (`codecs/entropy/encode_entropy_binding.c`):
//! `fse` (3), `huffman` (4), `huffman_huf0` (5), `huffman_pivco` (6) and
//! `entropy` (7), plus `ZL_Edge_setEntropyDestination`.
//!
//! All five are one dynamic graph, `entropyDynamicGraph`, with a backend
//! mode (FSE, Huffman or whichever is estimated smaller) and a Huffman mode
//! (the zstd-style `huffman_v2` or `pivco_huffman`). It converts its input
//! to serial bytes (or 16-bit structs), splits large inputs into 32 KiB
//! chunks, and for each chunk picks a successor from statistics:
//!
//! * a single repeated value goes to `constant`;
//! * a chunk the entropy coder would barely shrink goes to `store`, one that
//!   bit-packing or flat-packing beats goes there;
//! * otherwise the Huffman or FSE node runs with the histogram (and
//!   Huffman table) as reference parameters, its table stream goes to
//!   `bitpack_int` or `fse_ncount` and its bits are stored.
//!
//! The successors (bitpack, flatpack, tokenize, split) are reached through
//! the registry exactly as the reference does; a successor whose Cargo
//! feature is off fails the graph like a missing codec.
//!
//! Formats below 15 (the deprecated entropy codecs) are not supported; the
//! encoder never writes them (the minimum format is 21).

use alloc::borrow::Cow;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::super::codecs::entropy::{build_huf_ctable, state_params};
use super::super::data::Type;
use super::super::error::ExecError;
use super::super::exec::{Edge, GraphCx};
use super::super::params::{LocalParams, RefParam};
use super::super::registry::{GraphFn, GraphId, NodeId, RuntimeParams};
use crate::shared::entropy::huf::HufCTable;
use crate::shared::stats::data_stats::{DataStatsU8, calculate_entropy};
use crate::shared::stats::histogram::Histogram;

/// `ZL_ENTROPY_MIN_GAIN_BYTES_PID`.
pub(crate) const MIN_GAIN_BYTES_PID: i32 = crate::graph::params::entropy::MIN_GAIN_BYTES;
/// `ZL_ENTROPY_MIN_GAIN_PCT_PID`.
pub(crate) const MIN_GAIN_PCT_PID: i32 = crate::graph::params::entropy::MIN_GAIN_PCT;
/// `ZL_SPLITN_SEGMENTSIZES_PID`: the split node's segment sizes. The
/// reference passes a pointer to `size_t`s; here the parameter is an
/// `Arc<Vec<usize>>` reference parameter.
pub(crate) const SPLITN_SEGMENTSIZES_PID: i32 = 323;

const NODE_CONVERT_STRUCT_TO_SERIAL: NodeId = NodeId::std(10);
const NODE_CONVERT_NUM_TO_STRUCT_LE: NodeId = NodeId::std(11);
const NODE_CONVERT_NUM_TO_SERIAL_LE: NodeId = NodeId::std(12);
const NODE_INTERPRET_TOKEN_AS_LE: NodeId = NodeId::std(17);
const NODE_TOKENIZE: NodeId = NodeId::std(40);
const NODE_FSE_V2: NodeId = NodeId::std(58);
const NODE_HUFFMAN_V2: NodeId = NodeId::std(59);
const NODE_HUFFMAN_STRUCT_V2: NodeId = NodeId::std(60);
const NODE_SPLIT_N: NodeId = NodeId::std(66);
const NODE_SPLIT_N_STRUCT: NodeId = NodeId::std(67);
const NODE_SPLIT_N_NUM: NodeId = NodeId::std(68);
const NODE_PIVCO_HUFFMAN: NodeId = NodeId::std(87);

const GRAPH_STORE: GraphId = GraphId::std(2);
const GRAPH_FSE: GraphId = GraphId::std(3);
const GRAPH_HUFFMAN: GraphId = GraphId::std(4);
const GRAPH_CONSTANT: GraphId = GraphId::std(8);
const GRAPH_BITPACK: GraphId = GraphId::std(10);
const GRAPH_FLATPACK: GraphId = GraphId::std(11);
const GRAPH_BITPACK_INT: GraphId = GraphId::std(41);
const GRAPH_FSE_NCOUNT: GraphId = GraphId::std(44);

/// The first format version with PivCo-Huffman.
const HUFFMAN_PIVCO_MIN_FORMAT_VERSION: u32 = 27;
/// The first format version with the v2 entropy codecs.
const MIN_ENTROPY_FORMAT_VERSION: u32 = 15;

/// `kChunkSize`.
const CHUNK_SIZE: usize = 1 << 15;
/// `kMinSizeToChunk`.
const MIN_SIZE_TO_CHUNK: usize = 100_000;

/// `EntropyBackendMode`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Backend {
    Huf,
    Fse,
    Store,
    Any,
}

/// `HuffmanMode`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum HufMode {
    Huf0,
    Pivco,
    Any,
}

/// `EI_fseDynamicGraph` (`ZL_GRAPH_FSE`).
#[derive(Debug)]
pub(crate) struct FseGraph;
/// `EI_huffmanDynamicGraph` (`ZL_GRAPH_HUFFMAN`).
#[derive(Debug)]
pub(crate) struct HuffmanGraph;
/// `EI_huffmanDynamicGraphHuf0`.
#[derive(Debug)]
pub(crate) struct HuffmanHuf0Graph;
/// `EI_huffmanDynamicGraphPivco`.
#[derive(Debug)]
pub(crate) struct HuffmanPivcoGraph;
/// `EI_entropyDynamicGraph` (`ZL_GRAPH_ENTROPY`).
#[derive(Debug)]
pub(crate) struct EntropyGraph;

/// See [`FseGraph`].
pub(crate) static FSE: FseGraph = FseGraph;
/// See [`HuffmanGraph`].
pub(crate) static HUFFMAN: HuffmanGraph = HuffmanGraph;
/// See [`HuffmanHuf0Graph`].
pub(crate) static HUFFMAN_HUF0: HuffmanHuf0Graph = HuffmanHuf0Graph;
/// See [`HuffmanPivcoGraph`].
pub(crate) static HUFFMAN_PIVCO: HuffmanPivcoGraph = HuffmanPivcoGraph;
/// See [`EntropyGraph`].
pub(crate) static ENTROPY: EntropyGraph = EntropyGraph;

impl GraphFn for FseGraph {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let input = single_input(inputs)?;
        let input = do_entropy_conversion(cx, input)?;
        check_version(cx)?;
        entropy_dynamic_graph(cx, input, Backend::Fse, HufMode::Any)
    }
}

/// `EI_huffmanDynamicGraphImpl`.
fn huffman_dynamic_graph(
    cx: &mut GraphCx<'_, '_, '_>,
    inputs: &[Edge],
    huf_mode: HufMode,
) -> Result<(), ExecError> {
    let input = single_input(inputs)?;
    let input = do_entropy_conversion(cx, input)?;
    check_version(cx)?;
    entropy_dynamic_graph(cx, input, Backend::Huf, huf_mode)
}

impl GraphFn for HuffmanGraph {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        huffman_dynamic_graph(cx, inputs, HufMode::Any)
    }
}

impl GraphFn for HuffmanHuf0Graph {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        huffman_dynamic_graph(cx, inputs, HufMode::Huf0)
    }
}

impl GraphFn for HuffmanPivcoGraph {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        huffman_dynamic_graph(cx, inputs, HufMode::Pivco)
    }
}

impl GraphFn for EntropyGraph {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        let input = single_input(inputs)?;
        let input = do_entropy_conversion(cx, input)?;
        check_version(cx)?;
        entropy_dynamic_graph(cx, input, Backend::Any, HufMode::Any)
    }
}

/// `ZL_ERR_IF(nbIns != 1, graph_invalidNumInputs)`.
fn single_input(inputs: &[Edge]) -> Result<Edge, ExecError> {
    match inputs {
        [input] => Ok(*input),
        _ => Err(ExecError::Parameter("the graph takes exactly one input")),
    }
}

/// The deprecated pre-15 branches are not ported.
fn check_version(cx: &GraphCx<'_, '_, '_>) -> Result<(), ExecError> {
    if cx.ex.p.version < MIN_ENTROPY_FORMAT_VERSION {
        return Err(ExecError::Parameter(
            "entropy graphs need format version 15 or later",
        ));
    }
    Ok(())
}

/// `doEntropyConversion`: serial bytes or 16-bit structs.
fn do_entropy_conversion(cx: &mut GraphCx<'_, '_, '_>, edge: Edge) -> Result<Edge, ExecError> {
    let (ty, width) = {
        let data = cx.data(edge)?;
        (data.ty(), data.width())
    };
    if width == 1 {
        if ty != Type::Serial {
            // Convert width 1 to serial at the top for efficiency.
            let conversion = if ty == Type::Numeric {
                NODE_CONVERT_NUM_TO_SERIAL_LE
            } else {
                NODE_CONVERT_STRUCT_TO_SERIAL
            };
            return first_edge(cx.run_node(conversion, &[edge], None)?);
        }
    } else {
        if width != 2 {
            return Err(ExecError::Parameter(
                "entropy coding needs 1 or 2 byte elements",
            ));
        }
        if ty == Type::Numeric {
            return first_edge(cx.run_node(NODE_CONVERT_NUM_TO_STRUCT_LE, &[edge], None)?);
        }
    }
    Ok(edge)
}

/// The first output of a node run.
fn first_edge(edges: Vec<Edge>) -> Result<Edge, ExecError> {
    edges
        .first()
        .copied()
        .ok_or(ExecError::Parameter("a node produced no output"))
}

/// `getMinGainBytes`.
fn min_gain_bytes(cx: &GraphCx<'_, '_, '_>, content_size: usize) -> usize {
    const DEFAULT_MIN_GAIN_BYTES: usize = 32;
    const DEFAULT_MIN_GAIN_PCT: usize = 1;
    let params = cx.eff.params;
    let min_gain_bytes = params
        .int(MIN_GAIN_BYTES_PID)
        .map_or(DEFAULT_MIN_GAIN_BYTES, |value| value.max(0) as usize);
    let min_gain_pct = params
        .int(MIN_GAIN_PCT_PID)
        .map_or(DEFAULT_MIN_GAIN_PCT, |value| value.clamp(0, 100) as usize);
    min_gain_bytes.max((content_size / 100) * min_gain_pct)
}

/// `entropyDynamicGraph`.
fn entropy_dynamic_graph(
    cx: &mut GraphCx<'_, '_, '_>,
    edge: Edge,
    mode: Backend,
    huf_mode: HufMode,
) -> Result<(), ExecError> {
    let content_size = cx.data(edge)?.content_size();
    if content_size <= 1 {
        return cx.set_destination(&[edge], GRAPH_STORE, None);
    }
    let min_gain = min_gain_bytes(cx, content_size);
    let chunks = chunk_input_stream(cx, edge)?;
    for chunk in chunks {
        let chunk_size = cx.data(chunk)?.content_size();
        // Scale minGainBytes to divide evenly among the chunks.
        let scaled = (min_gain * chunk_size) / content_size;
        entropy_compress_chunk(cx, chunk, mode, scaled, huf_mode)?;
    }
    Ok(())
}

/// The chunk sizes `chunkInputStream` asks for, or `None` below
/// [`MIN_SIZE_TO_CHUNK`] elements: fixed 32 KiB chunks, the last one holding
/// the remainder (0 when the input divides evenly, which the split node
/// reads as "the rest").
fn chunk_sizes(n_elts: usize) -> Option<Vec<usize>> {
    if n_elts < MIN_SIZE_TO_CHUNK {
        return None;
    }
    let nb_chunks = n_elts.div_ceil(CHUNK_SIZE);
    let mut sizes = vec![CHUNK_SIZE; nb_chunks];
    sizes[nb_chunks - 1] = n_elts % CHUNK_SIZE;
    Some(sizes)
}

/// `chunkInputStream`: fixed 32 KiB chunks for inputs of 100000 elements or
/// more.
fn chunk_input_stream(cx: &mut GraphCx<'_, '_, '_>, edge: Edge) -> Result<Vec<Edge>, ExecError> {
    let n_elts = cx.data(edge)?.n_elts();
    match chunk_sizes(n_elts) {
        Some(sizes) => run_split_node(cx, edge, sizes),
        None => Ok(vec![edge]),
    }
}

/// `ZL_Edge_runSplitNode`: splits the edge into segments of `sizes`
/// elements with the `splitN` node of its type.
///
/// The sizes travel as the reference parameter [`SPLITN_SEGMENTSIZES_PID`]
/// holding an `Arc<Vec<usize>>`.
///
/// # Errors
///
/// Too many segments, a string input, or a failing node.
pub(crate) fn run_split_node(
    cx: &mut GraphCx<'_, '_, '_>,
    edge: Edge,
    sizes: Vec<usize>,
) -> Result<Vec<Edge>, ExecError> {
    if sizes.len() > i32::MAX as usize {
        return Err(ExecError::Parameter("nbSegments is too large"));
    }
    let node = match cx.data(edge)?.ty() {
        Type::Serial => NODE_SPLIT_N,
        Type::Struct => NODE_SPLIT_N_STRUCT,
        Type::Numeric => NODE_SPLIT_N_NUM,
        Type::String => return Err(ExecError::Parameter("strings cannot be split")),
    };
    let mut params = LocalParams::default();
    params.refs.push((
        SPLITN_SEGMENTSIZES_PID,
        RefParam(Arc::new(sizes) as Arc<dyn core::any::Any + Send + Sync>),
    ));
    cx.run_node(node, &[edge], Some(&params))
}

/// `runBitpack`: reinterpret the tokens as little-endian numbers and pack.
fn run_bitpack(cx: &mut GraphCx<'_, '_, '_>, edge: Edge) -> Result<(), ExecError> {
    let streams = cx.run_node(NODE_INTERPRET_TOKEN_AS_LE, &[edge], None)?;
    cx.set_destination(&[first_edge(streams)?], GRAPH_BITPACK, None)
}

/// `huffmanNode`.
fn huffman_node(mode: HufMode, format_version: u32) -> NodeId {
    let mode = if format_version < HUFFMAN_PIVCO_MIN_FORMAT_VERSION {
        // PivCo-Huffman not yet supported.
        HufMode::Huf0
    } else {
        mode
    };
    match mode {
        HufMode::Any | HufMode::Huf0 => NODE_HUFFMAN_V2,
        HufMode::Pivco => NODE_PIVCO_HUFFMAN,
    }
}

/// The two output edges of an entropy node.
fn two_edges(edges: &[Edge]) -> Result<(Edge, Edge), ExecError> {
    match edges {
        [first, second] => Ok((*first, *second)),
        _ => Err(ExecError::Parameter("an entropy node has two outputs")),
    }
}

/// `entropyCompressChunk`.
fn entropy_compress_chunk(
    cx: &mut GraphCx<'_, '_, '_>,
    chunk: Edge,
    mode: Backend,
    min_gain: usize,
    huf_mode: HufMode,
) -> Result<(), ExecError> {
    let (n_elts, is_serial) = {
        let input = cx.data(chunk)?;
        (input.n_elts(), input.ty() == Type::Serial)
    };
    if n_elts <= 1 {
        return cx.set_destination(&[chunk], GRAPH_STORE, None);
    }
    if !is_serial {
        return compress_wide_chunk(cx, chunk, n_elts, min_gain);
    }
    compress_byte_chunk(cx, chunk, n_elts, mode, min_gain, huf_mode)
}

/// What the 16-bit struct analysis decided.
#[derive(Debug)]
enum WidePlan {
    Constant,
    Bitpack,
    Store,
    /// Tokenize; bit-pack the alphabet (else store it), Huffman the indices.
    Tokenize {
        bitpack_alphabet: bool,
    },
    /// The 16-bit Huffman node with this histogram, its weights to FSE.
    Huffman(Arc<Histogram>),
}

/// The 16-bit struct branch of `entropyCompressChunk`, the decisions before
/// any node runs.
fn plan_wide_chunk(bytes: &[u8], n_elts: usize, min_gain: usize) -> WidePlan {
    let mut histogram = Histogram::new(65535);
    histogram.build16(bytes);
    let max_symbol = histogram.max_symbol as usize;

    if histogram.total == histogram.count[max_symbol] {
        return WidePlan::Constant;
    }

    // The Huffman size estimate.
    let mut entropy = calculate_entropy(&histogram.count, max_symbol, histogram.total as usize);
    entropy = if entropy > 1.0 { entropy } else { 1.0 };
    let entropy_size = ((entropy * n_elts as f64 + 7.0) as usize) / 8;
    let header_size_estimate = 100usize.max(max_symbol / 4);
    let huff_size = entropy_size + header_size_estimate;
    let store_size = 2 * n_elts;

    // Bit-packing?
    let nb_bits = next_pow2(max_symbol as u64 + 1) as usize;
    let bitpack_size = (n_elts * nb_bits).div_ceil(8) + 2;
    if bitpack_size <= huff_size && bitpack_size + min_gain <= store_size {
        return WidePlan::Bitpack;
    }

    // Storing?
    if entropy > 15.0 || huff_size + min_gain > store_size {
        return WidePlan::Store;
    }

    // Tokenization?
    if histogram.cardinality < 256 {
        return WidePlan::Tokenize {
            bitpack_alphabet: nb_bits < 16,
        };
    }
    WidePlan::Huffman(Arc::new(histogram))
}

/// The 16-bit struct branch of `entropyCompressChunk`.
fn compress_wide_chunk(
    cx: &mut GraphCx<'_, '_, '_>,
    chunk: Edge,
    n_elts: usize,
    min_gain: usize,
) -> Result<(), ExecError> {
    match plan_wide_chunk(cx.data(chunk)?.bytes(), n_elts, min_gain) {
        WidePlan::Constant => cx.set_destination(&[chunk], GRAPH_CONSTANT, None),
        WidePlan::Bitpack => run_bitpack(cx, chunk),
        WidePlan::Store => cx.set_destination(&[chunk], GRAPH_STORE, None),
        WidePlan::Tokenize { bitpack_alphabet } => {
            let streams = cx.run_node(NODE_TOKENIZE, &[chunk], None)?;
            let (alphabet, indices) = two_edges(&streams)?;
            // Bit-pack the values if possible.
            if bitpack_alphabet {
                run_bitpack(cx, alphabet)?;
            } else {
                cx.set_destination(&[alphabet], GRAPH_STORE, None)?;
            }
            // Huffman-compress the tokenized stream.
            cx.set_destination(&[indices], GRAPH_HUFFMAN, None)
        }
        WidePlan::Huffman(histogram) => {
            let params = state_params(&histogram, None);
            let streams = cx.run_node(NODE_HUFFMAN_STRUCT_V2, &[chunk], Some(&params))?;
            let (weights, bits) = two_edges(&streams)?;
            cx.set_destination(&[weights], GRAPH_FSE, None)?;
            cx.set_destination(&[bits], GRAPH_STORE, None)
        }
    }
}

/// `ZL_nextPow2`.
const fn next_pow2(upper_bound: u64) -> u32 {
    if upper_bound <= 1 {
        0
    } else {
        64 - (upper_bound - 1).leading_zeros()
    }
}

/// `estimateHuffmanSize`.
fn estimate_huffman_size(ctable: &HufCTable, histogram: &Histogram) -> usize {
    use crate::shared::entropy::huf::estimate_compressed_size;
    let symbols_size = estimate_compressed_size(ctable, &histogram.count, histogram.max_symbol);
    // Account for the Huffman header and the jump table in X4 mode.
    let header_size = 1
        + (next_pow2(u64::from(histogram.total.wrapping_add(1))) as usize).div_ceil(8)
        + if histogram.total >= 256 { 6 } else { 0 };
    let table_bits = 4 * (histogram.max_symbol as usize + 1);
    let table_size = table_bits.div_ceil(8);
    header_size + table_size + symbols_size
}

/// `estimateFseSize`.
fn estimate_fse_size(stats: &mut DataStatsU8<'_>) -> usize {
    let entropy = stats.entropy();
    let symbol_bits = (entropy * stats.total_elements() as f64) as usize;
    // Estimate the header as 10 bits per non-zero symbol.
    let header_bits = 10 * stats.cardinality();
    (header_bits + symbol_bits).div_ceil(8)
}

/// `resolveMode`: resolves `Any` to a concrete mode and returns the
/// estimated entropy-coded size.
fn resolve_mode(
    stats: &mut DataStatsU8<'_>,
    histogram: &Histogram,
    ctable: Option<&HufCTable>,
    min_gain: usize,
    mode: &mut Backend,
) -> usize {
    let n_elts = histogram.total as usize;
    if n_elts <= 64 {
        // Don't even attempt entropy compression on tiny inputs.
        *mode = Backend::Store;
        return n_elts;
    }
    match (*mode, ctable) {
        (Backend::Any, Some(ctable)) => {
            let huf_size = estimate_huffman_size(ctable, histogram);
            let fse_size = estimate_fse_size(stats);
            let min_fse_gain = n_elts / 100;
            let entropy_size = if fse_size + min_fse_gain <= huf_size {
                *mode = Backend::Fse;
                fse_size
            } else {
                *mode = Backend::Huf;
                huf_size
            };
            if entropy_size + min_gain <= n_elts {
                entropy_size
            } else {
                *mode = Backend::Store;
                n_elts
            }
        }
        (Backend::Huf, Some(ctable)) => {
            let huf_size = estimate_huffman_size(ctable, histogram);
            if huf_size + min_gain <= n_elts {
                huf_size
            } else {
                *mode = Backend::Store;
                n_elts
            }
        }
        (Backend::Fse, _) => {
            let fse_size = estimate_fse_size(stats);
            if fse_size + min_gain <= n_elts {
                fse_size
            } else {
                *mode = Backend::Store;
                n_elts
            }
        }
        _ => n_elts,
    }
}

/// `getHistogram8`: the histogram of the byte statistics.
fn histogram8(stats: &mut DataStatsU8<'_>) -> Histogram {
    let max_symbol = u32::from(stats.max_elt());
    let mut histogram = Histogram::new(255);
    histogram.count.copy_from_slice(&stats.histogram()[..]);
    histogram.total = stats.total_elements() as u32;
    histogram.max_symbol = max_symbol;
    histogram.element_size = 1;
    histogram.cardinality = stats.cardinality() as u32;
    histogram.largest_count = histogram.count[..=max_symbol as usize]
        .iter()
        .copied()
        .max()
        .unwrap_or(0);
    histogram
}

/// What the byte-chunk analysis decided.
enum Plan {
    Constant,
    Flatpack,
    Bitpack,
    Store,
    Huffman(Arc<Histogram>, Option<Arc<HufCTable>>),
    Fse(Arc<Histogram>, Option<Arc<HufCTable>>),
}

/// The serial branch of `entropyCompressChunk`, the decisions before any
/// node runs.
fn plan_byte_chunk(bytes: &[u8], mut mode: Backend, min_gain: usize) -> Result<Plan, ExecError> {
    let n_elts = bytes.len();
    let mut stats = DataStatsU8::new(bytes);
    let histogram = histogram8(&mut stats);

    if mode == Backend::Huf {
        // Short-circuit for incompressible data (taken from FSE's Huffman).
        if histogram.largest_count <= (histogram.total >> 7) + 4 {
            mode = Backend::Store;
        }
    }

    if stats.cardinality() == 1 {
        return Ok(Plan::Constant);
    }

    // Build the Huffman table if Huffman is allowed. This allows an accurate
    // estimate of the Huffman size; if chosen, the table is passed down.
    let ctable = if mode == Backend::Huf || mode == Backend::Any {
        Some(
            build_huf_ctable(&histogram)
                .ok()
                .map(Arc::new)
                .ok_or(ExecError::Parameter("Huffman table construction failed"))?,
        )
    } else {
        None
    };

    let entropy_size = resolve_mode(
        &mut stats,
        &histogram,
        ctable.as_deref(),
        min_gain,
        &mut mode,
    );
    debug_assert!(entropy_size <= n_elts);

    let flatpacked_size = stats.flatpacked_size();
    let bitpacked_size = stats.bitpacked_size();

    if flatpacked_size < bitpacked_size {
        if flatpacked_size < entropy_size && flatpacked_size + min_gain <= n_elts {
            return Ok(Plan::Flatpack);
        }
    } else if bitpacked_size <= entropy_size && bitpacked_size + min_gain <= n_elts {
        return Ok(Plan::Bitpack);
    }

    Ok(match mode {
        Backend::Huf => Plan::Huffman(Arc::new(histogram), ctable),
        Backend::Fse => Plan::Fse(Arc::new(histogram), ctable),
        _ => Plan::Store,
    })
}

/// The serial branch of `entropyCompressChunk`.
fn compress_byte_chunk(
    cx: &mut GraphCx<'_, '_, '_>,
    chunk: Edge,
    n_elts: usize,
    mode: Backend,
    min_gain: usize,
    huf_mode: HufMode,
) -> Result<(), ExecError> {
    debug_assert!(n_elts > 1);
    let plan = plan_byte_chunk(cx.data(chunk)?.bytes(), mode, min_gain)?;
    match plan {
        Plan::Constant => cx.set_destination(&[chunk], GRAPH_CONSTANT, None),
        Plan::Flatpack => cx.set_destination(&[chunk], GRAPH_FLATPACK, None),
        Plan::Bitpack => cx.set_destination(&[chunk], GRAPH_BITPACK, None),
        Plan::Store => cx.set_destination(&[chunk], GRAPH_STORE, None),
        Plan::Huffman(histogram, ctable) => {
            let node = huffman_node(huf_mode, cx.ex.p.version);
            let params = state_params(&histogram, ctable.as_ref());
            let streams = cx.run_node(node, &[chunk], Some(&params))?;
            let (table, bits) = two_edges(&streams)?;
            // Bitpack is fast but leaves a lot on the table; FSE is too slow
            // on small inputs to be worthwhile.
            cx.set_destination(&[table], GRAPH_BITPACK_INT, None)?;
            cx.set_destination(&[bits], GRAPH_STORE, None)
        }
        Plan::Fse(histogram, ctable) => {
            let params = state_params(&histogram, ctable.as_ref());
            let streams = cx.run_node(NODE_FSE_V2, &[chunk], Some(&params))?;
            let (table, bits) = two_edges(&streams)?;
            cx.set_destination(&[table], GRAPH_FSE_NCOUNT, None)?;
            cx.set_destination(&[bits], GRAPH_STORE, None)
        }
    }
}

/// `ZL_Edge_setEntropyDestination`: sends `edge` to `entropy_graph` with
/// optional minimum-gain parameters (a negative value leaves the graph's
/// default).
///
/// # Errors
///
/// Whatever fails the destination assignment.
#[cfg_attr(
    not(any(all(test, feature = "conversion"), feature = "lz")),
    expect(dead_code, reason = "only the `lz` graph and its tests call it")
)]
pub(crate) fn set_entropy_destination(
    cx: &mut GraphCx<'_, '_, '_>,
    edge: Edge,
    entropy_graph: GraphId,
    min_gain_bytes: i32,
    min_gain_pct: i32,
) -> Result<(), ExecError> {
    let mut ints = Vec::with_capacity(2);
    if min_gain_bytes >= 0 {
        ints.push((MIN_GAIN_BYTES_PID, min_gain_bytes));
    }
    if min_gain_pct >= 0 {
        ints.push((MIN_GAIN_PCT_PID, min_gain_pct));
    }
    let params = RuntimeParams {
        params: Some(LocalParams {
            ints: Cow::Owned(ints),
            ..LocalParams::default()
        }),
        ..RuntimeParams::default()
    };
    cx.set_destination(&[edge], entropy_graph, Some(params))
}

#[cfg(all(test, feature = "decompression", feature = "all-codecs"))]
mod tests {
    //! The graph's decisions against the reference's: the codecs a reference
    //! frame contains must be those the plan says the graph runs. This checks
    //! the whole decision logic without the codecs of other packages
    //! (bitpack, flatpack, tokenize), which `tests/encode_entropy.rs`
    //! compares byte for byte once they exist.

    use super::*;
    use crate::Backend as SimdBackend;
    use crate::decompressor::core::chunk::Layout;
    use crate::decompressor::core::frame;
    use alloc::format;
    use alloc::string::String;
    use alloc::vec;
    use openzl_ffi::graph::{self as cgraph, Graph, Input as CInput, cparam};

    const BITPACK_SERIAL: u32 = 27;
    const BITPACK_INT: u32 = 28;
    const FLATPACK: u32 = 29;
    const CONSTANT_SERIAL: u32 = 44;
    const CONSTANT_FIXED: u32 = 45;
    const FSE_V2: u32 = 49;
    const HUFFMAN_V2: u32 = 50;
    const HUFFMAN_STRUCT_V2: u32 = 51;
    const FSE_NCOUNT: u32 = 52;
    const PIVCO: u32 = 67;
    const TOKENIZE_STRUCT: u32 = 36;

    /// The codec IDs of the first chunk of a frame, in the frame's order.
    fn trace(frame_bytes: &[u8]) -> Vec<u32> {
        let info = frame::parse_with(frame_bytes, SimdBackend::default().0, false, |_| {})
            .expect("frame header");
        let mut layout = Layout::default();
        layout
            .parse(&frame_bytes[info.header_len..], &info)
            .expect("chunk header");
        layout.nodes.iter().map(|node| node.desc.id).collect()
    }

    fn c_trace(graph: u32, input: CInput<'_>, version: u32) -> Option<Vec<u32>> {
        let params = [
            (cparam::LEVEL, 6),
            (cparam::FORMAT_VERSION, version as i32),
            (cparam::MIN_STREAM_SIZE, 1),
            (cparam::STORE_ON_EXPANSION, 2),
        ];
        // An expanding frame can exceed the reference's output bound.
        let frame_bytes = cgraph::compress(&[input], &Graph::Id(graph), &params).ok()?;
        Some(trace(&frame_bytes))
    }

    fn lcg(seed: u32) -> impl FnMut() -> u32 {
        let mut state = seed.wrapping_mul(2_654_435_761) | 1;
        move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        }
    }

    fn uniform(len: usize, symbols: u32, seed: u32) -> Vec<u8> {
        let mut next = lcg(seed);
        (0..len).map(|_| (next() % symbols) as u8).collect()
    }

    fn geometric(len: usize, seed: u32) -> Vec<u8> {
        let mut next = lcg(seed);
        (0..len)
            .map(|_| {
                let mut s = 0u8;
                while next() & 3 != 0 && s < 60 {
                    s += 1;
                }
                s
            })
            .collect()
    }

    fn wide_symbols(len: usize, symbols: u32, spread: u32, seed: u32) -> Vec<u8> {
        let mut next = lcg(seed);
        (0..len)
            .flat_map(|_| ((next() % symbols).wrapping_mul(spread) as u16).to_le_bytes())
            .collect()
    }

    /// The codecs the plan implies, for a serial chunk.
    fn assert_serial_plan(plan: &Plan, codecs: &[u32], label: &str) {
        let has = |id: u32| codecs.contains(&id);
        let entropy = [HUFFMAN_V2, FSE_V2, FSE_NCOUNT, PIVCO];
        match plan {
            Plan::Constant => assert!(has(CONSTANT_SERIAL), "{label}: {codecs:?}"),
            Plan::Flatpack => assert!(has(FLATPACK), "{label}: {codecs:?}"),
            Plan::Bitpack => assert!(
                (has(BITPACK_SERIAL) || has(BITPACK_INT)) && !entropy.iter().any(|&id| has(id)),
                "{label}: {codecs:?}"
            ),
            Plan::Store => assert!(
                ![
                    BITPACK_SERIAL,
                    BITPACK_INT,
                    FLATPACK,
                    CONSTANT_SERIAL,
                    CONSTANT_FIXED
                ]
                .iter()
                .chain(&entropy)
                .any(|&id| has(id)),
                "{label}: {codecs:?}"
            ),
            Plan::Huffman(..) => assert!(
                (has(HUFFMAN_V2) || has(PIVCO)) && has(BITPACK_INT),
                "{label}: {codecs:?}"
            ),
            Plan::Fse(..) => assert!(has(FSE_V2) && has(FSE_NCOUNT), "{label}: {codecs:?}"),
        }
    }

    /// Few distinct values spread over the byte range: flat-packing beats
    /// bit-packing.
    fn spread(len: usize, symbols: u32, seed: u32) -> Vec<u8> {
        let mut next = lcg(seed);
        (0..len)
            .map(|_| ((next() % symbols) * 251 / symbols.max(2)) as u8)
            .collect()
    }

    fn serial_corpus() -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        for n in [58usize, 60, 62, 64, 66, 200, 1000, 5000] {
            for symbols in [2u32, 5, 17, 100, 128] {
                out.push((format!("spread{symbols}-{n}"), spread(n, symbols, symbols)));
            }
        }
        for n in [2usize, 3, 10, 64, 65, 100, 255, 256, 1000, 3000, 12_000] {
            out.push((format!("uniform2-{n}"), uniform(n, 2, 1)));
            out.push((format!("uniform3-{n}"), uniform(n, 3, 2)));
            out.push((format!("uniform17-{n}"), uniform(n, 17, 3)));
            out.push((format!("uniform100-{n}"), uniform(n, 100, 4)));
            out.push((format!("uniform256-{n}"), uniform(n, 256, 5)));
            out.push((format!("geometric-{n}"), geometric(n, 6)));
            out.push((format!("constant-{n}"), vec![5u8; n]));
        }
        out
    }

    #[test]
    fn byte_plans_agree_with_the_reference_frames() {
        let mut checked = [0usize; 3];
        let mut kinds = [0usize; 6];
        for (name, bytes) in serial_corpus() {
            let min_gain = min_gain_for(bytes.len());
            for (graph, backend, slot) in [
                (7u32, Backend::Any, 0),
                (4, Backend::Huf, 1),
                (3, Backend::Fse, 2),
            ] {
                if bytes.len() <= 1 {
                    continue;
                }
                let Some(codecs) = c_trace(graph, CInput::Serial(&bytes), 27) else {
                    continue;
                };
                let plan = plan_byte_chunk(&bytes, backend, min_gain).unwrap();
                assert_serial_plan(&plan, &codecs, &format!("graph {graph} {name}"));
                checked[slot] += 1;
                kinds[match plan {
                    Plan::Constant => 0,
                    Plan::Flatpack => 1,
                    Plan::Bitpack => 2,
                    Plan::Store => 3,
                    Plan::Huffman(..) => 4,
                    Plan::Fse(..) => 5,
                }] += 1;
            }
        }
        assert!(checked.iter().all(|&count| count > 40), "{checked:?}");
        assert!(kinds.iter().all(|&count| count > 0), "{kinds:?}");
    }

    /// `entropyDynamicGraph`'s default minimum gain for a chunk that is the
    /// whole input.
    fn min_gain_for(content_size: usize) -> usize {
        32.max(content_size / 100)
    }

    #[test]
    fn wide_plans_agree_with_the_reference_frames() {
        let mut checked = 0;
        let mut kinds = [0usize; 5];
        for n in [2usize, 3, 10, 100, 300, 1000, 4000, 20_000] {
            let mut next = lcg(n as u32);
            // 300 values scattered over 0..8000, half of the draws among 8 of them.
            let pool: Vec<u16> = (0..300u32).map(|i| (i * 26 + next() % 20) as u16).collect();
            let pooled: Vec<u8> = (0..n)
                .flat_map(|_| {
                    let index = if next() & 1 == 0 {
                        next() as usize % 300
                    } else {
                        next() as usize % 8
                    };
                    pool[index].to_le_bytes()
                })
                .collect();
            for (name, bytes) in [
                ("pool300", pooled),
                ("few", wide_symbols(n, 5, 1000, 1)),
                ("few-small", wide_symbols(n, 3, 1, 2)),
                ("many", wide_symbols(n, 3000, 21, 3)),
                ("mid", wide_symbols(n, 200, 300, 4)),
                ("tokens", wide_symbols(n, 40, 777, 5)),
                ("const", vec![9u8; 2 * n]),
                ("uniform", wide_symbols(n, 60_000, 1, 6)),
            ] {
                let Some(codecs) = c_trace(
                    7,
                    CInput::Struct {
                        bytes: &bytes,
                        width: 2,
                    },
                    27,
                ) else {
                    continue;
                };
                let has = |id: u32| codecs.contains(&id);
                let plan = plan_wide_chunk(&bytes, n, min_gain_for(2 * n));
                let label = format!("{name}-{n}: {plan:?} {codecs:?}");
                kinds[match plan {
                    WidePlan::Constant => 0,
                    WidePlan::Bitpack => 1,
                    WidePlan::Store => 2,
                    WidePlan::Tokenize { .. } => 3,
                    WidePlan::Huffman(..) => 4,
                }] += 1;
                match plan {
                    WidePlan::Constant => {
                        assert!(has(CONSTANT_SERIAL) || has(CONSTANT_FIXED), "{label}")
                    }
                    WidePlan::Bitpack => assert!(
                        has(BITPACK_INT) && !has(HUFFMAN_STRUCT_V2) && !has(TOKENIZE_STRUCT),
                        "{label}"
                    ),
                    WidePlan::Store => assert!(
                        !has(BITPACK_INT)
                            && !has(HUFFMAN_STRUCT_V2)
                            && !has(TOKENIZE_STRUCT)
                            && !has(CONSTANT_FIXED),
                        "{label}"
                    ),
                    WidePlan::Tokenize { .. } => assert!(has(TOKENIZE_STRUCT), "{label}"),
                    WidePlan::Huffman(..) => assert!(has(HUFFMAN_STRUCT_V2), "{label}"),
                }
                checked += 1;
            }
        }
        assert!(checked > 30, "{checked}");
        assert!(kinds.iter().all(|&count| count > 0), "{kinds:?}");
    }
}

#[cfg(all(test, feature = "conversion"))]
mod engine_tests {
    //! The graphs on hand-built registries: parameters, metadata, reference
    //! parameters and the error paths no reference frame reaches.

    use super::*;
    use crate::Backend as SimdBackend;
    use crate::FormatVersion;
    use crate::compressor::config::EncoderConfig;
    use crate::compressor::core::Params;
    use crate::compressor::core::data::{Bytes, Stream};
    use crate::compressor::core::exec::{Exec, Resources};
    use crate::compressor::core::registry::{Callback, GraphDef, GraphKind, Registry, gid};
    use crate::compressor::core::sink::VecSink;
    #[cfg(feature = "zstd")]
    use crate::compressor::core::zstd::ZstdNode;

    type ProbeFn = for<'g, 'e, 'a> fn(&mut GraphCx<'g, 'e, 'a>, &[Edge]) -> Result<(), ExecError>;

    /// A function graph made of a plain function.
    struct Probe(ProbeFn);

    impl GraphFn for Probe {
        fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
            (self.0)(cx, inputs)
        }
    }

    fn params(version: u32) -> Params {
        let config =
            EncoderConfig::default().with_format_version(FormatVersion::try_from(version).unwrap());
        Params::from(&config)
    }

    /// Runs `probe` as the start graph over `inputs` and returns the frame.
    fn run_probe(
        probe: ProbeFn,
        mask: &'static [u8],
        params: Params,
        inputs: Vec<Stream<'_>>,
    ) -> Result<Vec<u8>, ExecError> {
        let mut registry = Registry::new(1, gid::COMPRESS_GENERIC);
        let callback: Arc<dyn GraphFn> = Arc::new(Probe(probe));
        let graph = registry.add_graph(GraphDef {
            name: Cow::Borrowed("probe"),
            kind: GraphKind::Function(Callback::Shared(callback)),
            inputs: Cow::Borrowed(mask),
            last_variable: mask.len() > 1,
            custom_graphs: Cow::Borrowed(&[]),
            custom_nodes: Cow::Borrowed(&[]),
            params: LocalParams::default(),
        });
        #[cfg(feature = "zstd")]
        let mut zstd = ZstdNode::new(params.level, SimdBackend::default());
        let resources = Resources {
            #[cfg(feature = "zstd")]
            zstd: &mut zstd,
            simd: SimdBackend::default().0,
            #[cfg(not(feature = "zstd"))]
            marker: core::marker::PhantomData,
        };
        let mut out = Vec::new();
        let mut sink = VecSink::new(&mut out);
        Exec::new(&registry, params, resources, None, inputs, &mut sink)
            .compress_frame(graph, None)?;
        Ok(out)
    }

    fn lcg(seed: u32) -> impl FnMut() -> u32 {
        let mut state = seed.wrapping_mul(2_654_435_761) | 1;
        move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        }
    }

    /// Geometric-looking bytes: FSE wins on these.
    fn geometric(len: usize) -> Vec<u8> {
        let mut next = lcg(6);
        (0..len)
            .map(|_| {
                let mut s = 0u8;
                while next() & 3 != 0 && s < 60 {
                    s += 1;
                }
                s
            })
            .collect()
    }

    fn fse_with_gain(
        cx: &mut GraphCx<'_, '_, '_>,
        inputs: &[Edge],
        bytes: i32,
        pct: i32,
    ) -> Result<(), ExecError> {
        set_entropy_destination(cx, inputs[0], GraphId::std(3), bytes, pct)
    }

    #[test]
    fn minimum_gain_parameters_reach_the_graph() {
        let data = geometric(12_000);
        let size = |probe: ProbeFn| {
            run_probe(probe, &[1], params(27), vec![Stream::serial(&data)])
                .unwrap()
                .len()
        };
        let default = size(|cx, inputs| fse_with_gain(cx, inputs, -1, -1));
        assert!(default < data.len() / 2, "{default}");
        // Parameters equal to the defaults change nothing.
        assert_eq!(size(|cx, inputs| fse_with_gain(cx, inputs, 32, 1)), default);
        // A gain the data cannot reach stores it.
        let stored_by_bytes = size(|cx, inputs| fse_with_gain(cx, inputs, 1_000_000, -1));
        assert!(stored_by_bytes > data.len());
        let stored_by_pct = size(|cx, inputs| fse_with_gain(cx, inputs, 0, 100));
        assert!(stored_by_pct > data.len());
        // A percentage above 100 is clamped, a negative byte count is a
        // zero minimum.
        assert_eq!(
            size(|cx, inputs| fse_with_gain(cx, inputs, 0, 100)),
            size(|cx, inputs| fse_with_gain(cx, inputs, 0, 400)),
        );
    }

    #[test]
    fn inputs_and_formats_the_graphs_cannot_take_are_rejected() {
        let data = geometric(500);
        // Two inputs.
        let result = run_probe(
            |cx, inputs| ENTROPY.run(cx, inputs),
            &[1, 1],
            params(27),
            vec![Stream::serial(&data), Stream::serial(&data)],
        );
        assert!(matches!(result, Err(ExecError::Parameter(_))));
        // Format versions before 15 had other entropy codecs.
        let old = Params {
            version: 14,
            ..params(27)
        };
        let result = run_probe(
            |cx, inputs| FSE.run(cx, inputs),
            &[1],
            old,
            vec![Stream::serial(&data)],
        );
        assert!(matches!(result, Err(ExecError::Parameter(_))));
        // Struct elements of width 3.
        let wide = Stream::new(Type::Struct, 3, Bytes::Borrowed(&data[..300]));
        let result = run_probe(
            |cx, inputs| HUFFMAN.run(cx, inputs),
            &[2],
            params(27),
            vec![wide],
        );
        assert!(matches!(result, Err(ExecError::Parameter(_))));
    }

    #[test]
    fn typed_inputs_are_converted_to_serial_or_two_byte_structs() {
        let data = geometric(3000);
        let wide: Vec<u8> = data.iter().flat_map(|&b| [b, 0]).collect();
        // The conversions are stored: the frames decode to the original and
        // never fail on the conversions themselves.
        let cases = [
            Stream::new(Type::Numeric, 1, Bytes::Borrowed(&data)),
            Stream::new(Type::Struct, 1, Bytes::Borrowed(&data)),
            Stream::new(Type::Numeric, 2, Bytes::Borrowed(&wide)),
            Stream::new(Type::Struct, 2, Bytes::Borrowed(&wide)),
        ];
        for input in cases {
            let result = run_probe(
                |cx, inputs| {
                    let converted = do_entropy_conversion(cx, inputs[0])?;
                    let ty = cx.data(converted)?.ty();
                    assert!(matches!(ty, Type::Serial | Type::Struct));
                    cx.set_destination(&[converted], GRAPH_STORE, None)
                },
                &[15],
                params(27),
                vec![input],
            );
            assert!(result.is_ok());
        }
    }

    #[test]
    fn splitting_needs_a_splittable_type() {
        let strings = Stream::strings(b"abcdef", &[3, 3]);
        let result = run_probe(
            |cx, inputs| run_split_node(cx, inputs[0], vec![1, 1]).map(drop),
            &[8],
            params(27),
            vec![strings],
        );
        assert!(matches!(result, Err(ExecError::Parameter(_))));
    }

    #[test]
    fn chunk_sizes_are_32_kib_with_the_remainder_last() {
        assert_eq!(chunk_sizes(99_999), None);
        assert_eq!(chunk_sizes(100_000), Some(vec![32768, 32768, 32768, 1696]));
        // An even division asks for "the rest" with a last size of 0.
        assert_eq!(chunk_sizes(131_072), Some(vec![32768, 32768, 32768, 0]));
    }

    #[test]
    fn the_huffman_mode_picks_the_node_by_format_version() {
        assert_eq!(huffman_node(HufMode::Any, 27), NODE_HUFFMAN_V2);
        assert_eq!(huffman_node(HufMode::Huf0, 27), NODE_HUFFMAN_V2);
        assert_eq!(huffman_node(HufMode::Pivco, 27), NODE_PIVCO_HUFFMAN);
        assert_eq!(huffman_node(HufMode::Pivco, 26), NODE_HUFFMAN_V2);
    }

    #[test]
    fn reference_parameters_and_table_metadata_flow_through_the_nodes() {
        let data = geometric(5000);
        // The node given the graph's state equals the node computing its own.
        let run = |with_state: bool| {
            let probe: ProbeFn = if with_state {
                |cx, inputs| run_huffman(cx, inputs[0], true)
            } else {
                |cx, inputs| run_huffman(cx, inputs[0], false)
            };
            run_probe(probe, &[1], params(27), vec![Stream::serial(&data)])
        };
        assert_eq!(run(true).unwrap(), run(false).unwrap());
    }

    /// Runs `huffman_v2` (with the graph's state as reference parameters, or
    /// alone), checks the table stream's metadata and stores both outputs.
    fn run_huffman(
        cx: &mut GraphCx<'_, '_, '_>,
        edge: Edge,
        with_state: bool,
    ) -> Result<(), ExecError> {
        let local = if with_state {
            let mut histogram = Histogram::new(255);
            histogram.build8(cx.data(edge)?.bytes());
            let ctable = build_huf_ctable(&histogram).unwrap();
            Some(state_params(&Arc::new(histogram), Some(&Arc::new(ctable))))
        } else {
            None
        };
        let streams = cx.run_node(NODE_HUFFMAN_V2, &[edge], local.as_ref())?;
        let (weights, bits) = two_edges(&streams)?;
        let metadata = cx.data(weights)?.metadata().to_vec();
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].0, 0);
        assert!((1..=12).contains(&metadata[0].1));
        cx.set_destination(&[weights], GRAPH_STORE, None)?;
        cx.set_destination(&[bits], GRAPH_STORE, None)
    }

    #[test]
    fn a_histogram_that_contradicts_the_input_fails_the_node() {
        let data = geometric(500);
        // The graph claims a constant source.
        let result = run_probe(
            |cx, inputs| {
                let mut histogram = Histogram::new(255);
                histogram.build8(&[3u8; 500]);
                let params = state_params(&Arc::new(histogram), None);
                cx.run_node(NODE_FSE_V2, &[inputs[0]], Some(&params))
                    .map(drop)
            },
            &[1],
            params(27),
            vec![Stream::serial(&data)],
        );
        assert!(matches!(result, Err(ExecError::Node { .. })));
    }
}
