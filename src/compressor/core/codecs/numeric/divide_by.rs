//! The divide-by node (`EI_divide_by_int`, `encode_divide_by_binding.c`,
//! `encode_divide_by_kernel.c` and `common_gcd.h`), codec 53.
//!
//! Divides every element by a divisor and sends the divisor as a varint
//! header. The divisor is the copy parameter `ZL_DIVIDE_BY_PID` (8 bytes,
//! little-endian) or, when absent or zero, the greatest common divisor of the
//! elements (1 for fewer than two elements or only zeros).
//!
//! The reference divides by multiplying with the modular inverse of the
//! divisor's odd part and shifting, and tests divisibility with the same
//! trick. For a divisor that divides the element both give `element /
//! divisor`, and the divisibility test accepts exactly the multiples, so this
//! port uses plain integer division and remainder with identical results.

use alloc::vec::Vec;

use super::super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;
use crate::shared::numeric::{elements, push_le};
use crate::shared::varint;

/// `ZL_DIVIDE_BY_PID`.
const DIVIDE_BY_PID: i32 = crate::graph::params::divide_by::DIVIDE_BY;

/// `ZL_NODE_DIVIDE_BY` (node 31).
pub(crate) static DIVIDE_BY: FnEncoder = FnEncoder(divide_by);

/// `ZL_gcdImpl`: the greatest common divisor, `a` when `b` is zero.
const fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// `ZL_gcdVec`: the greatest common divisor of the elements, 1 when there are
/// fewer than two or all are zero.
fn gcd_vec(bytes: &[u8], width: usize) -> u64 {
    if bytes.len() / width <= 1 {
        return 1;
    }
    let divisor = elements(bytes, width).fold(0, gcd);
    if divisor == 0 { 1 } else { divisor }
}

/// `getDivisor`: the divisor to use, or the error the reference raises for
/// an explicit divisor that is too large or does not divide every element.
fn divisor_for(bytes: &[u8], width: usize, requested: u64) -> Result<u64, NodeError> {
    if requested == 0 {
        return Ok(gcd_vec(bytes, width));
    }
    if width < 8 && requested >= 1 << (8 * width) {
        return Err(NodeError::InvalidInput("Divisor too large"));
    }
    if elements(bytes, width).any(|value| value % requested != 0) {
        return Err(NodeError::InvalidInput("an element is not divisible"));
    }
    Ok(requested)
}

fn divide_by<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let width = input.width();
    let requested = match super::param_ref(cx, DIVIDE_BY_PID) {
        None => 0,
        Some(bytes) => {
            let bytes: [u8; 8] = bytes
                .try_into()
                .map_err(|_| NodeError::InvalidParam("the divisor must be a uint64_t"))?;
            u64::from_le_bytes(bytes)
        }
    };
    let divisor = divisor_for(input.bytes(), width, requested)?;
    let mut out = Vec::with_capacity(input.content_size());
    for value in elements(input.bytes(), width) {
        push_le(&mut out, value / divisor, width);
    }
    let mut header = Vec::new();
    varint::push(&mut header, divisor);
    cx.send_header(&header)?;
    cx.emit(0, width, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gcd_of_few_or_zero_elements_is_one() {
        assert_eq!(gcd_vec(&[], 4), 1);
        assert_eq!(gcd_vec(&[12], 1), 1);
        assert_eq!(gcd_vec(&[0, 0, 0], 1), 1);
        assert_eq!(gcd_vec(&[0, 12, 18], 1), 6);
        assert_eq!(gcd_vec(&[12, 0, 8, 20], 1), 4);
        assert_eq!(gcd_vec(&[6, 0, 9, 0], 2), 3);
        assert_eq!(gcd(0, 5), 5);
        assert_eq!(gcd(12, 0), 12);
    }

    #[test]
    fn explicit_divisors_are_validated() {
        assert_eq!(divisor_for(&[6, 12], 1, 3), Ok(3));
        assert_eq!(divisor_for(&[6, 12], 1, 256), Err(NodeError::InvalidInput("Divisor too large")));
        assert_eq!(divisor_for(&[6, 13], 1, 3), Err(NodeError::InvalidInput("an element is not divisible")));
        assert_eq!(divisor_for(&[6, 12], 1, 0), Ok(6));
        let wide = [6u64, 12].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>();
        assert_eq!(divisor_for(&wide, 8, 1 << 40), Err(NodeError::InvalidInput("an element is not divisible")));
        assert_eq!(divisor_for(&wide, 8, 2), Ok(2));
    }
}
