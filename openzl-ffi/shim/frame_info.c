// Test helper of the frame header: flat C entry points over `ZL_FrameInfo_*`
// so Rust tests can compare the decoder's frame-info accessor with C.

#include <stddef.h>
#include <stdint.h>
#include <string.h>

#include "openzl/zl_decompress.h"
#include "openzl/zl_errors.h"

static size_t mzl_fi_unwrap(ZL_Report report, int* error)
{
    if (ZL_isError(report)) {
        *error = (int)ZL_errorCode(report);
        return 0;
    }
    *error = 0;
    return ZL_validResult(report);
}

// `ZL_FrameInfo_create`; NULL when the header cannot be parsed.
void* mzl_frame_info_create(const void* src, size_t src_size)
{
    return ZL_FrameInfo_create(src, src_size);
}

// `ZL_FrameInfo_free`.
void mzl_frame_info_free(void* info)
{
    ZL_FrameInfo_free((ZL_FrameInfo*)info);
}

// `ZL_FrameInfo_getFormatVersion`.
size_t mzl_frame_info_version(const void* info, int* error)
{
    return mzl_fi_unwrap(
            ZL_FrameInfo_getFormatVersion((const ZL_FrameInfo*)info), error);
}

// `ZL_FrameInfo_getNumOutputs`.
size_t mzl_frame_info_num_outputs(const void* info, int* error)
{
    return mzl_fi_unwrap(
            ZL_FrameInfo_getNumOutputs((const ZL_FrameInfo*)info), error);
}

// `ZL_FrameInfo_getOutputType`: the `ZL_Type` bit (1 serial, 2 struct, 4
// numeric, 8 string).
size_t mzl_frame_info_output_type(const void* info, int id, int* error)
{
    return mzl_fi_unwrap(
            ZL_FrameInfo_getOutputType((const ZL_FrameInfo*)info, id), error);
}

// `ZL_FrameInfo_getDecompressedSize`.
size_t mzl_frame_info_decompressed_size(const void* info, int id, int* error)
{
    return mzl_fi_unwrap(
            ZL_FrameInfo_getDecompressedSize((const ZL_FrameInfo*)info, id),
            error);
}

// `ZL_FrameInfo_getNumElts`.
size_t mzl_frame_info_num_elts(const void* info, int id, int* error)
{
    return mzl_fi_unwrap(
            ZL_FrameInfo_getNumElts((const ZL_FrameInfo*)info, id), error);
}

// `ZL_FrameInfo_getComment`: copies at most `capacity` bytes to `dst` and
// returns the comment's size (0 when there is none).
size_t mzl_frame_info_comment(
        const void* info,
        void* dst,
        size_t capacity,
        int* error)
{
    ZL_RESULT_OF(ZL_Comment)
    result = ZL_FrameInfo_getComment((const ZL_FrameInfo*)info);
    if (ZL_RES_isError(result)) {
        *error = (int)ZL_RES_code(result);
        return 0;
    }
    ZL_Comment comment = ZL_RES_value(result);
    *error = 0;
    if (comment.size > 0 && comment.size <= capacity) {
        memcpy(dst, comment.data, comment.size);
    }
    return comment.size;
}

// `ZL_FrameInfo_getBundleID`: copies the 32 ID bytes to `dst` and returns 1,
// or returns 0 when the frame names no bundle.
int mzl_frame_info_bundle_id(const void* info, uint8_t* dst)
{
    const ZL_BundleID* id = ZL_FrameInfo_getBundleID((const ZL_FrameInfo*)info);
    if (id == NULL) {
        return 0;
    }
    memcpy(dst, id->id.bytes, 32);
    return 1;
}
