//! Primitives shared by the encoder and the decoder.
//!
//! Nothing here is public: the format constants, varints, bit packing and
//! the XXH3 checksum are implementation details of both codecs.

pub(crate) mod bitpack;
#[cfg(all(
    feature = "compression",
    any(feature = "bitpack", feature = "bitunpack")
))]
pub(crate) mod bitpack_kernels;
#[cfg(feature = "transpose")]
pub(crate) mod byte_transpose;
#[cfg(feature = "compression")]
pub(crate) mod cbor;
pub(crate) mod destination;
pub(crate) mod dictwire;
#[cfg(all(
    feature = "compression",
    any(feature = "entropy", feature = "pivco-huffman")
))]
pub(crate) mod entropy;
#[cfg(feature = "compression")]
pub(crate) mod estimate;
#[cfg(any(
    all(feature = "compression", feature = "lz4"),
    all(feature = "decompression", feature = "entropy")
))]
pub(crate) mod fixed;
#[cfg(feature = "compression")]
pub(crate) mod numeric;
pub(crate) mod sha256;
#[cfg(all(
    feature = "compression",
    any(feature = "entropy", feature = "pivco-huffman")
))]
pub(crate) mod stats;
pub(crate) mod varint;
pub(crate) mod wire;
pub(crate) mod xxh3;
