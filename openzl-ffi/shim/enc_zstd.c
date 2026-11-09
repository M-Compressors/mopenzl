// Test oracle of the Zstandard node (`EI_zstdWithCCtx`,
// codecs/zstd/encode_zstd_binding.c): the same libzstd calls in the same
// order, outside the graph engine, so a test can feed it struct input and
// check block splitting, which no registered node reaches.

#define ZSTD_STATIC_LINKING_ONLY
#include <stddef.h>
#include <stdint.h>
#include <zstd.h>

#define MEMORY_USAGE_CAP_LOG_FACTOR 3

static int parameter_valid(ZSTD_cParameter param, int value)
{
    if (param == ZSTD_c_format || param == ZSTD_c_contentSizeFlag) {
        return 0;
    }
    if (param == ZSTD_c_windowLog) {
        return value <= ZSTD_WINDOWLOG_MAX - MEMORY_USAGE_CAP_LOG_FACTOR;
    }
    if (param == ZSTD_c_hashLog) {
        return value <= ZSTD_HASHLOG_MAX - MEMORY_USAGE_CAP_LOG_FACTOR;
    }
    if (param == ZSTD_c_chainLog) {
        return value <= ZSTD_CHAINLOG_MAX - MEMORY_USAGE_CAP_LOG_FACTOR;
    }
    if (param == ZSTD_c_ldmHashLog) {
        return value <= ZSTD_LDM_HASHLOG_MAX - MEMORY_USAGE_CAP_LOG_FACTOR;
    }
    return 1;
}

static size_t varint_encode(uint64_t value, uint8_t* dst)
{
    size_t n = 0;
    while (value >= 0x80) {
        dst[n++] = (uint8_t)(value | 0x80);
        value >>= 7;
    }
    dst[n++] = (uint8_t)value;
    return n;
}

// The node's output stream (width varint, then the magicless frame) for
// `n_elts` elements of `width` bytes, or -1 when the reference fails.
// `params` holds `n_params` (id, value) pairs. `dst` has `dst_capacity`
// bytes, at least the reference's `outCapacity`.
long long mzl_zstd_node(
        const void* src,
        size_t n_elts,
        size_t width,
        int level,
        int decompression_level,
        const int* params,
        size_t n_params,
        void* dst,
        size_t dst_capacity)
{
    size_t const src_size = n_elts * width;
    int const block_split = n_elts > 0 && width >= 1024 && n_elts <= 8;
    size_t const block_size = block_split ? width : src_size;
    uint8_t* const ostart = (uint8_t*)dst;
    ZSTD_CCtx* cctx = ZSTD_createCCtx();
    long long result = -1;
    if (cctx == NULL) {
        return -1;
    }
    size_t const header_size = varint_encode((uint64_t)width, ostart);
    if (ZSTD_isError(ZSTD_CCtx_reset(cctx, ZSTD_reset_session_and_parameters))
        || ZSTD_isError(ZSTD_CCtx_setParameter(
                cctx, ZSTD_c_format, ZSTD_f_zstd1_magicless))
        || ZSTD_isError(ZSTD_CCtx_setParameter(
                cctx, ZSTD_c_compressionLevel, level))) {
        goto done;
    }
    if (decompression_level == 1
        && ZSTD_isError(ZSTD_CCtx_setParameter(
                cctx, ZSTD_c_literalCompressionMode, ZSTD_lcm_uncompressed))) {
        goto done;
    }
    for (size_t n = 0; n < n_params; n++) {
        ZSTD_cParameter const param = (ZSTD_cParameter)params[2 * n];
        int const value             = params[2 * n + 1];
        if (!parameter_valid(param, value)
            || ZSTD_isError(ZSTD_CCtx_setParameter(cctx, param, value))) {
            goto done;
        }
    }
    if (block_size == src_size) {
        size_t const c_size = ZSTD_compress2(
                cctx,
                ostart + header_size,
                dst_capacity - header_size,
                src,
                src_size);
        if (ZSTD_isError(c_size)) {
            goto done;
        }
        result = (long long)(header_size + c_size);
    } else {
        ZSTD_CCtx_setPledgedSrcSize(cctx, src_size);
        ZSTD_outBuffer out = { ostart, dst_capacity, header_size };
        ZSTD_inBuffer in   = { src, block_size, 0 };
        for (; in.pos < src_size; in.size += block_size) {
            while (in.pos < in.size) {
                ZSTD_EndDirective const flush =
                        in.size == src_size ? ZSTD_e_end : ZSTD_e_flush;
                size_t const ret = ZSTD_compressStream2(cctx, &out, &in, flush);
                if (ZSTD_isError(ret)) {
                    goto done;
                }
            }
        }
        result = (long long)out.pos;
    }
done:
    ZSTD_freeCCtx(cctx);
    return result;
}
