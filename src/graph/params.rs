//! Local-parameter and metadata IDs of the standard codecs and graphs.
//!
//! [`LocalParams`](super::LocalParams) addresses a parameter by its raw
//! integer ID, as the reference does. The constants here name those IDs, one
//! module per codec or graph; each cites the C macro or enum it mirrors.
//! Whether a parameter is an integer or a byte blob follows the reference:
//! use [`LocalParams::with_int`](super::LocalParams::with_int) or
//! [`LocalParams::with_bytes`](super::LocalParams::with_bytes) accordingly.
//!
//! # Examples
//!
//! ```
//! use mopenzl::graph::LocalParams;
//! use mopenzl::graph::params::{lz, zstd};
//!
//! let zstd19 = LocalParams::default().with_int(zstd::COMPRESSION_LEVEL, 19);
//! let lz = LocalParams::default().with_int(lz::WINDOW_LOG, 16);
//! assert_eq!(zstd19.int(100), Some(19));
//! assert_eq!(lz.int(101), Some(16));
//! ```

/// Zstandard node (`ZL_NODE_ZSTD`): the raw `ZSTD_cParameter` IDs it applies in list order
/// (`ZSTD_CCtx_setParameter`).
pub mod zstd {
    /// `ZSTD_c_format` (10). Refused by the node: the frame is always magicless.
    pub const FORMAT: i32 = 10;
    /// `ZSTD_c_compressionLevel` (100). Clamped to the level range; 0 is the default level.
    pub const COMPRESSION_LEVEL: i32 = 100;
    /// `ZSTD_c_windowLog` (101). 0 restores the default.
    pub const WINDOW_LOG: i32 = 101;
    /// `ZSTD_c_hashLog` (102). 0 restores the default.
    pub const HASH_LOG: i32 = 102;
    /// `ZSTD_c_chainLog` (103). 0 restores the default.
    pub const CHAIN_LOG: i32 = 103;
    /// `ZSTD_c_searchLog` (104). 0 restores the default.
    pub const SEARCH_LOG: i32 = 104;
    /// `ZSTD_c_minMatch` (105). 0 restores the default.
    pub const MIN_MATCH: i32 = 105;
    /// `ZSTD_c_targetLength` (106).
    pub const TARGET_LENGTH: i32 = 106;
    /// `ZSTD_c_strategy` (107). 1 to 9; 0 restores the default.
    pub const STRATEGY: i32 = 107;
    /// `ZSTD_c_targetCBlockSize` (130). Only 0 is supported.
    pub const TARGET_C_BLOCK_SIZE: i32 = 130;
    /// `ZSTD_c_enableLongDistanceMatching` (160).
    pub const ENABLE_LDM: i32 = 160;
    /// `ZSTD_c_ldmHashLog` (161).
    pub const LDM_HASH_LOG: i32 = 161;
    /// `ZSTD_c_ldmMinMatch` (162).
    pub const LDM_MIN_MATCH: i32 = 162;
    /// `ZSTD_c_ldmBucketSizeLog` (163).
    pub const LDM_BUCKET_SIZE_LOG: i32 = 163;
    /// `ZSTD_c_ldmHashRateLog` (164).
    pub const LDM_HASH_RATE_LOG: i32 = 164;
    /// `ZSTD_c_contentSizeFlag` (200). Refused by the node: the binding owns it.
    pub const CONTENT_SIZE_FLAG: i32 = 200;
    /// `ZSTD_c_checksumFlag` (201).
    pub const CHECKSUM_FLAG: i32 = 201;
    /// `ZSTD_c_dictIDFlag` (202).
    pub const DICT_ID_FLAG: i32 = 202;
    /// `ZSTD_c_nbWorkers` (400). Only 0 is supported (single-threaded).
    pub const NB_WORKERS: i32 = 400;
    /// `ZSTD_c_jobSize` (401). Only 0 is supported.
    pub const JOB_SIZE: i32 = 401;
    /// `ZSTD_c_overlapLog` (402). Only 0 is supported.
    pub const OVERLAP_LOG: i32 = 402;
    /// `ZSTD_c_rsyncable` (500). Only 0 is supported.
    pub const RSYNCABLE: i32 = 500;
    /// `ZSTD_c_forceMaxWindow` (1000).
    pub const FORCE_MAX_WINDOW: i32 = 1000;
    /// `ZSTD_c_forceAttachDict` (1001).
    pub const FORCE_ATTACH_DICT: i32 = 1001;
    /// `ZSTD_c_literalCompressionMode` (1002). 0 auto, 1 enable, 2 disable.
    pub const LITERAL_COMPRESSION_MODE: i32 = 1002;
    /// `ZSTD_c_srcSizeHint` (1004).
    pub const SRC_SIZE_HINT: i32 = 1004;
    /// `ZSTD_c_enableDedicatedDictSearch` (1005).
    pub const ENABLE_DEDICATED_DICT_SEARCH: i32 = 1005;
    /// `ZSTD_c_stableInBuffer` (1006).
    pub const STABLE_IN_BUFFER: i32 = 1006;
    /// `ZSTD_c_stableOutBuffer` (1007).
    pub const STABLE_OUT_BUFFER: i32 = 1007;
    /// `ZSTD_c_blockDelimiters` (1008).
    pub const BLOCK_DELIMITERS: i32 = 1008;
    /// `ZSTD_c_validateSequences` (1009).
    pub const VALIDATE_SEQUENCES: i32 = 1009;
    /// `ZSTD_c_splitAfterSequences` (1010).
    pub const SPLIT_AFTER_SEQUENCES: i32 = 1010;
    /// `ZSTD_c_useRowMatchFinder` (1011).
    pub const USE_ROW_MATCH_FINDER: i32 = 1011;
    /// `ZSTD_c_deterministicRefPrefix` (1012).
    pub const DETERMINISTIC_REF_PREFIX: i32 = 1012;
    /// `ZSTD_c_prefetchCDictTables` (1013).
    pub const PREFETCH_CDICT_TABLES: i32 = 1013;
    /// `ZSTD_c_enableSeqProducerFallback` (1014).
    pub const ENABLE_SEQ_PRODUCER_FALLBACK: i32 = 1014;
    /// `ZSTD_c_maxBlockSize` (1015).
    pub const MAX_BLOCK_SIZE: i32 = 1015;
    /// `ZSTD_c_repcodeResolution` (1016).
    pub const REPCODE_RESOLUTION: i32 = 1016;
    /// `ZSTD_c_blockSplitterLevel` (1017).
    pub const BLOCK_SPLITTER_LEVEL: i32 = 1017;
}

/// `lz` node and graph (`ZL_NODE_LZ`, `ZL_GRAPH_LZ`): the `ZL_LzParam` enum and its metadata ID.
pub mod lz {
    /// `ZL_LzParam_compressionLevel` (1). 0 takes the default.
    pub const COMPRESSION_LEVEL: i32 = 1;
    /// `ZL_LzParam_acceleration` (100). 0 takes the default of 1.
    pub const ACCELERATION: i32 = 100;
    /// `ZL_LzParam_windowLog` (101). 10 to 28; 0 takes the default.
    pub const WINDOW_LOG: i32 = 101;
    /// `ZL_LzParam_strategy` (102). `ZL_LzStrategy`: 1 fast, 2 double fast, 3 greedy, 4 lazy, 5 lazy2; 0 takes the default.
    pub const STRATEGY: i32 = 102;
    /// `ZL_LzParam_hashLog1` (103). 0 takes the default.
    pub const HASH_LOG1: i32 = 103;
    /// `ZL_LzParam_hashLog2` (104). 0 takes the default.
    pub const HASH_LOG2: i32 = 104;
    /// `ZL_LzParam_hashLength` (105). 0 takes the default.
    pub const HASH_LENGTH: i32 = 105;
    /// `ZL_LzParam_searchLog` (106). 0 takes the default.
    pub const SEARCH_LOG: i32 = 106;
    /// `ZL_LzParam_literalsGraphIdx` (1000). Index into the graph's custom graphs for the literals.
    pub const LITERALS_GRAPH_IDX: i32 = 1000;
    /// `ZL_LzParam_offsetsGraphIdx` (1001). Index into the graph's custom graphs for the offsets.
    pub const OFFSETS_GRAPH_IDX: i32 = 1001;
    /// `ZL_LzParam_muxedBytesGraphIdx` (1002). Index into the graph's custom graphs for the muxed bytes.
    pub const MUXED_BYTES_GRAPH_IDX: i32 = 1002;
    /// `ZL_LzParam_overflowLengthsGraphIdx` (1003). Index into the graph's custom graphs for the overflow lengths.
    pub const OVERFLOW_LENGTHS_GRAPH_IDX: i32 = 1003;
    /// `ZL_LzParam_muxLengthsGraphIdx` (1004). Index into the graph's custom graphs for the two 16-bit length streams.
    pub const MUX_LENGTHS_GRAPH_IDX: i32 = 1004;
    /// `ZL_LzParam_minGainForEntropyBytes` (10000). Bytes an entropy backend must save.
    pub const MIN_GAIN_FOR_ENTROPY_BYTES: i32 = 10000;
    /// `ZL_LzParam_minGainForEntropyPct` (10001). Percent of its input an entropy backend must save.
    pub const MIN_GAIN_FOR_ENTROPY_PCT: i32 = 10001;
    /// `ZL_LZ_MIN_MATCH_LENGTH_METADATA_ID` (77). Integer metadata (not a local parameter) on the `lz` node's output.
    pub const MIN_MATCH_LENGTH_METADATA_ID: i32 = 77;
}

/// `lz4` node and graph (`zl_lz4.h`).
pub mod lz4 {
    /// `ZL_LZ4_COMPRESSION_LEVEL_OVERRIDE_PID` (0). An integer parameter overriding the global compression level.
    pub const COMPRESSION_LEVEL_OVERRIDE: i32 = 0;
}

/// `field_lz` node and graph (`zl_field_lz.h`).
pub mod field_lz {
    /// `ZL_FIELD_LZ_COMPRESSION_LEVEL_OVERRIDE_PID` (181). Overrides the global compression level.
    pub const COMPRESSION_LEVEL_OVERRIDE: i32 = 181;
    /// `ZL_FIELD_LZ_LITERALS_GRAPH_OVERRIDE_INDEX_PID` (0). Custom-graph index for the literals.
    pub const LITERALS_GRAPH_OVERRIDE_INDEX: i32 = 0;
    /// `ZL_FIELD_LZ_TOKENS_GRAPH_OVERRIDE_INDEX_PID` (1). Custom-graph index for the tokens.
    pub const TOKENS_GRAPH_OVERRIDE_INDEX: i32 = 1;
    /// `ZL_FIELD_LZ_OFFSETS_GRAPH_OVERRIDE_INDEX_PID` (2). Custom-graph index for the offsets.
    pub const OFFSETS_GRAPH_OVERRIDE_INDEX: i32 = 2;
    /// `ZL_FIELD_LZ_EXTRA_LITERAL_LENGTHS_GRAPH_OVERRIDE_INDEX_PID` (3). Custom-graph index for the extra literal lengths.
    pub const EXTRA_LITERAL_LENGTHS_GRAPH_OVERRIDE_INDEX: i32 = 3;
    /// `ZL_FIELD_LZ_EXTRA_MATCH_LENGTHS_GRAPH_OVERRIDE_INDEX_PID` (4). Custom-graph index for the extra match lengths.
    pub const EXTRA_MATCH_LENGTHS_GRAPH_OVERRIDE_INDEX: i32 = 4;
}

/// `entropy` graph (`zl_entropy.h`).
pub mod entropy {
    /// `ZL_ENTROPY_MIN_GAIN_BYTES_PID` (66). Bytes an entropy coder must save to be used.
    pub const MIN_GAIN_BYTES: i32 = 66;
    /// `ZL_ENTROPY_MIN_GAIN_PCT_PID` (80). Percent of the input an entropy coder must save to be used.
    pub const MIN_GAIN_PCT: i32 = 80;
}

/// `divide_by` node (`zl_divide_by.h`).
pub mod divide_by {
    /// `ZL_DIVIDE_BY_PID` (112). The divisor, a copy parameter of 8 little-endian bytes; absent or zero takes the greatest common divisor.
    pub const DIVIDE_BY: i32 = 112;
}

/// `bitunpack` node (`zl_bitunpack.h`).
pub mod bitunpack {
    /// `ZL_Bitunpack_numBits` (1). Bits per packed integer.
    pub const NUM_BITS: i32 = 1;
}

/// `partition` node and `partition_bitpack` graph (`zl_partition.h`).
pub mod partition {
    /// `ZL_PARTITION_PRESET_PID` (120). Selects a built-in preset, an integer parameter.
    pub const PRESET: i32 = 120;
    /// `ZL_PARTITION_CUSTOM_PID` (121). A custom partition, a copy parameter: `startValue` then the partition sizes as little-endian 64-bit numbers.
    pub const CUSTOM: i32 = 121;
    /// `ZL_GRAPH_PARTITION_BITPACK_OPTIMAL_PID` (0). Graph parameter enabling the optimal (ternary) search of `partition_bitpack`.
    pub const BITPACK_OPTIMAL: i32 = 0;
}

/// `sparse_num` node (`zl_sparse_num.h`).
pub mod sparse_num {
    /// `ZL_SPARSE_NUM_DOMINANT_VALUE_PID` (132). The dominant value of `sparse_num_auto` as a copy parameter of 8 bytes, instead of the estimated one.
    pub const DOMINANT_VALUE: i32 = 132;
}

/// `sentinel` node (`zl_sentinel.h`).
pub mod sentinel {
    /// `ZL_SENTINEL_INDICES_PID` (130). The exception positions of `sentinel_num`: a copy parameter of increasing 64-bit little-endian indices.
    pub const INDICES: i32 = 130;
    /// `ZL_SENTINEL_VALUE_PID` (131). The sentinel of `sentinel_num` as a copy parameter of 8 bytes; the default is the width's maximum.
    pub const VALUE: i32 = 131;
}

/// `tokenize` node (`zl_tokenize.h`).
pub mod tokenize {
    /// `ZL_TOKENIZE_SORT_PID` (0). An integer parameter; non-zero sorts the alphabet.
    pub const SORT: i32 = 0;
}

/// `split` nodes (`zl_split.h`).
pub mod split {
    /// `ZL_SPLIT_BYRANGE_MIN_SEGMENT_SIZE_PID` (324). Smallest segment of the split-by-range node, an integer parameter.
    pub const BYRANGE_MIN_SEGMENT_SIZE: i32 = 324;
    /// `ZL_SPLIT_CHANNEL_ID` (867). Integer metadata (not a local parameter): the channel of a split output.
    pub const CHANNEL_ID: i32 = 867;
}

/// `split_by_struct` node (`ZL_SPLITBYSTRUCT_FIELDSIZES_PID`, private header).
pub mod split_by_struct {
    /// `ZL_SPLITBYSTRUCT_FIELDSIZES_PID` (387). The field sizes, a copy parameter.
    pub const FIELD_SIZES: i32 = 387;
}

/// `dispatch` nodes (`zl_dispatch.h`).
pub mod dispatch {
    /// `ZL_DISPATCH_CHANNEL_ID` (83). Integer metadata (not a local parameter): the channel of a dispatched output.
    pub const CHANNEL_ID: i32 = 83;
    /// `ZL_DISPATCH_STRING_NUM_OUTPUTS_PID` (47). Number of outputs of the string dispatch, an integer parameter.
    pub const STRING_NUM_OUTPUTS: i32 = 47;
    /// `ZL_DISPATCH_STRING_INDICES_PID` (48). The `u16` output index of each string, as a copy parameter of little-endian bytes.
    pub const STRING_INDICES: i32 = 48;
}

/// `mux_lengths` node (`zl_mux_lengths.h`).
pub mod mux_lengths {
    /// `ZL_MUX_LENGTHS_SPLIT_POINT_PID` (0). Split point of the length encoding, an integer parameter; 0 minimises the overflow count.
    pub const SPLIT_POINT: i32 = 0;
    /// `ZL_MUX_LENGTHS_MATCH_LENGTH_BIAS_PID` (1). Bias added to match lengths, an integer parameter.
    pub const MATCH_LENGTH_BIAS: i32 = 1;
}

/// Conversion nodes (`zl_conversion.h`).
pub mod conversion {
    /// `ZL_CONVERT_SERIAL_TO_STRUCT_SIZE_PID` (1). Struct size of `convert_serial_to_struct`, an integer parameter.
    pub const SERIAL_TO_STRUCT_SIZE: i32 = 1;
}

/// `dedup` node (`ZL_DEDUP_TRUST_IDENTICAL`, private header).
pub mod dedup {
    /// `ZL_DEDUP_TRUST_IDENTICAL` (9438). An integer parameter; 1 trusts that equal-hash inputs are identical.
    pub const TRUST_IDENTICAL: i32 = 9438;
}

/// Standard segmenters (`zl_segmenters.h`).
pub mod segmenters {
    /// `ZL_SEGMENT_SERIAL_CHUNK_BYTE_SIZE_PARAM` (7700). Chunk size of `segment_serial`, in bytes.
    pub const SERIAL_CHUNK_BYTE_SIZE: i32 = 7700;
    /// `SEGM_NUM_FROM_SERIAL_CHUNK_BYTE_SIZE_ID` (300). Chunk size of the `segment_numN_from_serial` graphs, in bytes.
    pub const NUM_FROM_SERIAL_CHUNK_BYTE_SIZE: i32 = 300;
    /// `SEGM_NUM_FROM_SERIAL_ELT_WIDTH_ID` (301). Element width of the `segment_numN_from_serial` graphs.
    pub const NUM_FROM_SERIAL_ELT_WIDTH: i32 = 301;
}

/// `simple_data_description_language` graph (`zl_sddl.h`).
pub mod sddl {
    /// `ZL_SDDL_DESCRIPTION_PID` (522). The compiled description, a copy parameter.
    pub const DESCRIPTION: i32 = 522;
}

/// `simple_data_description_language_v2` graph (`zl_sddl2.h`).
pub mod sddl2 {
    /// `SDDL2_BYTECODE_PARAM` (7685). The bytecode, a copy parameter.
    pub const BYTECODE: i32 = 7685;
    /// `SDDL2_CHUNK_BYTE_SIZE_PARAM` (7686). Target chunk size in bytes; 0 takes 16 MiB.
    pub const CHUNK_BYTE_SIZE: i32 = 7686;
}

/// `clustering` graph (`zl_clustering.h`).
pub mod clustering {
    /// `ZL_CLUSTERING_TAG_METADATA_ID` (0). Integer metadata (not a local parameter) carrying an input's tag.
    pub const TAG_METADATA_ID: i32 = 0;
    /// `ZL_GENERIC_CLUSTERING_CONFIG_ID` (315). The CBOR configuration, a copy parameter.
    pub const CONFIG: i32 = 315;
}
