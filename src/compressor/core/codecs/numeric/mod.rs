//! Numeric transform encoders (WP4): delta, zigzag, range pack, divide by,
//! merge sorted, partition, sparse num, sentinel, bit split and the float
//! deconstruction nodes.
//!
//! Each codec has a file named after its reference directory under
//! `src/openzl/codecs/` and a Cargo feature. A codec's statics exist in every
//! build: with the feature off they alias [`DISABLED`](super::DISABLED), so
//! the registry rows keep their `cfg!(feature = ...)` shape and the node
//! fails with [`NodeError::Disabled`](super::super::error::NodeError::Disabled)
//! like any codec the build lacks.

/// Declares a codec module behind its feature, re-exports its encoder statics
/// and aliases them to `DISABLED` when the feature is off.
macro_rules! codec {
    ($feature:literal, $module:ident, [$($name:ident),+ $(,)?]) => {
        #[cfg(feature = $feature)]
        mod $module;
        #[cfg(feature = $feature)]
        pub(crate) use $module::{$($name),+};
        $(
            #[cfg(not(feature = $feature))]
            pub(crate) use super::DISABLED as $name;
        )+
    };
}

codec!("delta", delta, [DELTA_INT]);
codec!("zigzag", zigzag, [ZIGZAG]);
codec!("range-pack", range_pack, [RANGE_PACK]);
codec!("divide-by", divide_by, [DIVIDE_BY]);
codec!("merge-sorted", merge_sorted, [MERGE_SORTED]);
codec!("partition", partition, [PARTITION]);
codec!("sparse-num", sparse_num, [SPARSE_NUM, SPARSE_NUM_AUTO]);
codec!("sentinel", sentinel, [SENTINEL_BYTE, SENTINEL_NUM]);
codec!(
    "bitsplit",
    bitsplit,
    [BITSPLIT, BITSPLIT_TOP8, BITSPLIT_FP, BITSPLIT_BF16]
);
codec!(
    "float-deconstruct",
    float_deconstruct,
    [
        FLOAT32_DECONSTRUCT,
        BFLOAT16_DECONSTRUCT,
        FLOAT16_DECONSTRUCT
    ]
);

/// What the `merge_sorted` graph needs from the merge-sorted node.
#[cfg(feature = "merge-sorted")]
pub(crate) use merge_sorted::within_run_limit;
/// The partition parameters, for the graph's tests.
#[cfg(all(test, feature = "partition"))]
pub(crate) use partition::PartitionParams;
/// What the `partition_bitpack` graph needs from the partition node.
#[cfg(feature = "partition")]
pub(crate) use partition::custom_params_bytes;

/// `ZL_Encoder_getLocalParam(...).paramRef != NULL`: the bytes of copy
/// parameter `id`, treating an empty one as absent (the reference stores no
/// pointer for a zero-length copy).
#[cfg(any(feature = "divide-by", feature = "sparse-num", feature = "sentinel"))]
fn param_ref<'p>(cx: &'p super::super::exec::NodeCx<'_, '_>, id: i32) -> Option<&'p [u8]> {
    cx.copy_param(id).filter(|bytes| !bytes.is_empty())
}
