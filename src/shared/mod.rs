//! Primitives shared by the encoder and the decoder.
//!
//! Nothing here is public: the format constants, varints, bit packing and
//! the XXH3 checksum are implementation details of both codecs.

pub(crate) mod bitpack;
pub(crate) mod destination;
pub(crate) mod varint;
pub(crate) mod wire;
pub(crate) mod xxh3;
