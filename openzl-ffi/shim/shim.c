// Flat C entry points over OpenZL's public API for the Rust test oracle.
//
// `ZL_Report` is a by-value struct with inline accessors, which Rust cannot
// call; each function here returns a `size_t` result and writes the OpenZL
// error code (0 on success) to `*error`.

#include <stddef.h>
#include <stdint.h>

#include "openzl/codecs/zl_segmenters.h"
#include "openzl/zl_compress.h"
#include "openzl/zl_compressor.h"
#include "openzl/zl_decompress.h"
#include "openzl/zl_errors.h"

static size_t mzl_unwrap(ZL_Report report, int* error)
{
    if (ZL_isError(report)) {
        *error = (int)ZL_errorCode(report);
        return 0;
    }
    *error = 0;
    return ZL_validResult(report);
}

size_t mzl_compress_bound(size_t src_size)
{
    return ZL_compressBound(src_size);
}

/* Compresses `src` the way `ZL_CCtx_compress` does with the given global
 * parameters. A `chunk_size` of 0 uses the default graph (one chunk);
 * otherwise the serial segmenter cuts chunks of `chunk_size` bytes, as the
 * `serial` profile of the CLI does. Parameters equal to 0 keep their default. */
size_t mzl_compress(
        void* dst,
        size_t dst_capacity,
        const void* src,
        size_t src_size,
        int level,
        int format_version,
        int content_checksum,
        int compressed_checksum,
        size_t chunk_size,
        int* error)
{
    ZL_CCtx* const cctx = ZL_CCtx_create();
    ZL_Compressor* compressor = NULL;
    size_t result = 0;
    *error = 1;
    if (cctx == NULL)
        return 0;
    ZL_Report r = ZL_CCtx_setParameter(cctx, ZL_CParam_stickyParameters, 1);
    if (!ZL_isError(r) && level != 0)
        r = ZL_CCtx_setParameter(cctx, ZL_CParam_compressionLevel, level);
    if (!ZL_isError(r))
        r = ZL_CCtx_setParameter(cctx, ZL_CParam_formatVersion, format_version);
    if (!ZL_isError(r) && content_checksum != 0)
        r = ZL_CCtx_setParameter(cctx, ZL_CParam_contentChecksum, content_checksum);
    if (!ZL_isError(r) && compressed_checksum != 0)
        r = ZL_CCtx_setParameter(
                cctx, ZL_CParam_compressedChecksum, compressed_checksum);
    if (!ZL_isError(r) && chunk_size != 0) {
        compressor = ZL_Compressor_create();
        if (compressor == NULL) {
            ZL_CCtx_free(cctx);
            return 0;
        }
        ZL_GraphID const graph = ZL_Compressor_buildSerialSegmenter(
                compressor, chunk_size, ZL_GRAPH_ILLEGAL);
        r = ZL_Compressor_selectStartingGraphID(compressor, graph);
        if (!ZL_isError(r))
            r = ZL_CCtx_refCompressor(cctx, compressor);
    }
    if (ZL_isError(r)) {
        *error = (int)ZL_errorCode(r);
    } else {
        result = mzl_unwrap(
                ZL_CCtx_compress(cctx, dst, dst_capacity, src, src_size),
                error);
    }
    ZL_CCtx_free(cctx);
    ZL_Compressor_free(compressor);
    return result;
}

size_t mzl_decompressed_size(const void* src, size_t src_size, int* error)
{
    return mzl_unwrap(ZL_getDecompressedSize(src, src_size), error);
}

size_t mzl_decompress(
        void* dst,
        size_t dst_capacity,
        const void* src,
        size_t src_size,
        int* error)
{
    return mzl_unwrap(ZL_decompress(dst, dst_capacity, src, src_size), error);
}

#define XXH_INLINE_ALL
#include "openzl/shared/xxhash.h"

uint64_t mzl_xxh3_64(const void* src, size_t src_size)
{
    return XXH3_64bits(src, src_size);
}
