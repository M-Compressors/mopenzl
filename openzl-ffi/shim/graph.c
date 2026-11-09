// Arbitrary compression graphs, typed and multiple inputs, and typed
// outputs for the Rust test oracle.
//
// A graph is described by a flat program of 32-bit words, read
// depth-first:
//
//   0, graph_id                         a graph by ID (standard graphs)
//   1, node_id, PARAMS, n, GRAPH * n    node `node_id`, parameterized by
//                                       PARAMS when it has any, whose n
//                                       outputs go to the n graphs
//   2, graph_id, PARAMS, n, GRAPH * n   graph `graph_id` parameterized by
//                                       PARAMS, with n custom graphs
//   3, n, NODE * n, GRAPH               a pipeline of n plain node IDs
//                                       ending in GRAPH
//
// PARAMS is `ni, (id, value) * ni, nc, (id, len, ceil(len / 4) words) * nc`:
// integer parameters, then copy parameters whose bytes are packed
// little-endian into words.
//
// Each function returns a `size_t` and writes the OpenZL error code, 0 on
// success, to `*error`; a malformed program is reported as error -1.

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "openzl/zl_compress.h"
#include "openzl/zl_compressor.h"
#include "openzl/zl_decompress.h"
#include "openzl/zl_errors.h"

#define MZL_MAX_PARAMS 64
#define MZL_MAX_SUCCESSORS 256
#define MZL_MAX_DEPTH 64

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

typedef struct {
    ZL_IntParam ints[MZL_MAX_PARAMS];
    ZL_CopyParam copies[MZL_MAX_PARAMS];
    ZL_LocalParams local;
    int any;
} mzl_params;

static void mzl_read_params(mzl_program* p, mzl_params* out)
{
    uint32_t const ni = mzl_next(p);
    if (ni > MZL_MAX_PARAMS) {
        p->bad = 1;
        return;
    }
    for (uint32_t i = 0; i < ni; i++) {
        out->ints[i].paramId    = (int)mzl_next(p);
        out->ints[i].paramValue = (int)mzl_next(p);
    }
    uint32_t const nc = mzl_next(p);
    if (nc > MZL_MAX_PARAMS) {
        p->bad = 1;
        return;
    }
    for (uint32_t i = 0; i < nc && !p->bad; i++) {
        out->copies[i].paramId   = (int)mzl_next(p);
        size_t const len         = mzl_next(p);
        size_t const words       = (len + 3) / 4;
        out->copies[i].paramPtr  = p->pos;
        out->copies[i].paramSize = len;
        if ((size_t)(p->end - p->pos) < words) {
            p->bad = 1;
            return;
        }
        p->pos += words;
    }
    out->local = (ZL_LocalParams){
        .intParams  = { out->ints, ni },
        .copyParams = { out->copies, nc },
        .refParams  = { NULL, 0 },
    };
    out->any = ni != 0 || nc != 0;
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
    if (kind == 3) {
        uint32_t const n = mzl_next(p);
        if (n > MZL_MAX_SUCCESSORS) {
            p->bad = 1;
            return illegal;
        }
        ZL_NodeID nodes[MZL_MAX_SUCCESSORS];
        for (uint32_t i = 0; i < n; i++) {
            nodes[i] = (ZL_NodeID){ mzl_next(p) };
        }
        ZL_GraphID const last = mzl_build(c, p, depth + 1);
        if (p->bad)
            return illegal;
        return ZL_Compressor_registerStaticGraph_fromPipelineNodes1o(
                c, nodes, n, last);
    }
    if (kind != 1 && kind != 2) {
        p->bad = 1;
        return illegal;
    }
    uint32_t const id = mzl_next(p);
    mzl_params* const params = malloc(sizeof(mzl_params));
    if (params == NULL) {
        p->bad = 1;
        return illegal;
    }
    mzl_read_params(p, params);
    uint32_t const n = mzl_next(p);
    if (n > MZL_MAX_SUCCESSORS) {
        p->bad = 1;
    }
    ZL_GraphID successors[MZL_MAX_SUCCESSORS];
    for (uint32_t i = 0; i < n && !p->bad; i++) {
        successors[i] = mzl_build(c, p, depth + 1);
    }
    ZL_GraphID result = illegal;
    if (!p->bad && kind == 1) {
        ZL_NodeID node = { id };
        if (params->any) {
            ZL_NodeParameters const np = { .localParams = &params->local };
            ZL_RESULT_OF(ZL_NodeID)
            const r = ZL_Compressor_parameterizeNode(c, node, &np);
            if (ZL_RES_isError(r)) {
                p->bad = 1;
            } else {
                node = ZL_RES_value(r);
            }
        }
        if (!p->bad)
            result = ZL_Compressor_registerStaticGraph_fromNode(
                    c, node, successors, n);
    } else if (!p->bad) {
        ZL_GraphParameters const gp = {
            .customGraphs   = n ? successors : NULL,
            .nbCustomGraphs = n,
            .localParams    = params->any ? &params->local : NULL,
        };
        ZL_RESULT_OF(ZL_GraphID)
        const r = ZL_Compressor_parameterizeGraph(c, (ZL_GraphID){ id }, &gp);
        if (ZL_RES_isError(r)) {
            p->bad = 1;
        } else {
            result = ZL_RES_value(r);
        }
    }
    free(params);
    return result;
}

static ZL_Type mzl_type(uint32_t code)
{
    switch (code) {
        case 1:
            return ZL_Type_struct;
        case 2:
            return ZL_Type_numeric;
        case 3:
            return ZL_Type_string;
        default:
            return ZL_Type_serial;
    }
}

static uint32_t mzl_code(ZL_Type type)
{
    switch (type) {
        case ZL_Type_struct:
            return 1;
        case ZL_Type_numeric:
            return 2;
        case ZL_Type_string:
            return 3;
        default:
            return 0;
    }
}

/* Compresses `n_inputs` typed inputs through the graph `program` describes.
 * Input `i` has type code `types[i]` (0 serial, 1 struct, 2 numeric,
 * 3 string), `sizes[i]` bytes at `srcs[i]`, element width `widths[i]`
 * (struct and numeric) and, for strings, `n_lens[i]` lengths at `lens[i]`.
 * `cparams` holds `n_cparams` pairs of `ZL_CParam` and value. */
size_t mzl_compress_graph_comment(
        const void* comment,
        size_t comment_len,
        void* dst,
        size_t dst_capacity,
        size_t n_inputs,
        const uint32_t* types,
        const void* const* srcs,
        const size_t* sizes,
        const size_t* widths,
        const uint32_t* const* lens,
        const size_t* n_lens,
        const uint32_t* program,
        size_t program_len,
        const int* cparams,
        size_t n_cparams,
        int* error)
{
    *error = 1;
    if (n_inputs == 0 || n_inputs > 64)
        return 0;
    ZL_CCtx* const cctx             = ZL_CCtx_create();
    ZL_Compressor* const compressor = ZL_Compressor_create();
    ZL_TypedRef* refs[64]           = { 0 };
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
    if (comment_len != 0) {
        r = ZL_CCtx_addHeaderComment(cctx, comment, comment_len);
        if (ZL_isError(r)) {
            *error = (int)ZL_errorCode(r);
            goto end;
        }
    }
    mzl_program p = { program, program + program_len, 0 };
    ZL_GraphID const graph = mzl_build(compressor, &p, 0);
    if (p.bad || p.pos != p.end || !ZL_GraphID_isValid(graph)) {
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
    for (size_t i = 0; i < n_inputs; i++) {
        switch (mzl_type(types[i])) {
            case ZL_Type_struct:
                refs[i] = ZL_TypedRef_createStruct(
                        srcs[i], widths[i], widths[i] ? sizes[i] / widths[i] : 0);
                break;
            case ZL_Type_numeric:
                refs[i] = ZL_TypedRef_createNumeric(
                        srcs[i], widths[i], widths[i] ? sizes[i] / widths[i] : 0);
                break;
            case ZL_Type_string:
                refs[i] = ZL_TypedRef_createString(
                        srcs[i], sizes[i], lens[i], n_lens[i]);
                break;
            default:
                refs[i] = ZL_TypedRef_createSerial(srcs[i], sizes[i]);
                break;
        }
        if (refs[i] == NULL)
            goto end;
    }
    result = 0;
    {
        ZL_Report const c = ZL_CCtx_compressMultiTypedRef(
                cctx,
                dst,
                dst_capacity,
                (const ZL_TypedRef**)refs,
                n_inputs);
        if (ZL_isError(c)) {
            *error = (int)ZL_errorCode(c);
        } else {
            *error = 0;
            result = ZL_validResult(c);
        }
    }
end:
    for (size_t i = 0; i < n_inputs; i++)
        ZL_TypedRef_free(refs[i]);
    ZL_Compressor_free(compressor);
    ZL_CCtx_free(cctx);
    return result;
}

size_t mzl_compress_graph(
        void* dst,
        size_t dst_capacity,
        size_t n_inputs,
        const uint32_t* types,
        const void* const* srcs,
        const size_t* sizes,
        const size_t* widths,
        const uint32_t* const* lens,
        const size_t* n_lens,
        const uint32_t* program,
        size_t program_len,
        const int* cparams,
        size_t n_cparams,
        int* error)
{
    return mzl_compress_graph_comment(
            NULL,
            0,
            dst,
            dst_capacity,
            n_inputs,
            types,
            srcs,
            sizes,
            widths,
            lens,
            n_lens,
            program,
            program_len,
            cparams,
            n_cparams,
            error);
}

typedef struct {
    size_t count;
    ZL_TypedBuffer* outputs[];
} mzl_decoded;

/* Decodes every typed output of the frame at `src`; the handle is read
 * with the accessors below and released with `mzl_decoded_free`. */
mzl_decoded* mzl_decode_typed(const void* src, size_t src_size, int* error)
{
    *error = 1;
    ZL_FrameInfo* const info = ZL_FrameInfo_create(src, src_size);
    if (info == NULL)
        return NULL;
    ZL_Report const n = ZL_FrameInfo_getNumOutputs(info);
    ZL_FrameInfo_free(info);
    if (ZL_isError(n)) {
        *error = (int)ZL_errorCode(n);
        return NULL;
    }
    size_t const count = ZL_validResult(n);
    mzl_decoded* const decoded =
            calloc(1, sizeof(mzl_decoded) + count * sizeof(ZL_TypedBuffer*));
    ZL_DCtx* const dctx = ZL_DCtx_create();
    if (decoded == NULL || dctx == NULL) {
        free(decoded);
        ZL_DCtx_free(dctx);
        return NULL;
    }
    decoded->count = count;
    for (size_t i = 0; i < count; i++) {
        decoded->outputs[i] = ZL_TypedBuffer_create();
    }
    ZL_Report const r = ZL_DCtx_decompressMultiTBuffer(
            dctx, decoded->outputs, count, src, src_size);
    ZL_DCtx_free(dctx);
    if (ZL_isError(r)) {
        *error = (int)ZL_errorCode(r);
        for (size_t i = 0; i < count; i++)
            ZL_TypedBuffer_free(decoded->outputs[i]);
        free(decoded);
        return NULL;
    }
    *error = 0;
    return decoded;
}

size_t mzl_decoded_count(const mzl_decoded* d)
{
    return d->count;
}

uint32_t mzl_decoded_type(const mzl_decoded* d, size_t i)
{
    return mzl_code(ZL_TypedBuffer_type(d->outputs[i]));
}

size_t mzl_decoded_width(const mzl_decoded* d, size_t i)
{
    return ZL_TypedBuffer_eltWidth(d->outputs[i]);
}

size_t mzl_decoded_bytes(const mzl_decoded* d, size_t i)
{
    return ZL_TypedBuffer_byteSize(d->outputs[i]);
}

const void* mzl_decoded_ptr(const mzl_decoded* d, size_t i)
{
    return ZL_TypedBuffer_rPtr(d->outputs[i]);
}

size_t mzl_decoded_elts(const mzl_decoded* d, size_t i)
{
    return ZL_TypedBuffer_numElts(d->outputs[i]);
}

const uint32_t* mzl_decoded_lens(const mzl_decoded* d, size_t i)
{
    return ZL_TypedBuffer_rStringLens(d->outputs[i]);
}

void mzl_decoded_free(mzl_decoded* d)
{
    if (d == NULL)
        return;
    for (size_t i = 0; i < d->count; i++)
        ZL_TypedBuffer_free(d->outputs[i]);
    free(d);
}
