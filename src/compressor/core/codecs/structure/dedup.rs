//! The dedup nodes (`encode_dedup_binding.c`): `dedup_num` 37 and the
//! private `dedup_num_trusted` 74.
//!
//! Several numeric inputs that are all identical are stored once: the output
//! references the first input. `dedup_num` verifies the inputs are identical
//! (unless the local parameter `ZL_DEDUP_TRUST_IDENTICAL` is 1) and fails
//! otherwise; the trusted node does not look.

use crate::compressor::core::codecs::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;

/// `ZL_DEDUP_TRUST_IDENTICAL`.
pub(crate) const TRUST_IDENTICAL_PID: i32 = crate::graph::params::dedup::TRUST_IDENTICAL;

/// `EI_dedup_num`.
pub(crate) static DEDUP_NUM: FnEncoder = FnEncoder(dedup_num);
/// `EI_dedup_num_trusted`.
pub(crate) static DEDUP_NUM_TRUSTED: FnEncoder = FnEncoder(dedup_num_trusted);

/// `EI_dedup_num_internal`.
fn dedup<'a>(
    cx: &mut NodeCx<'_, 'a>,
    inputs: &[Stream<'a>],
    trusted: bool,
) -> Result<(), NodeError> {
    let first = &inputs[0];
    if !trusted {
        for other in &inputs[1..] {
            if other.width() != first.width()
                || other.n_elts() != first.n_elts()
                || other.bytes() != first.bytes()
            {
                return Err(NodeError::InvalidInput("dedup inputs are not identical"));
            }
        }
    }
    cx.reference(0, first, first.width(), first.n_elts(), 0)?;
    Ok(())
}

fn dedup_num<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let trusted = cx.int(TRUST_IDENTICAL_PID) == Some(1);
    dedup(cx, inputs, trusted)
}

fn dedup_num_trusted<'a>(
    cx: &mut NodeCx<'_, 'a>,
    inputs: &[Stream<'a>],
) -> Result<(), NodeError> {
    dedup(cx, inputs, true)
}
