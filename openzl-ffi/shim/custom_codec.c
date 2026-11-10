// A custom serial XOR transform, exercising custom IDs and private headers.
#include <stdint.h>
#include "openzl/codecs/zl_store.h"
#include "openzl/zl_compress.h"
#include "openzl/zl_compressor.h"
#include "openzl/zl_ctransform.h"
#include "openzl/zl_dtransform.h"
#include "openzl/zl_decompress.h"
#include "openzl/zl_data.h"

static ZL_Report encode(ZL_Encoder* ctx, const ZL_Input* inputs[], size_t n)
{
    if (n != 1) return ZL_returnError(ZL_ErrorCode_GENERIC);
    size_t const size = ZL_Input_numElts(inputs[0]);
    ZL_Output* out = ZL_Encoder_createTypedStream(ctx, 0, size, 1);
    if (!out) return ZL_returnError(ZL_ErrorCode_GENERIC);
    uint8_t const* src = ZL_Input_ptr(inputs[0]);
    uint8_t* dst = ZL_Output_ptr(out);
    for (size_t i = 0; i < size; ++i) dst[i] = src[i] ^ 0xa5;
    uint8_t const key = 0xa5;
    ZL_Encoder_sendCodecHeader(ctx, &key, 1);
    return ZL_Output_commit(out, size);
}

static ZL_Report decode(ZL_Decoder* ctx, const ZL_Input* inputs[], size_t n,
                        const ZL_Input* variable[], size_t nv)
{
    (void)variable;
    ZL_RBuffer const header = ZL_Decoder_getCodecHeader(ctx);
    if (n != 1 || nv != 0 || header.size != 1) return ZL_returnError(ZL_ErrorCode_GENERIC);
    size_t const size = ZL_Input_numElts(inputs[0]);
    ZL_Output* out = ZL_Decoder_createTypedStream(ctx, 0, size, 1);
    if (!out) return ZL_returnError(ZL_ErrorCode_GENERIC);
    uint8_t const* src = ZL_Input_ptr(inputs[0]);
    uint8_t* dst = ZL_Output_ptr(out);
    uint8_t const key = *(uint8_t const*)header.start;
    for (size_t i = 0; i < size; ++i) dst[i] = src[i] ^ key;
    return ZL_Output_commit(out, size);
}

static ZL_MIGraphDesc signature(uint32_t id)
{
    static ZL_Type const types[] = { ZL_Type_serial };
    ZL_MIGraphDesc result = { .CTid = id, .inputTypes = types, .nbInputs = 1,
                              .soTypes = types, .nbSOs = 1 };
    return result;
}

size_t mzl_custom_codec(int decoding, uint32_t id, int version,
                       const void* src, size_t size, void* dst, size_t cap)
{
    ZL_Report report = ZL_returnError(ZL_ErrorCode_GENERIC);
    if (decoding) {
        ZL_DCtx* ctx = ZL_DCtx_create();
        if (!ctx) return SIZE_MAX;
        ZL_MIDecoderDesc desc = { .gd = signature(id), .transform_f = decode,
                                  .name = "custom_xor" };
        report = ZL_DCtx_registerMIDecoder(ctx, &desc);
        if (!ZL_isError(report)) report = ZL_DCtx_decompress(ctx, dst, cap, src, size);
        ZL_DCtx_free(ctx);
    } else {
        ZL_CCtx* ctx = ZL_CCtx_create();
        ZL_Compressor* compressor = ZL_Compressor_create();
        if (!ctx || !compressor) { ZL_CCtx_free(ctx); ZL_Compressor_free(compressor); return SIZE_MAX; }
        ZL_MIEncoderDesc desc = { .gd = signature(id), .transform_f = encode,
                                  .name = "custom_xor" };
        ZL_NodeID const node = ZL_Compressor_registerMIEncoder(compressor, &desc);
        ZL_GraphID const store = ZL_GRAPH_STORE;
        ZL_GraphID const graph = ZL_Compressor_registerStaticGraph_fromNode(compressor, node, &store, 1);
        report = ZL_Compressor_selectStartingGraphID(compressor, graph);
        if (!ZL_isError(report)) report = ZL_Compressor_setParameter(compressor, ZL_CParam_formatVersion, version);
        if (!ZL_isError(report)) report = ZL_Compressor_setParameter(compressor, ZL_CParam_minStreamSize, 1);
        if (!ZL_isError(report)) report = ZL_Compressor_setParameter(compressor, ZL_CParam_storeOnExpansion, ZL_TernaryParam_disable);
        if (!ZL_isError(report)) report = ZL_CCtx_refCompressor(ctx, compressor);
        if (!ZL_isError(report)) report = ZL_CCtx_compress(ctx, dst, cap, src, size);
        ZL_CCtx_free(ctx);
        ZL_Compressor_free(compressor);
    }
    return ZL_isError(report) ? SIZE_MAX : ZL_validResult(report);
}
