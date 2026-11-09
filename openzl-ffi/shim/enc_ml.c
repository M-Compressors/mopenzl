// Oracle helpers for the ML selector, clustering and CBOR ports:
//
// * the A1CBOR decoder and encoder, outside any graph;
// * the integer feature generator and the GBT predictor of the ML selector;
// * a graph builder (like graph.c's) that also supports MParam blobs, custom
//   nodes, the typed clustering registration and typed inputs carrying
//   integer metadata.
//
// The graph program is read as graph.c's, with two more kinds:
//
//   4, graph_id, PARAMS, n, GRAPH * n, nn, NODE * nn, len, words...
//       `ZL_Compressor_registerParameterizedGraph` of `graph_id` with
//       PARAMS, n custom graphs, nn custom nodes and an MParam blob of `len`
//       bytes packed little-endian into words (len 0: none)
//   5, nc, CLUSTER * nc, nd, TYPEDEFAULT * nd, ns, GRAPH * ns, nn, NODE * nn
//       `ZL_Clustering_registerGraph[WithCustomClusteringCodecs]` where
//       CLUSTER is `type, eltWidth, successorIdx, codecIdx, nt, tag * nt` and
//       TYPEDEFAULT is `type, eltWidth, successorIdx, codecIdx`; nn == 0 uses
//       the four standard concat nodes.
//
// Kinds 0 (graph by ID), 1 (node with successors) and 2 (parameterized
// graph) behave as in graph.c.

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "openzl/codecs/zl_clustering.h"
#include "openzl/codecs/zl_concat.h"
#include "openzl/common/a1cbor_helpers.h"
#include "openzl/compress/selectors/ml/features.h"
#include "openzl/compress/selectors/ml/gbt.h"
#include "openzl/compress/selectors/ml/ml_selector_graph.h"
#include "openzl/shared/a1cbor.h"
#include "openzl/zl_compress.h"
#include "openzl/zl_compressor.h"
#include "openzl/zl_errors.h"
#include "openzl/zl_input.h"

/* ------------------------------------------------------------------ */
/* CBOR                                                               */
/* ------------------------------------------------------------------ */

static void* mzl_cbor_calloc(void* opaque, size_t bytes)
{
    (void)opaque;
    return calloc(1, bytes);
}

/* Decodes `data` with the given configuration and re-encodes it canonically
 * into `out` (capacity `out_cap`). Returns the encoded size, or 0 on
 * failure, with `err[0..3]` = type, srcPos, depth (type 0 on success). */
size_t mzl_cbor_roundtrip(
        const uint8_t* data,
        size_t size,
        size_t max_depth,
        size_t limit_bytes,
        int reference_source,
        int reject_unknown_simple,
        uint8_t* out,
        size_t out_cap,
        size_t* err)
{
    A1C_Arena arena = { mzl_cbor_calloc, NULL };
    A1C_DecoderConfig config = {
        .maxDepth            = max_depth,
        .limitBytes          = limit_bytes,
        .referenceSource     = reference_source != 0,
        .rejectUnknownSimple = reject_unknown_simple != 0,
    };
    A1C_Decoder decoder;
    A1C_Decoder_init(&decoder, arena, config);
    err[0] = err[1] = err[2] = 0;
    A1C_Item* root = A1C_Decoder_decode(&decoder, data, size);
    if (root == NULL) {
        A1C_Error e = A1C_Decoder_getError(&decoder);
        err[0]      = e.type;
        err[1]      = e.srcPos;
        err[2]      = e.depth;
        return 0;
    }
    A1C_Error e;
    size_t written = A1C_Item_encode(root, out, out_cap, &e);
    if (written == 0) {
        err[0] = 100 + e.type;
        err[1] = e.srcPos;
        err[2] = e.depth;
    }
    return written;
}

/* Decodes `data` permissively, then encodes the item into a buffer of
 * `cap` bytes. Returns the bytes written (0 on failure); `err` as above
 * with the encoder's error; `*size_out` is `A1C_Item_encodedSize`. */
size_t mzl_cbor_encode_capped(
        const uint8_t* data,
        size_t size,
        size_t cap,
        uint8_t* out,
        size_t* size_out,
        size_t* err)
{
    A1C_Arena arena = { mzl_cbor_calloc, NULL };
    A1C_DecoderConfig config = { 0 };
    A1C_Decoder decoder;
    A1C_Decoder_init(&decoder, arena, config);
    err[0] = err[1] = err[2] = 0;
    A1C_Item* root = A1C_Decoder_decode(&decoder, data, size);
    if (root == NULL) {
        err[0] = 200;
        return 0;
    }
    *size_out = A1C_Item_encodedSize(root);
    A1C_Error e;
    size_t written = A1C_Item_encode(root, out, cap, &e);
    if (written == 0) {
        err[0] = e.type;
        err[1] = e.srcPos;
        err[2] = e.depth;
    }
    return written;
}

/* Encodes the simple value `value` into `cap` bytes. */
size_t mzl_cbor_encode_simple(
        uint8_t value,
        size_t cap,
        uint8_t* out,
        size_t* err)
{
    A1C_Item item;
    memset(&item, 0, sizeof(item));
    item.type   = A1C_ItemType_simple;
    item.simple = value;
    A1C_Error e;
    err[0] = err[1] = err[2] = 0;
    size_t written = A1C_Item_encode(&item, out, cap, &e);
    if (written == 0) {
        err[0] = e.type;
        err[1] = e.srcPos;
        err[2] = e.depth;
    }
    return written;
}

/* ------------------------------------------------------------------ */
/* ML selector                                                        */
/* ------------------------------------------------------------------ */

/* `FeatureGen_integer` on a numeric input; writes 11 floats to `out`. */
int mzl_ml_features(const void* src, size_t n_elts, size_t width, float* out)
{
    ZL_TypedRef* ref = ZL_TypedRef_createNumeric(src, width, n_elts);
    if (ref == NULL)
        return -1;
    VECTOR(LabeledFeature) features = VECTOR_EMPTY(1024);
    ZL_Report r = FeatureGen_integer(ref, &features);
    int result  = -1;
    if (!ZL_isError(r) && VECTOR_SIZE(features) == 11) {
        for (size_t i = 0; i < 11; i++)
            out[i] = VECTOR_AT(features, i).value;
        result = 0;
    }
    VECTOR_DESTROY(features);
    ZL_TypedRef_free(ref);
    return result;
}

/* Deserializes the CBOR config and predicts the successor of the numeric
 * input. Returns the index, or -1 when the config is rejected or the
 * prediction fails. */
long long mzl_ml_predict(
        const uint8_t* config,
        size_t config_size,
        const void* src,
        size_t n_elts,
        size_t width)
{
    A1C_Arena arena = { mzl_cbor_calloc, NULL };
    ZL_RESULT_OF(ZL_MLSelectorConfig)
    r = MLSelector_deserializeMLSelectorConfig(
            NULL, (const char*)config, config_size, &arena);
    if (ZL_RES_isError(r))
        return -1;
    ZL_MLSelectorConfig cfg = ZL_RES_value(r);
    if (cfg.model != ZL_GBT || cfg.runtimeConfig == NULL)
        return -1;
    ZL_TypedRef* ref = ZL_TypedRef_createNumeric(src, width, n_elts);
    if (ref == NULL)
        return -1;
    ZL_RESULT_OF(size_t) p = GBTModel_predict(cfg.runtimeConfig, ref);
    ZL_TypedRef_free(ref);
    if (ZL_RES_isError(p))
        return -1;
    return (long long)ZL_RES_value(p);
}

/* ------------------------------------------------------------------ */
/* Graph programs                                                     */
/* ------------------------------------------------------------------ */

#define MZL_MAX_PARAMS 64
#define MZL_MAX_SUCCESSORS 64
#define MZL_MAX_DEPTH 32

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
    if (kind == 5) {
        uint32_t const nc = mzl_next(p);
        if (nc > MZL_MAX_PARAMS) {
            p->bad = 1;
            return illegal;
        }
        ZL_ClusteringConfig_Cluster clusters[MZL_MAX_PARAMS];
        int tags[MZL_MAX_PARAMS * 8];
        size_t nt_total = 0;
        for (uint32_t i = 0; i < nc && !p->bad; i++) {
            clusters[i].typeSuccessor.type = (ZL_Type)mzl_next(p);
            clusters[i].typeSuccessor.eltWidth = mzl_next(p);
            clusters[i].typeSuccessor.successorIdx = mzl_next(p);
            clusters[i].typeSuccessor.clusteringCodecIdx = mzl_next(p);
            uint32_t const nt = mzl_next(p);
            if (nt_total + nt > MZL_MAX_PARAMS * 8) {
                p->bad = 1;
                return illegal;
            }
            clusters[i].memberTags   = tags + nt_total;
            clusters[i].nbMemberTags = nt;
            for (uint32_t j = 0; j < nt; j++)
                tags[nt_total++] = (int)mzl_next(p);
        }
        uint32_t const nd = mzl_next(p);
        if (nd > MZL_MAX_PARAMS) {
            p->bad = 1;
            return illegal;
        }
        ZL_ClusteringConfig_TypeSuccessor defaults[MZL_MAX_PARAMS];
        for (uint32_t i = 0; i < nd && !p->bad; i++) {
            defaults[i].type               = (ZL_Type)mzl_next(p);
            defaults[i].eltWidth           = mzl_next(p);
            defaults[i].successorIdx       = mzl_next(p);
            defaults[i].clusteringCodecIdx = mzl_next(p);
        }
        uint32_t const ns = mzl_next(p);
        if (ns > MZL_MAX_SUCCESSORS) {
            p->bad = 1;
            return illegal;
        }
        ZL_GraphID successors[MZL_MAX_SUCCESSORS];
        for (uint32_t i = 0; i < ns && !p->bad; i++)
            successors[i] = mzl_build(c, p, depth + 1);
        uint32_t const nn = mzl_next(p);
        if (nn > MZL_MAX_SUCCESSORS) {
            p->bad = 1;
            return illegal;
        }
        ZL_NodeID nodes[MZL_MAX_SUCCESSORS];
        for (uint32_t i = 0; i < nn && !p->bad; i++)
            nodes[i] = (ZL_NodeID){ mzl_next(p) };
        if (p->bad)
            return illegal;
        ZL_ClusteringConfig config = {
            .clusters       = clusters,
            .nbClusters     = nc,
            .typeDefaults   = defaults,
            .nbTypeDefaults = nd,
        };
        if (nn == 0)
            return ZL_Clustering_registerGraph(c, &config, successors, ns);
        return ZL_Clustering_registerGraphWithCustomClusteringCodecs(
                c, &config, successors, ns, nodes, nn);
    }
    if (kind != 1 && kind != 2 && kind != 4) {
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
    ZL_NodeID nodes[MZL_MAX_SUCCESSORS];
    uint32_t nn = 0;
    const void* mparam = NULL;
    size_t mparam_len  = 0;
    if (kind == 4 && !p->bad) {
        nn = mzl_next(p);
        if (nn > MZL_MAX_SUCCESSORS) {
            p->bad = 1;
        }
        for (uint32_t i = 0; i < nn && !p->bad; i++)
            nodes[i] = (ZL_NodeID){ mzl_next(p) };
        mparam_len = mzl_next(p);
        size_t const words = (mparam_len + 3) / 4;
        if ((size_t)(p->end - p->pos) < words) {
            p->bad = 1;
        } else {
            mparam = p->pos;
            p->pos += words;
        }
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
    } else if (!p->bad && kind == 4) {
        ZL_ParameterizedGraphDesc const desc = {
            .graph          = (ZL_GraphID){ id },
            .customGraphs   = n ? successors : NULL,
            .nbCustomGraphs = n,
            .customNodes    = nn ? nodes : NULL,
            .nbCustomNodes  = nn,
            .localParams    = params->any ? &params->local : NULL,
            .mparam         = { .content = mparam_len ? mparam : NULL, .size = mparam_len },
        };
        result = ZL_Compressor_registerParameterizedGraph(c, &desc);
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

/* Compresses `n_inputs` typed inputs through the graph `program` describes.
 * Input `i` has type code `types[i]` (0 serial, 1 struct, 2 numeric,
 * 3 string), `sizes[i]` bytes at `srcs[i]`, element width `widths[i]`, for
 * strings `n_lens[i]` lengths at `lens[i]`, and `meta_counts[i]` integer
 * metadata pairs, consumed in order from `meta` as (id, value). `cparams`
 * holds `n_cparams` pairs of `ZL_CParam` and value. */
size_t mzl_enc_ml_compress(
        void* dst,
        size_t dst_capacity,
        size_t n_inputs,
        const uint32_t* types,
        const void* const* srcs,
        const size_t* sizes,
        const size_t* widths,
        const uint32_t* const* lens,
        const size_t* n_lens,
        const size_t* meta_counts,
        const int* meta,
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
    size_t meta_at = 0;
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
        for (size_t m = 0; m < meta_counts[i]; m++) {
            ZL_Report const s = ZL_Data_setIntMetadata(
                    ZL_codemodMutInputAsData(refs[i]),
                    meta[2 * meta_at],
                    meta[2 * meta_at + 1]);
            meta_at++;
            if (ZL_isError(s)) {
                *error = (int)ZL_errorCode(s);
                goto end;
            }
        }
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
