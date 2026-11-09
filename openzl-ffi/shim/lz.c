// Test helper of the LZ family: the C LZ4 block decoder behind the `lz4`
// codec, so Rust tests can compare it with their port on arbitrary blocks.

#include <stddef.h>

#include "lz4.h"

// `LZ4_decompress_safe`, or -1 when a size does not fit an `int`.
long long mzl_lz4_decompress_safe(
        const void* src,
        size_t src_size,
        void* dst,
        size_t dst_capacity)
{
    if (src_size > 0x7fffffff || dst_capacity > 0x7fffffff) {
        return -1;
    }
    return LZ4_decompress_safe(
            (const char*)src, (char*)dst, (int)src_size, (int)dst_capacity);
}
