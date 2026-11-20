// Test helpers of the encoder "backends" package: the C LZ4 block
// compressors OpenZL's `lz4` node calls, and the bit-packing kernel the
// `bitunpack` decoder runs, so Rust tests can compare their output with the
// safe-Rust port byte for byte.

#include <stddef.h>

#include "lz4.h"
#include "lz4hc.h"
#include "openzl/codecs/bitpack/common_bitpack_kernel.h"

// `LZ4_compress_fast`, or -1 when a size does not fit an `int`.
int mzl_lz4_compress_fast(
        const void* src,
        size_t src_size,
        void* dst,
        size_t dst_capacity,
        int acceleration)
{
    if (src_size > 0x7fffffff || dst_capacity > 0x7fffffff) {
        return -1;
    }
    return LZ4_compress_fast(
            (const char*)src,
            (char*)dst,
            (int)src_size,
            (int)dst_capacity,
            acceleration);
}

// `LZ4_compress_HC`, or -1 when a size does not fit an `int`.
int mzl_lz4_compress_hc(
        const void* src,
        size_t src_size,
        void* dst,
        size_t dst_capacity,
        int level)
{
    if (src_size > 0x7fffffff || dst_capacity > 0x7fffffff) {
        return -1;
    }
    return LZ4_compress_HC(
            (const char*)src, (char*)dst, (int)src_size, (int)dst_capacity, level);
}

// `LZ4_compressBound`.
int mzl_lz4_compress_bound(int size)
{
    return LZ4_compressBound(size);
}

// `ZS_bitpackEncode` of `nb_elts` numbers of `elt_width` bytes, `nb_bits`
// each, into `dst`, which holds `ZS_bitpackEncodeBound` bytes: the bytes
// written. `src` is aligned for `elt_width`; the caller checks the width
// (1, 2, 4 or 8) and that `nb_bits` is at most `8 * elt_width`.
size_t mzl_bitpack_encode(
        void* dst,
        size_t dst_capacity,
        const void* src,
        size_t nb_elts,
        size_t elt_width,
        int nb_bits)
{
    return ZS_bitpackEncode(dst, dst_capacity, src, nb_elts, elt_width, nb_bits);
}
