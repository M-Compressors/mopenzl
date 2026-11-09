//! Data statistics shared by the encoder's selectors and graphs.
//!
//! Ports of `shared/histogram.c`, `shared/data_stats.c` and
//! `codecs/common/count.h`:
//!
//! * [`histogram`]: [`Histogram`](histogram::Histogram), the counting every entropy codec starts
//!   from;
//! * [`data_stats`]: [`DataStatsU8`](data_stats::DataStatsU8), the lazily computed statistics of a
//!   byte stream (entropy, cardinality, bit-packed and flat-packed sizes,
//!   Huffman estimates) that the selectors compare;
//! * [`count`]: match-length counting (`ZS_count` and its variants) the LZ
//!   match finders use.
//!
//! Every function is a port of the reference function of the same name and
//! returns the same numbers, including its integer entropy approximation and
//! its estimate quirks.

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the LZ match finders count matches with their own lz_common::count"
    )
)]
pub(crate) mod count;
#[cfg_attr(
    not(any(test, feature = "field-lz")),
    expect(
        dead_code,
        reason = "the delta and Huffman estimates serve the field_lz literals selector only"
    )
)]
pub(crate) mod data_stats;
pub(crate) mod histogram;
