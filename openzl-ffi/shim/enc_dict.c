// Test oracle of dictionary bundles (dict/*, cgraph.c, the fat-bundle dict
// loader): the wire-format packers, SHA-256, a trained-dictionary builder,
// compression through graphs whose nodes carry dictionary IDs, and
// decompression with a fat-bundle dict loader.
//
// A graph is a flat program of 32-bit words, read depth-first:
//
//   0, graph_id                                  a graph by ID
//   1, node_id, PARAMS, has_dict, [8 words], n, GRAPH * n
//                                                node `node_id`, parameterized
//                                                by PARAMS and, when has_dict
//                                                is 1, by the 32-byte
//                                                dictionary ID in the 8 words
//                                                (little-endian), whose n
//                                                outputs go to the n graphs
//
// PARAMS is `ni, (id, value) * ni`. Each function returns a `size_t` and
// writes the OpenZL error code, 0 on success, to `*error`; a malformed
// program is error -1.

#define ZSTD_STATIC_LINKING_ONLY
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <zdict.h>

#include "openzl/codecs/zstd/common_zstd.h"
#include "openzl/dict/bundle.h"
#include "openzl/dict/dict.h"
#include "openzl/zl_compress.h"
#include "openzl/zl_compressor.h"
#include "openzl/zl_decompress.h"
#include "openzl/zl_dict.h"
#include "openzl/zl_dictloader.h"
#include "openzl/zl_errors.h"
#include "openzl/zl_unique_id.h"

#define MZL_MAX_PARAMS 64
#define MZL_MAX_SUCCESSORS 64
#define MZL_MAX_DEPTH 32

void mzl_sha256(const void* data, size_t size, uint8_t* out)
{
    ZL_UniqueID const id = ZL_UniqueID_computeSHA256(data, size);
    memcpy(out, id.bytes, 32);
}

/* Dict_pack: `id` is NULL for "derive from the content". Returns the packed
 * size, or 0 when `dst` is too small. */
size_t mzl_dict_pack(
        const uint8_t* id,
        uint32_t codec,
        int custom,
        const void* content,
        size_t content_size,
        void* dst,
        size_t dst_capacity)
{
    ZL_DictID did = ZL_DICT_ID_NULL;
    if (id != NULL)
        memcpy(did.id.bytes, id, 32);
    ZL_Report const r = Dict_pack(
            dst, dst_capacity, did, codec, custom != 0, content, content_size);
    return ZL_isError(r) ? 0 : ZL_validResult(r);
}

/* ZL_DictBundle_packFatBundle over `n` packed dictionaries. */
size_t mzl_fat_pack(
        const void* const* dicts,
        const size_t* sizes,
        size_t n,
        void* dst,
        size_t dst_capacity)
{
    ZL_Report const r = ZL_DictBundle_packFatBundle(
            dst, dst_capacity, (const void**)dicts, (size_t*)sizes, n);
    return ZL_isError(r) ? 0 : ZL_validResult(r);
}

size_t mzl_zstd_content_pack(
        int level,
        const void* raw,
        size_t raw_size,
        void* dst,
        size_t dst_capacity)
{
    return ZL_TrainedZstdContent_pack(dst, dst_capacity, level, raw, raw_size);
}

/* ZDICT_trainFromBuffer; returns the dictionary size, or 0 on failure. */
size_t mzl_zdict_train(
        void* dict,
        size_t dict_capacity,
        const void* samples,
        const size_t* sizes,
        unsigned n)
{
    size_t const r = ZDICT_trainFromBuffer(dict, dict_capacity, samples, sizes, n);
    return ZDICT_isError(r) ? 0 : r;
}

typedef struct {
    const uint32_t* pos;
    const uint32_t* end;
    int bad;
} mzl_program;

static uint32_t mzl_next(mzl_program* p)
{
    if (p->pos >= p->end) {
        p->bad = 1;
        return 0;
    }
    return *p->pos++;
}

static ZL_GraphID mzl_build(ZL_Compressor* c, mzl_program* p, int depth)
{
    ZL_GraphID const illegal = ZL_GRAPH_ILLEGAL;
    if (depth > MZL_MAX_DEPTH || p->bad) {
        p->bad = 1;
        return illegal;
    }
    uint32_t const kind = mzl_next(p);
    if (kind == 0) {
        return (ZL_GraphID){ mzl_next(p) };
    }
    if (kind != 1) {
        p->bad = 1;
        return illegal;
    }
    ZL_NodeID node = { mzl_next(p) };
    ZL_IntParam ints[MZL_MAX_PARAMS];
    uint32_t const ni = mzl_next(p);
    if (ni > MZL_MAX_PARAMS) {
        p->bad = 1;
        return illegal;
    }
    for (uint32_t i = 0; i < ni; i++) {
        ints[i].paramId    = (int)mzl_next(p);
        ints[i].paramValue = (int)mzl_next(p);
    }
    ZL_LocalParams local = {
        .intParams  = { ints, ni },
        .copyParams = { NULL, 0 },
        .refParams  = { NULL, 0 },
    };
    ZL_DictID dict   = ZL_DICT_ID_NULL;
    uint32_t hasDict = mzl_next(p);
    if (hasDict) {
        for (int i = 0; i < 8; i++) {
            uint32_t const w = mzl_next(p);
            memcpy(dict.id.bytes + 4 * i, &w, 4);
        }
    }
    uint32_t const n = mzl_next(p);
    if (n > MZL_MAX_SUCCESSORS) {
        p->bad = 1;
        return illegal;
    }
    ZL_GraphID successors[MZL_MAX_SUCCESSORS];
    for (uint32_t i = 0; i < n && !p->bad; i++) {
        successors[i] = mzl_build(c, p, depth + 1);
    }
    if (p->bad)
        return illegal;
    if (ni != 0 || hasDict) {
        ZL_NodeParameters const np = {
            .localParams = ni != 0 ? &local : NULL,
            .dictID      = dict,
        };
        ZL_RESULT_OF(ZL_NodeID)
        const r = ZL_Compressor_parameterizeNode(c, node, &np);
        if (ZL_RES_isError(r)) {
            p->bad = 1;
            return illegal;
        }
        node = ZL_RES_value(r);
    }
    return ZL_Compressor_registerStaticGraph_fromNode(c, node, successors, n);
}

/* Compresses one serial input through `program`, loading the fat bundle
 * `bundle` (NULL for none) after the nodes are parameterized and before the
 * starting graph is selected, as the reference's flow requires. */
size_t mzl_compress_dict(
        void* dst,
        size_t dst_capacity,
        const void* src,
        size_t src_size,
        const void* bundle,
        size_t bundle_size,
        const uint32_t* program,
        size_t program_len,
        const int* cparams,
        size_t n_cparams,
        int* error)
{
    *error = 1;
    ZL_CCtx* const cctx             = ZL_CCtx_create();
    ZL_Compressor* const compressor = ZL_Compressor_create();
    ZL_TypedRef* ref                = NULL;
    size_t result                   = 0;
    if (cctx == NULL || compressor == NULL)
        goto end;
    ZL_Report r = ZL_CCtx_setParameter(cctx, ZL_CParam_stickyParameters, 1);
    for (size_t i = 0; i < n_cparams && !ZL_isError(r); i++) {
        r = ZL_CCtx_setParameter(
                cctx, (ZL_CParam)cparams[2 * i], cparams[2 * i + 1]);
    }
    if (ZL_isError(r)) {
        *error = (int)ZL_errorCode(r);
        goto end;
    }
    mzl_program p = { program, program + program_len, 0 };
    ZL_GraphID const graph = mzl_build(compressor, &p, 0);
    if (p.bad || p.pos != p.end || !ZL_GraphID_isValid(graph)) {
        *error = -1;
        goto end;
    }
    if (bundle != NULL) {
        r = ZL_Compressor_loadDictBundle(compressor, bundle, bundle_size);
        if (ZL_isError(r)) {
            *error = (int)ZL_errorCode(r);
            goto end;
        }
    }
    r = ZL_Compressor_selectStartingGraphID(compressor, graph);
    if (!ZL_isError(r))
        r = ZL_CCtx_refCompressor(cctx, compressor);
    if (ZL_isError(r)) {
        *error = (int)ZL_errorCode(r);
        goto end;
    }
    ref = ZL_TypedRef_createSerial(src, src_size);
    if (ref == NULL)
        goto end;
    {
        ZL_Report const c = ZL_CCtx_compressMultiTypedRef(
                cctx, dst, dst_capacity, (const ZL_TypedRef**)&ref, 1);
        if (ZL_isError(c)) {
            *error = (int)ZL_errorCode(c);
        } else {
            *error = 0;
            result = ZL_validResult(c);
        }
    }
end:
    ZL_TypedRef_free(ref);
    ZL_Compressor_free(compressor);
    ZL_CCtx_free(cctx);
    return result;
}

/* The same layout as `mzl_decoded` in graph.c, whose accessors read it. */
typedef struct {
    size_t count;
    ZL_TypedBuffer* outputs[];
} mzl_decoded;

/* Decodes every typed output of the frame at `src` with a fat-bundle dict
 * loader holding the `n_bundles` bundles; the handle is read with the
 * accessors of graph.c. A bundle the loader rejects is the error. */
mzl_decoded* mzl_decode_typed_dict(
        const void* src,
        size_t src_size,
        const void* const* bundles,
        const size_t* bundle_sizes,
        size_t n_bundles,
        int* error)
{
    *error = 1;
    mzl_decoded* decoded                 = NULL;
    ZL_DCtx* dctx                        = NULL;
    ZL_FatBundleDictLoader* const loader = ZL_FatBundleDictLoader_create();
    if (loader == NULL)
        return NULL;
    for (size_t i = 0; i < n_bundles; i++) {
        ZL_Report const l = ZL_FatBundleDictLoader_loadFatBundle(
                loader, bundles[i], bundle_sizes[i]);
        if (ZL_isError(l)) {
            *error = (int)ZL_errorCode(l);
            goto end;
        }
    }
    ZL_FrameInfo* const info = ZL_FrameInfo_create(src, src_size);
    if (info == NULL)
        goto end;
    ZL_Report const n = ZL_FrameInfo_getNumOutputs(info);
    ZL_FrameInfo_free(info);
    if (ZL_isError(n)) {
        *error = (int)ZL_errorCode(n);
        goto end;
    }
    size_t const count = ZL_validResult(n);
    decoded = calloc(1, sizeof(mzl_decoded) + count * sizeof(ZL_TypedBuffer*));
    dctx    = ZL_DCtx_create();
    if (decoded == NULL || dctx == NULL) {
        free(decoded);
        decoded = NULL;
        goto end;
    }
    decoded->count = count;
    for (size_t i = 0; i < count; i++) {
        decoded->outputs[i] = ZL_TypedBuffer_create();
    }
    ZL_DCtx_refDictLoader(dctx, ZL_FatBundleDictLoader_getDictLoader(loader));
    ZL_Report const r = ZL_DCtx_decompressMultiTBuffer(
            dctx, decoded->outputs, count, src, src_size);
    if (ZL_isError(r)) {
        *error = (int)ZL_errorCode(r);
        for (size_t i = 0; i < count; i++)
            ZL_TypedBuffer_free(decoded->outputs[i]);
        free(decoded);
        decoded = NULL;
        goto end;
    }
    *error = 0;
end:
    ZL_DCtx_free(dctx);
    ZL_FatBundleDictLoader_free(loader);
    return decoded;
}
