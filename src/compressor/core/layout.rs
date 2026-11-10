//! The layout of frames and chunks for any graph (`CCTX_getFinalGraph`,
//! `writeFrameHeader_internal`, `writeChunkHeaderV8_internal`).
//!
//! A chunk lists its codecs in *decoding* order, the reverse of the order the
//! nodes ran, then the regenerated-stream distances, then the sizes of the
//! stored streams. Stream `e` of the encoder (in creation order) is decoder
//! stream `N - 1 - e`; a codec's inputs are therefore addressed by distance
//! from the first stream it created.

use alloc::vec::Vec;

use fearless_simd::Level;

use super::Params;
use super::chunk::{CodecRecord, push_header_with};
use super::data::{Stream, Type, type_code};
use super::error::ExecError;
use super::rtgraph::RtGraph;
use super::sink::{Full, Sink};
use crate::dict::BundleId;
use crate::shared::wire::{
    BUNDLE_VERSION_MIN, COMMENT_VERSION_MIN, FLAG_BUNDLE_ID, FLAG_COMMENT,
    FLAG_COMPRESSED_CHECKSUM, FLAG_CONTENT_CHECKSUM, MAGIC_BASE,
};
use crate::shared::{varint, xxh3};

/// The longest header comment (`ZL_MAX_HEADER_COMMENT_SIZE_LIMIT`).
pub(crate) const COMMENT_MAX: usize = 10_000;

/// `ZL_varintSize`.
const fn varint_size(value: u64) -> usize {
    ((64 - (value | 1).leading_zeros()) as usize).div_ceil(7)
}

/// The decoding map of a chunk, computed from its runtime graph.
#[derive(Debug)]
pub(crate) struct Plan {
    /// Codecs in decoding order.
    pub(crate) codecs: Vec<CodecRecord>,
    /// Distances of every codec input, reduced by one.
    pub(crate) distances: Vec<u32>,
    /// Dictionary index of each codec (the dictionary lane); the lane is
    /// written only when a bundle is loaded at format version 25 or newer.
    pub(crate) dict: Vec<Option<u32>>,
    /// The private-header stream: every codec header in decoding order.
    pub(crate) headers: Vec<u8>,
    /// Stored streams, last created first.
    pub(crate) stored: Vec<u32>,
    /// Inputs of the frame.
    pub(crate) n_inputs: usize,
}

/// `CCTX_getFinalGraph`.
pub(crate) fn final_graph(
    rt: &RtGraph<'_>,
    n_inputs: usize,
    version: u32,
) -> Result<Plan, ExecError> {
    let _ = version;
    let n_nodes = rt.nodes.len();
    if n_nodes >= 20_000 || rt.streams.len() + 1 >= 110_000 {
        return Err(ExecError::Limit("nodes or streams"));
    }
    let mut plan = Plan {
        codecs: Vec::with_capacity(n_nodes),
        distances: Vec::new(),
        dict: Vec::with_capacity(n_nodes),
        headers: Vec::new(),
        stored: Vec::new(),
        n_inputs,
    };
    for rank in (0..n_nodes).rev() {
        let node = &rt.nodes[rank];
        if node.n_out < node.n_singletons {
            return Err(ExecError::GraphInvalid);
        }
        let (start, len) = node.header;
        plan.headers
            .extend_from_slice(&rt.staging[start..start + len]);
        plan.codecs.push(CodecRecord {
            standard: node.standard,
            id: node.codec,
            header_size: len as u32,
            variable_outputs: node.n_out - node.n_singletons,
            regenerated: node.inputs.len() as u32,
        });
        plan.dict.push(node.dict_index);
        for &input in &node.inputs {
            plan.distances.push(node.start_out - input - 1);
        }
    }
    for index in (0..rt.streams.len()).rev() {
        if rt.streams[index].to_store {
            plan.stored.push(index as u32);
        }
    }
    Ok(plan)
}

/// `computeFHBound` for a chunk header.
pub(crate) fn header_bound(plan: &Plan) -> usize {
    let nodes = plan.codecs.len();
    let buffers = plan.stored.len() + 1;
    4 + plan.n_inputs * 21
        + varint_size(nodes as u64)
        + varint_size((buffers - 1) as u64)
        + buffers * 4
        + nodes * 22
        + plan.distances.len() * 4
        + 4
        + 4
        + nodes.div_ceil(8)
        + 1
        + nodes * 3
}

fn stored_bytes<'r>(rt: &'r RtGraph<'_>, index: u32) -> &'r [u8] {
    rt.stream(index).map_or(&[][..], Stream::bytes)
}

/// Appends the chunk header of `plan` to `out`.
///
/// `lane` writes the dictionary lane (`compressDictIdxs`): format version 25
/// or newer with a bundle loaded.
pub(crate) fn write_chunk_header(
    plan: &Plan,
    version: u32,
    lane: bool,
    rt: &RtGraph<'_>,
    out: &mut Vec<u8>,
) {
    let sizes: Vec<usize> = plan
        .stored
        .iter()
        .map(|&index| stored_bytes(rt, index).len())
        .collect();
    push_header_with(
        out,
        version,
        &plan.codecs,
        lane.then_some(&plan.dict[..]),
        &plan.distances,
        &sizes,
    );
}

/// Bytes of every stored buffer, the private-header stream included.
pub(crate) fn stored_size(plan: &Plan, rt: &RtGraph<'_>) -> usize {
    plan.headers.len()
        + plan
            .stored
            .iter()
            .map(|&index| stored_bytes(rt, index).len())
            .sum::<usize>()
}

/// Writes the private-header stream and the stored streams.
pub(crate) fn put_stored(plan: &Plan, rt: &RtGraph<'_>, sink: &mut dyn Sink) -> Result<(), Full> {
    sink.put(&plan.headers)?;
    for &index in &plan.stored {
        sink.put(stored_bytes(rt, index))?;
    }
    Ok(())
}

/// `STREAM_hashLastCommit_xxh3low32`: XXH3 over the content of every input,
/// followed for strings by their length array (little-endian `u32`s).
pub(crate) fn content_checksum(inputs: &[Stream<'_>], version: u32, simd: Level) -> u32 {
    let _ = version;
    if let [only] = inputs
        && only.ty() != Type::String
    {
        return xxh3::hash32(simd, only.bytes());
    }
    let mut all = Vec::new();
    for input in inputs {
        all.extend_from_slice(input.bytes());
        if input.ty() == Type::String {
            all.extend(input.lens().iter().flat_map(|len| len.to_le_bytes()));
        }
    }
    xxh3::hash32(simd, &all)
}

/// What the frame header records of an input (`InputDesc`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct InputDesc {
    /// The input's type.
    pub(crate) ty: Type,
    /// Bytes of content (the sum of the lengths for strings).
    pub(crate) size: usize,
    /// Elements; only recorded for strings.
    pub(crate) n_elts: usize,
}

impl From<&Stream<'_>> for InputDesc {
    fn from(stream: &Stream<'_>) -> Self {
        Self {
            ty: stream.ty(),
            size: stream.content_size(),
            n_elts: stream.n_elts(),
        }
    }
}

/// [`write_frame_header`] for streams.
pub(crate) fn write_frame_header_for(
    params: &Params,
    simd: Level,
    inputs: &[Stream<'_>],
    comment: Option<&[u8]>,
    bundle: Option<&BundleId>,
    sink: &mut dyn Sink,
) -> Result<(), ExecError> {
    let descs: Vec<InputDesc> = inputs.iter().map(InputDesc::from).collect();
    write_frame_header(params, simd, &descs, comment, bundle, sink)
}

/// `writeFrameHeader_internal` for v21+: magic, flags, the dictionary bundle
/// ID (v25+, when `bundle` is set), input types and sizes, comment and check
/// byte. The bundle ID is written without its trailing zero bytes.
pub(crate) fn write_frame_header(
    params: &Params,
    simd: Level,
    inputs: &[InputDesc],
    comment: Option<&[u8]>,
    bundle: Option<&BundleId>,
    sink: &mut dyn Sink,
) -> Result<(), ExecError> {
    let bundle = bundle.filter(|_| params.version >= BUNDLE_VERSION_MIN);
    let comment =
        comment.filter(|comment| !comment.is_empty() && params.version >= COMMENT_VERSION_MIN);
    let comment_len = comment.map_or(0, <[u8]>::len);
    if comment_len > COMMENT_MAX {
        return Err(ExecError::Parameter("header comment too long"));
    }
    let bound = 4
        + inputs.len() * 21
        + 1
        + varint_size(u64::MAX)
        + 3
        + 8
        + if comment_len > 0 { 4 + comment_len } else { 0 }
        + if bundle.is_some() { 1 + 32 } else { 0 };
    if sink.remaining().is_some_and(|room| room < bound) {
        return Err(ExecError::Full);
    }
    let mut out = Vec::with_capacity(bound);
    out.extend_from_slice(&(MAGIC_BASE + params.version).to_le_bytes());
    let mut flags = 0;
    if params.content_checksum {
        flags |= FLAG_CONTENT_CHECKSUM;
    }
    if params.compressed_checksum {
        flags |= FLAG_COMPRESSED_CHECKSUM;
    }
    if comment_len > 0 {
        flags |= FLAG_COMMENT;
    }
    if bundle.is_some() {
        flags |= FLAG_BUNDLE_ID;
    }
    out.push(flags);
    if let Some(bundle) = bundle {
        let bytes: &[u8] = bundle.as_ref();
        let significant = bytes
            .iter()
            .rposition(|&byte| byte != 0)
            .map_or(0, |at| at + 1);
        if significant == 0 {
            return Err(ExecError::Parameter("invalid (zero) bundle ID"));
        }
        out.push(significant as u8);
        out.extend_from_slice(&bytes[..significant]);
    }
    let n = inputs.len();
    let code = |index: usize| u64::from(type_code(inputs[index].ty));
    if n < 15 {
        let mut token = n as u8;
        for index in 0..n.min(2) {
            token |= (code(index) as u8) << (2 * index + 4);
        }
        out.push(token);
        let mut types = 0u64;
        for index in 2..n {
            types |= code(index) << (2 * (index - 2));
        }
        let bytes = n.saturating_sub(2).div_ceil(4);
        out.extend_from_slice(&types.to_le_bytes()[..bytes]);
    } else {
        out.push((((n - 15) << 4) | 15) as u8);
        out.push(((n - 15) >> 4) as u8);
        let mut index = 0;
        let mut packed = Vec::with_capacity(n.div_ceil(4));
        while index < n {
            let limit = (index + 32).min(n);
            let mut types = 0u64;
            for position in index..limit {
                types |= code(position) << ((2 * position) % 64);
            }
            packed.extend_from_slice(&types.to_le_bytes());
            index = limit;
        }
        packed.truncate(n.div_ceil(4));
        out.extend_from_slice(&packed);
    }
    for input in inputs {
        varint::push(&mut out, input.size as u64 + 1);
    }
    for input in inputs {
        if input.ty == Type::String {
            varint::push(&mut out, input.n_elts as u64);
        }
    }
    if let Some(comment) = comment {
        varint::push(&mut out, comment_len as u64);
        out.extend_from_slice(comment);
    }
    if params.compressed_checksum {
        out.push(xxh3::hash64(simd, &out) as u8);
    }
    sink.put(&out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Backend;
    use crate::compressor::core::sink::VecSink;

    fn params(version: u32) -> Params {
        Params {
            version,
            ..Params::from(&crate::EncoderConfig::DEFAULT)
        }
    }

    fn header(version: u32, bundle: Option<&BundleId>) -> Result<Vec<u8>, ExecError> {
        let mut out = Vec::new();
        let input = InputDesc {
            ty: Type::Serial,
            size: 300,
            n_elts: 0,
        };
        write_frame_header(
            &params(version),
            Backend::default().0,
            &[input],
            None,
            bundle,
            &mut VecSink::new(&mut out),
        )?;
        Ok(out)
    }

    #[test]
    fn the_bundle_id_follows_the_flags_without_trailing_zeros() {
        let mut id = [0u8; 32];
        id[..3].copy_from_slice(&[9, 8, 7]);
        let bundle = BundleId::from(id);
        let out = header(27, Some(&bundle)).unwrap();
        // Flags carry the bundle bit; then a length byte and the three bytes.
        assert_eq!(out[4] & 8, 8);
        assert_eq!(&out[5..9], &[3, 9, 8, 7]);
        // The header with the bundle is the plain header plus those bytes.
        let plain = header(27, None).unwrap();
        assert_eq!(plain[4] & 8, 0);
        assert_eq!(out.len(), plain.len() + 4);
        // Below version 25 the bundle is not recorded at all.
        assert_eq!(
            header(24, Some(&bundle)).unwrap(),
            header(24, None).unwrap()
        );
    }

    #[test]
    fn an_all_zero_bundle_id_is_refused() {
        let zero = BundleId::from([0; 32]);
        assert!(matches!(
            header(27, Some(&zero)),
            Err(ExecError::Parameter(_))
        ));
    }
}
