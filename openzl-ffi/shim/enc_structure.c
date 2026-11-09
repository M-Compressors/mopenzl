// Oracle helpers for the structure encoders: graphs the generic builder of
// graph.c cannot express (they need custom nodes), and the estimators of
// shared/estimate.c.

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

#include "openzl/codecs/zl_split.h"
#include "openzl/codecs/zl_split_by_struct.h"
#include "openzl/codecs/zl_transpose.h"
#include "openzl/shared/estimate.h"
#include "openzl/zl_compress.h"
#include "openzl/zl_compressor.h"
#include "openzl/zl_errors.h"

/* Compresses one typed input through a split-family graph whose successors
 * are all `ZL_GRAPH_STORE`.
 *
 * mode 0: `ZL_Compressor_registerSplitGraph` with `n` segment sizes.
 * mode 1: `ZL_Compressor_registerSplitByStructGraph` with `n` field sizes.
 * mode 2: `ZL_Compressor_registerTransposeSplitGraph`.
 *
 * `type` is 0 serial, 1 struct, 2 numeric. */
size_t mzl_enc_split_graph(
        void* dst,
        size_t dst_capacity,
        uint32_t mode,
        uint32_t type,
        const size_t* sizes,
        size_t n,
        const void* src,
        size_t src_size,
        size_t width,
        const int* cparams,
        size_t n_cparams,
        int* error)
{
    *error = 1;
    ZL_CCtx* const cctx             = ZL_CCtx_create();
    ZL_Compressor* const compressor = ZL_Compressor_create();
    ZL_TypedRef* ref                = NULL;
    ZL_GraphID* successors          = NULL;
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
    successors = malloc((n + 1) * sizeof(ZL_GraphID));
    if (successors == NULL)
        goto end;
    for (size_t i = 0; i < n; i++)
        successors[i] = ZL_GRAPH_STORE;
    ZL_Type zl_type = type == 1 ? ZL_Type_struct
            : type == 2         ? ZL_Type_numeric
                                : ZL_Type_serial;
    ZL_GraphID graph;
    switch (mode) {
        case 0:
            graph = ZL_Compressor_registerSplitGraph(
                    compressor, zl_type, sizes, successors, n);
            break;
        case 1:
            graph = ZL_Compressor_registerSplitByStructGraph(
                    compressor, sizes, successors, n);
            break;
        default:
            graph = ZL_Compressor_registerTransposeSplitGraph(
                    compressor, ZL_GRAPH_STORE);
            break;
    }
    if (!ZL_GraphID_isValid(graph)) {
        *error = -1;
        goto end;
    }
    r = ZL_Compressor_selectStartingGraphID(compressor, graph);
    if (!ZL_isError(r))
        r = ZL_CCtx_refCompressor(cctx, compressor);
    if (ZL_isError(r)) {
        *error = (int)ZL_errorCode(r);
        goto end;
    }
    switch (type) {
        case 1:
            ref = ZL_TypedRef_createStruct(
                    src, width, width ? src_size / width : 0);
            break;
        case 2:
            ref = ZL_TypedRef_createNumeric(
                    src, width, width ? src_size / width : 0);
            break;
        default:
            ref = ZL_TypedRef_createSerial(src, src_size);
            break;
    }
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
    free(successors);
    ZL_TypedRef_free(ref);
    ZL_Compressor_free(compressor);
    ZL_CCtx_free(cctx);
    return result;
}

/* `ZL_estimateCardinality_fixed` into out[0..5]: lowerBound,
 * estimateLowerBound, estimate, estimateUpperBound, upperBound. */
void mzl_est_cardinality_fixed(
        const void* src,
        size_t nb_elts,
        size_t elt_size,
        uint64_t early_exit,
        uint64_t* out)
{
    ZL_CardinalityEstimate const e =
            ZL_estimateCardinality_fixed(src, nb_elts, elt_size, early_exit);
    out[0] = e.lowerBound;
    out[1] = e.estimateLowerBound;
    out[2] = e.estimate;
    out[3] = e.estimateUpperBound;
    out[4] = e.upperBound;
}

/* `ZL_estimateCardinality_variable`, the elements being consecutive slices
 * of `src` of the given sizes. */
void mzl_est_cardinality_variable(
        const uint8_t* src,
        const size_t* sizes,
        size_t nb_elts,
        uint64_t early_exit,
        uint64_t* out)
{
    const void** ptrs = malloc((nb_elts + 1) * sizeof(void*));
    size_t offset     = 0;
    for (size_t i = 0; i < nb_elts; i++) {
        ptrs[i] = src + offset;
        offset += sizes[i];
    }
    ZL_CardinalityEstimate const e = ZL_estimateCardinality_variable(
            (void const* const*)ptrs, sizes, nb_elts, early_exit);
    free(ptrs);
    out[0] = e.lowerBound;
    out[1] = e.estimateLowerBound;
    out[2] = e.estimate;
    out[3] = e.estimateUpperBound;
    out[4] = e.upperBound;
}

typedef struct {
    const uint64_t* hashes;
} mzl_hash_state;

static uint64_t mzl_hash_at(void* state, size_t index)
{
    return ((mzl_hash_state*)state)->hashes[index];
}

/* `ZL_estimateCardinality_hashed` with both hash callbacks reading
 * precomputed arrays. */
uint64_t mzl_est_cardinality_hashed(
        const uint64_t* linear_hashes,
        const uint64_t* hll_hashes,
        size_t nb_hashes,
        size_t upper_bound)
{
    /* The callbacks receive one state; use two calls' worth of arrays by
     * encoding the choice in the bound, as the library does. */
    mzl_hash_state state = { upper_bound <= 65536 ? linear_hashes : hll_hashes };
    return ZL_estimateCardinality_hashed(
            &state, nb_hashes, upper_bound, mzl_hash_at, mzl_hash_at);
}

/* `ZL_estimateDimensionality` into out[0..4]: status, stride, strideMatches,
 * totalMatches. */
void mzl_est_dimensionality(
        const void* src,
        size_t nb_elts,
        size_t elt_size,
        size_t* out)
{
    ZL_DimensionalityEstimate const e =
            ZL_estimateDimensionality(src, nb_elts, elt_size);
    out[0] = (size_t)e.dimensionality;
    out[1] = e.stride;
    out[2] = e.strideMatches;
    out[3] = e.totalMatches;
}

size_t mzl_est_float_width(const void* src, size_t size)
{
    return ZL_guessFloatWidth(src, size);
}

void mzl_est_range(const void* src, size_t nb_elts, size_t elt_size, uint64_t* out)
{
    ZL_ElementRange const r = ZL_computeUnsignedRange(src, nb_elts, elt_size);
    out[0] = r.min;
    out[1] = r.max;
}
