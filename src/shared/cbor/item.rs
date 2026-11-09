//! The CBOR value tree and the error type of the codec (`A1C_Item`,
//! `A1C_Error`).

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use thiserror::Error;

/// `A1C_ErrorType` (without `ok`).
#[derive(Error, Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub(crate) enum ErrorKind {
    /// `badAlloc`: the allocation limit of the decoder was reached.
    #[error("badAlloc")]
    BadAlloc,
    /// `truncated`: the source ended inside an item.
    #[error("truncated")]
    Truncated,
    /// `invalidItemHeader`: an additional-information value of 28 to 30, or
    /// an indefinite length on an integer or tag.
    #[error("invalidItemHeader")]
    InvalidItemHeader,
    /// `largeIntegersUnsupported`: an integer that does not fit an `i64`.
    #[error("largeIntegersUnsupported")]
    LargeIntegersUnsupported,
    /// `integerOverflow`: a length that does not fit `usize`.
    #[error("integerOverflow")]
    IntegerOverflow,
    /// `invalidChunkedString`: a chunk of another major type, or a nested
    /// indefinite chunk.
    #[error("invalidChunkedString")]
    InvalidChunkedString,
    /// `maxDepthExceeded`: more nested items than the configured depth.
    #[error("maxDepthExceeded")]
    MaxDepthExceeded,
    /// `invalidSimpleEncoding`: a simple value the decoder rejects.
    #[error("invalidSimpleEncoding")]
    InvalidSimpleEncoding,
    /// `breakNotAllowed`: a stray break code.
    #[error("breakNotAllowed")]
    BreakNotAllowed,
    /// `writeFailed`: the destination is full.
    #[error("writeFailed")]
    WriteFailed,
    /// `invalidSimpleValue`: a simple value in the reserved range 20 to 31.
    #[error("invalidSimpleValue")]
    InvalidSimpleValue,
    /// `trailingData`: bytes after the item.
    #[error("trailingData")]
    TrailingData,
}

/// A failed decode or encode: the kind, the position in the byte stream
/// where it was detected and the nesting depth at that moment
/// (`A1C_Error` without the debugging item, file and line).
#[derive(Error, Debug, Copy, Clone, Eq, PartialEq, Hash)]
#[error("CBOR {kind} at byte {position}, depth {depth}")]
pub(crate) struct Error {
    /// What went wrong.
    pub(crate) kind: ErrorKind,
    /// Decoding: the offset of the source cursor. Encoding: the number of
    /// bytes written so far.
    pub(crate) position: usize,
    /// The nesting depth of the item being processed.
    pub(crate) depth: usize,
}

/// One CBOR value (`A1C_Item`).
///
/// Maps keep their pairs in order and may repeat a key; lookups return the
/// first match, as `A1C_Map_get` does.
#[derive(Clone, Debug)]
pub(crate) enum Item<'a> {
    /// CBOR `undefined`; also the default item.
    Undefined,
    /// A major type 0 or 1 integer that fits `i64`.
    Int(i64),
    /// A byte string.
    Bytes(Cow<'a, [u8]>),
    /// A text string; the bytes are not checked to be UTF-8.
    Text(Cow<'a, [u8]>),
    /// An array.
    Array(Vec<Item<'a>>),
    /// A map, as ordered key and value pairs.
    Map(Vec<(Item<'a>, Item<'a>)>),
    /// `true` or `false`.
    Bool(bool),
    /// `null`.
    Null,
    /// An IEEE 754 half float, as its 16 bits.
    Float16(u16),
    /// A single float.
    Float32(f32),
    /// A double float.
    Float64(f64),
    /// A simple value outside the named ones (0 to 19, or 32 and up).
    Simple(u8),
    /// A tag and the item it wraps.
    Tag(u64, Box<Item<'a>>),
}

impl Default for Item<'_> {
    /// `A1C_ItemType_undefined`, what `A1C_Item_root` returns.
    fn default() -> Self {
        Self::Undefined
    }
}

impl PartialEq for Item<'_> {
    /// `A1C_Item_eq`: same type and same content; floats compare by bit
    /// pattern (so `NaN` equals itself) and maps by position.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Undefined, Self::Undefined) | (Self::Null, Self::Null) => true,
            (Self::Int(a), Self::Int(b)) => a == b,
            (Self::Bytes(a), Self::Bytes(b)) | (Self::Text(a), Self::Text(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => a == b,
            (Self::Map(a), Self::Map(b)) => a == b,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Float16(a), Self::Float16(b)) => a == b,
            (Self::Float32(a), Self::Float32(b)) => a.to_bits() == b.to_bits(),
            (Self::Float64(a), Self::Float64(b)) => a.to_bits() == b.to_bits(),
            (Self::Simple(a), Self::Simple(b)) => a == b,
            (Self::Tag(ta, a), Self::Tag(tb, b)) => ta == tb && a == b,
            _ => false,
        }
    }
}

impl Eq for Item<'_> {}

impl<'a> Item<'a> {
    /// A text string borrowing `text` (`A1C_Item_string_refCStr`).
    pub(crate) fn text(text: &'a str) -> Self {
        Self::Text(Cow::Borrowed(text.as_bytes()))
    }

    /// A byte string borrowing `bytes` (`A1C_Item_bytes_ref`).
    #[cfg(test)]
    pub(crate) fn bytes(bytes: &'a [u8]) -> Self {
        Self::Bytes(Cow::Borrowed(bytes))
    }

    /// A map of `pairs`, in order (`A1C_Item_map`).
    pub(crate) fn map(pairs: impl IntoIterator<Item = (Item<'a>, Item<'a>)>) -> Self {
        Self::Map(pairs.into_iter().collect())
    }

    /// An array of `items` (`A1C_Item_array`).
    pub(crate) fn array(items: impl IntoIterator<Item = Item<'a>>) -> Self {
        Self::Array(items.into_iter().collect())
    }

    /// The same item with every string owning its bytes, detached from the
    /// source it was decoded from (`A1C_Item_deepcopy`).
    #[cfg(test)]
    pub(crate) fn into_owned(self) -> Item<'static> {
        match self {
            Self::Undefined => Item::Undefined,
            Self::Int(v) => Item::Int(v),
            Self::Bytes(b) => Item::Bytes(Cow::Owned(b.into_owned())),
            Self::Text(t) => Item::Text(Cow::Owned(t.into_owned())),
            Self::Array(items) => Item::Array(items.into_iter().map(Item::into_owned).collect()),
            Self::Map(pairs) => Item::Map(
                pairs
                    .into_iter()
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect(),
            ),
            Self::Bool(v) => Item::Bool(v),
            Self::Null => Item::Null,
            Self::Float16(v) => Item::Float16(v),
            Self::Float32(v) => Item::Float32(v),
            Self::Float64(v) => Item::Float64(v),
            Self::Simple(v) => Item::Simple(v),
            Self::Tag(tag, inner) => Item::Tag(tag, Box::new(inner.into_owned())),
        }
    }

    /// `A1C_Map_get`: the value of the first pair whose key equals `key`.
    /// `None` for a key that is absent or when this item is not a map.
    pub(crate) fn map_get(&self, key: &Item<'_>) -> Option<&Item<'a>> {
        let Self::Map(pairs) = self else {
            return None;
        };
        pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// `A1C_Map_get_cstr`: the value stored under the text key `key`.
    pub(crate) fn map_get_str(&self, key: &str) -> Option<&Item<'a>> {
        self.map_get(&Item::text(key))
    }

    /// `A1C_Map_get_int`: the value stored under the integer key `key`.
    #[cfg(test)]
    pub(crate) fn map_get_int(&self, key: i64) -> Option<&Item<'a>> {
        self.map_get(&Item::Int(key))
    }

    /// `A1C_Array_get`: element `index`, `None` out of bounds or when this
    /// item is not an array.
    #[cfg(test)]
    pub(crate) fn array_get(&self, index: usize) -> Option<&Item<'a>> {
        match self {
            Self::Array(items) => items.get(index),
            _ => None,
        }
    }

    /// `A1C_Item_tryGetInt64`.
    pub(crate) const fn as_int64(&self) -> Option<i64> {
        match *self {
            Self::Int(v) => Some(v),
            _ => None,
        }
    }

    /// `A1C_Item_tryGetBool`.
    #[cfg(test)]
    pub(crate) const fn as_bool(&self) -> Option<bool> {
        match *self {
            Self::Bool(v) => Some(v),
            _ => None,
        }
    }

    /// `A1C_Item_tryGetFloat16`: the raw half-float bits.
    #[cfg(test)]
    pub(crate) const fn as_float16(&self) -> Option<u16> {
        match *self {
            Self::Float16(v) => Some(v),
            _ => None,
        }
    }

    /// `A1C_Item_tryGetFloat32`; a double is not converted.
    #[cfg(test)]
    pub(crate) const fn as_float32(&self) -> Option<f32> {
        match *self {
            Self::Float32(v) => Some(v),
            _ => None,
        }
    }

    /// `A1C_Item_tryGetFloat64`; a single is not converted.
    pub(crate) const fn as_float64(&self) -> Option<f64> {
        match *self {
            Self::Float64(v) => Some(v),
            _ => None,
        }
    }

    /// `A1C_Item_tryGetSimple`.
    #[cfg(test)]
    pub(crate) const fn as_simple(&self) -> Option<u8> {
        match *self {
            Self::Simple(v) => Some(v),
            _ => None,
        }
    }

    /// `A1C_Item_tryGetBytes`.
    #[cfg(test)]
    pub(crate) fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// `A1C_Item_tryGetString`: the raw bytes of a text string.
    pub(crate) fn as_text_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Text(t) => Some(t),
            _ => None,
        }
    }

    /// A text string that is valid UTF-8.
    #[cfg(test)]
    pub(crate) fn as_text(&self) -> Option<&str> {
        core::str::from_utf8(self.as_text_bytes()?).ok()
    }

    /// `A1C_Item_tryGetArray`.
    pub(crate) fn as_array(&self) -> Option<&[Item<'a>]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// `A1C_Item_tryGetMap`.
    pub(crate) fn as_map(&self) -> Option<&[(Item<'a>, Item<'a>)]> {
        match self {
            Self::Map(pairs) => Some(pairs),
            _ => None,
        }
    }

    /// `A1C_Item_tryGetTag`: the tag number and the wrapped item.
    #[cfg(test)]
    pub(crate) fn as_tag(&self) -> Option<(u64, &Item<'a>)> {
        match self {
            Self::Tag(tag, inner) => Some((*tag, inner)),
            _ => None,
        }
    }
}

impl fmt::Display for Item<'_> {
    /// A compact diagnostic notation (not JSON, not RFC 8949 diagnostics).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Undefined => f.write_str("undefined"),
            Self::Int(v) => write!(f, "{v}"),
            Self::Bytes(b) => write!(f, "h'{}'", b.len()),
            Self::Text(t) => write!(f, "\"{}\"", alloc::string::String::from_utf8_lossy(t)),
            Self::Array(items) => {
                f.write_str("[")?;
                for (n, item) in items.iter().enumerate() {
                    if n != 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
            Self::Map(pairs) => {
                f.write_str("{")?;
                for (n, (k, v)) in pairs.iter().enumerate() {
                    if n != 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                f.write_str("}")
            }
            Self::Bool(v) => write!(f, "{v}"),
            Self::Null => f.write_str("null"),
            Self::Float16(v) => write!(f, "f16({v:#06x})"),
            Self::Float32(v) => write!(f, "{v}f"),
            Self::Float64(v) => write!(f, "{v}"),
            Self::Simple(v) => write!(f, "simple({v})"),
            Self::Tag(tag, inner) => write!(f, "{tag}({inner})"),
        }
    }
}
