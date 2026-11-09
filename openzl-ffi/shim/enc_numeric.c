// Oracle for the partition_bitpack graph's partitioning search.
//
// `PB_fixedPartition` is `static` in encode_partition_bitpack.c, so this file
// includes that source with the entry point renamed (the archive already holds
// the real one) and the graph scratch allocator replaced by a heap arena that
// is released at the start of the next call.

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

#include "openzl/zl_graph_api.h"

static void* mzl_scratch[64];
static size_t mzl_scratch_count;

static void* mzl_scratch_alloc(size_t size)
{
    if (mzl_scratch_count == 64) {
        return NULL;
    }
    void* ptr = malloc(size == 0 ? 1 : size);
    if (ptr != NULL) {
        mzl_scratch[mzl_scratch_count++] = ptr;
    }
    return ptr;
}

#define ZL_Graph_getScratchSpace(graph, size) mzl_scratch_alloc(size)
#define EI_partitionBitpackDynGraph mzl_unused_EI_partitionBitpackDynGraph
#include "openzl/codecs/partition/encode_partition_bitpack.c"
#undef EI_partitionBitpackDynGraph
#undef ZL_Graph_getScratchSpace

// Runs `PB_fixedPartition` and returns the number of partitions (0 when it
// found none). `starts` takes up to 256 entries.
size_t mzl_pb_fixed_partition(
        const void* data,
        size_t numElts,
        size_t eltWidth,
        int optimal,
        uint64_t* starts,
        uint64_t* bitCost,
        uint64_t* maxSymbolValue,
        size_t* bitpackBits,
        size_t* bitpackCost)
{
    while (mzl_scratch_count > 0) {
        free(mzl_scratch[--mzl_scratch_count]);
    }
    PB_PartitionResult result =
            PB_fixedPartition(data, numElts, eltWidth, optimal != 0, NULL);
    *bitpackBits = result.bitpackBits;
    *bitpackCost = result.bitpackCost;
    *bitCost = result.bitCost;
    *maxSymbolValue = result.maxSymbolValue;
    if (result.partitions == NULL) {
        return 0;
    }
    for (size_t i = 0; i < result.numPartitions; ++i) {
        starts[i] = result.partitions[i];
    }
    return result.numPartitions;
}
