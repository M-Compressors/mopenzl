//! Typed outputs of [`Decompressor::decompress_typed`](super::Decompressor::decompress_typed).

use alloc::vec::Vec;

use crate::DataType;

/// One decoded output of a frame, with its type.
///
/// # Examples
///
/// ```
/// use mopenzl::{Compressor, DataType, Decompressor};
///
/// let frame = Compressor::default().compress(b"bytes")?;
/// let outputs = Decompressor::default().decompress_typed(&frame)?;
/// assert_eq!(outputs.len(), 1);
/// assert_eq!(outputs[0].data_type(), DataType::Serial);
/// assert_eq!(outputs[0].content(), b"bytes");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TypedOutput {
    data_type: DataType,
    width: usize,
    content: Vec<u8>,
    string_lens: Vec<u32>,
}

impl TypedOutput {
    /// An empty output of type `data_type`.
    pub(crate) fn new(data_type: DataType) -> Self {
        Self {
            data_type,
            width: usize::from(matches!(data_type, DataType::Serial | DataType::String)),
            content: Vec::new(),
            string_lens: Vec::new(),
        }
    }

    /// Appends one chunk's content: `width`-byte elements, plus string
    /// lengths for strings. Fails when the width changes between chunks.
    pub(crate) fn append(&mut self, width: usize, bytes: &[u8], lens: &[u32]) -> Result<(), ()> {
        if self.content.is_empty() && self.string_lens.is_empty() && self.width == 0 {
            self.width = width;
        }
        if width != self.width && !bytes.is_empty() {
            return Err(());
        }
        self.content.extend_from_slice(bytes);
        self.string_lens.extend_from_slice(lens);
        Ok(())
    }

    /// The output's type.
    #[must_use]
    pub const fn data_type(&self) -> DataType {
        self.data_type
    }

    /// Bytes per element: the record width of structs, the integer width
    /// of numbers, 1 for serial data and strings, 0 for an empty struct or
    /// numeric output whose width the frame never recorded.
    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// The content: bytes, concatenated records, little-endian integers or
    /// concatenated strings.
    #[must_use]
    pub fn content(&self) -> &[u8] {
        &self.content
    }

    /// The length of each string; empty for other types.
    #[must_use]
    pub fn string_lens(&self) -> &[u32] {
        &self.string_lens
    }

    /// Number of elements: strings for strings, otherwise
    /// `content().len() / width()`.
    #[must_use]
    pub fn len(&self) -> usize {
        match self.data_type {
            DataType::String => self.string_lens.len(),
            _ if self.width == 0 => 0,
            _ => self.content.len() / self.width,
        }
    }

    /// Whether the output has no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The content, consuming the output.
    #[must_use]
    pub fn into_content(self) -> Vec<u8> {
        self.content
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_are_fixed_by_the_first_chunk() {
        let mut numbers = TypedOutput::new(DataType::Numeric);
        assert_eq!(
            (numbers.width(), numbers.len(), numbers.is_empty()),
            (0, 0, true)
        );
        numbers.append(4, &[1, 0, 0, 0], &[]).unwrap();
        assert_eq!(numbers.append(2, &[1, 0], &[]), Err(()));
        numbers.append(2, &[], &[]).unwrap();
        assert_eq!((numbers.width(), numbers.len()), (4, 1));
        let mut strings = TypedOutput::new(DataType::String);
        strings.append(1, b"abc", &[1, 2]).unwrap();
        assert_eq!((strings.len(), strings.string_lens()), (2, &[1u32, 2][..]));
        assert_eq!(strings.clone().into_content(), b"abc");
        assert_eq!(strings.content(), b"abc");
    }
}
