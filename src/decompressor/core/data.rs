//! Typed streams between the codecs of a chunk (`ZL_Data`).
//!
//! A stream has a type, an element width and its content. Stored streams
//! are serial with width 1 and borrow the chunk; regenerated streams are
//! [`Stream`]s the engine owns and reuses from chunk to chunk. Numeric
//! content is little-endian whatever the host, which is what the frame
//! stores and what conversions read and write. Serial streams have width 1
//! and numeric ones 1, 2, 4 or 8 (`STREAM_validateTypeWidth`).
//!
//! A codec sees its inputs as [`Input`] views and fills each regenerated
//! stream through [`Stream::fixed`] or [`Stream::strings`], which enforce
//! the byte budget the engine set; the engine checks the result once the
//! codec returns.

use alloc::vec::Vec;

use crate::decompressor::error::{DecodeError, InvalidDataKind};
pub(crate) use crate::shared::wire::StreamType as Type;

/// A corrupted codec stream.
pub(crate) const fn corrupt() -> DecodeError {
    DecodeError::invalid(InvalidDataKind::CodecStream)
}

/// A read-only view of a codec input.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Input<'a> {
    /// The stream type.
    pub(crate) ty: Type,
    /// Bytes per element; 1 for strings.
    pub(crate) width: usize,
    /// The content.
    pub(crate) bytes: &'a [u8],
    /// String lengths (strings only).
    pub(crate) lens: &'a [u32],
}

impl<'a> Input<'a> {
    /// A stored stream: serial bytes of width 1.
    pub(crate) const fn serial(bytes: &'a [u8]) -> Self {
        Self {
            ty: Type::Serial,
            width: 1,
            bytes,
            lens: &[],
        }
    }

    /// Number of elements: strings for string streams, otherwise
    /// `bytes / width`.
    #[cfg_attr(
        not(any(
            test,
            feature = "bitpack",
            feature = "bitsplit",
            feature = "bitunpack",
            feature = "concat",
            feature = "constant",
            feature = "conversion",
            feature = "dedup",
            feature = "delta",
            feature = "dispatch",
            feature = "divide-by",
            feature = "entropy",
            feature = "field-lz",
            feature = "flatpack",
            feature = "float-deconstruct",
            feature = "interleave",
            feature = "lz",
            feature = "lz4",
            feature = "merge-sorted",
            feature = "mux-lengths",
            feature = "parse-int",
            feature = "partition",
            feature = "pivco-huffman",
            feature = "prefix",
            feature = "quantize",
            feature = "range-pack",
            feature = "sentinel",
            feature = "sparse-num",
            feature = "split",
            feature = "split-by-struct",
            feature = "tokenize",
            feature = "transpose",
            feature = "zigzag"
        )),
        expect(
            dead_code,
            reason = "only the codecs other than Zstandard count input elements"
        )
    )]
    pub(crate) const fn len(&self) -> usize {
        match self.ty {
            Type::String => self.lens.len(),
            _ => self.bytes.len() / self.width,
        }
    }
}

/// A regenerated stream.
#[derive(Debug, Default)]
pub(crate) struct Stream {
    ty: Type,
    width: usize,
    bytes: Vec<u8>,
    lens: Vec<u32>,
    /// Whether a codec created the stream in this chunk.
    ready: bool,
    /// Bytes the content (and four per string length) may occupy.
    budget: usize,
}

impl Stream {
    /// Clears the stream for a codec to regenerate as type `ty` within
    /// `budget` bytes.
    pub(crate) fn prepare(&mut self, ty: Type, budget: usize) {
        self.ty = ty;
        self.width = 0;
        self.bytes.clear();
        self.lens.clear();
        self.ready = false;
        self.budget = budget;
    }

    /// The type the stream is regenerated as.
    pub(crate) const fn ty(&self) -> Type {
        self.ty
    }

    /// Creates the stream as `count` elements of `width` bytes and returns
    /// its empty content, with room for all of them, for the codec to fill.
    ///
    /// # Errors
    ///
    /// A corrupted stream for width 0, a string stream (which needs
    /// [`Self::strings`]) or a size beyond the budget.
    #[cfg_attr(
        not(any(
            test,
            feature = "bitpack",
            feature = "bitsplit",
            feature = "bitunpack",
            feature = "concat",
            feature = "constant",
            feature = "conversion",
            feature = "dedup",
            feature = "delta",
            feature = "dispatch",
            feature = "divide-by",
            feature = "entropy",
            feature = "field-lz",
            feature = "flatpack",
            feature = "float-deconstruct",
            feature = "interleave",
            feature = "lz",
            feature = "lz4",
            feature = "merge-sorted",
            feature = "mux-lengths",
            feature = "parse-int",
            feature = "partition",
            feature = "pivco-huffman",
            feature = "prefix",
            feature = "quantize",
            feature = "range-pack",
            feature = "sentinel",
            feature = "sparse-num",
            feature = "split",
            feature = "split-by-struct",
            feature = "tokenize",
            feature = "transpose",
            feature = "zigzag",
            feature = "zstd"
        )),
        expect(
            dead_code,
            reason = "every codec creates its outputs here; a build without codecs has none"
        )
    )]
    pub(crate) fn fixed(
        &mut self,
        width: usize,
        count: usize,
    ) -> Result<&mut Vec<u8>, DecodeError> {
        let bytes = width.checked_mul(count).ok_or_else(corrupt)?;
        // `STREAM_validateTypeWidth`.
        let valid = match self.ty {
            Type::Serial => width == 1,
            Type::Numeric => matches!(width, 1 | 2 | 4 | 8),
            Type::Struct => width != 0,
            Type::String => false,
        };
        if !valid || bytes > self.budget {
            return Err(corrupt());
        }
        self.width = width;
        self.bytes.clear();
        self.bytes.try_reserve(bytes).or(Err(corrupt()))?;
        self.ready = true;
        Ok(&mut self.bytes)
    }

    /// Creates a string stream of `count` strings totalling `total` bytes
    /// and returns its empty content and lengths for the codec to fill.
    ///
    /// # Errors
    ///
    /// A corrupted stream for a non-string type or a size beyond the budget.
    #[cfg_attr(
        not(any(
            test,
            feature = "concat",
            feature = "conversion",
            feature = "dedup",
            feature = "dispatch",
            feature = "interleave",
            feature = "parse-int",
            feature = "prefix",
            feature = "tokenize"
        )),
        expect(
            dead_code,
            reason = "only these codecs regenerate strings (dedup through `copy_of`)"
        )
    )]
    pub(crate) fn strings(
        &mut self,
        count: usize,
        total: usize,
    ) -> Result<(&mut Vec<u8>, &mut Vec<u32>), DecodeError> {
        let need = count
            .checked_mul(4)
            .and_then(|lens| lens.checked_add(total))
            .ok_or_else(corrupt)?;
        if self.ty != Type::String || need > self.budget {
            return Err(corrupt());
        }
        self.width = 1;
        self.bytes.clear();
        self.lens.clear();
        self.bytes.try_reserve(total).or(Err(corrupt()))?;
        self.lens.try_reserve(count).or(Err(corrupt()))?;
        self.ready = true;
        Ok((&mut self.bytes, &mut self.lens))
    }

    /// Creates the stream as a copy of `input`, with this stream's type.
    ///
    /// # Errors
    ///
    /// As [`Self::fixed`] and [`Self::strings`].
    #[cfg_attr(
        not(any(test, feature = "conversion", feature = "dedup")),
        expect(dead_code, reason = "only these codecs pass an input through")
    )]
    pub(crate) fn copy_of(&mut self, input: &Input<'_>) -> Result<(), DecodeError> {
        if self.ty == Type::String {
            let (bytes, lens) = self.strings(input.lens.len(), input.bytes.len())?;
            bytes.extend_from_slice(input.bytes);
            lens.extend_from_slice(input.lens);
        } else {
            self.fixed(input.width, input.len())?
                .extend_from_slice(input.bytes);
        }
        Ok(())
    }

    /// Checks what the codec produced: a width dividing the content within
    /// the budget, and string lengths summing to the content.
    pub(crate) fn validate(&self) -> Result<(), DecodeError> {
        if !self.ready {
            return Err(DecodeError::invalid(InvalidDataKind::Graph));
        }
        let lens = self.lens.len().saturating_mul(4);
        if self.width == 0
            || !self.bytes.len().is_multiple_of(self.width)
            || self.bytes.len().saturating_add(lens) > self.budget
        {
            return Err(corrupt());
        }
        if self.ty == Type::String {
            let sum = self.lens.iter().map(|&len| len as u64).sum::<u64>();
            if sum != self.bytes.len() as u64 {
                return Err(corrupt());
            }
        } else if !self.lens.is_empty() {
            return Err(corrupt());
        }
        Ok(())
    }

    /// The stream as a codec input.
    pub(crate) fn as_input(&self) -> Input<'_> {
        Input {
            ty: self.ty,
            width: self.width.max(1),
            bytes: &self.bytes,
            lens: &self.lens,
        }
    }

    /// The content.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Bytes the stream keeps allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.bytes.capacity() + self.lens.capacity() * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_streams_respect_width_type_and_budget() {
        let mut stream = Stream::default();
        stream.prepare(Type::Serial, 16);
        assert_eq!(stream.fixed(2, 1).map(|_| ()), Err(corrupt()));
        stream.prepare(Type::Numeric, 16);
        assert_eq!(
            stream.validate(),
            Err(DecodeError::invalid(InvalidDataKind::Graph))
        );
        assert_eq!(stream.fixed(0, 1).map(|_| ()), Err(corrupt()));
        assert_eq!(stream.fixed(3, 1).map(|_| ()), Err(corrupt()));
        assert_eq!(stream.fixed(4, 5).map(|_| ()), Err(corrupt()));
        assert_eq!(stream.fixed(usize::MAX, 2).map(|_| ()), Err(corrupt()));
        stream
            .fixed(4, 2)
            .unwrap()
            .extend_from_slice(&[1, 0, 0, 0, 2, 0, 0, 0]);
        assert_eq!(stream.validate(), Ok(()));
        assert_eq!(stream.ty(), Type::Numeric);
        let input = stream.as_input();
        assert_eq!((input.len(), input.width), (2, 4));
        assert_eq!(stream.bytes().len(), 8);
        assert!(stream.retained_bytes() >= 8);
        // A partial element or a write past the budget.
        stream.fixed(4, 1).unwrap().extend_from_slice(&[1, 2, 3]);
        assert_eq!(stream.validate(), Err(corrupt()));
        stream.fixed(1, 1).unwrap().extend_from_slice(&[0; 17]);
        assert_eq!(stream.validate(), Err(corrupt()));
        assert_eq!(stream.strings(0, 0).map(|_| ()), Err(corrupt()));
    }

    #[test]
    fn string_streams_check_their_lengths() {
        let mut stream = Stream::default();
        stream.prepare(Type::String, 16);
        assert_eq!(stream.fixed(1, 1).map(|_| ()), Err(corrupt()));
        assert_eq!(stream.strings(3, 5).map(|_| ()), Err(corrupt()));
        assert_eq!(stream.strings(usize::MAX, 0).map(|_| ()), Err(corrupt()));
        let (bytes, lens) = stream.strings(2, 3).unwrap();
        bytes.extend_from_slice(b"abc");
        lens.extend_from_slice(&[1, 2]);
        assert_eq!(stream.validate(), Ok(()));
        assert_eq!(stream.as_input().len(), 2);
        let (_, lens) = stream.strings(1, 0).unwrap();
        lens.push(1);
        assert_eq!(stream.validate(), Err(corrupt()));
    }

    #[test]
    fn copies_keep_the_target_type() {
        let source = [7u8, 8, 9, 10];
        let mut stream = Stream::default();
        stream.prepare(Type::Struct, 64);
        let mut input = Input::serial(&source);
        input.width = 2;
        stream.copy_of(&input).unwrap();
        assert_eq!(stream.validate(), Ok(()));
        assert_eq!(
            (stream.as_input().ty, stream.as_input().len()),
            (Type::Struct, 2)
        );
        let lens = [1u32, 3];
        let strings = Input {
            ty: Type::String,
            width: 1,
            bytes: &source,
            lens: &lens,
        };
        stream.prepare(Type::String, 64);
        stream.copy_of(&strings).unwrap();
        assert_eq!(stream.as_input().lens, lens);
        // Lengths on a fixed-width stream are corrupt.
        stream.prepare(Type::Serial, 64);
        stream.fixed(1, 0).unwrap();
        stream.lens.push(0);
        assert_eq!(stream.validate(), Err(corrupt()));
        assert_eq!(Input::serial(&[]).len(), 0);
    }
}
