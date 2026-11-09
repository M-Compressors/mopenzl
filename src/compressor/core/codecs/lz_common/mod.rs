//! The LZ infrastructure shared by the `field_lz` and `lz` codecs.
//!
//! Ports of `codecs/common/{window,fast_table,row_table,count}` and the
//! hash functions of `shared/hash.h`, written over indices into the source
//! slice instead of raw pointers.
//!
//! Not ported because nothing in the reference calls them:
//! `fast_table16`, `fast_tag_table` (dead code of the library) and
//! `copy.h` (its wild copies are plain slice copies here). The window's
//! overflow correction is not needed either: a node never sees 3 GiB of
//! indices (the codecs reject inputs above 4,000,000,000 bytes, and the
//! window is used for a single segment).

pub(crate) mod count;
pub(crate) mod fast_table;
pub(crate) mod hash;
pub(crate) mod row_table;
#[cfg(feature = "field-lz")]
pub(crate) mod window;
