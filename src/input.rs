//! Typed inputs of a frame.
//!
//! OpenZL compresses *typed* streams: raw bytes, fixed-width records,
//! little-endian integers and variable-length strings. A frame may carry
//! several inputs of different types. [`Input`] wraps borrowed data of one
//! type; the constructors validate the layout so that a frame is never
//! built from data that does not fit its type.
//!
//! # Examples
//!
//! ```
//! use mopenzl::input::{Input, Numbers, Strings};
//!
//! let numbers = Numbers::try_from((&[1u8, 0, 0, 0, 2, 0, 0, 0][..], 4))?;
//! let strings = Strings::try_from((&b"abcdef"[..], &[1u32, 2, 3][..]))?;
//! let inputs = [Input::from(numbers), Input::from(strings), Input::from(&b"raw"[..])];
//! assert_eq!(inputs[0].len(), 2);
//! assert_eq!(inputs[1].len(), 3);
//! assert!(Numbers::try_from((&[0u8; 3][..], 2)).is_err());
//! # Ok::<(), mopenzl::input::InputError>(())
//! ```

use thiserror::Error;

use crate::DataType;

/// Why data does not fit the input type it was offered as.
#[derive(Error, Debug, Clone, Copy, Eq, PartialEq)]
#[non_exhaustive]
pub enum InputError {
    /// A numeric width other than 1, 2, 4 or 8, or a record width of 0.
    #[error("invalid element width {0}")]
    InvalidWidth(usize),
    /// A byte length that is not a whole number of elements.
    #[error("{len} bytes are not a multiple of the {width}-byte element width")]
    NotMultiple {
        /// Bytes offered.
        len: usize,
        /// Element width.
        width: usize,
    },
    /// String lengths that do not sum to the content size.
    #[error("the string lengths sum to {sum} but the content is {len} bytes")]
    LengthMismatch {
        /// Sum of the lengths.
        sum: u64,
        /// Content size.
        len: usize,
    },
}

fn check_multiple(bytes: &[u8], width: usize) -> Result<(), InputError> {
    if !bytes.len().is_multiple_of(width) {
        return Err(InputError::NotMultiple {
            len: bytes.len(),
            width,
        });
    }
    Ok(())
}

/// Little-endian unsigned integers of one width.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Numbers<'a> {
    bytes: &'a [u8],
    width: usize,
}

impl<'a> TryFrom<(&'a [u8], usize)> for Numbers<'a> {
    type Error = InputError;

    /// Little-endian integers of `width` bytes each.
    ///
    /// # Errors
    ///
    /// A width other than 1, 2, 4 or 8, or a length that is not a multiple
    /// of it.
    fn try_from((bytes, width): (&'a [u8], usize)) -> Result<Self, Self::Error> {
        if !matches!(width, 1 | 2 | 4 | 8) {
            return Err(InputError::InvalidWidth(width));
        }
        check_multiple(bytes, width)?;
        Ok(Self { bytes, width })
    }
}

/// Fixed-width records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Records<'a> {
    bytes: &'a [u8],
    width: usize,
}

impl<'a> TryFrom<(&'a [u8], usize)> for Records<'a> {
    type Error = InputError;

    /// Records of `width` bytes each.
    ///
    /// # Errors
    ///
    /// A width of 0, or a length that is not a multiple of it.
    fn try_from((bytes, width): (&'a [u8], usize)) -> Result<Self, Self::Error> {
        if width == 0 {
            return Err(InputError::InvalidWidth(width));
        }
        check_multiple(bytes, width)?;
        Ok(Self { bytes, width })
    }
}

/// Variable-length strings: concatenated contents and one length each.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Strings<'a> {
    bytes: &'a [u8],
    lens: &'a [u32],
}

impl<'a> TryFrom<(&'a [u8], &'a [u32])> for Strings<'a> {
    type Error = InputError;

    /// Strings of the given lengths over `bytes`.
    ///
    /// # Errors
    ///
    /// Lengths that do not sum to the content size.
    fn try_from((bytes, lens): (&'a [u8], &'a [u32])) -> Result<Self, Self::Error> {
        let sum: u64 = lens.iter().map(|&len| u64::from(len)).sum();
        if sum != bytes.len() as u64 {
            return Err(InputError::LengthMismatch {
                sum,
                len: bytes.len(),
            });
        }
        Ok(Self { bytes, lens })
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Repr<'a> {
    Serial(&'a [u8]),
    Records(Records<'a>),
    Numbers(Numbers<'a>),
    Strings(Strings<'a>),
}

/// One typed input of a frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Input<'a> {
    repr: Repr<'a>,
    metadata: &'a [(i32, i32)],
}

impl<'a> Input<'a> {
    const fn new(repr: Repr<'a>) -> Self {
        Self {
            repr,
            metadata: &[],
        }
    }

    /// The input with integer metadata attached (`ZL_Data_setIntMetadata`):
    /// `(id, value)` pairs graphs can read, such as the tag the clustering
    /// graph groups by. Metadata is never written to the frame. A pair
    /// repeating an earlier ID replaces its value.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::input::Input;
    ///
    /// let tags = [(0, 7)];
    /// let input = Input::from(&b"abc"[..]).with_metadata(&tags);
    /// assert_eq!(input.len(), 3);
    /// ```
    #[must_use]
    pub const fn with_metadata(mut self, metadata: &'a [(i32, i32)]) -> Self {
        self.metadata = metadata;
        self
    }
}

impl<'a> From<&'a [u8]> for Input<'a> {
    /// Raw bytes: a serial input.
    fn from(bytes: &'a [u8]) -> Self {
        Self::new(Repr::Serial(bytes))
    }
}

impl<'a> From<Numbers<'a>> for Input<'a> {
    fn from(numbers: Numbers<'a>) -> Self {
        Self::new(Repr::Numbers(numbers))
    }
}

impl<'a> From<Records<'a>> for Input<'a> {
    fn from(records: Records<'a>) -> Self {
        Self::new(Repr::Records(records))
    }
}

impl<'a> From<Strings<'a>> for Input<'a> {
    fn from(strings: Strings<'a>) -> Self {
        Self::new(Repr::Strings(strings))
    }
}

impl<'a> Input<'a> {
    /// The input's type.
    #[must_use]
    pub const fn data_type(&self) -> DataType {
        match self.repr {
            Repr::Serial(_) => DataType::Serial,
            Repr::Records(_) => DataType::Struct,
            Repr::Numbers(_) => DataType::Numeric,
            Repr::Strings(_) => DataType::String,
        }
    }

    /// Elements: bytes, records, integers or strings.
    #[must_use]
    pub const fn len(&self) -> usize {
        match self.repr {
            Repr::Serial(bytes) => bytes.len(),
            Repr::Records(Records { bytes, width }) | Repr::Numbers(Numbers { bytes, width }) => {
                bytes.len() / width
            }
            Repr::Strings(strings) => strings.lens.len(),
        }
    }

    /// Whether the input has no elements.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes the input occupies: its content plus four per string length.
    #[cfg(feature = "compression")]
    pub(crate) const fn byte_len(&self) -> usize {
        match self.repr {
            Repr::Serial(bytes)
            | Repr::Records(Records { bytes, .. })
            | Repr::Numbers(Numbers { bytes, .. }) => bytes.len(),
            Repr::Strings(strings) => strings.bytes.len().saturating_add(strings.lens.len() * 4),
        }
    }

    #[cfg(feature = "compression")]
    pub(crate) fn to_stream(self) -> crate::compressor::core::data::Stream<'a> {
        use crate::compressor::core::data::{Bytes, Stream};
        let mut stream = match self.repr {
            Repr::Serial(bytes) => Stream::serial(bytes),
            Repr::Records(Records { bytes, width }) => {
                Stream::new(DataType::Struct, width, Bytes::Borrowed(bytes))
            }
            Repr::Numbers(Numbers { bytes, width }) => {
                Stream::new(DataType::Numeric, width, Bytes::Borrowed(bytes))
            }
            Repr::Strings(Strings { bytes, lens }) => Stream::strings(bytes, lens),
        };
        for &(id, value) in self.metadata {
            stream.set_metadata(id, value);
        }
        stream
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_validate_widths_and_lengths() {
        assert_eq!(
            Numbers::try_from((&[0u8; 6][..], 3)),
            Err(InputError::InvalidWidth(3))
        );
        assert_eq!(
            Numbers::try_from((&[0u8; 6][..], 4)),
            Err(InputError::NotMultiple { len: 6, width: 4 })
        );
        assert_eq!(
            Records::try_from((&[0u8; 6][..], 0)),
            Err(InputError::InvalidWidth(0))
        );
        assert!(Records::try_from((&[0u8; 6][..], 3)).is_ok());
        assert_eq!(
            Strings::try_from((&b"abc"[..], &[1u32, 1][..])),
            Err(InputError::LengthMismatch { sum: 2, len: 3 })
        );
        assert!(Strings::try_from((&b""[..], &[][..])).is_ok());
    }

    #[test]
    fn inputs_report_type_and_length() {
        let records = Records::try_from((&[0u8; 12][..], 3)).unwrap();
        let strings = Strings::try_from((&b"abc"[..], &[3u32][..])).unwrap();
        let inputs = [
            Input::from(&b"xy"[..]),
            Input::from(records),
            Input::from(Numbers::try_from((&[0u8; 8][..], 2)).unwrap()),
            Input::from(strings),
        ];
        let types: alloc::vec::Vec<_> = inputs.iter().map(Input::data_type).collect();
        assert_eq!(
            types,
            [
                DataType::Serial,
                DataType::Struct,
                DataType::Numeric,
                DataType::String
            ]
        );
        let lens: alloc::vec::Vec<_> = inputs.iter().map(Input::len).collect();
        assert_eq!(lens, [2, 4, 4, 1]);
        assert!(Input::from(&b""[..]).is_empty());
        assert!(!inputs[0].is_empty());
        assert!(alloc::format!("{}", InputError::InvalidWidth(3)).contains('3'));
        let streams: alloc::vec::Vec<_> = inputs.iter().map(|i| i.to_stream()).collect();
        assert_eq!(streams[1].width(), 3);
        assert_eq!(streams[3].lens(), [3]);
    }

    #[cfg(feature = "compression")]
    #[test]
    fn byte_lengths_count_content_and_string_lengths() {
        assert_eq!(Input::from(&b"abc"[..]).byte_len(), 3);
        let numbers = Numbers::try_from((&[0u8; 8][..], 4)).unwrap();
        assert_eq!(Input::from(numbers).byte_len(), 8);
        let records = Records::try_from((&[0u8; 6][..], 3)).unwrap();
        assert_eq!(Input::from(records).byte_len(), 6);
        let strings = Strings::try_from((&b"abcdef"[..], &[1u32, 2, 3][..])).unwrap();
        assert_eq!(Input::from(strings).byte_len(), 6 + 12);
    }
}
