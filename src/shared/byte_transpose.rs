//! Byte-column transposition kernels of the `transpose` codec.
//!
//! The encoder splits a stream of `W`-byte elements into `W` byte columns
//! ([`split`]); the decoder interleaves the columns back ([`merge`]). For
//! the widths 2, 4 and 8 both run on 16-byte vectors: `log2(W)` rounds of
//! even/odd byte extraction (`unzip`) deinterleave sixteen elements, and the
//! same number of `zip` rounds put them back. Other widths, and the elements
//! after the last full group of sixteen, take the scalar loops, which are
//! also the reference the vector paths are tested against.

#[cfg(any(feature = "compression", test))]
use alloc::vec::Vec;

use fearless_simd::{Level, Simd, SimdBase, dispatch, u8x16};

/// Elements per vector group.
const GROUP: usize = 16;

/// Splits `src` (elements of `lanes.len()` bytes) into one column per byte:
/// `lanes[j]` receives byte `j` of every element, `n` bytes each (the
/// vectors are resized to `n`).
#[cfg(any(feature = "compression", test))]
pub(crate) fn split(level: Level, src: &[u8], lanes: &mut [Vec<u8>]) {
    let width = lanes.len();
    let n = src.len() / width;
    for lane in lanes.iter_mut() {
        lane.clear();
        lane.resize(n, 0);
    }
    let done = match width {
        2 => dispatch!(level, simd => split_groups::<_, 2>(simd, src, lanes)),
        4 => dispatch!(level, simd => split_groups::<_, 4>(simd, src, lanes)),
        8 => dispatch!(level, simd => split_groups::<_, 8>(simd, src, lanes)),
        _ => 0,
    };
    split_scalar(src, lanes, done);
}

/// The scalar columns of the elements from `from` on.
#[cfg(any(feature = "compression", test))]
fn split_scalar(src: &[u8], lanes: &mut [Vec<u8>], from: usize) {
    let width = lanes.len();
    for (index, element) in src.chunks_exact(width).enumerate().skip(from) {
        for (lane, &byte) in lanes.iter_mut().zip(element) {
            lane[index] = byte;
        }
    }
}

/// [`split`] of the full groups of sixteen elements, returning the elements
/// done.
#[cfg(any(feature = "compression", test))]
#[inline(always)]
fn split_groups<S: Simd, const W: usize>(simd: S, src: &[u8], lanes: &mut [Vec<u8>]) -> usize {
    let groups = lanes[0].len() / GROUP;
    for (group, bytes) in src.chunks_exact(GROUP * W).take(groups).enumerate() {
        let mut v: [u8x16<S>; W] =
            core::array::from_fn(|i| u8x16::from_slice(simd, &bytes[i * GROUP..(i + 1) * GROUP]));
        for _ in 0..W.trailing_zeros() {
            let previous = v;
            for j in 0..W / 2 {
                v[j] = simd.unzip_low_u8x16(previous[2 * j], previous[2 * j + 1]);
                v[W / 2 + j] = simd.unzip_high_u8x16(previous[2 * j], previous[2 * j + 1]);
            }
        }
        for (lane, vector) in lanes.iter_mut().zip(v) {
            vector.store_slice(&mut lane[group * GROUP..(group + 1) * GROUP]);
        }
    }
    groups * GROUP
}

/// Interleaves the byte `columns` (each `n` bytes long) into the `n`
/// elements of `columns.len()` bytes that fill `dst`.
#[cfg(any(feature = "decompression", test))]
pub(crate) fn merge(level: Level, dst: &mut [u8], columns: &[&[u8]], n: usize) {
    let done = match columns.len() {
        2 => dispatch!(level, simd => merge_groups::<_, 2>(simd, dst, columns, n)),
        4 => dispatch!(level, simd => merge_groups::<_, 4>(simd, dst, columns, n)),
        8 => dispatch!(level, simd => merge_groups::<_, 8>(simd, dst, columns, n)),
        _ => 0,
    };
    merge_scalar(dst, columns, n, done);
}

/// The scalar elements from `from` on.
#[cfg(any(feature = "decompression", test))]
fn merge_scalar(dst: &mut [u8], columns: &[&[u8]], n: usize, from: usize) {
    let width = columns.len();
    for (position, column) in columns.iter().enumerate() {
        for (element, &byte) in dst.chunks_exact_mut(width).zip(&column[..n]).skip(from) {
            element[position] = byte;
        }
    }
}

/// [`merge`] of the full groups of sixteen elements, returning the elements
/// done.
#[cfg(any(feature = "decompression", test))]
#[inline(always)]
fn merge_groups<S: Simd, const W: usize>(
    simd: S,
    dst: &mut [u8],
    columns: &[&[u8]],
    n: usize,
) -> usize {
    let groups = n / GROUP;
    for (group, out) in dst.chunks_exact_mut(GROUP * W).take(groups).enumerate() {
        let mut v: [u8x16<S>; W] = core::array::from_fn(|i| {
            u8x16::from_slice(simd, &columns[i][group * GROUP..(group + 1) * GROUP])
        });
        for _ in 0..W.trailing_zeros() {
            let previous = v;
            for j in 0..W / 2 {
                let (low, high) = simd.interleave_u8x16(previous[j], previous[W / 2 + j]);
                v[2 * j] = low;
                v[2 * j + 1] = high;
            }
        }
        for (i, vector) in v.into_iter().enumerate() {
            vector.store_slice(&mut out[i * GROUP..(i + 1) * GROUP]);
        }
    }
    groups * GROUP
}

#[cfg(all(test, feature = "compression", feature = "decompression"))]
mod tests {
    use super::*;
    use crate::Backend;

    fn sample(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (i.wrapping_mul(0x9E37_79B1) >> 13) as u8)
            .collect()
    }

    #[test]
    fn split_matches_the_scalar_columns_at_every_level() {
        let counts: &[usize] = if cfg!(miri) {
            &[0, 1, 15, 16, 17, 40]
        } else {
            &[0, 1, 15, 16, 17, 31, 32, 33, 100, 1000]
        };
        for width in [1usize, 2, 3, 4, 5, 8, 12] {
            for &n in counts {
                let src = sample(n * width);
                let mut expected: Vec<Vec<u8>> = (0..width).map(|_| alloc::vec![0; n]).collect();
                split_scalar(&src, &mut expected, 0);
                for backend in Backend::available() {
                    let mut lanes: Vec<Vec<u8>> = (0..width).map(|_| Vec::new()).collect();
                    split(backend.0, &src, &mut lanes);
                    assert_eq!(lanes, expected, "{backend} w{width} n{n}");
                }
            }
        }
    }

    #[test]
    fn merge_inverts_split_at_every_level() {
        let counts: &[usize] = if cfg!(miri) {
            &[0, 1, 16, 17, 40]
        } else {
            &[0, 1, 15, 16, 17, 33, 100, 1000]
        };
        for width in [1usize, 2, 3, 4, 8, 16] {
            for &n in counts {
                let src = sample(n * width);
                for backend in Backend::available() {
                    let mut lanes: Vec<Vec<u8>> = (0..width).map(|_| Vec::new()).collect();
                    split(backend.0, &src, &mut lanes);
                    let columns: Vec<&[u8]> = lanes.iter().map(Vec::as_slice).collect();
                    let mut dst = alloc::vec![0u8; n * width];
                    merge(backend.0, &mut dst, &columns, n);
                    assert_eq!(dst, src, "{backend} w{width} n{n}");
                }
            }
        }
    }
}
