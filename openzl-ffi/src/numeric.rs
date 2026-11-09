//! Node IDs and parameter IDs of the numeric-family codecs (`shim/numeric.c`).
//!
//! The IDs are the values of the `ZL_NODE_*` macros of the vendored headers,
//! for [`crate::graph::Graph::node`] and friends.

use core::ffi::c_uint;

unsafe extern "C" {
    fn mzl_numeric_id(which: c_uint) -> u32;
}

/// A standard node of the numeric family.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Node {
    /// `ZL_NODE_DELTA_INT`.
    DeltaInt,
    /// `ZL_NODE_ZIGZAG`.
    Zigzag,
    /// `ZL_NODE_DIVIDE_BY`.
    DivideBy,
    /// `ZL_NODE_QUANTIZE_OFFSETS`.
    QuantizeOffsets,
    /// `ZL_NODE_QUANTIZE_LENGTHS`.
    QuantizeLengths,
    /// `ZL_NODE_RANGE_PACK`.
    RangePack,
    /// `ZL_NODE_BITSPLIT` (widths in [`BITSPLIT_WIDTHS`]).
    BitSplit,
    /// `ZL_NODE_BITSPLIT_TOP8`.
    BitSplitTop8,
    /// `ZL_NODE_BITSPLIT_FP`.
    BitSplitFp,
    /// `ZL_NODE_BITSPLIT_BF16`.
    BitSplitBf16,
    /// `ZL_NODE_FLOAT32_DECONSTRUCT`.
    Float32Deconstruct,
    /// `ZL_NODE_BFLOAT16_DECONSTRUCT`.
    Bfloat16Deconstruct,
    /// `ZL_NODE_FLOAT16_DECONSTRUCT`.
    Float16Deconstruct,
    /// `ZL_NODE_SPARSE_NUM`.
    SparseNum,
    /// `ZL_NODE_SPARSE_NUM_AUTO`.
    SparseNumAuto,
    /// `ZL_NODE_MERGE_SORTED`.
    MergeSorted,
    /// `ZL_NODE_CONSTANT_SERIAL`.
    ConstantSerial,
    /// `ZL_NODE_CONSTANT_FIXED`.
    ConstantFixed,
    /// `ZL_NODE_PREFIX`.
    Prefix,
}

impl From<Node> for u32 {
    fn from(node: Node) -> Self {
        // SAFETY: a pure table lookup in the shim.
        unsafe { mzl_numeric_id(node as c_uint) }
    }
}

/// `ZL_DIVIDE_BY_PID`: the 8-byte divisor copy parameter.
pub const DIVIDE_BY_PID: i32 = 112;
/// `ZL_SPARSE_NUM_DOMINANT_VALUE_PID`: the 8-byte dominant symbol.
pub const SPARSE_NUM_DOMINANT_PID: i32 = 132;
/// `ZL_BITSPLIT_WIDTHS_PID`: the bit width of every field, one byte each.
pub const BITSPLIT_WIDTHS: i32 = 701;
