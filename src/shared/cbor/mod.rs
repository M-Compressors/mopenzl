//! CBOR (RFC 8949) decoding and encoding: a port of the reference's A1CBOR
//! library (`shared/a1cbor.c`, `shared/a1cbor.h`) and its helper layer
//! (`common/a1cbor_helpers.c`).
//!
//! OpenZL serializes configurations (ML selectors, clustering, trained
//! compressors) as CBOR. This module reads and writes the same bytes with
//! the same limits and the same failures as the reference, so a
//! configuration the reference accepts decodes here, and one it rejects is
//! rejected here with the same [`ErrorKind`], position and depth.
//!
//! # Model
//!
//! * [`Item`] is a decoded CBOR value (`A1C_Item`). Byte and text strings
//!   borrow the source when [`DecoderConfig::reference_source`] is set
//!   (`Cow::Borrowed`), and own a copy otherwise. Text is **not** validated as
//!   UTF-8, as in the reference; use [`Item::as_text`] to compare it with
//!   a Rust string.
//! * [`decode`] reads exactly one item and rejects trailing bytes. Definite
//!   and indefinite lengths, chunked strings, tags, simple values and
//!   half/single/double floats are supported; integers outside `i64` are not
//!   ([`ErrorKind::LargeIntegersUnsupported`]).
//! * [`encode_to_vec`], [`encode::encode_to_slice`] and [`encode::encoded_size`] write
//!   definite-length, shortest-argument CBOR (the reference does not sort
//!   map keys or deduplicate them: a map is written in item order).
//! * `Item::map_get*` and `Item::array_get` are the lookups of
//!   `A1C_Map_get*`/`A1C_Array_get`; `Item::as_*` are the
//!   `A1C_Item_tryGet*` accessors that fail on a type mismatch.
//!
//! # Limits
//!
//! The decoder stops at [`DecoderConfig::max_depth`] nested items (128 by
//! default) and, when [`DecoderConfig::limit_bytes`] is non zero, fails with
//! [`ErrorKind::BadAlloc`] once the reference's arena accounting
//! (`sizeof(A1C_Item)` = 32 and `sizeof(A1C_Pair)` = 64 bytes per item and
//! pair, plus copied string bytes) would exceed it. Nested containers recurse,
//! so a very large `max_depth` needs a large stack; the encoder has no depth
//! limit of its own and recurses along the item tree.
//!
//! # Not ported
//!
//! The JSON output of `A1C_Encoder_json` (and the base64 it uses) is not
//! needed by any encoder and is left out. The arena API is replaced by owned
//! values.
//!
//! # Examples
//!
//! ```ignore
//! use crate::shared::cbor::{Item, decode, encode_to_vec, DecoderConfig};
//!
//! let item = Item::map([(Item::text("n"), Item::Int(7))]);
//! let bytes = encode_to_vec(&item).unwrap();
//! assert_eq!(bytes, [0xA1, 0x61, b'n', 0x07]);
//! let back = decode(&bytes, &DecoderConfig::default()).unwrap();
//! assert_eq!(back.map_get_str("n").and_then(Item::as_int64), Some(7));
//! ```

mod decode;
pub(crate) mod encode;
pub(crate) mod item;

pub(crate) use decode::{DecoderConfig, decode};
pub(crate) use encode::encode_to_vec;
pub(crate) use item::{Error, Item};

#[cfg(test)]
mod tests;
