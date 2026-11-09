// Test helpers of the encoder "backends" package: the C LZ4 block
// compressors OpenZL's `lz4` node calls, so Rust tests can compare their
// output with the safe-Rust port byte for byte.

#include <stddef.h>

#include "lz4.h"
#include "lz4hc.h"

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
