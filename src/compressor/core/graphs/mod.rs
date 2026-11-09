//! Standard graph functions, selectors and segmenters.
//!
//! Every standard graph of the reference is here: the store graphs, the
//! generic compression routers, the selectors (generic LZ backend, numeric,
//! bitpack, constant, the transformer family, brute force, the field LZ
//! literals channel), the function graphs (entropy, field LZ, lz,
//! partition_bitpack, merge_sorted, try_parse_int, the split graphs,
//! compress_small_lengths, ml_selector, clustering, sddl) and the segmenters
//! (serial, numeric, num-from-serial, sddl2). A graph whose codec's Cargo
//! feature is off runs a stub instead, which fails with
//! [`ExecError::Unported`](super::error::ExecError::Unported) like a missing
//! codec in the reference.

mod bitpack;
pub(crate) mod clustering;
mod constant;
#[cfg(feature = "entropy")]
pub(crate) mod entropy;
#[cfg(all(feature = "field-lz", feature = "entropy"))]
mod field_lz;
mod generic;
#[cfg(all(feature = "lz", feature = "entropy"))]
mod lz;
#[cfg(feature = "merge-sorted")]
mod merge_sorted;
pub(crate) mod ml_selector;
#[cfg(feature = "partition")]
mod partition_bitpack;
mod sddl;
mod sddl2;
mod segmenters;
mod small_lengths;
mod store;
mod structure;
mod transformer;

pub(crate) use bitpack::BITPACK;
pub(crate) use clustering::CLUSTERING;
pub(crate) use constant::CONSTANT;
#[cfg(feature = "entropy")]
pub(crate) use entropy::{ENTROPY, FSE, HUFFMAN, HUFFMAN_HUF0, HUFFMAN_PIVCO};
#[cfg(all(feature = "field-lz", feature = "entropy"))]
pub(crate) use field_lz::{FIELD_LZ, FIELD_LZ_LITERALS, FIELD_LZ_LITERALS_CHANNEL};
pub(crate) use generic::{
    BRUTE_FORCE, COMPRESS_GENERIC, COMPRESS1, NUMERIC_COMPRESS, SELECT_GENERIC_LZ, SELECT_NUMERIC,
    SERIAL_COMPRESS, STRING_COMPRESS, STRUCT_COMPRESS,
};
#[cfg(all(feature = "lz", feature = "entropy"))]
pub(crate) use lz::LZ;
#[cfg(feature = "merge-sorted")]
pub(crate) use merge_sorted::MERGE_SORTED_GRAPH;
pub(crate) use ml_selector::ML_SELECTOR;
#[cfg(feature = "partition")]
pub(crate) use partition_bitpack::PARTITION_BITPACK;
pub(crate) use sddl::{DESCRIPTION_PID as SDDL_DESCRIPTION_PID, SDDL, validate as validate_sddl};
pub(crate) use sddl2::{
    BYTECODE_PARAM as SDDL2_BYTECODE_PARAM, CHUNK_BYTE_SIZE_PARAM as SDDL2_CHUNK_BYTE_SIZE_PARAM,
    SDDL2, SDDL2_CHUNK,
};
pub(crate) use segmenters::{
    NUM_FROM_SERIAL_CHUNK_BYTE_SIZE, NUM_FROM_SERIAL_ELT_WIDTH, SEGMENT_NUM_FROM_SERIAL,
    SEGMENT_NUMERIC, SEGMENT_SERIAL, SERIAL_CHUNK_BYTE_SIZE,
};
pub(crate) use small_lengths::COMPRESS_SMALL_LENGTHS;
pub(crate) use store::{STORE, STORE1};
pub(crate) use structure::{
    N_TO_N, SPLIT, TRANSPOSE_SPLIT_SELECTOR, TRY_PARSE_INT, run_dispatch_node,
    run_dispatch_string_node, run_split_node,
};
pub(crate) use transformer::{
    STATIC_CORE, STATIC_FALLBACK, STATIC_INDEX, TRANSFORMER_NUMERIC, TRANSFORMER_NUMERIC1,
};

/// The function graph a row runs when its codec's Cargo feature is off.
#[cfg(any(
    all(test, feature = "zstd", feature = "conversion"),
    not(all(
        feature = "field-lz",
        feature = "lz",
        feature = "entropy",
        feature = "partition",
        feature = "merge-sorted"
    ))
))]
#[derive(Debug)]
pub(crate) struct UnportedFunction;

/// The selector a row runs when its codec's Cargo feature is off: it picks `ZL_GRAPH_ILLEGAL`, which
/// fails the destination assignment like any invalid choice.
#[cfg(any(
    all(test, feature = "zstd", feature = "conversion"),
    not(all(feature = "field-lz", feature = "entropy"))
))]
#[derive(Debug)]
pub(crate) struct UnportedSelector;

/// A segmenter that fails, a placeholder for tests of the engine: every
/// standard segmenter is ported.
#[cfg(all(test, feature = "zstd", feature = "conversion"))]
#[derive(Debug)]
pub(crate) struct UnportedSegmenter;

/// See [`UnportedFunction`].
#[cfg(any(
    all(test, feature = "zstd", feature = "conversion"),
    not(all(
        feature = "field-lz",
        feature = "lz",
        feature = "entropy",
        feature = "partition",
        feature = "merge-sorted"
    ))
))]
pub(crate) static UNPORTED_FUNCTION: UnportedFunction = UnportedFunction;
/// Without the `field-lz` and `entropy` features the `field_lz` graphs are
/// unavailable.
#[cfg(not(all(feature = "field-lz", feature = "entropy")))]
pub(crate) use self::UNPORTED_FUNCTION as FIELD_LZ;
/// See [`FIELD_LZ`].
#[cfg(not(all(feature = "field-lz", feature = "entropy")))]
pub(crate) use self::UNPORTED_FUNCTION as FIELD_LZ_LITERALS;
/// Without the `lz` and `entropy` features the `lz` graph is unavailable.
#[cfg(not(all(feature = "lz", feature = "entropy")))]
pub(crate) use self::UNPORTED_FUNCTION as LZ;
/// Without the `partition` feature the `partition_bitpack` graph is
/// unavailable and fails like a graph that is not ported.
#[cfg(not(feature = "partition"))]
pub(crate) use self::UNPORTED_FUNCTION as PARTITION_BITPACK;
/// Without the `merge-sorted` feature the `merge_sorted` graph is unavailable.
#[cfg(not(feature = "merge-sorted"))]
pub(crate) use self::UNPORTED_FUNCTION as MERGE_SORTED_GRAPH;
/// See [`FIELD_LZ`].
#[cfg(not(all(feature = "field-lz", feature = "entropy")))]
pub(crate) use self::UNPORTED_SELECTOR as FIELD_LZ_LITERALS_CHANNEL;
/// See [`UnportedSelector`].
#[cfg(any(
    all(test, feature = "zstd", feature = "conversion"),
    not(all(feature = "field-lz", feature = "entropy"))
))]
pub(crate) static UNPORTED_SELECTOR: UnportedSelector = UnportedSelector;
/// See [`UnportedSegmenter`].
#[cfg(all(test, feature = "zstd", feature = "conversion"))]
pub(crate) static UNPORTED_SEGMENTER: UnportedSegmenter = UnportedSegmenter;

/// Without the `entropy` feature the entropy graphs are not available: their
/// rows run this stub, which fails like any graph whose codec is missing.
#[cfg(not(feature = "entropy"))]
macro_rules! disabled_graphs {
    ($($name:ident),*) => {$(
        #[doc = concat!("The `", stringify!($name), "` graph without the `entropy` feature.")]
        pub(crate) static $name: UnportedFunction = UnportedFunction;
    )*};
}

#[cfg(not(feature = "entropy"))]
disabled_graphs!(ENTROPY, FSE, HUFFMAN, HUFFMAN_HUF0, HUFFMAN_PIVCO);

#[cfg(any(
    all(test, feature = "zstd", feature = "conversion"),
    not(all(
        feature = "field-lz",
        feature = "lz",
        feature = "entropy",
        feature = "partition",
        feature = "merge-sorted"
    ))
))]
impl super::registry::GraphFn for UnportedFunction {
    fn run(
        &self,
        _: &mut super::exec::GraphCx<'_, '_, '_>,
        _: &[super::exec::Edge],
    ) -> Result<(), super::error::ExecError> {
        Err(super::error::ExecError::Unported)
    }
}

#[cfg(all(test, feature = "zstd", feature = "conversion"))]
impl super::registry::SegmentFn for UnportedSegmenter {
    fn segment(
        &self,
        _: &mut super::exec::SegmenterCx<'_, '_, '_>,
    ) -> Result<(), super::error::ExecError> {
        Err(super::error::ExecError::Unported)
    }
}

#[cfg(any(
    all(test, feature = "zstd", feature = "conversion"),
    not(all(feature = "field-lz", feature = "entropy"))
))]
impl super::registry::SelectFn for UnportedSelector {
    fn select(
        &self,
        _: &mut super::exec::SelectorCx<'_, '_, '_>,
        _: &super::data::Stream<'_>,
        _: &[super::registry::GraphId],
    ) -> super::registry::GraphId {
        super::registry::GraphId::ILLEGAL
    }
}
