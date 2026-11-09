// Node and graph IDs of the numeric-family codecs, for tests that build
// graphs through `shim/graph.c`. The IDs come from the headers' macros so
// they cannot drift from the vendored library.

#include <stddef.h>
#include <stdint.h>

#include "openzl/compress/private_nodes.h"
#include "openzl/zl_graphs.h"
#include "openzl/zl_public_nodes.h"

// Order must match `openzl_ffi::numeric::Id`. Returns 0 (illegal) past the end.
uint32_t mzl_numeric_id(uint32_t which)
{
    switch (which) {
        case 0:
            return ZL_NODE_DELTA_INT.nid;
        case 1:
            return ZL_NODE_ZIGZAG.nid;
        case 2:
            return ZL_NODE_DIVIDE_BY.nid;
        case 3:
            return ZL_NODE_QUANTIZE_OFFSETS.nid;
        case 4:
            return ZL_NODE_QUANTIZE_LENGTHS.nid;
        case 5:
            return ZL_NODE_RANGE_PACK.nid;
        case 6:
            return ZL_NODE_BITSPLIT.nid;
        case 7:
            return ZL_NODE_BITSPLIT_TOP8.nid;
        case 8:
            return ZL_NODE_BITSPLIT_FP.nid;
        case 9:
            return ZL_NODE_BITSPLIT_BF16.nid;
        case 10:
            return ZL_NODE_FLOAT32_DECONSTRUCT.nid;
        case 11:
            return ZL_NODE_BFLOAT16_DECONSTRUCT.nid;
        case 12:
            return ZL_NODE_FLOAT16_DECONSTRUCT.nid;
        case 13:
            return ZL_NODE_SPARSE_NUM.nid;
        case 14:
            return ZL_NODE_SPARSE_NUM_AUTO.nid;
        case 15:
            return ZL_NODE_MERGE_SORTED.nid;
        case 16:
            return ZL_NODE_CONSTANT_SERIAL.nid;
        case 17:
            return ZL_NODE_CONSTANT_FIXED.nid;
        case 18:
            return ZL_NODE_PREFIX.nid;
        default:
            return 0;
    }
}
