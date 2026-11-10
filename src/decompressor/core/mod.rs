//! The OpenZL decoder: frame headers, chunk graphs and the session state
//! machine.
//!
//! [`Engine`] owns the reusable chunk layout, the Zstandard decoder and the
//! buffers of intermediate streams, plus the SIMD level selected once when
//! the decompressor was built; every checksum below it runs at that level.

pub(crate) mod chunk;
pub(crate) mod codecs;
pub(crate) mod data;
pub(crate) mod frame;
pub(crate) mod out;
pub(crate) mod reader;
pub(crate) mod stream;
#[cfg(feature = "zstd")]
pub(crate) mod zstd;

use alloc::vec::Vec;

use alloc::sync::Arc;

use fearless_simd::Level;

use super::config::{DecoderConfig, FrameMode};
use super::error::{DecodeError, InvalidDataKind};
use super::typed::TypedOutput;
use crate::dict::{BundleStore, StoredBundle};
use crate::shared::wire::END_OF_FRAME;
use crate::shared::xxh3;
use crate::{Backend, RetentionPolicy};
use chunk::{Layout, Source};
use codecs::{Ctx, State};
use data::{Input, Stream, Type};
use frame::{FrameInfo, OutputSpec};
use out::Out;
use reader::Parse;

/// Bytes a regenerated stream may occupy per byte of output still to come,
/// plus [`STREAM_SLACK`]. Codecs regenerate streams close to the data they
/// were made from; the widest expansion of a reference graph is a parsed
/// integer, eight bytes for a one-digit string, so 16 leaves room while a
/// hostile frame cannot make the decoder allocate without bound.
const STREAM_EXPANSION: u64 = 16;

/// Bytes every regenerated stream may occupy beyond its share of the output.
const STREAM_SLACK: u64 = 1 << 20;

/// What the next unit of a frame is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Unit {
    /// A chunk of this many bytes.
    Chunk(usize),
    /// The end-of-frame marker.
    End,
}

/// Reusable decoding state.
#[derive(Debug)]
pub(crate) struct Engine {
    layout: Layout,
    state: State,
    /// Regenerated streams of the current chunk, by stream index.
    buffers: Vec<Stream>,
    /// The streams of the node running now, taken out of `buffers`.
    running: Vec<Stream>,
    /// Typed chunk outputs gathered for the content checksum.
    hashed: Vec<u8>,
    simd: Level,
    /// The bundles frames may name.
    store: Option<Arc<BundleStore>>,
    /// The materialized bundle of the frame being decoded.
    bundle: Option<Arc<StoredBundle>>,
}

/// `ZL_readCE32` at `at`.
fn read_le32(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(word)
}

/// Input `i` of node `index`: stream `end - 1 - i` of its input range,
/// checked against the codec's signature.
fn node_input<'a>(
    layout: &Layout,
    index: usize,
    chunk: &'a [u8],
    buffers: &'a [Stream],
    i: usize,
) -> Result<Input<'a>, DecodeError> {
    let node = &layout.nodes[index];
    let id = node.inputs.end - 1 - i;
    let input = match &layout.streams[id] {
        Source::Stored(range) => Input::serial(&chunk[range.clone()]),
        Source::Node(_) => buffers[id].as_input(),
    };
    if node.desc.accepts(i, input.ty) {
        Ok(input)
    } else {
        Err(DecodeError::invalid(InvalidDataKind::Graph))
    }
}

/// A frame producing more than it declares.
#[cfg(feature = "zstd")]
const fn overrun(frame: &FrameInfo) -> DecodeError {
    DecodeError::ContentSizeMismatch {
        declared: frame.size,
        decoded: frame.size.saturating_add(1),
    }
}

impl Engine {
    pub(crate) fn new(backend: Backend) -> Self {
        Self {
            layout: Layout::default(),
            state: State::new(backend),
            buffers: Vec::new(),
            running: Vec::new(),
            hashed: Vec::new(),
            simd: backend.0,
            store: None,
            bundle: None,
        }
    }

    /// Sets the bundles frames may name.
    pub(crate) fn set_store(&mut self, store: Option<Arc<BundleStore>>) {
        self.store = store;
        self.bundle = None;
    }

    /// Resolves the dictionary bundle a frame names, as the reference does
    /// once its header is decoded (`ZL_DictLoader_fetchDictBundle`).
    ///
    /// # Errors
    ///
    /// [`DecodeError::MissingBundle`] when no store was given or it lacks the
    /// bundle.
    pub(crate) fn open_frame(&mut self, frame: &FrameInfo) -> Result<(), DecodeError> {
        self.bundle = None;
        let Some(id) = frame.bundle else {
            return Ok(());
        };
        let bundle = self
            .store
            .as_ref()
            .and_then(|store| store.get(&id))
            .ok_or(DecodeError::MissingBundle { bundle: id })?;
        self.bundle = Some(Arc::clone(bundle));
        Ok(())
    }

    /// The SIMD level checksums run at.
    pub(crate) const fn simd(&self) -> Level {
        self.simd
    }

    /// What the unit at the front of `bytes` is, from its header alone.
    pub(crate) fn measure(&mut self, bytes: &[u8], frame: &FrameInfo) -> Result<Unit, Parse> {
        match bytes.first() {
            None => Err(Parse::Truncated),
            Some(&END_OF_FRAME) => Ok(Unit::End),
            Some(_) => {
                self.layout.parse(bytes, frame)?;
                Ok(Unit::Chunk(self.layout.len))
            }
        }
    }

    /// Decodes the whole chunk `chunk` into `out` and returns the bytes it
    /// regenerated, at most `remaining`.
    #[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
    pub(crate) fn decode_chunk(
        &mut self,
        chunk: &[u8],
        frame: &FrameInfo,
        config: &DecoderConfig,
        remaining: u64,
        out: &mut impl Out,
    ) -> Result<u64, DecodeError> {
        let budget = self.prepare(chunk, frame, config, remaining)?;
        let start = out.len();
        let last = self.layout.streams.len() - 1;
        let mut direct = false;
        for index in 0..self.layout.nodes.len() {
            direct |= self.run_node(index, chunk, frame, remaining, budget, Some(&mut *out))?;
        }
        if !direct {
            let oversized = |len: usize| DecodeError::ContentSizeMismatch {
                declared: frame.size,
                decoded: frame.size - remaining + len as u64,
            };
            let written = out.len();
            match &self.layout.streams[last] {
                Source::Stored(range) => {
                    let content = &chunk[range.clone()];
                    if content.len() as u64 > remaining {
                        return Err(oversized(content.len()));
                    }
                    out.put(content)
                }
                Source::Node(_) => {
                    let stream = &mut self.buffers[last];
                    if stream.ty() != Type::Serial {
                        return Err(DecodeError::invalid(InvalidDataKind::Graph));
                    }
                    if stream.bytes().len() as u64 > remaining {
                        return Err(oversized(stream.bytes().len()));
                    }
                    // The stream is not read again in this chunk: hand its
                    // buffer over instead of copying it when possible.
                    out.put_vec(stream.bytes_vec_mut())
                }
            }
            .map_err(|_| DecodeError::OutputTooSmall { written })?;
        }
        if let Some(at) = self.layout.content_checksum
            && config.content_checksum_check()
        {
            let expected = read_le32(chunk, at);
            let computed = xxh3::hash32(self.simd, out.since(start));
            if expected != computed {
                return Err(DecodeError::ContentChecksumMismatch { expected, computed });
            }
        }
        Ok((out.len() - start) as u64)
    }

    /// Lays out `chunk`, checks its compressed checksum and sizes the stream
    /// buffers; returns the byte budget of each regenerated stream when
    /// `remaining` output bytes are still to come.
    fn prepare(
        &mut self,
        chunk: &[u8],
        frame: &FrameInfo,
        config: &DecoderConfig,
        remaining: u64,
    ) -> Result<usize, DecodeError> {
        match self.layout.parse(chunk, frame) {
            Ok(()) if self.layout.len == chunk.len() => {}
            Ok(()) | Err(Parse::Truncated) => {
                return Err(DecodeError::invalid(InvalidDataKind::ChunkHeader));
            }
            Err(Parse::Failed(error)) => return Err(error),
        }
        if let Some(at) = self.layout.compressed_checksum
            && config.compressed_checksum_check()
        {
            let expected = read_le32(chunk, at);
            let computed = xxh3::hash32(self.simd, &chunk[..at]);
            if expected != computed {
                return Err(DecodeError::CompressedChecksumMismatch { expected, computed });
            }
        }
        if self.buffers.len() < self.layout.streams.len() {
            self.buffers
                .resize_with(self.layout.streams.len(), Stream::default);
        }
        let budget = remaining
            .saturating_mul(STREAM_EXPANSION)
            .saturating_add(STREAM_SLACK);
        Ok(usize::try_from(budget).unwrap_or(usize::MAX))
    }

    /// Runs node `index` of the current layout over `chunk`, regenerating
    /// its streams into `buffers`, or, for Zstandard regenerating the
    /// chunk's output, straight into `out`; returns whether it wrote `out`.
    fn run_node(
        &mut self,
        index: usize,
        chunk: &[u8],
        #[cfg_attr(
            not(feature = "zstd"),
            expect(unused_variables, reason = "only the Zstandard fast path reads it")
        )]
        frame: &FrameInfo,
        remaining: u64,
        budget: usize,
        out: Option<&mut impl Out>,
    ) -> Result<bool, DecodeError> {
        let Self {
            layout,
            state,
            buffers,
            running,
            bundle,
            ..
        } = self;
        let node = &layout.nodes[index];
        let desc = node.desc;
        let regenerated = &layout.regenerated[node.regens.clone()];
        let last = layout.streams.len() - 1;
        let count = node.inputs.len();
        // `decompress2.c`: an index past the bundle is corruption.
        let dict = match node.dict {
            None => None,
            Some(index) => Some(
                bundle
                    .as_ref()
                    .and_then(|bundle| bundle.dicts.get(index as usize))
                    .map(|dict| &**dict)
                    .ok_or(DecodeError::invalid(InvalidDataKind::DictionaryIndex))?,
            ),
        };
        let input = |i, buffers| node_input(layout, index, chunk, buffers, i);
        #[cfg(feature = "zstd")]
        if let Some(out) = out
            && desc.id == codecs::zstd::ZSTD.id
            && regenerated == [last]
            && frame.outputs == 1
        {
            let stream = input(0, buffers)?;
            let (width, frame_bytes) = zstd::split(stream.bytes)?;
            let size = zstd::content_size(frame_bytes)?;
            // The output is serial, so of width 1 (`STREAM_validateTypeWidth`).
            if width != 1 {
                return Err(data::corrupt());
            }
            if size > remaining {
                return Err(overrun(frame));
            }
            out.zstd(
                &mut state.zstd,
                frame_bytes,
                size as usize,
                zstd::zstd_dictionary(dict),
            )?;
            return Ok(true);
        }
        #[cfg(not(feature = "zstd"))]
        let _ = (last, remaining, out);
        // Regenerated streams lie past the node's inputs, so taking them
        // out first leaves every input in place.
        running.clear();
        for (n, &id) in regenerated.iter().enumerate() {
            let mut stream = core::mem::take(&mut buffers[id]);
            stream.prepare(desc.regen_type(n), budget);
            running.push(stream);
        }
        let inputs = (0..count)
            .map(|i| input(i, buffers))
            .collect::<Result<Vec<_>, _>>();
        let result = inputs.and_then(|inputs| {
            let mut ctx = Ctx::new(state, &chunk[node.header.clone()]).with_dictionary(dict);
            (desc.decode)(&mut ctx, &inputs, running)?;
            running.iter().try_for_each(Stream::validate)
        });
        for (stream, &id) in running.drain(..).zip(regenerated) {
            buffers[id] = stream;
        }
        result.map(|()| false)
    }

    /// Decodes the single frame at the front of `src` into its typed
    /// outputs and returns them with the bytes consumed.
    pub(crate) fn decode_typed(
        &mut self,
        config: &DecoderConfig,
        src: &[u8],
    ) -> Result<(Vec<TypedOutput>, usize), DecodeError> {
        let limits = config.limits();
        let truncated = |error: Parse| match error {
            Parse::Truncated => DecodeError::UnexpectedEndOfInput,
            Parse::Failed(error) => error,
        };
        let mut specs = Vec::new();
        let info = frame::parse_with(src, self.simd, config.compressed_checksum_check(), |spec| {
            specs.push(spec);
        })
        .map_err(truncated)?;
        if let Some(limit) = limits.max_output_bytes()
            && info.size > limit
        {
            return Err(DecodeError::OutputLimitExceeded { limit });
        }
        self.open_frame(&info)?;
        let mut outputs: Vec<TypedOutput> = specs
            .iter()
            .map(|spec| TypedOutput::new(spec.ty.into()))
            .collect();
        let mut pos = info.header_len;
        let mut chunks = 0u64;
        loop {
            if let Some(limit) = limits.max_input_bytes()
                && pos as u64 > limit
            {
                return Err(DecodeError::InputLimitExceeded { limit });
            }
            match self.measure(&src[pos..], &info).map_err(truncated)? {
                Unit::End if chunks == 0 => {
                    return Err(DecodeError::invalid(InvalidDataKind::EmptyFrame));
                }
                Unit::End => {
                    pos += 1;
                    break;
                }
                Unit::Chunk(len) => {
                    let chunk = src
                        .get(pos..pos.saturating_add(len))
                        .ok_or(DecodeError::UnexpectedEndOfInput)?;
                    self.decode_typed_chunk(chunk, &info, &specs, config, &mut outputs)?;
                    pos += len;
                    chunks += 1;
                }
            }
        }
        if let Some(limit) = limits.max_input_bytes()
            && pos as u64 > limit
        {
            return Err(DecodeError::InputLimitExceeded { limit });
        }
        let decoded = outputs.iter().map(|out| out.content().len() as u64).sum();
        let complete = outputs.iter().zip(&specs).all(|(out, spec)| {
            out.content().len() as u64 == spec.size
                && out.string_lens().len() as u64 == spec.strings
        });
        if !complete {
            return Err(DecodeError::ContentSizeMismatch {
                declared: info.size,
                decoded,
            });
        }
        Ok((outputs, pos))
    }

    /// Decodes `chunk` and appends its share of every output.
    fn decode_typed_chunk(
        &mut self,
        chunk: &[u8],
        frame: &FrameInfo,
        specs: &[OutputSpec],
        config: &DecoderConfig,
        outputs: &mut [TypedOutput],
    ) -> Result<(), DecodeError> {
        let remaining: u64 = outputs
            .iter()
            .zip(specs)
            .map(|(out, spec)| {
                let strings = spec.strings.saturating_sub(out.string_lens().len() as u64);
                (spec.size - out.content().len() as u64).saturating_add(strings.saturating_mul(4))
            })
            .fold(0, u64::saturating_add);
        let budget = self.prepare(chunk, frame, config, remaining)?;
        for index in 0..self.layout.nodes.len() {
            self.run_node(
                index,
                chunk,
                frame,
                remaining,
                budget,
                None::<&mut out::VecOut<'_>>,
            )?;
        }
        let total = self.layout.streams.len();
        let check = config.content_checksum_check() && self.layout.content_checksum.is_some();
        self.hashed.clear();
        // With one output and no string lengths the hashed bytes are the
        // output itself: hash it in place rather than staging a copy.
        let mut direct_hash = None;
        let decoded: u64 = outputs.iter().map(|out| out.content().len() as u64).sum();
        let single = outputs.len() == 1;
        for (n, (out, spec)) in outputs.iter_mut().zip(specs).enumerate() {
            let id = total - 1 - n;
            let input = match &self.layout.streams[id] {
                Source::Stored(range) => Input::serial(&chunk[range.clone()]),
                Source::Node(_) => self.buffers[id].as_input(),
            };
            if input.ty != spec.ty {
                return Err(DecodeError::invalid(InvalidDataKind::Graph));
            }
            if (out.content().len() + input.bytes.len()) as u64 > spec.size
                || (out.string_lens().len() + input.lens.len()) as u64 > spec.strings
            {
                return Err(DecodeError::ContentSizeMismatch {
                    declared: frame.size,
                    decoded: decoded + input.bytes.len() as u64,
                });
            }
            if check {
                // `STREAM_hashLastCommit_xxh3low32`: contents, then string
                // lengths, of every output in order.
                if single && input.lens.is_empty() {
                    direct_hash = Some(xxh3::hash32(self.simd, input.bytes));
                } else {
                    self.hashed.extend_from_slice(input.bytes);
                    for len in input.lens {
                        self.hashed.extend_from_slice(&len.to_le_bytes());
                    }
                }
            }
            let width = input.width;
            let appended =
                if out.has_no_content() && matches!(self.layout.streams[id], Source::Node(_)) {
                    // The stream is not read again in this chunk: the output
                    // takes its buffers instead of a copy of them.
                    let (bytes, lens) = self.buffers[id].take_content();
                    out.adopt(width, bytes, lens)
                } else {
                    out.append(width, input.bytes, input.lens)
                };
            appended.or(Err(DecodeError::invalid(InvalidDataKind::Graph)))?;
        }
        if let Some(at) = self.layout.content_checksum
            && check
        {
            let expected = read_le32(chunk, at);
            let computed = direct_hash.unwrap_or_else(|| xxh3::hash32(self.simd, &self.hashed));
            if expected != computed {
                return Err(DecodeError::ContentChecksumMismatch { expected, computed });
            }
        }
        Ok(())
    }

    /// Decodes the frames at the front of `src` into `out`, as `config`
    /// says, and returns the bytes consumed.
    pub(crate) fn decode_frames(
        &mut self,
        config: &DecoderConfig,
        src: &[u8],
        out: &mut impl Out,
    ) -> Result<usize, DecodeError> {
        let limits = config.limits();
        let truncated = |error: Parse| match error {
            Parse::Truncated => DecodeError::UnexpectedEndOfInput,
            Parse::Failed(error) => error,
        };
        let mut pos = 0usize;
        let mut total_out = 0u64;
        loop {
            let info = frame::parse(&src[pos..], self.simd, config.compressed_checksum_check())
                .map_err(truncated)?;
            if let Some(limit) = limits.max_output_bytes()
                && total_out.saturating_add(info.size) > limit
            {
                return Err(DecodeError::OutputLimitExceeded { limit });
            }
            self.open_frame(&info)?;
            pos += info.header_len;
            let mut decoded = 0u64;
            let mut chunks = 0u64;
            loop {
                if let Some(limit) = limits.max_input_bytes()
                    && pos as u64 > limit
                {
                    return Err(DecodeError::InputLimitExceeded { limit });
                }
                match self.measure(&src[pos..], &info).map_err(truncated)? {
                    Unit::End if chunks == 0 => {
                        return Err(DecodeError::invalid(InvalidDataKind::EmptyFrame));
                    }
                    Unit::End => {
                        pos += 1;
                        break;
                    }
                    Unit::Chunk(len) => {
                        let chunk = src
                            .get(pos..pos.saturating_add(len))
                            .ok_or(DecodeError::UnexpectedEndOfInput)?;
                        decoded +=
                            self.decode_chunk(chunk, &info, config, info.size - decoded, out)?;
                        pos += len;
                        chunks += 1;
                    }
                }
            }
            if let Some(limit) = limits.max_input_bytes()
                && pos as u64 > limit
            {
                return Err(DecodeError::InputLimitExceeded { limit });
            }
            if decoded != info.size {
                return Err(DecodeError::ContentSizeMismatch {
                    declared: info.size,
                    decoded,
                });
            }
            total_out += decoded;
            if config.frame_mode() == FrameMode::Single || pos == src.len() {
                return Ok(pos);
            }
        }
    }

    /// Bytes kept allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.layout.retained_bytes()
            + self.state.retained_bytes()
            + self
                .buffers
                .iter()
                .map(Stream::retained_bytes)
                .sum::<usize>()
            + (self.buffers.capacity() + self.running.capacity()) * core::mem::size_of::<Stream>()
            + self.hashed.capacity()
    }

    /// Releases what `policy` does not allow to keep.
    pub(crate) fn trim(&mut self, policy: RetentionPolicy) {
        if !policy.allows(self.retained_bytes()) {
            self.layout.release();
            self.state.release();
            self.buffers = Vec::new();
            self.running = Vec::new();
            self.hashed = Vec::new();
        }
    }
}

// Every case here decodes reference frames, which use Zstandard.
#[cfg(all(test, feature = "zstd"))]
mod tests {
    use super::out::{SliceOut, VecOut};
    use super::*;

    fn engine() -> Engine {
        Engine::new(Backend::default())
    }

    fn reference(input: &[u8]) -> Vec<u8> {
        openzl_ffi::compress(input, openzl_ffi::Params::default()).unwrap()
    }

    fn decode(engine: &mut Engine, src: &[u8]) -> Result<Vec<u8>, DecodeError> {
        let mut out = Vec::new();
        engine.decode_frames(&DecoderConfig::default(), src, &mut VecOut::new(&mut out))?;
        Ok(out)
    }

    #[test]
    fn reference_frames_decode() {
        let mut engine = engine();
        let text: Vec<u8> = (0..50_000u32)
            .map(|i| b"openzl"[i as usize % 6] ^ (i / 997) as u8)
            .collect();
        for input in [&b""[..], b"hello", b"0123456789", &text] {
            assert_eq!(decode(&mut engine, &reference(input)).unwrap(), input);
        }
    }

    #[test]
    fn corrupted_chunks_fail_their_checksums() {
        let mut engine = engine();
        let input = alloc::vec![9u8; 4000];
        let frame = reference(&input);
        let mut damaged = frame.clone();
        damaged[20] ^= 1;
        assert!(matches!(
            decode(&mut engine, &damaged),
            Err(DecodeError::CompressedChecksumMismatch { .. })
        ));
        // Same damage with the compressed checksum ignored.
        let config = DecoderConfig::default().with_compressed_checksum_check(false);
        let mut out = Vec::new();
        assert!(
            engine
                .decode_frames(&config, &damaged, &mut VecOut::new(&mut out))
                .is_err()
        );
    }

    #[test]
    fn truncation_limits_and_small_slices_are_reported() {
        let mut engine = engine();
        let frame = reference(&alloc::vec![1u8; 3000]);
        for cut in [0, 5, frame.len() - 1] {
            assert_eq!(
                decode(&mut engine, &frame[..cut]),
                Err(DecodeError::UnexpectedEndOfInput)
            );
        }
        let stored = reference(b"stored payload");
        let mut slice = [0u8; 3];
        assert!(matches!(
            engine.decode_frames(
                &DecoderConfig::default(),
                &stored,
                &mut SliceOut::new(&mut slice)
            ),
            Err(DecodeError::OutputTooSmall { .. })
        ));
        let mut slice = [0u8; 100];
        assert!(matches!(
            engine.decode_frames(
                &DecoderConfig::default(),
                &frame,
                &mut SliceOut::new(&mut slice)
            ),
            Err(DecodeError::OutputTooSmall { .. })
        ));
        let limited = DecoderConfig::from(
            super::super::config::DecodeLimits::DEFAULT.with_max_output_bytes(Some(10)),
        );
        let mut out = Vec::new();
        assert_eq!(
            engine.decode_frames(&limited, &frame, &mut VecOut::new(&mut out)),
            Err(DecodeError::OutputLimitExceeded { limit: 10 })
        );
        let limited = DecoderConfig::from(
            super::super::config::DecodeLimits::DEFAULT.with_max_input_bytes(Some(10)),
        );
        assert_eq!(
            engine.decode_frames(&limited, &frame, &mut VecOut::new(&mut out)),
            Err(DecodeError::InputLimitExceeded { limit: 10 })
        );
    }

    #[test]
    fn trimming_releases_buffers() {
        let mut engine = engine();
        decode(&mut engine, &reference(&alloc::vec![3u8; 5000])).unwrap();
        assert!(engine.retained_bytes() > 0);
        engine.trim(RetentionPolicy::ReleaseAll);
        assert!(engine.buffers.is_empty());
        assert_eq!(Backend(engine.simd()), Backend::default());
    }
}
