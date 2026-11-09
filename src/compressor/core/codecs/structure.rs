//! The structure codecs: tokenize, split, split-by-struct, dispatch, concat,
//! dedup, interleave, prefix, parse-int and transpose.
//!
//! Each codec family lives in its own module behind its own Cargo feature.
//! The statics the registry points at exist either way: with the feature on
//! they are the real encoders, with it off they are [`Disabled`], so the
//! registry rows read `if cfg!(feature = "...") { &structure::X } else {
//! &DISABLED }` like every other codec.
//!
//! This module also owns what the graph side shares with the codecs and so
//! cannot be gated with them: the parameter IDs `run_split_node`,
//! `run_dispatch_node` and `try_parse_int` pass to the nodes, and the dispatch
//! instructions type.

use alloc::vec::Vec;

#[cfg(not(all(
    feature = "tokenize",
    feature = "split",
    feature = "split-by-struct",
    feature = "dispatch",
    feature = "concat",
    feature = "dedup",
    feature = "interleave",
    feature = "prefix",
    feature = "parse-int",
    feature = "transpose"
)))]
use super::Disabled;

/// `ZL_SPLITN_SEGMENTSIZES_PID`: the `size_t` array `splitN` cuts by.
pub(crate) const SEGMENT_SIZES_PID: i32 = 323;
/// `ZL_DISPATCH_INSTRUCTIONS_PID`: the [`DispatchInstructions`] object.
pub(crate) const INSTRUCTIONS_PID: i32 = 520;
/// `ZL_DISPATCH_STRING_NUM_OUTPUTS_PID`.
pub(crate) const STRING_NUM_OUTPUTS_PID: i32 = crate::graph::params::dispatch::STRING_NUM_OUTPUTS;
/// `ZL_DISPATCH_STRING_INDICES_PID`: the `u16` index array.
pub(crate) const STRING_INDICES_PID: i32 = crate::graph::params::dispatch::STRING_INDICES;
/// `ZL_PARSE_INT_PREPARSED_PARAMS`: integers parsed ahead of the node.
pub(crate) const PREPARSED_PARAMS_PID: i32 = 350;

/// `ZL_DispatchInstructions`: how `dispatchN_byTag` cuts and routes its input.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DispatchInstructions {
    /// The size in bytes of every segment, in input order.
    pub(crate) segment_sizes: Vec<usize>,
    /// The tag of every segment: the stream it goes to.
    pub(crate) tags: Vec<u32>,
    /// The number of streams (tags are below it).
    pub(crate) nb_tags: usize,
}

/// Declares a codec family: the module and its encoders when `$feature` is
/// on, [`Disabled`] stand-ins for the same names when it is off.
mod util;

macro_rules! family {
    ($feature:literal, $module:ident, [$($name:ident),+ $(,)?]) => {
        #[cfg(feature = $feature)]
        mod $module;
        #[cfg(feature = $feature)]
        pub(crate) use $module::{$($name),+};
        $(
            #[cfg(not(feature = $feature))]
            #[doc = "Stand-in for the encoder of a disabled family."]
            pub(crate) static $name: Disabled = Disabled;
        )+
    };
}

family!("tokenize", tokenize, [TOKENIZE, TOKENIZE_STRING]);
family!("split", split, [SPLIT_N, SPLIT_BYRANGE]);
family!("split-by-struct", split_by_struct, [SPLIT_BY_STRUCT]);
family!("dispatch", dispatch, [DISPATCH_N_BY_TAG, DISPATCH_STRING]);
family!("concat", concat, [CONCAT]);
family!("dedup", dedup, [DEDUP_NUM, DEDUP_NUM_TRUSTED]);
family!("interleave", interleave, [INTERLEAVE]);
family!("prefix", prefix, [PREFIX]);
family!("parse-int", parse_int, [PARSE_INT]);
family!("transpose", transpose, [TRANSPOSE_SPLIT]);

/// `ZL_parseInt64_fallback`: strictly parses the whole of `text`.
///
/// Fails on an empty string, a `+`, leading zeros, a non-digit, or a value
/// outside the `i64` range.
pub(crate) fn parse_int64(text: &[u8]) -> Option<i64> {
    let (negative, digits) = match text.split_first() {
        None => return None,
        Some((b'-', rest)) => (true, rest),
        Some(_) => (false, text),
    };
    // Empty after the sign, or longer than any i64 plus the guaranteed
    // overflow of a 21st digit.
    if digits.is_empty() || digits.len() > 20 {
        return None;
    }
    if digits[0] == b'0' {
        return (digits.len() == 1 && !negative).then_some(0);
    }
    let mut value = 0u64;
    for &digit in digits {
        if !digit.is_ascii_digit() {
            return None;
        }
        value = value
            .checked_mul(10)?
            .checked_add(u64::from(digit - b'0'))?;
    }
    if negative {
        let signed = 0u64.wrapping_sub(value) as i64;
        (signed <= 0).then_some(signed)
    } else {
        let signed = value as i64;
        (signed >= 0).then_some(signed)
    }
}
