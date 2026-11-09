//! OpenZL's FSE and Huffman encoder libraries (`src/openzl/fse/`).
//!
//! OpenZL forked these from Zstandard 1.5.7 and changed them (a
//! configurable number of FSE states, a table-log aware histogram, the
//! workspace layout), so `mzstd` cannot stand in for them. This module is a
//! port of the encoder halves the entropy codecs use:
//!
//! * [`fse`]: `fse_compress.c`: count normalisation, the compression table,
//!   the count header (`FSE_writeNCount`) and the interleaved-state encoder
//!   (`FSE_compress_usingCTable`);
//! * [`huf`]: `huf_compress.c`: `HUF_buildCTable`, the table-log chooser,
//!   `HUF_writeCTable`, the size estimate and the one- and four-stream
//!   encoders;
//! * [`huf16`]: `codecs/entropy/encode_huffman_kernel.c`, the 16-bit
//!   "large" Huffman of `huffman_struct_v2`;
//! * [`bits`]: the forward-writing bit container (`BIT_CStream_t`) every
//!   encoder above shares.
//!
//! All encoders produce the reference's exact bytes. The reference's
//! unrolled and "fast" loop variants only differ in speed, so a single
//! straightforward bit writer replaces them; the buffer-capacity failure
//! rules are kept (see [`bits::BitWriter::close`]).
//!
//! The module is shared by every package that entropy-codes: `entropy`
//! (the FSE and Huffman nodes) and `pivco-huffman` (which builds Huffman
//! tables).

#![cfg_attr(
    all(not(test), not(feature = "entropy")),
    expect(
        dead_code,
        reason = "without the `entropy` codecs only the Huffman table construction is used (by pivco-huffman)"
    )
)]

pub(crate) mod bits;
pub(crate) mod fse;
pub(crate) mod huf;
pub(crate) mod huf16;

use thiserror::Error;

/// `ENTROPY_HISTORAM_PID`: the reference parameter through which the entropy
/// graph hands its input's [`Histogram`](super::stats::histogram::Histogram) to the
/// entropy nodes it drives, so they skip recomputing it.
pub(crate) const HISTOGRAM_PID: i32 = 246;
/// `ENTROPY_HUF_CTABLE_PID`: the reference parameter holding the
/// [`HufCTable`](huf::HufCTable) built from that histogram.
pub(crate) const HUF_CTABLE_PID: i32 = 247;

/// Why an entropy library call failed. The variants mirror the Zstandard
/// error codes the reference returns; their numeric values matter only to
/// [`EntropyError::code`], which reproduces the wrapped `size_t` the
/// reference hands back in the one place that adds an error to a size.
#[derive(Error, Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum EntropyError {
    /// `ZSTD_error_GENERIC`.
    #[error("entropy coding failed")]
    Generic,
    /// `ZSTD_error_tableLog_tooLarge`.
    #[error("table log too large")]
    TableLogTooLarge,
    /// `ZSTD_error_maxSymbolValue_tooLarge`.
    #[error("symbol value too large")]
    MaxSymbolValueTooLarge,
    /// `ZSTD_error_dstSize_tooSmall`.
    #[error("destination too small")]
    DstSizeTooSmall,
}

impl EntropyError {
    /// The positive Zstandard error number (`ERROR(x)` is `(size_t)-code`).
    pub(crate) const fn code(self) -> usize {
        match self {
            Self::Generic => 1,
            Self::TableLogTooLarge => 44,
            Self::MaxSymbolValueTooLarge => 46,
            Self::DstSizeTooSmall => 70,
        }
    }
}

/// `ZSTD_highbit32`: the index of the highest set bit of a non-zero value.
///
/// Zero is undefined in the reference; this returns 0 for it.
pub(crate) const fn highbit32(value: u32) -> u32 {
    31 - (value | 1).leading_zeros()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn error_codes_follow_zstd_numbering() {
        assert_eq!(EntropyError::Generic.code(), 1);
        assert_eq!(EntropyError::TableLogTooLarge.code(), 44);
        assert_eq!(EntropyError::MaxSymbolValueTooLarge.code(), 46);
        assert_eq!(EntropyError::DstSizeTooSmall.code(), 70);
        assert_eq!(EntropyError::Generic.to_string(), "entropy coding failed");
        assert_eq!(
            EntropyError::TableLogTooLarge.to_string(),
            "table log too large"
        );
        assert_eq!(
            EntropyError::MaxSymbolValueTooLarge.to_string(),
            "symbol value too large"
        );
        assert_eq!(
            EntropyError::DstSizeTooSmall.to_string(),
            "destination too small"
        );
    }

    #[test]
    fn highbit32_is_the_floor_of_log2() {
        assert_eq!(highbit32(1), 0);
        assert_eq!(highbit32(2), 1);
        assert_eq!(highbit32(255), 7);
        assert_eq!(highbit32(256), 8);
        assert_eq!(highbit32(u32::MAX), 31);
        assert_eq!(highbit32(0), 0);
    }
}
