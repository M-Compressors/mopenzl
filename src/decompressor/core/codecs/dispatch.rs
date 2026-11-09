//! Decoders of the `dispatch` feature: `dispatchN_byTag` (42) and
//! `dispatch_string` (54).
//!
//! Ports `decode_dispatchN_byTag_binding.c` / `_kernel.c` and
//! `decode_dispatch_string_binding.c` / `_kernel.c`. Both route pieces of
//! one output to numbered streams: `dispatchN_byTag` cuts a serial output
//! into segments of given sizes and tags, `dispatch_string` takes whole
//! strings by a per-string index.
//!
//! Format versions 21 to 27 only: before version 20 `dispatchN_byTag`
//! limited itself to 255 streams of 1-byte tags, and before version 21
//! `dispatch_string` used 1-byte indices and 255 streams; the 16-bit rules
//! of the reference's later branch apply here.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// Streams `dispatchN_byTag` may join (tags are at most 16 bits wide).
const MAX_TAGGED_STREAMS: usize = 1 << 16;

/// Streams `dispatch_string` may join (`ZL_DISPATCH_STRING_MAX_DISPATCHES`).
const MAX_STRING_STREAMS: usize = 2048;

/// `ZL_StandardTransformID_dispatchN_byTag`: tags, segment sizes, then the
/// serial streams.
pub(crate) const DISPATCH_N_BY_TAG: Desc = Desc {
    id: 42,
    min_version: 9,
    regens: &[Type::Serial],
    variable_regens: false,
    inputs: &[Type::Numeric, Type::Numeric],
    variable_inputs: Type::Serial.bit(),
    decode: by_tag,
};

/// `ZL_StandardTransformID_dispatch_string`: string indices, then the
/// string streams.
pub(crate) const DISPATCH_STRING: Desc = Desc {
    id: 54,
    min_version: 16,
    regens: &[Type::String],
    variable_regens: false,
    inputs: &[Type::Numeric],
    variable_inputs: Type::String.bit(),
    decode: by_index,
};

/// The little-endian number in `bytes` (at most 8 bytes).
fn read_le(bytes: &[u8]) -> u64 {
    let mut word = [0u8; 8];
    word[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(word)
}

/// `DI_dispatchN_byTag`: segment `i` (`sizes[i]` bytes) comes from the next
/// bytes of stream `tags[i]`.
fn by_tag(_: &mut Ctx<'_>, inputs: &[Input<'_>], regens: &mut [Stream]) -> Result<(), DecodeError> {
    let (tags, sizes, streams) = (&inputs[0], &inputs[1], &inputs[2..]);
    let segments = sizes.len();
    if tags.len() != segments || tags.width > 2 || streams.len() >= MAX_TAGGED_STREAMS {
        return Err(corrupt());
    }
    let mut total = 0usize;
    for stream in streams {
        total = total.checked_add(stream.len()).ok_or_else(corrupt)?;
    }
    let tag_of = |segment: usize| read_le(&tags.bytes[segment * tags.width..][..tags.width]);
    let size_of = |segment: usize| read_le(&sizes.bytes[segment * sizes.width..][..sizes.width]);

    // Every tag names a stream, and each stream is used up exactly.
    let mut used = alloc::vec![0usize; streams.len()];
    for segment in 0..segments {
        let tag = usize::try_from(tag_of(segment)).ok().ok_or_else(corrupt)?;
        let size = usize::try_from(size_of(segment)).ok().ok_or_else(corrupt)?;
        let slot = used.get_mut(tag).ok_or_else(corrupt)?;
        *slot = slot.checked_add(size).ok_or_else(corrupt)?;
    }
    if used.iter().zip(streams).any(|(&used, s)| used != s.len()) {
        return Err(corrupt());
    }

    let out = regens[0].fixed(1, total)?;
    let mut cursors = alloc::vec![0usize; streams.len()];
    for segment in 0..segments {
        // Both were range-checked above.
        let (tag, size) = (tag_of(segment) as usize, size_of(segment) as usize);
        let cursor = &mut cursors[tag];
        out.extend_from_slice(&streams[tag].bytes[*cursor..*cursor + size]);
        *cursor += size;
    }
    Ok(())
}

/// `DI_dispatch_string`: string `i` is the next string of stream
/// `indices[i]`.
fn by_index(
    _: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let (indices, streams) = (&inputs[0], &inputs[1..]);
    if indices.width != 2 || streams.len() > MAX_STRING_STREAMS {
        return Err(corrupt());
    }
    let count = indices.len();
    if count != 0 && streams.is_empty() {
        return Err(corrupt());
    }
    let index_of = |string: usize| usize::from(read_le(&indices.bytes[string * 2..][..2]) as u16);

    // Each stream holds exactly the strings routed to it.
    let mut routed = alloc::vec![0usize; streams.len()];
    for string in 0..count {
        *routed.get_mut(index_of(string)).ok_or_else(corrupt)? += 1;
    }
    let mut total = 0usize;
    for (stream, &routed) in streams.iter().zip(&routed) {
        if stream.len() != routed {
            return Err(corrupt());
        }
        total = total.checked_add(stream.bytes.len()).ok_or_else(corrupt)?;
    }

    let (bytes, lens) = regens[0].strings(count, total)?;
    // Per stream: bytes and strings consumed so far.
    let mut cursors = alloc::vec![(0usize, 0usize); streams.len()];
    for string in 0..count {
        let index = index_of(string);
        let (byte, next) = &mut cursors[index];
        let len = streams[index].lens[*next];
        let end = byte.checked_add(len as usize).ok_or_else(corrupt)?;
        bytes.extend_from_slice(streams[index].bytes.get(*byte..end).ok_or_else(corrupt)?);
        lens.push(len);
        *byte = end;
        *next += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn run(desc: &Desc, inputs: &[Input<'_>], budget: usize) -> Result<Stream, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[]);
        let mut regens = [Stream::default()];
        regens[0].prepare(desc.regen_type(0), budget);
        (desc.decode)(&mut ctx, inputs, &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream)
    }

    fn numbers(width: usize, bytes: &[u8]) -> Input<'_> {
        Input {
            ty: Type::Numeric,
            width,
            bytes,
            lens: &[],
        }
    }

    fn strings<'a>(bytes: &'a [u8], lens: &'a [u32]) -> Input<'a> {
        Input {
            ty: Type::String,
            width: 1,
            bytes,
            lens,
        }
    }

    #[test]
    fn tagged_segments_are_woven_back() {
        // Segments: "ab" -> 1, "c" -> 0, "def" -> 1, "g" -> 0.
        let (tags, sizes) = ([1u8, 0, 1, 0], [2u8, 1, 3, 1]);
        let out = run(
            &DISPATCH_N_BY_TAG,
            &[
                numbers(1, &tags),
                numbers(1, &sizes),
                Input::serial(b"cg"),
                Input::serial(b"abdef"),
            ],
            1 << 10,
        )
        .unwrap();
        assert_eq!(out.bytes(), b"abcdefg");
        // 16-bit tags and 32-bit sizes.
        let tags: Vec<u8> = [0u16, 1].iter().flat_map(|t| t.to_le_bytes()).collect();
        let sizes: Vec<u8> = [1u32, 1].iter().flat_map(|t| t.to_le_bytes()).collect();
        let out = run(
            &DISPATCH_N_BY_TAG,
            &[
                numbers(2, &tags),
                numbers(4, &sizes),
                Input::serial(b"x"),
                Input::serial(b"y"),
            ],
            1 << 10,
        )
        .unwrap();
        assert_eq!(out.bytes(), b"xy");
        // No segment, no stream.
        let out = run(&DISPATCH_N_BY_TAG, &[numbers(1, &[]), numbers(1, &[])], 8).unwrap();
        assert_eq!(out.bytes(), b"");
    }

    #[test]
    fn malformed_tagged_segments_are_corrupt() {
        let a = Input::serial(b"ab");
        type Case<'a> = (&'a [u8], &'a [u8], usize, &'a [Input<'a>]);
        let cases: [Case<'_>; 5] = [
            // A tag past the streams.
            (&[1], &[2], 1, &[a]),
            // A stream not used up.
            (&[0], &[1], 1, &[a]),
            // More than its stream holds.
            (&[0], &[3], 1, &[a]),
            // Tags and sizes of different counts.
            (&[0, 0], &[2], 1, &[a]),
            // Wide tags.
            (&[0, 0, 0, 0], &[2], 4, &[a]),
        ];
        for (tags, sizes, tag_width, streams) in cases {
            let mut inputs = alloc::vec![numbers(tag_width, tags), numbers(1, sizes)];
            inputs.extend_from_slice(streams);
            assert_eq!(
                run(&DISPATCH_N_BY_TAG, &inputs, 1 << 10).map(|_| ()),
                Err(corrupt()),
                "{tags:?} {sizes:?}"
            );
        }
        // Segment sizes that overflow the per-stream sum.
        let sizes: Vec<u8> = [u64::MAX, 2].iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(
            run(
                &DISPATCH_N_BY_TAG,
                &[numbers(1, &[0, 0]), numbers(8, &sizes), a],
                1 << 10
            )
            .map(|_| ()),
            Err(corrupt())
        );
        // The output does not fit the budget.
        assert_eq!(
            run(
                &DISPATCH_N_BY_TAG,
                &[numbers(1, &[0]), numbers(1, &[2]), a],
                1
            )
            .map(|_| ()),
            Err(corrupt())
        );
    }

    #[test]
    fn strings_are_routed_by_index() {
        let indices: Vec<u8> = [1u16, 0, 1, 1, 0]
            .iter()
            .flat_map(|i| i.to_le_bytes())
            .collect();
        let out = run(
            &DISPATCH_STRING,
            &[
                numbers(2, &indices),
                strings(b"bdd", &[1, 2]),
                strings(b"aceee", &[1, 1, 3]),
            ],
            1 << 10,
        )
        .unwrap();
        assert_eq!(out.bytes(), b"abceeedd");
        assert_eq!(out.as_input().lens, [1, 1, 1, 3, 2]);
        // Empty strings and no strings at all.
        let one = 0u16.to_le_bytes();
        let out = run(
            &DISPATCH_STRING,
            &[numbers(2, &one), strings(b"", &[0])],
            1 << 10,
        )
        .unwrap();
        assert_eq!(out.as_input().lens, [0]);
        let out = run(&DISPATCH_STRING, &[numbers(2, &[]), strings(b"", &[])], 8).unwrap();
        assert_eq!(out.as_input().len(), 0);
        let out = run(&DISPATCH_STRING, &[numbers(2, &[])], 8).unwrap();
        assert_eq!(out.as_input().len(), 0);
    }

    #[test]
    fn malformed_string_dispatches_are_corrupt() {
        let (zero, one) = (0u16.to_le_bytes(), [1u8, 0]);
        let a = strings(b"a", &[1]);
        type Case<'a> = (Input<'a>, &'a [Input<'a>]);
        let cases: [Case<'_>; 6] = [
            // Indices of the wrong width.
            (numbers(1, &[0]), &[a]),
            // Strings but no stream.
            (numbers(2, &zero), &[]),
            // An index past the streams.
            (numbers(2, &one), &[a]),
            // A stream with too many strings.
            (numbers(2, &[]), &[a]),
            // A stream with too few strings.
            (numbers(2, &zero), &[strings(b"", &[])]),
            // Streams with the right count but not the right content.
            (numbers(2, &zero), &[strings(b"", &[0, 0])]),
        ];
        for (indices, streams) in cases {
            let mut inputs = alloc::vec![indices];
            inputs.extend_from_slice(streams);
            assert_eq!(
                run(&DISPATCH_STRING, &inputs, 1 << 10).map(|_| ()),
                Err(corrupt()),
                "{inputs:?}"
            );
        }
        // More streams than the format allows, and an exceeded budget.
        let mut inputs = alloc::vec![numbers(2, &[])];
        inputs.extend(core::iter::repeat_n(
            strings(b"", &[]),
            MAX_STRING_STREAMS + 1,
        ));
        assert_eq!(
            run(&DISPATCH_STRING, &inputs, 1 << 20).map(|_| ()),
            Err(corrupt())
        );
        assert_eq!(
            run(&DISPATCH_STRING, &[numbers(2, &zero), a], 4).map(|_| ()),
            Err(corrupt())
        );
    }
}
