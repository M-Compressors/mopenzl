//! The entropy nodes (`codecs/entropy/encode_entropy_binding.c`):
//! `fse_v2` (58), `huffman_v2` (59), `huffman_struct_v2` (60) and
//! `fse_ncount` (61).
//!
//! Each coding node emits a table stream (FSE normalised counts as 16-bit
//! numbers, Huffman weights as bytes, carrying the table log as integer
//! metadata 0) and the coded bits, and sends the decoder a header
//! `[flag][decoded size, little-endian, nextPow2(size + 1) bits wide]`:
//! the flag is the number of FSE states (2 below 1000 symbols, else 4) or
//! the four-stream bit of Huffman. The entropy graph hands precomputed
//! state to the nodes as reference parameters ([`HISTOGRAM_PID`],
//! [`HUF_CTABLE_PID`]) so they skip recomputing it.
//!
//! The libraries come from [`crate::shared::entropy`], the port of OpenZL's
//! own FSE and Huffman fork.

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::compressor::core::params::{LocalParams, RefParam};
use crate::shared::entropy::huf16;
use crate::shared::entropy::{HISTOGRAM_PID, HUF_CTABLE_PID, fse, huf};
use crate::shared::stats::histogram::Histogram;

/// The `fse_v2` encoder.
pub(crate) static FSE_V2: FnEncoder = FnEncoder(encode_fse_v2);
/// The `huffman_v2` encoder.
pub(crate) static HUFFMAN_V2: FnEncoder = FnEncoder(encode_huffman_v2);
/// The `huffman_struct_v2` encoder.
pub(crate) static HUFFMAN_STRUCT_V2: FnEncoder = FnEncoder(encode_huffman_struct_v2);
/// The `fse_ncount` encoder.
pub(crate) static FSE_NCOUNT: FnEncoder = FnEncoder(encode_fse_ncount);

/// `runNode_withParams`' parameters: the histogram and, when there is one,
/// the Huffman table, as reference parameters.
pub(crate) fn state_params(
    histogram: &Arc<Histogram>,
    ctable: Option<&Arc<huf::HufCTable>>,
) -> LocalParams {
    let mut params = LocalParams::default();
    params
        .refs
        .push((HISTOGRAM_PID, RefParam(Arc::clone(histogram) as _)));
    if let Some(ctable) = ctable {
        params
            .refs
            .push((HUF_CTABLE_PID, RefParam(Arc::clone(ctable) as _)));
    }
    params
}

/// `getHistogram`: the histogram the graph passed, else one built from the
/// input (only widths 1 and 2 have one).
fn histogram<'a>(cx: &NodeCx<'_, 'a>, input: &Stream<'a>) -> Result<Histogram, NodeError> {
    let passed = cx
        .ref_param(HISTOGRAM_PID)
        .and_then(|object| object.downcast_ref::<Histogram>());
    if let Some(histogram) = passed {
        return Ok(histogram.clone());
    }
    match input.width() {
        1 => {
            let mut histogram = Histogram::new(255);
            histogram.build8(input.bytes());
            Ok(histogram)
        }
        2 => {
            let mut histogram = Histogram::new(65535);
            histogram.build16(input.bytes());
            Ok(histogram)
        }
        _ => Err(NodeError::InvalidInput(
            "no histogram for this element width",
        )),
    }
}

/// The checks every coding node starts with: at least two elements and not
/// a single repeated value.
fn check_codable(
    histogram: &Histogram,
    n_elts: usize,
    too_few: &'static str,
    constant: &'static str,
) -> Result<(), NodeError> {
    if n_elts < 2 {
        return Err(NodeError::InvalidInput(too_few));
    }
    if histogram.count[histogram.max_symbol as usize] == histogram.total {
        return Err(NodeError::InvalidInput(constant));
    }
    Ok(())
}

/// The codec header: `[flag][size]` with the size in `nextPow2(size + 1)`
/// bits rounded up to bytes.
fn send_size_header(cx: &mut NodeCx<'_, '_>, flag: u8, size: usize) -> Result<(), NodeError> {
    let size = size as u64;
    let nb_bytes = (next_pow2(size + 1) as usize).div_ceil(8);
    let mut header = [0u8; 9];
    header[0] = flag;
    header[1..].copy_from_slice(&size.to_le_bytes());
    cx.send_header(&header[..nb_bytes + 1])
}

/// `ZL_nextPow2`.
const fn next_pow2(upper_bound: u64) -> u32 {
    if upper_bound <= 1 {
        0
    } else {
        64 - (upper_bound - 1).leading_zeros()
    }
}

/// `EI_fse_v2`.
fn encode_fse_v2<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let src = input.bytes();
    let src_size = src.len();
    let histogram = histogram(cx, input)?;
    check_codable(
        &histogram,
        src_size,
        "FSE needs at least two elements",
        "FSE cannot code constant data",
    )?;

    // 1. The number of states, and the header.
    let nb_states: usize = if src_size < 1000 { 2 } else { 4 };
    send_size_header(cx, nb_states as u8, src_size)?;

    // 2. The table.
    let max_symbol = histogram.max_symbol;
    let table_log = fse::optimal_table_log(fse::DEFAULT_TABLELOG, src_size, max_symbol);
    let mut norm = alloc::vec![0i16; max_symbol as usize + 1];
    fse::normalize_count(
        &mut norm,
        table_log,
        &histogram.count,
        src_size,
        max_symbol,
        true,
    )
    .map_err(|_| NodeError::InvalidInput("FSE normalisation failed"))?;
    let ctable = fse::CTable::build(&norm, max_symbol, table_log)
        .map_err(|_| NodeError::InvalidInput("FSE table construction failed"))?;
    let norm_bytes: Vec<u8> = norm.iter().flat_map(|count| count.to_le_bytes()).collect();
    cx.emit_with_metadata(0, 2, norm_bytes, &[(0, table_log as i32)])?;

    // 3. The bits.
    let bits = fse::compress_using_ctable(
        cx.simd,
        src,
        &ctable,
        nb_states,
        fse::compress_bound(src_size),
    )
    .ok_or(NodeError::InvalidInput("FSE source is not compressible"))?;
    cx.emit(1, 1, bits)?;
    Ok(())
}

/// `EI_fse_ncount`: the serialised count header of 16-bit normalised
/// counts.
fn encode_fse_ncount<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    if input.width() != 2 {
        return Err(NodeError::InvalidInput("counts must be 16-bit"));
    }
    let ncount: Vec<i16> = input
        .bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| i16::from_le_bytes(pair))
        .collect();
    let nb_counts = ncount.len();
    if nb_counts == 0 || nb_counts > 256 {
        return Err(NodeError::InvalidInput(
            "a count table has 1 to 256 entries",
        ));
    }
    if ncount[nb_counts - 1] == 0 {
        return Err(NodeError::InvalidInput("the last count must not be zero"));
    }
    let mut invalid = false;
    let mut sum: u64 = 0;
    for &count in &ncount {
        sum = sum.wrapping_add(if count == -1 { 1 } else { count as u64 });
        invalid |= count < -1;
    }
    if invalid {
        return Err(NodeError::InvalidInput("a count must not be below -1"));
    }
    if sum & sum.wrapping_sub(1) != 0 || sum == 0 {
        return Err(NodeError::InvalidInput("counts must sum to a power of two"));
    }
    let table_log = sum.ilog2();
    if !(fse::MIN_TABLELOG..=fse::MAX_TABLELOG).contains(&table_log) {
        return Err(NodeError::InvalidInput("the table log is out of range"));
    }
    let header = fse::write_ncount(&ncount, nb_counts as u32 - 1, table_log)
        .map_err(|_| NodeError::InvalidInput("FSE count header failed"))?;
    cx.emit(0, 1, header)?;
    Ok(())
}

/// `getHufCTable`: the table the graph passed, else one built from the
/// histogram.
fn huf_ctable(cx: &NodeCx<'_, '_>, histogram: &Histogram) -> Result<huf::HufCTable, NodeError> {
    if let Some(passed) = cx
        .ref_param(HUF_CTABLE_PID)
        .and_then(|object| object.downcast_ref::<huf::HufCTable>())
    {
        return Ok(passed.clone());
    }
    build_huf_ctable(histogram)
}

/// `buildHufCTable`: the table at the optimal log for the histogram.
pub(crate) fn build_huf_ctable(histogram: &Histogram) -> Result<huf::HufCTable, NodeError> {
    let table_log = huf::optimal_table_log(
        huf::TABLELOG_DEFAULT,
        histogram.total as usize,
        histogram.max_symbol,
    );
    huf::build_ctable(&histogram.count, histogram.max_symbol, table_log)
        .map_err(|_| NodeError::InvalidInput("Huffman table construction failed"))
}

/// `EI_huffman_v2`.
fn encode_huffman_v2<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let src = input.bytes();
    let src_size = src.len();
    let histogram = histogram(cx, input)?;
    check_codable(
        &histogram,
        src_size,
        "Huffman needs at least two elements",
        "Huffman cannot code constant data",
    )?;

    // 1. The table.
    let ctable = huf_ctable(cx, &histogram)?;
    let table_log = ctable.table_log();
    let weights: Vec<u8> = (0..=histogram.max_symbol as usize)
        .map(|symbol| {
            let length = ctable.nb_bits(symbol);
            if length == 0 {
                0
            } else {
                (table_log + 1 - length) as u8
            }
        })
        .collect();
    cx.emit_with_metadata(0, 1, weights, &[(0, table_log as i32)])?;

    // 2. One or four streams, and the header.
    let x4 = src_size >= 256;
    send_size_header(cx, u8::from(x4), src_size)?;

    // 3. The bits.
    let capacity = huf::compress_bound(src_size);
    let bits = if x4 {
        huf::compress4x_using_ctable(cx.simd, src, &ctable, capacity)
    } else {
        huf::compress1x_using_ctable(cx.simd, src, &ctable, capacity)
    }
    .ok_or(NodeError::InvalidInput(
        "Huffman source is not compressible",
    ))?;
    cx.emit(1, 1, bits)?;
    Ok(())
}

/// `EI_huffman_struct_v2`: 16-bit symbols.
fn encode_huffman_struct_v2<'a>(
    cx: &mut NodeCx<'_, 'a>,
    inputs: &[Stream<'a>],
) -> Result<(), NodeError> {
    let input = &inputs[0];
    if input.width() != 2 {
        return Err(NodeError::InvalidInput("symbols must be 16-bit"));
    }
    let src: Vec<u16> = input
        .bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .collect();
    let src_size = src.len();
    let histogram = histogram(cx, input)?;
    check_codable(
        &histogram,
        src_size,
        "Huffman needs at least two elements",
        "Huffman cannot code constant data",
    )?;

    // 1. The table.
    let weights_size = histogram.max_symbol as usize + 1;
    let (ctable, table_log) = huf16::build_ctable(&histogram.count, histogram.max_symbol as u16, 0)
        .map_err(|_| NodeError::InvalidInput("Huffman table construction failed"))?;
    let weights: Vec<u8> = ctable[..weights_size]
        .iter()
        .map(|elt| {
            let length = i32::from(elt.nb_bits);
            if length == 0 {
                0
            } else {
                (table_log + 1 - length) as u8
            }
        })
        .collect();
    cx.emit_with_metadata(0, 1, weights, &[(0, table_log)])?;

    // 2. One or four streams, and the header.
    let x4 = src_size > 1000;
    send_size_header(cx, u8::from(x4), src_size)?;

    // 3. The bits.
    let capacity = 2 * src_size + 32;
    let mut bits = Vec::new();
    let result = if x4 {
        huf16::encode_using_ctable_x4(&mut bits, capacity, &src, &ctable)
    } else {
        huf16::encode_using_ctable(&mut bits, capacity, &src, &ctable)
    };
    result.map_err(|_| NodeError::InvalidInput("16-bit Huffman encoding failed"))?;
    cx.emit(1, 1, bits)?;
    Ok(())
}
