//! Typed streams of the compression graph (`ZL_Data`).
//!
//! A [`Stream`] has a type, an element width, content and, for strings,
//! the length of each string. Content is either borrowed from an input of
//! the frame (the zero-copy references the reference engine keeps with
//! `ZL_Data` slices) or owned by an `Arc`, so that a node output which
//! references a slice of another owned stream shares the buffer instead of
//! copying it. Numeric content is always little-endian bytes.

use alloc::sync::Arc;
use alloc::vec::Vec;

pub(crate) use crate::DataType as Type;

/// `ZL_Type` bit of `ty`: serial 1, struct 2, numeric 4, string 8.
pub(crate) const fn type_mask(ty: Type) -> u8 {
    match ty {
        Type::Serial => 1,
        Type::Struct => 2,
        Type::Numeric => 4,
        Type::String => 8,
    }
}

/// The 2-bit type code of the frame header.
pub(crate) const fn type_code(ty: Type) -> u8 {
    match ty {
        Type::Serial => 0,
        Type::Struct => 1,
        Type::Numeric => 2,
        Type::String => 3,
    }
}

/// The content bytes of a stream.
#[derive(Clone, Debug)]
pub(crate) enum Bytes<'a> {
    /// Borrowed from an input of the frame.
    Borrowed(&'a [u8]),
    /// A window of a shared buffer.
    Shared {
        /// The buffer.
        buf: Arc<Vec<u8>>,
        /// First byte of the window.
        start: usize,
        /// Length of the window.
        len: usize,
    },
}

impl<'a> Bytes<'a> {
    /// The bytes.
    pub(crate) fn as_slice(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Shared { buf, start, len } => &buf[*start..*start + *len],
        }
    }

    /// The window `offset..offset + len`, without copying; `None` when it
    /// falls outside.
    pub(crate) fn window(&self, offset: usize, len: usize) -> Option<Self> {
        let end = offset.checked_add(len)?;
        if end > self.as_slice().len() {
            return None;
        }
        Some(match self {
            Self::Borrowed(bytes) => Self::Borrowed(&bytes[offset..end]),
            Self::Shared { buf, start, .. } => Self::Shared {
                buf: Arc::clone(buf),
                start: start + offset,
                len,
            },
        })
    }
}

impl From<Vec<u8>> for Bytes<'_> {
    fn from(buf: Vec<u8>) -> Self {
        let len = buf.len();
        Self::Shared {
            buf: Arc::new(buf),
            start: 0,
            len,
        }
    }
}

/// The string lengths of a string stream.
#[derive(Clone, Debug)]
pub(crate) enum Lens<'a> {
    /// No lengths: not a string stream.
    None,
    /// Borrowed from a string input.
    Borrowed(&'a [u32]),
    /// Owned by a node output.
    Shared(Arc<Vec<u32>>),
}

impl Lens<'_> {
    /// The lengths.
    pub(crate) fn as_slice(&self) -> &[u32] {
        match self {
            Self::None => &[],
            Self::Borrowed(lens) => lens,
            Self::Shared(lens) => lens,
        }
    }
}

/// A committed stream.
#[derive(Clone, Debug)]
pub(crate) struct Stream<'a> {
    ty: Type,
    width: usize,
    bytes: Bytes<'a>,
    lens: Lens<'a>,
    metadata: Vec<(i32, i32)>,
}

impl<'a> Stream<'a> {
    /// A stream of `ty` over `bytes`; `width` is 1 for serial and strings.
    pub(crate) const fn new(ty: Type, width: usize, bytes: Bytes<'a>) -> Self {
        Self {
            ty,
            width,
            bytes,
            lens: Lens::None,
            metadata: Vec::new(),
        }
    }

    /// A serial input.
    pub(crate) const fn serial(bytes: &'a [u8]) -> Self {
        Self::new(Type::Serial, 1, Bytes::Borrowed(bytes))
    }

    /// A string input: concatenated contents and one length each.
    pub(crate) const fn strings(bytes: &'a [u8], lens: &'a [u32]) -> Self {
        Self {
            ty: Type::String,
            width: 1,
            bytes: Bytes::Borrowed(bytes),
            lens: Lens::Borrowed(lens),
            metadata: Vec::new(),
        }
    }

    /// A string stream over existing content.
    pub(crate) fn strings_from(bytes: Bytes<'a>, lens: Vec<u32>) -> Self {
        Self {
            ty: Type::String,
            width: 1,
            bytes,
            lens: Lens::Shared(Arc::new(lens)),
            metadata: Vec::new(),
        }
    }

    /// The stream type.
    pub(crate) const fn ty(&self) -> Type {
        self.ty
    }

    /// Bytes per element (1 for serial and strings).
    pub(crate) const fn width(&self) -> usize {
        self.width
    }

    /// The content.
    pub(crate) fn bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    /// The content with its sharing, for referencing slices of it.
    pub(crate) const fn content(&self) -> &Bytes<'a> {
        &self.bytes
    }

    /// The string lengths; empty for other types.
    pub(crate) fn lens(&self) -> &[u32] {
        self.lens.as_slice()
    }

    /// `ZL_Data_contentSize`: bytes of content, which for strings is the
    /// sum of the lengths.
    pub(crate) fn content_size(&self) -> usize {
        self.bytes.as_slice().len()
    }

    /// `ZL_Data_numElts`.
    pub(crate) fn n_elts(&self) -> usize {
        match self.ty {
            Type::String => self.lens.as_slice().len(),
            _ => self.content_size() / self.width.max(1),
        }
    }

    /// Bytes the stream weighs in a size comparison: the content, plus four
    /// per string.
    pub(crate) fn weight(&self) -> usize {
        self.content_size()
            + if self.ty == Type::String {
                4 * self.n_elts()
            } else {
                0
            }
    }

    /// `ZL_Data_setIntMetadata`: a value read by downstream graphs, never
    /// written to the frame.
    pub(crate) fn set_metadata(&mut self, id: i32, value: i32) {
        match self.metadata.iter_mut().find(|(key, _)| *key == id) {
            Some(slot) => slot.1 = value,
            None => self.metadata.push((id, value)),
        }
    }

    /// The integer metadata set so far, as `(id, value)` pairs.
    pub(crate) fn metadata(&self) -> &[(i32, i32)] {
        &self.metadata
    }

    /// The elements `first..first + count` as a slice of this stream: the
    /// slicing a segmenter does. Strings take the matching lengths and
    /// content.
    pub(crate) fn slice(&self, first: usize, count: usize) -> Option<Stream<'a>> {
        let end = first.checked_add(count)?;
        if end > self.n_elts() {
            return None;
        }
        if self.ty == Type::String {
            let lens = self.lens.as_slice();
            let skip: usize = lens[..first].iter().map(|&n| n as usize).sum();
            let take: usize = lens[first..end].iter().map(|&n| n as usize).sum();
            let bytes = self.bytes.window(skip, take)?;
            let lens = match &self.lens {
                Lens::Borrowed(all) => Lens::Borrowed(&all[first..end]),
                _ => Lens::Shared(Arc::new(lens[first..end].to_vec())),
            };
            return Some(Self {
                ty: self.ty,
                width: 1,
                bytes,
                lens,
                metadata: Vec::new(),
            });
        }
        Some(Self::new(
            self.ty,
            self.width,
            self.bytes.window(first * self.width, count * self.width)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_and_codes_follow_the_reference() {
        let types = [Type::Serial, Type::Struct, Type::Numeric, Type::String];
        assert_eq!(types.map(type_mask), [1, 2, 4, 8]);
        assert_eq!(types.map(type_code), [0, 1, 2, 3]);
    }

    #[test]
    fn windows_share_or_borrow_and_stay_in_bounds() {
        let owned = Bytes::from(alloc::vec![1u8, 2, 3, 4]);
        let window = owned.window(1, 2).unwrap();
        assert_eq!(window.as_slice(), [2, 3]);
        assert_eq!(window.window(1, 1).unwrap().as_slice(), [3]);
        assert!(owned.window(3, 2).is_none());
        assert!(owned.window(usize::MAX, 2).is_none());
        let data = [9u8, 8, 7];
        let borrowed = Bytes::Borrowed(&data);
        assert_eq!(borrowed.window(2, 1).unwrap().as_slice(), [7]);
        assert!(borrowed.window(2, 2).is_none());
    }

    #[test]
    fn streams_report_elements_weight_and_metadata() {
        let mut numbers = Stream::new(Type::Numeric, 4, Bytes::from(alloc::vec![0u8; 12]));
        assert_eq!(
            (numbers.n_elts(), numbers.content_size(), numbers.weight()),
            (3, 12, 12)
        );
        assert!(numbers.metadata().is_empty());
        numbers.set_metadata(7, 1);
        numbers.set_metadata(7, 2);
        numbers.set_metadata(8, 3);
        assert_eq!(numbers.metadata(), [(7, 2), (8, 3)]);
        assert_eq!(numbers.content().as_slice().len(), 12);
        assert_eq!((numbers.ty(), numbers.width()), (Type::Numeric, 4));
        let strings = Stream::strings(b"abcdef", &[1, 2, 3]);
        assert_eq!((strings.n_elts(), strings.weight()), (3, 6 + 12));
        assert_eq!(strings.lens(), [1, 2, 3]);
        assert!(numbers.lens().is_empty());
    }

    #[test]
    fn slicing_cuts_elements_and_strings() {
        let numbers = Stream::new(
            Type::Numeric,
            2,
            Bytes::from(alloc::vec![1u8, 0, 2, 0, 3, 0]),
        );
        let tail = numbers.slice(1, 2).unwrap();
        assert_eq!((tail.bytes(), tail.n_elts()), (&[2u8, 0, 3, 0][..], 2));
        assert!(numbers.slice(2, 2).is_none());
        let strings = Stream::strings(b"abcdef", &[1, 2, 3]);
        let part = strings.slice(1, 2).unwrap();
        assert_eq!((part.bytes(), part.lens()), (&b"bcdef"[..], &[2u32, 3][..]));
        let owned = Stream::strings_from(Bytes::from(b"abcdef".to_vec()), alloc::vec![1, 2, 3]);
        let part = owned.slice(0, 2).unwrap();
        assert_eq!((part.bytes(), part.lens()), (&b"abc"[..], &[1u32, 2][..]));
        assert!(owned.slice(1, 3).is_none());
    }
}
