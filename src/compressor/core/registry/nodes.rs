//! The standard node table (`ER_standardNodes`, encoder_registry.c).
//!
//! Generated once from the reference registry and then maintained by hand:
//! every row carries the node's ID (the table index of `ER_standardNodes`),
//! its wire codec ID, supported format versions, port types and default
//! local parameters. Each row's `encoder` is the node's
//! [`Encode`](super::super::exec::Encode) implementation behind its feature
//! gate (`DISABLED` when the codec's Cargo feature is off).

use super::super::codecs;
use super::NodeSpec;
use crate::DataType as Type;

/// Every standard node, sorted by ID.
pub(crate) static STANDARD_NODES: [(u32, NodeSpec); 85] = [
    (
        2,
        NodeSpec {
            name: "delta_int",
            codec: 1,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "delta") {
                &codecs::numeric::DELTA_INT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        3,
        NodeSpec {
            name: "transpose_split",
            codec: 4,
            min_version: 11,
            max_version: 27,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[],
            variable: &[Type::Serial],
            defaults: &[],
            encoder: if cfg!(feature = "transpose") {
                &codecs::structure::TRANSPOSE_SPLIT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        4,
        NodeSpec {
            name: "zigzag",
            codec: 3,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "zigzag") {
                &codecs::numeric::ZIGZAG
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        5,
        NodeSpec {
            name: "dispatchN_byTag",
            codec: 42,
            min_version: 9,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Numeric],
            variable: &[Type::Serial],
            defaults: &[],
            encoder: if cfg!(feature = "dispatch") {
                &codecs::structure::DISPATCH_N_BY_TAG
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        6,
        NodeSpec {
            name: "float32_deconstruct",
            codec: 33,
            min_version: 4,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Struct, Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "float-deconstruct") {
                &codecs::numeric::FLOAT32_DECONSTRUCT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        7,
        NodeSpec {
            name: "bfloat16_deconstruct",
            codec: 33,
            min_version: 5,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Struct, Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "float-deconstruct") {
                &codecs::numeric::BFLOAT16_DECONSTRUCT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        8,
        NodeSpec {
            name: "float16_deconstruct",
            codec: 33,
            min_version: 5,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Struct, Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "float-deconstruct") {
                &codecs::numeric::FLOAT16_DECONSTRUCT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        9,
        NodeSpec {
            name: "field_lz",
            codec: 24,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[
                Type::Struct,
                Type::Struct,
                Type::Numeric,
                Type::Numeric,
                Type::Numeric,
            ],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "field-lz") {
                &codecs::FIELD_LZ
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        10,
        NodeSpec {
            name: "convert_struct_to_serial",
            codec: 6,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_STRUCT_TO_SERIAL
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        11,
        NodeSpec {
            name: "convert_num_to_struct_le",
            codec: 8,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Struct],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_NUM_TO_STRUCT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        12,
        NodeSpec {
            name: "convert_num_to_serial_le",
            codec: 10,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_NUM_TO_SERIAL
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        13,
        NodeSpec {
            name: "convert_serial_to_struct",
            codec: 5,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Struct],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_SERIAL_TO_STRUCT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        14,
        NodeSpec {
            name: "convert_serial_to_struct2",
            codec: 5,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Struct],
            variable: &[],
            defaults: &[(1, 2)],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_SERIAL_TO_STRUCT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        15,
        NodeSpec {
            name: "convert_serial_to_struct4",
            codec: 5,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Struct],
            variable: &[],
            defaults: &[(1, 4)],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_SERIAL_TO_STRUCT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        16,
        NodeSpec {
            name: "convert_serial_to_struct8",
            codec: 5,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Struct],
            variable: &[],
            defaults: &[(1, 8)],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_SERIAL_TO_STRUCT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        17,
        NodeSpec {
            name: "convert_struct_to_num_le",
            codec: 7,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_STRUCT_TO_NUM_LE
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        18,
        NodeSpec {
            name: "convert_struct_to_num_be",
            codec: 13,
            min_version: 21,
            max_version: 27,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_STRUCT_TO_NUM_BE
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        19,
        NodeSpec {
            name: "convert_serial_to_num8",
            codec: 9,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_SERIAL_TO_NUM8
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        20,
        NodeSpec {
            name: "convert_serial_to_num_le16",
            codec: 9,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_SERIAL_TO_NUM_LE16
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        21,
        NodeSpec {
            name: "convert_serial_to_num_le32",
            codec: 9,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_SERIAL_TO_NUM_LE32
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        22,
        NodeSpec {
            name: "convert_serial_to_num_le64",
            codec: 9,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_SERIAL_TO_NUM_LE64
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        23,
        NodeSpec {
            name: "convert_serial_to_num_be16",
            codec: 14,
            min_version: 21,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_SERIAL_TO_NUM_BE16
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        24,
        NodeSpec {
            name: "convert_serial_to_num_be32",
            codec: 14,
            min_version: 21,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_SERIAL_TO_NUM_BE32
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        25,
        NodeSpec {
            name: "convert_serial_to_num_be64",
            codec: 14,
            min_version: 21,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::CONVERT_SERIAL_TO_NUM_BE64
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        26,
        NodeSpec {
            name: "separate_string_components",
            codec: 12,
            min_version: 10,
            max_version: 27,
            inputs: &[Type::String],
            variable_input: false,
            singletons: &[Type::Serial, Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::SEPARATE_STRING_COMPONENTS
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        27,
        NodeSpec {
            name: "bitunpack",
            codec: 34,
            min_version: 6,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "bitunpack") {
                &codecs::BITUNPACK
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        28,
        NodeSpec {
            name: "range_pack",
            codec: 35,
            min_version: 8,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "range-pack") {
                &codecs::numeric::RANGE_PACK
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        29,
        NodeSpec {
            name: "merge_sorted",
            codec: 43,
            min_version: 9,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "merge-sorted") {
                &codecs::numeric::MERGE_SORTED
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        30,
        NodeSpec {
            name: "prefix",
            codec: 46,
            min_version: 11,
            max_version: 27,
            inputs: &[Type::String],
            variable_input: false,
            singletons: &[Type::String, Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "prefix") {
                &codecs::structure::PREFIX
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        31,
        NodeSpec {
            name: "divide_by",
            codec: 53,
            min_version: 16,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "divide-by") {
                &codecs::numeric::DIVIDE_BY
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        32,
        NodeSpec {
            name: "dispatch_string",
            codec: 54,
            min_version: 16,
            max_version: 27,
            inputs: &[Type::String],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[Type::String],
            defaults: &[],
            encoder: if cfg!(feature = "dispatch") {
                &codecs::structure::DISPATCH_STRING
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        33,
        NodeSpec {
            name: "concat_serial",
            codec: 55,
            min_version: 16,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: true,
            singletons: &[Type::Numeric, Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "concat") {
                &codecs::structure::CONCAT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        34,
        NodeSpec {
            name: "concat_num",
            codec: 57,
            min_version: 17,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: true,
            singletons: &[Type::Numeric, Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "concat") {
                &codecs::structure::CONCAT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        35,
        NodeSpec {
            name: "concat_struct",
            codec: 58,
            min_version: 17,
            max_version: 27,
            inputs: &[Type::Struct],
            variable_input: true,
            singletons: &[Type::Numeric, Type::Struct],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "concat") {
                &codecs::structure::CONCAT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        36,
        NodeSpec {
            name: "concat_string",
            codec: 59,
            min_version: 18,
            max_version: 27,
            inputs: &[Type::String],
            variable_input: true,
            singletons: &[Type::Numeric, Type::String],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "concat") {
                &codecs::structure::CONCAT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        37,
        NodeSpec {
            name: "dedup_num",
            codec: 56,
            min_version: 16,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: true,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "dedup") {
                &codecs::structure::DEDUP_NUM
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        38,
        NodeSpec {
            name: "parse_int",
            codec: 60,
            min_version: 19,
            max_version: 27,
            inputs: &[Type::String],
            variable_input: false,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "parse-int") {
                &codecs::structure::PARSE_INT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        39,
        NodeSpec {
            name: "interleave_string",
            codec: 61,
            min_version: 20,
            max_version: 27,
            inputs: &[Type::String],
            variable_input: true,
            singletons: &[Type::String],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "interleave") {
                &codecs::structure::INTERLEAVE
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        40,
        NodeSpec {
            name: "tokenize_struct",
            codec: 36,
            min_version: 8,
            max_version: 27,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[Type::Struct, Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "tokenize") {
                &codecs::structure::TOKENIZE
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        41,
        NodeSpec {
            name: "tokenize_numeric",
            codec: 37,
            min_version: 8,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "tokenize") {
                &codecs::structure::TOKENIZE
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        42,
        NodeSpec {
            name: "tokenize_string",
            codec: 38,
            min_version: 11,
            max_version: 27,
            inputs: &[Type::String],
            variable_input: false,
            singletons: &[Type::String, Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "tokenize") {
                &codecs::structure::TOKENIZE_STRING
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        43,
        NodeSpec {
            name: "quantize_offsets",
            codec: 25,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "quantize") {
                &codecs::QUANTIZE_OFFSETS
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        44,
        NodeSpec {
            name: "quantize_lengths",
            codec: 26,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "quantize") {
                &codecs::QUANTIZE_LENGTHS
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        45,
        NodeSpec {
            name: "bitsplit_top8",
            codec: 63,
            min_version: 24,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[],
            variable: &[Type::Numeric],
            defaults: &[],
            encoder: if cfg!(feature = "bitsplit") {
                &codecs::numeric::BITSPLIT_TOP8
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        46,
        NodeSpec {
            name: "bitsplit_fp",
            codec: 63,
            min_version: 24,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[],
            variable: &[Type::Numeric],
            defaults: &[],
            encoder: if cfg!(feature = "bitsplit") {
                &codecs::numeric::BITSPLIT_FP
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        47,
        NodeSpec {
            name: "bitsplit_bf16",
            codec: 63,
            min_version: 24,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[],
            variable: &[Type::Numeric],
            defaults: &[],
            encoder: if cfg!(feature = "bitsplit") {
                &codecs::numeric::BITSPLIT_BF16
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        48,
        NodeSpec {
            name: "partition",
            codec: 64,
            min_version: 24,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "partition") {
                &codecs::numeric::PARTITION
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        49,
        NodeSpec {
            name: "split_byrange",
            codec: 48,
            min_version: 24,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[],
            variable: &[Type::Numeric],
            defaults: &[],
            encoder: if cfg!(feature = "split") {
                &codecs::structure::SPLIT_BYRANGE
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        50,
        NodeSpec {
            name: "sentinel_byte",
            codec: 18,
            min_version: 24,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "sentinel") {
                &codecs::numeric::SENTINEL_BYTE
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        51,
        NodeSpec {
            name: "sentinel_num",
            codec: 18,
            min_version: 24,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "sentinel") {
                &codecs::numeric::SENTINEL_NUM
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        52,
        NodeSpec {
            name: "lz",
            codec: 19,
            min_version: 24,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Serial, Type::Numeric, Type::Numeric, Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "lz") {
                &codecs::LZ
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        53,
        NodeSpec {
            name: "mux_lengths",
            codec: 65,
            min_version: 24,
            max_version: 27,
            inputs: &[Type::Numeric, Type::Numeric],
            variable_input: false,
            singletons: &[Type::Serial, Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "mux-lengths") {
                &codecs::MUX_LENGTHS
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        54,
        NodeSpec {
            name: "sparse_num",
            codec: 66,
            min_version: 26,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "sparse-num") {
                &codecs::numeric::SPARSE_NUM
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        55,
        NodeSpec {
            name: "sparse_num_auto",
            codec: 66,
            min_version: 26,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "sparse-num") {
                &codecs::numeric::SPARSE_NUM_AUTO
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        57,
        NodeSpec {
            name: "set_string_lens",
            codec: 11,
            min_version: 10,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::String],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "conversion") {
                &codecs::conversion::SET_STRING_LENS
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        58,
        NodeSpec {
            name: "fse_v2",
            codec: 49,
            min_version: 15,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "entropy") {
                &codecs::FSE_V2
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        59,
        NodeSpec {
            name: "huffman_v2",
            codec: 50,
            min_version: 15,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "entropy") {
                &codecs::HUFFMAN_V2
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        60,
        NodeSpec {
            name: "huffman_struct_v2",
            codec: 51,
            min_version: 15,
            max_version: 27,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "entropy") {
                &codecs::HUFFMAN_STRUCT_V2
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        61,
        NodeSpec {
            name: "fse_ncount",
            codec: 52,
            min_version: 15,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "entropy") {
                &codecs::FSE_NCOUNT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        62,
        NodeSpec {
            name: "zstd",
            codec: 22,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "zstd") {
                &codecs::ZSTD
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        63,
        NodeSpec {
            name: "bitpack_serial",
            codec: 27,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "bitpack") {
                &codecs::BITPACK
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        64,
        NodeSpec {
            name: "bitpack_int",
            codec: 28,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "bitpack") {
                &codecs::BITPACK
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        65,
        NodeSpec {
            name: "flatpack",
            codec: 29,
            min_version: 3,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Serial, Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "flatpack") {
                &codecs::FLATPACK
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        66,
        NodeSpec {
            name: "splitN",
            codec: 40,
            min_version: 9,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[],
            variable: &[Type::Serial],
            defaults: &[],
            encoder: if cfg!(feature = "split") {
                &codecs::structure::SPLIT_N
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        67,
        NodeSpec {
            name: "splitN_struct",
            codec: 47,
            min_version: 15,
            max_version: 27,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[],
            variable: &[Type::Struct],
            defaults: &[],
            encoder: if cfg!(feature = "split") {
                &codecs::structure::SPLIT_N
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        68,
        NodeSpec {
            name: "splitN_num",
            codec: 48,
            min_version: 15,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[],
            variable: &[Type::Numeric],
            defaults: &[],
            encoder: if cfg!(feature = "split") {
                &codecs::structure::SPLIT_N
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        69,
        NodeSpec {
            name: "split_by_struct",
            codec: 41,
            min_version: 9,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[],
            variable: &[Type::Struct],
            defaults: &[],
            encoder: if cfg!(feature = "split-by-struct") {
                &codecs::structure::SPLIT_BY_STRUCT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        70,
        NodeSpec {
            name: "constant_serial",
            codec: 44,
            min_version: 11,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "constant") {
                &codecs::CONSTANT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        71,
        NodeSpec {
            name: "constant_fixed",
            codec: 45,
            min_version: 11,
            max_version: 27,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[Type::Struct],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "constant") {
                &codecs::CONSTANT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        72,
        NodeSpec {
            name: "tokenize_sorted",
            codec: 37,
            min_version: 8,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Numeric],
            variable: &[],
            defaults: &[(0, 1)],
            encoder: if cfg!(feature = "tokenize") {
                &codecs::structure::TOKENIZE
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        73,
        NodeSpec {
            name: "tokenize_string_sorted",
            codec: 38,
            min_version: 11,
            max_version: 27,
            inputs: &[Type::String],
            variable_input: false,
            singletons: &[Type::String, Type::Numeric],
            variable: &[],
            defaults: &[(0, 1)],
            encoder: if cfg!(feature = "tokenize") {
                &codecs::structure::TOKENIZE_STRING
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        74,
        NodeSpec {
            name: "dedup_num_trusted",
            codec: 56,
            min_version: 16,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: true,
            singletons: &[Type::Numeric],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "dedup") {
                &codecs::structure::DEDUP_NUM_TRUSTED
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        75,
        NodeSpec {
            name: "bit_split",
            codec: 63,
            min_version: 24,
            max_version: 27,
            inputs: &[Type::Numeric],
            variable_input: false,
            singletons: &[],
            variable: &[Type::Numeric],
            defaults: &[],
            encoder: if cfg!(feature = "bitsplit") {
                &codecs::numeric::BITSPLIT
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        76,
        NodeSpec {
            name: "rolz_deprecated",
            codec: 20,
            min_version: 3,
            max_version: 12,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: &codecs::LEGACY,
        },
    ),
    (
        77,
        NodeSpec {
            name: "fast_lz_deprecated",
            codec: 21,
            min_version: 3,
            max_version: 12,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: &codecs::LEGACY,
        },
    ),
    (
        78,
        NodeSpec {
            name: "fse_deprecated",
            codec: 15,
            min_version: 3,
            max_version: 14,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: &codecs::LEGACY,
        },
    ),
    (
        79,
        NodeSpec {
            name: "huffman_deprecated",
            codec: 16,
            min_version: 3,
            max_version: 14,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: &codecs::LEGACY,
        },
    ),
    (
        80,
        NodeSpec {
            name: "huffman_fixed_deprecated",
            codec: 17,
            min_version: 3,
            max_version: 14,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: &codecs::LEGACY,
        },
    ),
    (
        81,
        NodeSpec {
            name: "zstd_fixed_deprecated",
            codec: 23,
            min_version: 3,
            max_version: 10,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: &codecs::LEGACY,
        },
    ),
    (
        82,
        NodeSpec {
            name: "transpose_deprecated",
            codec: 2,
            min_version: 3,
            max_version: 10,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[Type::Struct],
            variable: &[],
            defaults: &[],
            encoder: &codecs::LEGACY,
        },
    ),
    (
        83,
        NodeSpec {
            name: "transpose_split2_deprecated",
            codec: 30,
            min_version: 3,
            max_version: 10,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[Type::Serial, Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: &codecs::LEGACY,
        },
    ),
    (
        84,
        NodeSpec {
            name: "transpose_split4_deprecated",
            codec: 31,
            min_version: 3,
            max_version: 10,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[Type::Serial, Type::Serial, Type::Serial, Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: &codecs::LEGACY,
        },
    ),
    (
        85,
        NodeSpec {
            name: "transpose_split8_deprecated",
            codec: 32,
            min_version: 3,
            max_version: 10,
            inputs: &[Type::Struct],
            variable_input: false,
            singletons: &[
                Type::Serial,
                Type::Serial,
                Type::Serial,
                Type::Serial,
                Type::Serial,
                Type::Serial,
                Type::Serial,
                Type::Serial,
            ],
            variable: &[],
            defaults: &[],
            encoder: &codecs::LEGACY,
        },
    ),
    (
        86,
        NodeSpec {
            name: "lz4",
            codec: 62,
            min_version: 23,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "lz4") {
                &codecs::LZ4
            } else {
                &codecs::DISABLED
            },
        },
    ),
    (
        87,
        NodeSpec {
            name: "pivco_huffman",
            codec: 67,
            min_version: 27,
            max_version: 27,
            inputs: &[Type::Serial],
            variable_input: false,
            singletons: &[Type::Numeric, Type::Serial],
            variable: &[],
            defaults: &[],
            encoder: if cfg!(feature = "pivco-huffman") {
                &codecs::PIVCO_HUFFMAN
            } else {
                &codecs::DISABLED
            },
        },
    ),
];

/// The standard node `index`, if there is one (`ER_standardNodes[index]`).
pub(crate) fn standard(index: u32) -> Option<&'static NodeSpec> {
    let position = STANDARD_NODES
        .binary_search_by_key(&index, |&(id, _)| id)
        .ok()?;
    Some(&STANDARD_NODES[position].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_names_the_reference_nodes() {
        assert!(STANDARD_NODES.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(
            standard(62).map(|spec| (spec.name, spec.codec)),
            Some(("zstd", 22))
        );
        assert_eq!(standard(57).map(|spec| spec.codec), Some(11));
        assert!(standard(1).is_none() && standard(56).is_none() && standard(88).is_none());
        assert_eq!(standard(15).unwrap().defaults, &[(1, 4)]);
    }
}
