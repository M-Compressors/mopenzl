// Oracle for the Transformer numeric selector: the generated scorers, the
// feature extractors and the decision, called with the exact workspace the
// selector (`SI_transformer_numeric_select`) hands them.

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "openzl/compress/selectors/transformer/generated/score_numeric16.h"
#include "openzl/compress/selectors/transformer/generated/score_numeric32.h"
#include "openzl/compress/selectors/transformer/generated/score_numeric64.h"
#include "openzl/compress/selectors/transformer/generated/score_numeric8.h"
#include "openzl/compress/selectors/transformer/generic_numeric_decide.h"
#include "openzl/compress/selectors/transformer/numeric_stats.h"
#include "openzl/compress/selectors/transformer/score_guard.h"

// Number of u64 slots of the legacy feature dump.
#define MZL_TRS_LEGACY_FIELDS 24
// Number of u64 and double slots of the V2 feature dump.
#define MZL_TRS_V2_U64 16
#define MZL_TRS_V2_F64 16

static void mzl_trs_dump_legacy(const TRS_NumericFeatures* f, uint64_t* out)
{
    out[0]  = f->min_u;
    out[1]  = f->max_u;
    out[2]  = (uint64_t)f->min_s;
    out[3]  = (uint64_t)f->max_s;
    out[4]  = f->count;
    out[5]  = f->max_run_length;
    out[6]  = f->zero_count;
    out[7]  = f->delta_up_count;
    out[8]  = f->delta_down_count;
    out[9]  = (uint64_t)f->delta_min_s;
    out[10] = (uint64_t)f->delta_max_s;
    out[11] = f->mean_abs_dev_u_fp;
    out[12] = f->mean_abs_delta_u_fp;
    out[13] = f->cardinality_est;
    out[14] = f->pair_cardinality_est;
    out[15] = f->d8_cardinality_est;
    out[16] = f->sorted_gap_cv_fp;
    out[17] = f->transition_gap_cv_fp;
    out[18] = f->min_lb0;
    out[19] = f->match4;
    out[20] = f->range_u;
    out[21] = f->range_s;
    out[22] = f->zero_ratio_fp;
    out[23] = f->delta_up_ratio_fp;
}

static void mzl_trs_load_legacy(TRS_NumericFeatures* f, const uint64_t* in)
{
    memset(f, 0, sizeof(*f));
    f->min_u               = in[0];
    f->max_u               = in[1];
    f->min_s               = (int64_t)in[2];
    f->max_s               = (int64_t)in[3];
    f->count               = in[4];
    f->max_run_length      = in[5];
    f->zero_count          = in[6];
    f->delta_up_count      = in[7];
    f->delta_down_count    = in[8];
    f->delta_min_s         = (int64_t)in[9];
    f->delta_max_s         = (int64_t)in[10];
    f->mean_abs_dev_u_fp   = in[11];
    f->mean_abs_delta_u_fp = in[12];
    f->cardinality_est     = in[13];
    f->pair_cardinality_est = in[14];
    f->d8_cardinality_est  = in[15];
    f->sorted_gap_cv_fp    = in[16];
    f->transition_gap_cv_fp = in[17];
    f->min_lb0             = in[18];
    f->match4              = in[19];
    f->range_u             = in[20];
    f->range_s             = in[21];
    f->zero_ratio_fp       = in[22];
    f->delta_up_ratio_fp   = in[23];
    // delta_down_ratio_fp travels in the last slot of the dump below.
}

// Scores a legacy feature vector (`in` holds MZL_TRS_LEGACY_FIELDS values
// plus delta_down_ratio_fp at index 24). `width` is 1, 2 or 4.
void mzl_trs_score_legacy(
        size_t width,
        const uint64_t* in,
        float* scores,
        float* logits)
{
    TRS_NumericFeatures f;
    mzl_trs_load_legacy(&f, in);
    f.delta_down_ratio_fp = in[MZL_TRS_LEGACY_FIELDS];
    f.elt_width           = width;
    if (width == 1) {
        TRS_score_num8_operations_with_logits(&f, scores, logits);
    } else if (width == 2) {
        TRS_score_num16_operations_with_logits(&f, scores, logits);
    } else {
        TRS_score_num32_operations_with_logits(&f, scores, logits);
    }
}

// Scores a width-8 feature vector.
void mzl_trs_score_v2(
        const uint64_t* in,
        const double* fin,
        float* scores,
        float* logits)
{
    TRS_NumericFeaturesV2 f;
    memset(&f, 0, sizeof(f));
    f.count                = in[0];
    f.min_u                = in[1];
    f.max_u                = in[2];
    f.range_u              = in[3];
    f.range_s              = in[4];
    f.cardinality_est      = in[5];
    f.pair_cardinality_est = in[6];
    f.min_lb0              = in[7];
    f.zero_ratio           = fin[0];
    f.delta_up_ratio       = fin[1];
    f.delta_down_ratio     = fin[2];
    f.mean_abs_delta_u     = fin[3];
    f.sorted_gap_nmad      = fin[4];
    f.sorted_gap_mode      = fin[5];
    f.elt_width            = 8;
    TRS_score_num64_operations_with_logits(&f, scores, logits);
}

// Runs the decision of the selector for `width`-byte numbers. `data` must be
// aligned to `width`. Fills the decision and dumps the features that were
// extracted. Returns 0 when the extractor reports failure.
int mzl_trs_decide(
        size_t width,
        const uint8_t* data,
        size_t n_bytes,
        int* best_op,
        int* best_index,
        int* score_count,
        float* scores,
        float* logits,
        uint64_t* legacy,
        uint64_t* v2_u64,
        double* v2_f64)
{
    size_t const n_elts = n_bytes / width;
    TRS_NumericExtractWorkspace ws;
    memset(&ws, 0, sizeof(ws));
    uint32_t* match4 = calloc(TRS_NUMERIC_MATCH4_TABLE_ENTRIES, 4);
    TRS_NumericKmvEntry* kmv = calloc(TRS_NUMERIC_KMV_K, sizeof(*kmv));
    uint64_t* bitmap = calloc(TRS_NUMERIC_CARDINALITY_SCRATCH_WORDS, 8);
    uint64_t* gaps   = calloc(TRS_NUMERIC_SORTED_GAP_BUFFER_ENTRIES, 8);
    uint32_t* values = calloc(n_elts + 1, 4);
    ws.match4_table                = match4;
    ws.match4_capacity             = TRS_NUMERIC_MATCH4_TABLE_ENTRIES;
    ws.kmv_entries                 = kmv;
    ws.kmv_capacity                = TRS_NUMERIC_KMV_K;
    ws.cardinality_bitmap          = bitmap;
    ws.cardinality_bitmap_capacity = TRS_NUMERIC_CARDINALITY_SCRATCH_WORDS;
    ws.sorted_gap_buffer           = gaps;
    ws.sorted_gap_capacity         = TRS_NUMERIC_SORTED_GAP_BUFFER_ENTRIES;
    ws.values                      = values;
    ws.capacity                    = n_elts;
    int ok = 1;
    TRS_GenericNumericDecisionCore* core = NULL;
    TRS_GenericNumericDecision d;
    TRS_GenericNumericDecision64V2 d64;
    if (width == 1) {
        d    = TRS_generic_numeric_decide8(data, n_bytes, &ws);
        core = &d.core;
        mzl_trs_dump_legacy(&d.features, legacy);
        legacy[MZL_TRS_LEGACY_FIELDS] = d.features.delta_down_ratio_fp;
    } else if (width == 2) {
        // The 16-bit scratch bitmap needs the larger size.
        free(bitmap);
        bitmap = calloc(TRS_CARDINALITY_U16_BITMAP_WORDS, 8);
        ws.cardinality_bitmap          = bitmap;
        ws.cardinality_bitmap_capacity = TRS_CARDINALITY_U16_BITMAP_WORDS;
        d    = TRS_generic_numeric_decide16_fast(data, n_bytes, &ws);
        core = &d.core;
        mzl_trs_dump_legacy(&d.features, legacy);
        legacy[MZL_TRS_LEGACY_FIELDS] = d.features.delta_down_ratio_fp;
    } else if (width == 4) {
        d    = TRS_generic_numeric_decide32_fast(data, n_bytes);
        core = &d.core;
        mzl_trs_dump_legacy(&d.features, legacy);
        legacy[MZL_TRS_LEGACY_FIELDS] = d.features.delta_down_ratio_fp;
    } else {
        d64  = TRS_generic_numeric_decide64_v2_fast(data, n_bytes, &ws);
        core = &d64.core;
        v2_u64[0] = d64.features.count;
        v2_u64[1] = d64.features.min_u;
        v2_u64[2] = d64.features.max_u;
        v2_u64[3] = d64.features.range_u;
        v2_u64[4] = d64.features.range_s;
        v2_u64[5] = d64.features.cardinality_est;
        v2_u64[6] = d64.features.pair_cardinality_est;
        v2_u64[7] = d64.features.min_lb0;
        v2_f64[0] = d64.features.zero_ratio;
        v2_f64[1] = d64.features.delta_up_ratio;
        v2_f64[2] = d64.features.delta_down_ratio;
        v2_f64[3] = d64.features.mean_abs_delta_u;
        v2_f64[4] = d64.features.sorted_gap_nmad;
        v2_f64[5] = d64.features.sorted_gap_mode;
    }
    *best_op      = (int)core->best_op;
    *best_index   = core->best_index;
    *score_count  = core->score_count;
    memcpy(scores, core->scores, sizeof(core->scores));
    memcpy(logits, core->logits, sizeof(core->logits));
    free(match4);
    free(kmv);
    free(bitmap);
    free(gaps);
    free(values);
    return ok;
}

// The scorer's operation IDs, so the Rust side can check its tables.
int mzl_trs_operation_ids(size_t width, int* out)
{
    int n;
    const TRS_GenericNumericOpId* ids;
    if (width == 1) {
        n   = TRS_SCORE_NUM8_N_OPERATIONS;
        ids = TRS_score_num8_operation_ids;
    } else if (width == 2) {
        n   = TRS_SCORE_NUM16_N_OPERATIONS;
        ids = TRS_score_num16_operation_ids;
    } else if (width == 4) {
        n   = TRS_SCORE_NUM32_N_OPERATIONS;
        ids = TRS_score_num32_operation_ids;
    } else {
        n   = TRS_SCORE_NUM64_N_OPERATIONS;
        ids = TRS_score_num64_operation_ids;
    }
    for (int i = 0; i < n; i++) {
        out[i] = (int)ids[i];
    }
    return n;
}
