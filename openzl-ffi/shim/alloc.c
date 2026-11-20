// Wraps vendor/openzl/src/openzl/common/allocation.c with the fuzzer
// allocation cap enabled for the allocator alone. Defining
// FUZZING_BUILD_MODE_UNSAFE_FOR_PRODUCTION globally would also skip frame
// checksums, accept trailing bytes and cap LZ match lengths at 1024.

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define FUZZING_BUILD_MODE_UNSAFE_FOR_PRODUCTION
#include "openzl/common/allocation.c"

/* 100 MiB per allocation, matching allocation.c's own fuzzer cap. */
#define MZL_MAX_ALLOC ((size_t)(100U << 20))

/* Total bytes one decompression call may allocate across all arenas. */
#define MZL_MAX_DECODE_TOTAL ((size_t)(512U << 20))

static _Thread_local size_t mzl_decode_single_limit = MZL_MAX_ALLOC;
static _Thread_local size_t mzl_decode_remaining    = SIZE_MAX;

void mzl_decode_enter(size_t dsize)
{
    /* mopenzl's per-stream budget is 16 * dsize + 1 MiB; PivCo-Huffman's C
     * scratch buffer is 2 * (block + slop), so 2 * budget + 2 MiB covers
     * every allocation a frame within mopenzl's budget can make. */
    size_t const max_dsize = (MZL_MAX_ALLOC - (4U << 20)) / 32;
    if (dsize >= max_dsize) {
        mzl_decode_single_limit = MZL_MAX_ALLOC;
    } else {
        mzl_decode_single_limit = dsize * 32 + (4U << 20);
    }
    mzl_decode_remaining = MZL_MAX_DECODE_TOTAL;
}

void mzl_decode_exit(void)
{
    mzl_decode_single_limit = MZL_MAX_ALLOC;
    mzl_decode_remaining    = SIZE_MAX;
}

bool ZS2_malloc_should_fail(size_t size)
{
    if (size > mzl_decode_single_limit || size > mzl_decode_remaining) {
        return true;
    }
    if (mzl_decode_remaining != SIZE_MAX) {
        mzl_decode_remaining -= size;
    }
    return false;
}
