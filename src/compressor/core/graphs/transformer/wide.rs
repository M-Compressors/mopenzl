//! Wide integer helpers (`compress/selectors/transformer/wide_arith.c`).
//!
//! The reference builds 128-bit helpers from `__int128`; Rust has the types
//! natively, so each helper is the `__int128` branch with the reference's
//! release-build saturation (quotients that do not fit return `u64::MAX`).

/// `TRS_NUMERIC_Q32_SCALE`.
pub(super) const Q32_SCALE: u64 = 1 << 32;

/// `TRS_wide_u128_div_u64_to_u64`: `x / divisor`, saturating to `u64::MAX`
/// when the divisor is zero or the quotient does not fit.
pub(super) const fn div_u64_to_u64(x: u128, divisor: u64) -> u64 {
    if divisor == 0 || ((x >> 64) as u64) >= divisor {
        return u64::MAX;
    }
    (x / divisor as u128) as u64
}

/// `TRS_wide_u128_div_u128_to_u64`.
pub(super) const fn div_u128_to_u64(numerator: u128, denominator: u128) -> u64 {
    if denominator == 0 {
        return u64::MAX;
    }
    let fits = (denominator >> 64) != 0 || ((numerator >> 64) as u64) < denominator as u64;
    if !fits {
        return u64::MAX;
    }
    (numerator / denominator) as u64
}

/// `TRS_wide_u128_scaled_div_to_u64`: `sum / count` in Q32, saturating.
pub(super) const fn scaled_div_to_u64(sum: u128, count: usize) -> u64 {
    if count == 0 {
        return u64::MAX;
    }
    let count = count as u128;
    let quotient = sum / count;
    let remainder = sum % count;
    if quotient > (u64::MAX >> 32) as u128 {
        return u64::MAX;
    }
    (quotient * Q32_SCALE as u128 + (remainder * Q32_SCALE as u128) / count) as u64
}

/// `TRS_wide_u128_to_double`: separately rounded halves.
pub(super) fn u128_to_f64(x: u128) -> f64 {
    ((x >> 64) as u64 as f64) * 18_446_744_073_709_551_616.0 + (x as u64 as f64)
}

/// `TRS_wide_i64_abs_diff_to_u64`: `|lhs - rhs|`. The reference saturates
/// the 128-bit magnitude, which never exceeds `u64::MAX` for two `i64`s.
pub(super) const fn i64_abs_diff_to_u64(lhs: i64, rhs: i64) -> u64 {
    lhs.abs_diff(rhs)
}

/// `TRS_numeric_abs_diff_u64`.
pub(super) const fn abs_diff_u64(a: u64, b: u64) -> u64 {
    a.abs_diff(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn division_saturates_like_the_release_build() {
        assert_eq!(div_u64_to_u64(10, 3), 3);
        assert_eq!(div_u64_to_u64(10, 0), u64::MAX);
        assert_eq!(div_u64_to_u64(1 << 64, 1), u64::MAX);
        assert_eq!(div_u128_to_u64(10, 3), 3);
        assert_eq!(div_u128_to_u64(10, 0), u64::MAX);
        assert_eq!(div_u128_to_u64(1 << 100, 1), u64::MAX);
        assert_eq!(div_u128_to_u64(1 << 100, 1 << 70), 1 << 30);
    }

    #[test]
    fn scaled_division_keeps_the_fraction_and_saturates() {
        assert_eq!(scaled_div_to_u64(1, 2), 1 << 31);
        assert_eq!(scaled_div_to_u64(7, 7), 1 << 32);
        assert_eq!(scaled_div_to_u64(1, 0), u64::MAX);
        assert_eq!(scaled_div_to_u64(1 << 40, 1), u64::MAX);
        assert_eq!(scaled_div_to_u64(10, 3), (10u64 << 32) / 3);
    }

    #[test]
    fn conversions_to_double_and_absolute_differences() {
        assert_eq!(u128_to_f64(0), 0.0);
        assert_eq!(u128_to_f64(1 << 64), 18_446_744_073_709_551_616.0);
        assert_eq!(
            u128_to_f64((1 << 64) + 5),
            18_446_744_073_709_551_616.0 + 5.0
        );
        assert_eq!(i64_abs_diff_to_u64(5, -3), 8);
        assert_eq!(i64_abs_diff_to_u64(i64::MAX, i64::MIN), u64::MAX);
        assert_eq!(abs_diff_u64(3, 9), 6);
    }
}
