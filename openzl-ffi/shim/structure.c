// Frames for the "structure" codec family (concat, dedup, interleave,
// mux_lengths, dispatchN_byTag, sentinel) that the standard graphs cannot
// reach: multi-input codecs and codecs whose parameters are pointers. Each
// is run from a small function graph that sends every output to the store
// graph.
//
// `mzl_structure_compress` compresses typed inputs the same way as
// `mzl_compress_graph` (see graph.c), starting from that function graph.

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

#include "openzl/codecs/zl_concat.h"
#include "openzl/codecs/zl_dedup.h"
#include "openzl/codecs/zl_dispatch.h"
#include "openzl/codecs/zl_interleave.h"
#include "openzl/codecs/zl_mux_lengths.h"
#include "openzl/codecs/zl_sentinel.h"
#include "openzl/codecs/zl_transpose.h"
#include "openzl/compress/private_nodes.h"
#include "openzl/zl_compress.h"
#include "openzl/zl_compressor.h"
#include "openzl/zl_errors.h"
#include "openzl/zl_graph_api.h"

#define MZL_MAX_INTS 16
#define MZL_MAX_COPIES 8

typedef struct {
    uint32_t mode;
    uint32_t node;
    const int* ints; // (id, value) pairs
    size_t n_ints;
    const int* copy_ids;
    const void* const* copy_ptrs;
    const size_t* copy_sizes;
    size_t n_copies;
    const size_t* segments; // dispatch sizes, or sentinel indices
    const uint32_t* tags;
    size_t n_segments;
    uint32_t n_tags;
    uint64_t value; // sentinel
} mzl_structure_config;

static ZL_Report mzl_structure_graph(
        ZL_Graph* graph,
        ZL_Edge* inputs[],
        size_t n_inputs)
{
    ZL_RESULT_DECLARE_SCOPE_REPORT(graph);
    const mzl_structure_config* const cfg = ZL_Graph_getOpaquePtr(graph);
    ZL_RESULT_OF(ZL_EdgeList) run;
    switch (cfg->mode) {
        case 0: {
            ZL_IntParam ints[MZL_MAX_INTS];
            for (size_t i = 0; i < cfg->n_ints; i++) {
                ints[i].paramId    = cfg->ints[2 * i];
                ints[i].paramValue = cfg->ints[2 * i + 1];
            }
            ZL_CopyParam copies[MZL_MAX_COPIES];
            for (size_t i = 0; i < cfg->n_copies; i++) {
                copies[i].paramId   = cfg->copy_ids[i];
                copies[i].paramPtr  = cfg->copy_ptrs[i];
                copies[i].paramSize = cfg->copy_sizes[i];
            }
            ZL_LocalParams lp = {
                .intParams  = { ints, cfg->n_ints },
                .copyParams = { copies, cfg->n_copies },
            };
            run               = ZL_Edge_runMultiInputNode_withParams(
                    inputs,
                    n_inputs,
                    (ZL_NodeID){ cfg->node },
                    cfg->n_ints || cfg->n_copies ? &lp : NULL);
            break;
        }
        case 1: {
            ZL_DispatchInstructions instructions = {
                .segmentSizes = cfg->segments,
                .tags         = cfg->tags,
                .nbSegments   = cfg->n_segments,
                .nbTags       = cfg->n_tags,
            };
            run = ZL_Edge_runDispatchNode(inputs[0], &instructions);
            break;
        }
        default:
            run = ZL_Edge_runSentinelNode(
                    inputs[0], cfg->segments, cfg->n_segments, cfg->value);
            break;
    }
    ZL_TRY_LET(ZL_EdgeList, outputs, run);
    for (size_t i = 0; i < outputs.nbEdges; i++) {
        ZL_ERR_IF_ERR(ZL_Edge_setDestination(outputs.edges[i], ZL_GRAPH_STORE));
    }
    return ZL_returnSuccess();
}

/* Compresses the typed inputs through the function graph `cfg` describes.
 * The input arrays are those of `mzl_compress_graph`. */
size_t mzl_structure_compress(
        void* dst,
        size_t dst_capacity,
        size_t n_inputs,
        const uint32_t* types,
        const void* const* srcs,
        const size_t* sizes,
        const size_t* widths,
        const uint32_t* const* lens,
        const size_t* n_lens,
        uint32_t mode,
        uint32_t node,
        const int* ints,
        size_t n_ints,
        const int* copy_ids,
        const void* const* copy_ptrs,
        const size_t* copy_sizes,
        size_t n_copies,
        const size_t* segments,
        const uint32_t* tags,
        size_t n_segments,
        uint32_t n_tags,
        uint64_t value,
        const int* cparams,
        size_t n_cparams,
        int* error)
{
    *error = 1;
    if (n_inputs == 0 || n_inputs > 64 || n_ints > MZL_MAX_INTS
        || n_copies > MZL_MAX_COPIES)
        return 0;
    mzl_structure_config cfg = { mode,       node,       ints,       n_ints,
                                 copy_ids,   copy_ptrs,  copy_sizes, n_copies,
                                 segments,   tags,       n_segments, n_tags,
                                 value };
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
    const ZL_Type mask = ZL_Type_any;
    ZL_FunctionGraphDesc desc = {
        .name                = "mzl_structure",
        .graph_f             = mzl_structure_graph,
        .inputTypeMasks      = &mask,
        .nbInputs            = 1,
        .lastInputIsVariable = 1,
        .opaque              = { .ptr = &cfg },
    };
    ZL_GraphID const graph = ZL_Compressor_registerFunctionGraph(compressor, &desc);
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
    for (size_t i = 0; i < n_inputs; i++) {
        switch (types[i]) {
            case 1:
                refs[i] = ZL_TypedRef_createStruct(
                        srcs[i], widths[i], widths[i] ? sizes[i] / widths[i] : 0);
                break;
            case 2:
                refs[i] = ZL_TypedRef_createNumeric(
                        srcs[i], widths[i], widths[i] ? sizes[i] / widths[i] : 0);
                break;
            case 3:
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

/* The numeric ID of a node the generic graph builder cannot name. */
uint32_t mzl_structure_node(uint32_t which)
{
    switch (which) {
        case 0:
            return ZL_NODE_CONCAT_SERIAL.nid;
        case 1:
            return ZL_NODE_CONCAT_NUMERIC.nid;
        case 2:
            return ZL_NODE_CONCAT_STRUCT.nid;
        case 3:
            return ZL_NODE_CONCAT_STRING.nid;
        case 4:
            return ZL_NODE_DEDUP_NUMERIC.nid;
        case 5:
            return ZL_NODE_INTERLEAVE_STRING.nid;
        case 6:
            return ZL_NODE_MUX_LENGTHS.nid;
        case 7:
            return ZL_NODE_SENTINEL_BYTE.nid;
        case 8:
            return ZL_NODE_TRANSPOSE_SPLIT.nid;
        case 9:
            return ZL_NODE_DISPATCH_STRING.nid;
        case 10:
            return ZL_NODE_SPLIT_BY_STRUCT.nid;
        case 11:
            return ZL_PrivateStandardNodeID_splitN;
        case 12:
            return ZL_PrivateStandardNodeID_splitN_struct;
        case 13:
            return ZL_PrivateStandardNodeID_splitN_num;
        default:
            return 0;
    }
}
