// Wraps vendor/openzl/src/openzl/codecs/bitpack/common_bitpack_kernel.c with
// its AVX2+BMI2 kernels compiled out, on every host.
//
// `ZS_HAS_FAST_BITPACK` is `ZL_HAS_AVX2 && ZL_HAS_BMI2`, both read from the
// compiler's `__AVX2__` and `__BMI2__`. The fast kernels pack numbers wider
// than their field differently from the portable ones: they mask most of
// them and spill the bits of the last few. Only damaged frames hold such
// numbers (the `bitunpack` decoder), but the oracle's answer for them would
// then depend on the host and on flags such as cargo-afl's
// `-Ctarget-cpu=native`. mopenzl ports the portable kernels, so the oracle
// always runs those. Numbers that fit their field pack identically on both
// paths.

#undef __BMI2__
#include "openzl/codecs/bitpack/common_bitpack_kernel.c"
