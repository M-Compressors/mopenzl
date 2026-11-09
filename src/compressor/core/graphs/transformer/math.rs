//! The libm entry points the Transformer selector calls.
//!
//! With `std` they are the platform's, which is what the reference links, so
//! results agree on the same host; `no_std` builds use the `libm` crate,
//! whose `exp` and `log` can differ in the last bit (see the module docs of
//! [`shared::estimate`](crate::shared::estimate)). The network's `expf` is
//! the one place where that could flip a score by one ULP; scores are
//! compared against zero and ranked by logits, so a one-ULP difference
//! matters only at an exact tie.

/// `log`.
pub(super) fn ln(x: f64) -> f64 {
    #[cfg(not(feature = "no_std"))]
    return x.ln();
    #[cfg(feature = "no_std")]
    return libm::log(x);
}

/// `log2`.
pub(super) fn log2(x: f64) -> f64 {
    #[cfg(not(feature = "no_std"))]
    return x.log2();
    #[cfg(feature = "no_std")]
    return libm::log2(x);
}

/// `expf`.
pub(super) fn expf(x: f32) -> f32 {
    #[cfg(not(feature = "no_std"))]
    return x.exp();
    #[cfg(feature = "no_std")]
    return libm::expf(x);
}

/// `sqrt` (correctly rounded everywhere).
pub(super) fn sqrt(x: f64) -> f64 {
    #[cfg(not(feature = "no_std"))]
    return x.sqrt();
    #[cfg(feature = "no_std")]
    return libm::sqrt(x);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libm_entry_points_agree_with_known_values() {
        assert_eq!(ln(1.0), 0.0);
        assert_eq!(log2(8.0), 3.0);
        assert_eq!(expf(0.0), 1.0);
        assert_eq!(sqrt(9.0), 3.0);
    }
}
