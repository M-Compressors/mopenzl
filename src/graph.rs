//! Compression graphs: standard graphs and nodes, parameterization, and
//! user-defined selectors, function graphs and segmenters.
//!
//! A frame records the codecs that produced it, not the graph; the graph is
//! only the recipe the encoder follows. [`StandardGraph`] and
//! [`StandardNode`] name the reference's registry with its numeric IDs. A
//! [`GraphBuilder`] derives new graphs from them (a node followed by
//! successors, parameterized copies, selectors and segmenters written in
//! Rust) and [`GraphBuilder::build`] freezes the result into a cheap-to-share
//! [`CompressorGraph`] for [`CompressorBuilder::with_graph`](crate::CompressorBuilder::with_graph).
//!
//! Standard nodes whose codecs are not ported yet exist in the registry but
//! fail when run: the engine reacts as the reference does to a failing codec
//! (a permissive compression falls back to `compress_generic`, a chunk that
//! cannot be built is an error).
//!
//! # Examples
//!
//! Store typed data, then Zstandard-compress serial data:
//!
//! ```
//! use mopenzl::graph::{GraphBuilder, StandardGraph};
//! use mopenzl::{Compressor, EncoderConfig};
//!
//! let graph = GraphBuilder::new().build(StandardGraph::Store)?;
//! let mut compressor = Compressor::builder(EncoderConfig::default())
//!     .with_graph(graph)
//!     .build()?;
//! let frame = compressor.compress(b"hello world, hello world")?;
//! # #[cfg(feature = "decompression")]
//! assert_eq!(mopenzl::Decompressor::default().decompress(&frame)?, b"hello world, hello world");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use alloc::borrow::Cow;

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::ops::BitOr;
use core::sync::atomic::{AtomicU32, Ordering};

use thiserror::Error;

use crate::DataType;
use crate::compressor::core as engine;
use crate::compressor::core::data::{Bytes, Stream, type_mask};
use crate::compressor::core::error::ExecError;
use crate::compressor::core::exec::{Edge as CoreEdge, GraphCx, SegmenterCx, SelectorCx};
use crate::compressor::core::params::{LocalParams as CoreParams, RefParam};
use crate::compressor::core::registry::dict::LoadError;
use crate::compressor::core::registry::{
    self as reg, Callback, GraphDef, GraphKind, NodeDef, Registry, RuntimeParams,
};
mod config;
pub mod params;

pub use config::{ClusteringConfig, ConfigError, GbtModel, GbtNode, MlSelectorConfig};

/// The integer metadata ID that carries an input's tag for the clustering
/// graph (`ZL_CLUSTERING_TAG_METADATA_ID`).
pub const CLUSTERING_TAG_METADATA_ID: i32 = params::clustering::TAG_METADATA_ID;

/// The local parameter ID under which the clustering graph reads its CBOR
/// configuration (`ZL_GENERIC_CLUSTERING_CONFIG_ID`).
const CLUSTERING_CONFIG_PARAM: i32 = engine::graphs::clustering::CONFIG_PARAM_ID;

use crate::dict::{DictBundle, DictError, DictId};

/// A numeric ID that names no graph or node here.
#[derive(Error, Debug, Clone, Copy, Eq, PartialEq)]
#[error("{0} is not the ID of a supported standard graph or node")]
pub struct UnknownId(pub u32);

/// A node of the reference registry, by its `ZL_StandardNodeID`. IDs from 57 on are private in the reference but stable here.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
#[repr(u32)]
pub enum StandardNode {
    /// `delta_int` (2).
    DeltaInt = 2,
    /// `transpose_split` (3).
    TransposeSplit = 3,
    /// `zigzag` (4).
    Zigzag = 4,
    /// `dispatchN_byTag` (5).
    DispatchNByTag = 5,
    /// `float32_deconstruct` (6).
    Float32Deconstruct = 6,
    /// `bfloat16_deconstruct` (7).
    Bfloat16Deconstruct = 7,
    /// `float16_deconstruct` (8).
    Float16Deconstruct = 8,
    /// `field_lz` (9).
    FieldLz = 9,
    /// `convert_struct_to_serial` (10).
    ConvertStructToSerial = 10,
    /// `convert_num_to_struct_le` (11).
    ConvertNumToStructLe = 11,
    /// `convert_num_to_serial_le` (12).
    ConvertNumToSerialLe = 12,
    /// `convert_serial_to_struct` (13).
    ConvertSerialToStruct = 13,
    /// `convert_serial_to_struct2` (14).
    ConvertSerialToStruct2 = 14,
    /// `convert_serial_to_struct4` (15).
    ConvertSerialToStruct4 = 15,
    /// `convert_serial_to_struct8` (16).
    ConvertSerialToStruct8 = 16,
    /// `convert_struct_to_num_le` (17).
    ConvertStructToNumLe = 17,
    /// `convert_struct_to_num_be` (18).
    ConvertStructToNumBe = 18,
    /// `convert_serial_to_num8` (19).
    ConvertSerialToNum8 = 19,
    /// `convert_serial_to_num_le16` (20).
    ConvertSerialToNumLe16 = 20,
    /// `convert_serial_to_num_le32` (21).
    ConvertSerialToNumLe32 = 21,
    /// `convert_serial_to_num_le64` (22).
    ConvertSerialToNumLe64 = 22,
    /// `convert_serial_to_num_be16` (23).
    ConvertSerialToNumBe16 = 23,
    /// `convert_serial_to_num_be32` (24).
    ConvertSerialToNumBe32 = 24,
    /// `convert_serial_to_num_be64` (25).
    ConvertSerialToNumBe64 = 25,
    /// `separate_string_components` (26).
    SeparateStringComponents = 26,
    /// `bitunpack` (27).
    Bitunpack = 27,
    /// `range_pack` (28).
    RangePack = 28,
    /// `merge_sorted` (29).
    MergeSorted = 29,
    /// `prefix` (30).
    Prefix = 30,
    /// `divide_by` (31).
    DivideBy = 31,
    /// `dispatch_string` (32).
    DispatchString = 32,
    /// `concat_serial` (33).
    ConcatSerial = 33,
    /// `concat_num` (34).
    ConcatNum = 34,
    /// `concat_struct` (35).
    ConcatStruct = 35,
    /// `concat_string` (36).
    ConcatString = 36,
    /// `dedup_num` (37).
    DedupNum = 37,
    /// `parse_int` (38).
    ParseInt = 38,
    /// `interleave_string` (39).
    InterleaveString = 39,
    /// `tokenize_struct` (40).
    TokenizeStruct = 40,
    /// `tokenize_numeric` (41).
    TokenizeNumeric = 41,
    /// `tokenize_string` (42).
    TokenizeString = 42,
    /// `quantize_offsets` (43).
    QuantizeOffsets = 43,
    /// `quantize_lengths` (44).
    QuantizeLengths = 44,
    /// `bitsplit_top8` (45).
    BitsplitTop8 = 45,
    /// `bitsplit_fp` (46).
    BitsplitFp = 46,
    /// `bitsplit_bf16` (47).
    BitsplitBf16 = 47,
    /// `partition` (48).
    Partition = 48,
    /// `split_byrange` (49).
    SplitByrange = 49,
    /// `sentinel_byte` (50).
    SentinelByte = 50,
    /// `sentinel_num` (51).
    SentinelNum = 51,
    /// `lz` (52).
    Lz = 52,
    /// `mux_lengths` (53).
    MuxLengths = 53,
    /// `sparse_num` (54).
    SparseNum = 54,
    /// `sparse_num_auto` (55).
    SparseNumAuto = 55,
    /// `set_string_lens` (57).
    SetStringLens = 57,
    /// `fse_v2` (58).
    FseV2 = 58,
    /// `huffman_v2` (59).
    HuffmanV2 = 59,
    /// `huffman_struct_v2` (60).
    HuffmanStructV2 = 60,
    /// `fse_ncount` (61).
    FseNcount = 61,
    /// `zstd` (62).
    Zstd = 62,
    /// `bitpack_serial` (63).
    BitpackSerial = 63,
    /// `bitpack_int` (64).
    BitpackInt = 64,
    /// `flatpack` (65).
    Flatpack = 65,
    /// `splitN` (66).
    SplitN = 66,
    /// `splitN_struct` (67).
    SplitNStruct = 67,
    /// `splitN_num` (68).
    SplitNNum = 68,
    /// `split_by_struct` (69).
    SplitByStruct = 69,
    /// `constant_serial` (70).
    ConstantSerial = 70,
    /// `constant_fixed` (71).
    ConstantFixed = 71,
    /// `tokenize_sorted` (72).
    TokenizeSorted = 72,
    /// `tokenize_string_sorted` (73).
    TokenizeStringSorted = 73,
    /// `dedup_num_trusted` (74).
    DedupNumTrusted = 74,
    /// `bitSplit` (75).
    BitSplit = 75,
    /// `lz4` (86).
    Lz4 = 86,
    /// `pivco_huffman` (87).
    PivcoHuffman = 87,
}

impl TryFrom<u32> for StandardNode {
    type Error = UnknownId;

    /// Looks the node up by its numeric ID in the reference.
    ///
    /// # Errors
    ///
    /// [`UnknownId`] for IDs that name no node supported here.
    fn try_from(id: u32) -> Result<Self, Self::Error> {
        Ok(match id {
            2 => Self::DeltaInt,
            3 => Self::TransposeSplit,
            4 => Self::Zigzag,
            5 => Self::DispatchNByTag,
            6 => Self::Float32Deconstruct,
            7 => Self::Bfloat16Deconstruct,
            8 => Self::Float16Deconstruct,
            9 => Self::FieldLz,
            10 => Self::ConvertStructToSerial,
            11 => Self::ConvertNumToStructLe,
            12 => Self::ConvertNumToSerialLe,
            13 => Self::ConvertSerialToStruct,
            14 => Self::ConvertSerialToStruct2,
            15 => Self::ConvertSerialToStruct4,
            16 => Self::ConvertSerialToStruct8,
            17 => Self::ConvertStructToNumLe,
            18 => Self::ConvertStructToNumBe,
            19 => Self::ConvertSerialToNum8,
            20 => Self::ConvertSerialToNumLe16,
            21 => Self::ConvertSerialToNumLe32,
            22 => Self::ConvertSerialToNumLe64,
            23 => Self::ConvertSerialToNumBe16,
            24 => Self::ConvertSerialToNumBe32,
            25 => Self::ConvertSerialToNumBe64,
            26 => Self::SeparateStringComponents,
            27 => Self::Bitunpack,
            28 => Self::RangePack,
            29 => Self::MergeSorted,
            30 => Self::Prefix,
            31 => Self::DivideBy,
            32 => Self::DispatchString,
            33 => Self::ConcatSerial,
            34 => Self::ConcatNum,
            35 => Self::ConcatStruct,
            36 => Self::ConcatString,
            37 => Self::DedupNum,
            38 => Self::ParseInt,
            39 => Self::InterleaveString,
            40 => Self::TokenizeStruct,
            41 => Self::TokenizeNumeric,
            42 => Self::TokenizeString,
            43 => Self::QuantizeOffsets,
            44 => Self::QuantizeLengths,
            45 => Self::BitsplitTop8,
            46 => Self::BitsplitFp,
            47 => Self::BitsplitBf16,
            48 => Self::Partition,
            49 => Self::SplitByrange,
            50 => Self::SentinelByte,
            51 => Self::SentinelNum,
            52 => Self::Lz,
            53 => Self::MuxLengths,
            54 => Self::SparseNum,
            55 => Self::SparseNumAuto,
            57 => Self::SetStringLens,
            58 => Self::FseV2,
            59 => Self::HuffmanV2,
            60 => Self::HuffmanStructV2,
            61 => Self::FseNcount,
            62 => Self::Zstd,
            63 => Self::BitpackSerial,
            64 => Self::BitpackInt,
            65 => Self::Flatpack,
            66 => Self::SplitN,
            67 => Self::SplitNStruct,
            68 => Self::SplitNNum,
            69 => Self::SplitByStruct,
            70 => Self::ConstantSerial,
            71 => Self::ConstantFixed,
            72 => Self::TokenizeSorted,
            73 => Self::TokenizeStringSorted,
            74 => Self::DedupNumTrusted,
            75 => Self::BitSplit,
            86 => Self::Lz4,
            87 => Self::PivcoHuffman,
            _ => return Err(UnknownId(id)),
        })
    }
}

impl From<StandardNode> for u32 {
    fn from(value: StandardNode) -> Self {
        value as Self
    }
}

/// A graph of the reference registry, by its `ZL_StandardGraphID`. IDs from 32 on are private in the reference but stable here.
///
/// # Successors
///
/// Most graphs are self-contained. A few take successor graphs (and, for the
/// splits and clustering, nodes) as the *custom graphs* and *custom nodes* of
/// a [`GraphParams`]; their variants say so below, and a graph that is not
/// mentioned needs none. Run without what it needs, such a graph fails when it
/// runs (a permissive compression then falls back to `compress_generic`). The
/// [`GraphBuilder`] helpers named in each variant check what they can.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
#[repr(u32)]
pub enum StandardGraph {
    /// `store` (2).
    Store = 2,
    /// `fse` (3).
    Fse = 3,
    /// `huffman` (4).
    Huffman = 4,
    /// `huffman_huf0` (5).
    HuffmanHuf0 = 5,
    /// `huffman_pivco` (6).
    HuffmanPivco = 6,
    /// `entropy` (7).
    Entropy = 7,
    /// `constant` (8).
    Constant = 8,
    /// `zstd` (9).
    Zstd = 9,
    /// `bitpack` (10).
    Bitpack = 10,
    /// `flatpack` (11).
    Flatpack = 11,
    /// `field_lz` (12).
    ///
    /// Needs no successors. Custom graphs are optional overrides: the integer
    /// parameters [`params::field_lz`]`::*_GRAPH_OVERRIDE_INDEX` pick one of the custom graphs for
    /// the literals, tokens, offsets and the two extra-length streams.
    FieldLz = 12,
    /// `compress_generic` (13).
    CompressGeneric = 13,
    /// `select_generic_lz_backend` (14).
    SelectGenericLzBackend = 14,
    /// `segment_numeric` (15).
    ///
    /// Needs no successors: it cuts numeric input every 16 MiB and compresses
    /// each chunk with the default numeric graph.
    SegmentNumeric = 15,
    /// `select_numeric` (16).
    SelectNumeric = 16,
    /// `ml_selector` (17).
    ///
    /// Needs one successor per class of its model and the model itself as a
    /// materialized parameter; use [`GraphBuilder::ml_selector`] or
    /// [`GraphBuilder::untrained_ml_selector`]. Numeric input only.
    MlSelector = 17,
    /// `clustering` (18).
    ///
    /// Needs the CBOR configuration ([`params::clustering::CONFIG`]), the
    /// clustering codec nodes, and the successors the configuration indexes;
    /// without a successor for a tag it uses `compress_generic`. Use
    /// [`GraphBuilder::clustering`] or [`GraphBuilder::clustering_with_codecs`].
    Clustering = 18,
    /// `try_parse_int` (19).
    ///
    /// Needs exactly three successors, in this order: the integers (numeric,
    /// the parsed strings), the exceptions (strings that are not integers,
    /// or the whole input when none parses) and the dispatch indices
    /// (which strings were parsed). No builder helper; pass them to
    /// [`GraphBuilder::parameterize_graph`] with [`GraphParams::with_graphs`].
    TryParseInt = 19,
    /// `simple_data_description_language` (20).
    ///
    /// Needs exactly one successor, which receives every output of the
    /// dispatch node, and the compiled description
    /// ([`params::sddl::DESCRIPTION`]); use [`GraphBuilder::sddl`].
    SimpleDataDescriptionLanguage = 20,
    /// `simple_data_description_language_v2` (21).
    ///
    /// Takes no successor or one (default `compress_generic`) and needs the
    /// bytecode ([`params::sddl2::BYTECODE`]); use [`GraphBuilder::sddl2`].
    SimpleDataDescriptionLanguageV2 = 21,
    /// `lz4` (22).
    Lz4 = 22,
    /// `partition_bitpack` (23).
    PartitionBitpack = 23,
    /// `segment_num8_from_serial` (24).
    ///
    /// A segmenter: takes one successor, which receives each chunk as
    /// *serial* data (default `interpret_num8_compress`, which reinterprets
    /// it). Use [`GraphBuilder::num_from_serial_segmenter`].
    SegmentNum8FromSerial = 24,
    /// `segment_num16_from_serial` (25).
    ///
    /// As [`StandardGraph::SegmentNum8FromSerial`] with 2-byte elements
    /// (default `interpret_num16_compress`).
    SegmentNum16FromSerial = 25,
    /// `segment_num32_from_serial` (26).
    ///
    /// As [`StandardGraph::SegmentNum8FromSerial`] with 4-byte elements
    /// (default `interpret_num32_compress`).
    SegmentNum32FromSerial = 26,
    /// `segment_num64_from_serial` (27).
    ///
    /// As [`StandardGraph::SegmentNum8FromSerial`] with 8-byte elements
    /// (default `interpret_num64_compress`).
    SegmentNum64FromSerial = 27,
    /// `lz` (28).
    ///
    /// Needs no successors. Custom graphs are optional overrides picked by
    /// the integer parameters [`params::lz`]`::*_GRAPH_IDX`.
    Lz = 28,
    /// `segment_serial` (29).
    ///
    /// A segmenter: takes one successor for every chunk (default
    /// `compress_generic`). Use [`GraphBuilder::serial_segmenter`] to set the
    /// chunk size ([`params::segmenters::SERIAL_CHUNK_BYTE_SIZE`]).
    SegmentSerial = 29,
    /// `transformer_numeric` (30).
    TransformerNumeric = 30,
    /// `brute_force` (31).
    ///
    /// Needs one or more successors; it tries each that accepts the input
    /// and keeps the smallest, or stores the input when none wins (or none
    /// is given). Use [`GraphBuilder::brute_force`].
    BruteForce = 31,
    /// `serial_store` (1).
    SerialStore = 1,
    /// `store1` (32).
    Store1 = 32,
    /// `string_store` (33).
    StringStore = 33,
    /// `compress1` (34).
    Compress1 = 34,
    /// `serial_compress` (35).
    SerialCompress = 35,
    /// `struct_compress` (36).
    StructCompress = 36,
    /// `numeric_compress` (37).
    NumericCompress = 37,
    /// `string_compress` (38).
    StringCompress = 38,
    /// `string_separate_compress` (39).
    StringSeparateCompress = 39,
    /// `bitpack_serial` (40).
    BitpackSerial = 40,
    /// `bitpack_int` (41).
    BitpackInt = 41,
    /// `constant_serial` (42).
    ConstantSerial = 42,
    /// `constant_fixed` (43).
    ConstantFixed = 43,
    /// `fse_ncount` (44).
    FseNcount = 44,
    /// `field_lz_literals` (45).
    FieldLzLiterals = 45,
    /// `field_lz_literals_channel` (46).
    FieldLzLiteralsChannel = 46,
    /// `delta_huffman` (47).
    DeltaHuffman = 47,
    /// `delta_flatpack` (48).
    DeltaFlatpack = 48,
    /// `delta_zstd` (49).
    DeltaZstd = 49,
    /// `delta_huffman_internal` (50).
    DeltaHuffmanInternal = 50,
    /// `delta_flatpack_internal` (51).
    DeltaFlatpackInternal = 51,
    /// `delta_zstd_internal` (52).
    DeltaZstdInternal = 52,
    /// `delta_field_lz` (53).
    DeltaFieldLz = 53,
    /// `range_pack` (54).
    RangePack = 54,
    /// `range_pack_zstd` (55).
    RangePackZstd = 55,
    /// `tokenize_delta_field_lz` (56).
    TokenizeDeltaFieldLz = 56,
    /// `split_serial` (57).
    ///
    /// Needs exactly one custom node (the splitting node) and one custom
    /// graph per output of that node, in output order. No builder helper;
    /// use [`GraphBuilder::parameterize_graph`] with [`GraphParams::with_nodes`]
    /// and [`GraphParams::with_graphs`]. The same holds for
    /// [`StandardGraph::SplitStruct`], [`StandardGraph::SplitNumeric`] and
    /// [`StandardGraph::SplitString`], which differ in the input type.
    SplitSerial = 57,
    /// `split_struct` (58).
    SplitStruct = 58,
    /// `split_numeric` (59).
    SplitNumeric = 59,
    /// `split_string` (60).
    SplitString = 60,
    /// `sddl2_chunk` (61).
    ///
    /// Internal replay graph of SDDL2 (it needs the replay parameters that
    /// the SDDL2 segmenter sets); not meant to be used directly.
    Sddl2Chunk = 61,
    /// `n_to_n` (62).
    ///
    /// Needs exactly one successor per input: input `i` goes to successor
    /// `i`.
    NToN = 62,
    /// `merge_sorted` (63).
    ///
    /// Needs exactly two successors: the merge graph, used for sorted
    /// 4-byte input within the run limit, then the fallback for anything
    /// else.
    MergeSorted = 63,
    /// `transpose_split` (64).
    ///
    /// Needs exactly five successors: the deprecated width 1, 2, 4 and 8
    /// implementations, then the `transpose_split` node's graph (always used
    /// from format version 11).
    TransposeSplit = 64,
    /// `interpret_num8_compress` (65).
    InterpretNum8Compress = 65,
    /// `interpret_num16_compress` (66).
    InterpretNum16Compress = 66,
    /// `interpret_num32_compress` (67).
    InterpretNum32Compress = 67,
    /// `interpret_num64_compress` (68).
    InterpretNum64Compress = 68,
    /// `compress_small_lengths` (69).
    CompressSmallLengths = 69,
    /// `transformer_delta_int` (70).
    TransformerDeltaInt = 70,
    /// `transformer_range_pack` (71).
    TransformerRangePack = 71,
    /// `transformer_divide_by_gcd` (72).
    TransformerDivideByGcd = 72,
    /// `transformer_tokenize_numeric` (73).
    TransformerTokenizeNumeric = 73,
    /// `transformer_tokenize_numeric_sorted` (74).
    TransformerTokenizeNumericSorted = 74,
    /// `transformer_sparse_num` (75).
    TransformerSparseNum = 75,
    /// `transformer_static_core` (76).
    TransformerStaticCore = 76,
    /// `transformer_static_delta` (77).
    TransformerStaticDelta = 77,
    /// `transformer_static_range_pack` (78).
    TransformerStaticRangePack = 78,
    /// `transformer_static_range_pack_delta` (79).
    TransformerStaticRangePackDelta = 79,
    /// `transformer_static_zigzag` (80).
    TransformerStaticZigzag = 80,
    /// `transformer_static_gcd` (81).
    TransformerStaticGcd = 81,
    /// `transformer_static_index` (82).
    TransformerStaticIndex = 82,
    /// `transformer_static_tok_sorted` (83).
    TransformerStaticTokSorted = 83,
    /// `transformer_static_delta_tok` (84).
    TransformerStaticDeltaTok = 84,
    /// `transformer_static_tok_mono` (85).
    TransformerStaticTokMono = 85,
    /// `transformer_static_delta_tok_mono` (86).
    TransformerStaticDeltaTokMono = 86,
    /// `transformer_static_tok_mono_lz` (87).
    TransformerStaticTokMonoLz = 87,
    /// `transformer_static_delta_tok_mono_lz` (88).
    TransformerStaticDeltaTokMonoLz = 88,
    /// `transformer_static_fallback` (89).
    TransformerStaticFallback = 89,
    /// `transformer_numeric1` (90).
    TransformerNumeric1 = 90,
}

impl TryFrom<u32> for StandardGraph {
    type Error = UnknownId;

    /// Looks the graph up by its numeric ID in the reference.
    ///
    /// # Errors
    ///
    /// [`UnknownId`] for IDs that name no graph supported here.
    fn try_from(id: u32) -> Result<Self, Self::Error> {
        Ok(match id {
            2 => Self::Store,
            3 => Self::Fse,
            4 => Self::Huffman,
            5 => Self::HuffmanHuf0,
            6 => Self::HuffmanPivco,
            7 => Self::Entropy,
            8 => Self::Constant,
            9 => Self::Zstd,
            10 => Self::Bitpack,
            11 => Self::Flatpack,
            12 => Self::FieldLz,
            13 => Self::CompressGeneric,
            14 => Self::SelectGenericLzBackend,
            15 => Self::SegmentNumeric,
            16 => Self::SelectNumeric,
            17 => Self::MlSelector,
            18 => Self::Clustering,
            19 => Self::TryParseInt,
            20 => Self::SimpleDataDescriptionLanguage,
            21 => Self::SimpleDataDescriptionLanguageV2,
            22 => Self::Lz4,
            23 => Self::PartitionBitpack,
            24 => Self::SegmentNum8FromSerial,
            25 => Self::SegmentNum16FromSerial,
            26 => Self::SegmentNum32FromSerial,
            27 => Self::SegmentNum64FromSerial,
            28 => Self::Lz,
            29 => Self::SegmentSerial,
            30 => Self::TransformerNumeric,
            31 => Self::BruteForce,
            1 => Self::SerialStore,
            32 => Self::Store1,
            33 => Self::StringStore,
            34 => Self::Compress1,
            35 => Self::SerialCompress,
            36 => Self::StructCompress,
            37 => Self::NumericCompress,
            38 => Self::StringCompress,
            39 => Self::StringSeparateCompress,
            40 => Self::BitpackSerial,
            41 => Self::BitpackInt,
            42 => Self::ConstantSerial,
            43 => Self::ConstantFixed,
            44 => Self::FseNcount,
            45 => Self::FieldLzLiterals,
            46 => Self::FieldLzLiteralsChannel,
            47 => Self::DeltaHuffman,
            48 => Self::DeltaFlatpack,
            49 => Self::DeltaZstd,
            50 => Self::DeltaHuffmanInternal,
            51 => Self::DeltaFlatpackInternal,
            52 => Self::DeltaZstdInternal,
            53 => Self::DeltaFieldLz,
            54 => Self::RangePack,
            55 => Self::RangePackZstd,
            56 => Self::TokenizeDeltaFieldLz,
            57 => Self::SplitSerial,
            58 => Self::SplitStruct,
            59 => Self::SplitNumeric,
            60 => Self::SplitString,
            61 => Self::Sddl2Chunk,
            62 => Self::NToN,
            63 => Self::MergeSorted,
            64 => Self::TransposeSplit,
            65 => Self::InterpretNum8Compress,
            66 => Self::InterpretNum16Compress,
            67 => Self::InterpretNum32Compress,
            68 => Self::InterpretNum64Compress,
            69 => Self::CompressSmallLengths,
            70 => Self::TransformerDeltaInt,
            71 => Self::TransformerRangePack,
            72 => Self::TransformerDivideByGcd,
            73 => Self::TransformerTokenizeNumeric,
            74 => Self::TransformerTokenizeNumericSorted,
            75 => Self::TransformerSparseNum,
            76 => Self::TransformerStaticCore,
            77 => Self::TransformerStaticDelta,
            78 => Self::TransformerStaticRangePack,
            79 => Self::TransformerStaticRangePackDelta,
            80 => Self::TransformerStaticZigzag,
            81 => Self::TransformerStaticGcd,
            82 => Self::TransformerStaticIndex,
            83 => Self::TransformerStaticTokSorted,
            84 => Self::TransformerStaticDeltaTok,
            85 => Self::TransformerStaticTokMono,
            86 => Self::TransformerStaticDeltaTokMono,
            87 => Self::TransformerStaticTokMonoLz,
            88 => Self::TransformerStaticDeltaTokMonoLz,
            89 => Self::TransformerStaticFallback,
            90 => Self::TransformerNumeric1,
            _ => return Err(UnknownId(id)),
        })
    }
}

impl From<StandardGraph> for u32 {
    fn from(value: StandardGraph) -> Self {
        value as Self
    }
}

macro_rules! id_conversions {
    ($public:ident, $standard:ident, $core:ident) => {
        impl From<$standard> for $public {
            fn from(value: $standard) -> Self {
                Self(reg::$core::std(u32::from(value)))
            }
        }
    };
}

/// A graph of a compressor: a standard one or one a [`GraphBuilder`]
/// created. IDs only mean something to the builder that created them.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct GraphId(reg::GraphId);

/// A node of a compressor: a standard one or one a [`GraphBuilder`]
/// created.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct NodeId(reg::NodeId);

id_conversions!(GraphId, StandardGraph, GraphId);
id_conversions!(NodeId, StandardNode, NodeId);

/// A set of stream types a graph input accepts (`ZL_Type` masks).
///
/// # Examples
///
/// ```
/// use mopenzl::graph::TypeSet;
/// use mopenzl::DataType;
///
/// let set = TypeSet::from(DataType::Serial) | DataType::Numeric.into();
/// assert!(set.contains(DataType::Numeric) && !set.contains(DataType::String));
/// assert!(TypeSet::ANY.contains(DataType::String));
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct TypeSet(u8);

impl TypeSet {
    /// Every type.
    pub const ANY: Self = Self(15);

    /// Whether `ty` is in the set.
    #[must_use]
    pub const fn contains(self, ty: DataType) -> bool {
        self.0 & type_mask(ty) != 0
    }
}

impl From<DataType> for TypeSet {
    fn from(ty: DataType) -> Self {
        Self(type_mask(ty))
    }
}

impl BitOr for TypeSet {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Integer, byte and object parameters of a node or graph. Giving a node or
/// graph parameters replaces the ones it had.
///
/// # Examples
///
/// ```
/// use mopenzl::graph::LocalParams;
///
/// let params = LocalParams::default().with_int(100, 19).with_bytes(7, b"blob".to_vec());
/// assert_eq!(params.int(100), Some(19));
/// assert_eq!(params.int(1), None);
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalParams(CoreParams);

impl LocalParams {
    /// Adds an integer parameter (`ZL_IntParam`).
    #[must_use]
    pub fn with_int(mut self, id: i32, value: i32) -> Self {
        self.0.ints.to_mut().push((id, value));
        self
    }

    /// Adds a copy parameter (`ZL_CopyParam`).
    #[must_use]
    pub fn with_bytes(mut self, id: i32, bytes: impl Into<Arc<[u8]>>) -> Self {
        self.0.copies.push((id, bytes.into()));
        self
    }

    /// The `setStringLens` node's lengths (`ZL_SETSTRINGLENS_ARRAY_PID`):
    /// the string lengths that split a serial input into strings.
    #[must_use]
    pub fn with_string_lengths(mut self, lengths: Vec<u32>) -> Self {
        let object: Arc<dyn core::any::Any + Send + Sync> = Arc::new(lengths);
        self.0.refs.push((
            engine::codecs::conversion::SET_STRING_LENS_ARRAY,
            RefParam(object),
        ));
        self
    }

    /// The first value of integer parameter `id`.
    #[must_use]
    pub fn int(&self, id: i32) -> Option<i32> {
        self.0.int(id)
    }
}

/// Overrides for a node (`ZL_NodeParameters`): what is set replaces the
/// node's own, what is not set is kept.
///
/// # Examples
///
/// ```
/// use mopenzl::dict::DictId;
/// use mopenzl::graph::{LocalParams, NodeParams};
///
/// let params = NodeParams::default()
///     .with_params(LocalParams::default().with_int(100, 19))
///     .with_dictionary(DictId::from([7; 32]));
/// # let _ = params;
/// ```
#[derive(Clone, Debug, Default)]
pub struct NodeParams {
    params: Option<LocalParams>,
    dictionary: Option<DictId>,
}

impl NodeParams {
    /// Replaces the node's local parameters.
    #[must_use]
    pub fn with_params(mut self, params: LocalParams) -> Self {
        self.params = Some(params);
        self
    }

    /// References the dictionary `id` of the bundle given to
    /// [`GraphBuilder::load_dictionaries`]. Only the Zstandard node uses a
    /// dictionary, and only at format version 25 or newer.
    #[must_use]
    pub fn with_dictionary(mut self, id: DictId) -> Self {
        self.dictionary = Some(id);
        self
    }
}

/// Overrides for a graph (`ZL_ParameterizedGraphDesc`): each part that is
/// set replaces the graph's own.
#[derive(Clone, Debug, Default)]
pub struct GraphParams {
    params: Option<LocalParams>,
    custom_graphs: Option<Vec<GraphId>>,
    custom_nodes: Option<Vec<NodeId>>,
    name: Option<String>,
}

impl GraphParams {
    /// Replaces the graph's local parameters.
    #[must_use]
    pub fn with_params(mut self, params: LocalParams) -> Self {
        self.params = Some(params);
        self
    }

    /// Replaces the graph's successor graphs.
    #[must_use]
    pub fn with_graphs(mut self, graphs: impl Into<Vec<GraphId>>) -> Self {
        self.custom_graphs = Some(graphs.into());
        self
    }

    /// Replaces the graph's nodes.
    #[must_use]
    pub fn with_nodes(mut self, nodes: impl Into<Vec<NodeId>>) -> Self {
        self.custom_nodes = Some(nodes.into());
        self
    }

    /// Names the new graph.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// Why a graph could not be built or run.
#[derive(Error, Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum GraphError {
    /// An ID that is unknown or was created by another builder.
    #[error("unknown or foreign {0} ID")]
    UnknownId(&'static str),
    /// A graph description the registry rejects.
    #[error("{0}")]
    Invalid(&'static str),
    /// The engine failed while running graphs; the message names the node or
    /// graph and the cause.
    #[error("graph execution failed: {0}")]
    Execution(String),
    /// A user graph, selector or segmenter reported a failure.
    #[error("{0}")]
    User(&'static str),
    /// A dictionary bundle or one of its dictionaries was rejected.
    #[error("dictionary bundle: {0}")]
    Dictionary(#[from] DictError),
    /// [`GraphBuilder::load_dictionaries`] was called twice: a compressor
    /// holds one bundle.
    #[error("a dictionary bundle is already loaded")]
    BundleAlreadyLoaded,
    /// A node references a dictionary the loaded bundle does not hold, or no
    /// bundle is loaded.
    #[error("dictionary {0} is not in the loaded bundle")]
    DictionaryNotFound(DictId),
}

impl From<ExecError> for GraphError {
    fn from(error: ExecError) -> Self {
        match error {
            ExecError::User(message) => Self::User(message),
            other => Self::Execution(other.to_string()),
        }
    }
}

/// A header comment stored in the frame (`ZL_CCtx_addHeaderComment`):
/// 1 to 10 000 bytes, format version 22 or newer.
///
/// # Examples
///
/// ```
/// use mopenzl::graph::Comment;
///
/// let comment = Comment::try_from(&b"produced by me"[..])?;
/// assert_eq!(comment.as_ref(), b"produced by me");
/// assert!(Comment::try_from(&b""[..]).is_err());
/// # Ok::<(), mopenzl::graph::InvalidComment>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Comment(Arc<[u8]>);

/// A comment of 0 or more than 10 000 bytes.
#[derive(Error, Debug, Clone, Copy, Eq, PartialEq)]
#[error("a header comment holds 1 to 10000 bytes, not {0}")]
pub struct InvalidComment(pub usize);

impl TryFrom<&[u8]> for Comment {
    type Error = InvalidComment;

    /// Validates the comment's length.
    ///
    /// # Errors
    ///
    /// [`InvalidComment`] for an empty comment or one above 10 000 bytes.
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        if bytes.is_empty() || bytes.len() > engine::layout::COMMENT_MAX {
            return Err(InvalidComment(bytes.len()));
        }
        Ok(Self(Arc::from(bytes)))
    }
}

impl AsRef<[u8]> for Comment {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Comment {
    pub(crate) fn shared(&self) -> Arc<[u8]> {
        Arc::clone(&self.0)
    }
}

/// A read-only view of a stream, as selectors and graphs see it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InputView<'v> {
    data_type: DataType,
    width: usize,
    bytes: &'v [u8],
    lens: &'v [u32],
    metadata: &'v [(i32, i32)],
}

impl<'v> InputView<'v> {
    fn new(stream: &'v Stream<'_>) -> Self {
        Self {
            data_type: stream.ty(),
            width: stream.width(),
            bytes: stream.bytes(),
            lens: stream.lens(),
            metadata: stream.metadata(),
        }
    }

    fn to_stream(self) -> Stream<'v> {
        let mut stream = match self.data_type {
            DataType::String => Stream::strings(self.bytes, self.lens),
            ty => Stream::new(ty, self.width, Bytes::Borrowed(self.bytes)),
        };
        for &(id, value) in self.metadata {
            stream.set_metadata(id, value);
        }
        stream
    }

    /// An integer metadata value set on the stream by an upstream graph
    /// (`ZL_Data_getIntMetadata`).
    #[must_use]
    pub fn int_metadata(&self, id: i32) -> Option<i32> {
        self.metadata
            .iter()
            .find(|&&(key, _)| key == id)
            .map(|&(_, value)| value)
    }

    /// The stream type.
    #[must_use]
    pub const fn data_type(&self) -> DataType {
        self.data_type
    }

    /// Bytes per element (1 for serial data and strings).
    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// The content.
    #[must_use]
    pub const fn bytes(&self) -> &'v [u8] {
        self.bytes
    }

    /// The string lengths; empty for other types.
    #[must_use]
    pub const fn string_lengths(&self) -> &'v [u32] {
        self.lens
    }

    /// Elements in the stream.
    #[must_use]
    pub const fn len(&self) -> usize {
        match self.data_type {
            DataType::String => self.lens.len(),
            _ => self.bytes.len() / self.width,
        }
    }

    /// Whether the stream has no elements.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A handle to a stream inside one invocation of a function graph.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Edge(CoreEdge);

/// A selector (`ZL_SelectorFn`): picks which successor compresses a stream.
pub trait Selector: Send + Sync + 'static {
    /// Chooses among `successors` (the graph's custom graphs) or any other
    /// graph of the compressor.
    fn select(
        &self,
        cx: &mut SelectorContext<'_>,
        input: InputView<'_>,
        successors: &[GraphId],
    ) -> GraphId;
}

/// A function graph (`ZL_FunctionGraphFn`): runs nodes on its inputs and
/// gives every resulting edge a destination.
pub trait FunctionGraph: Send + Sync + 'static {
    /// Runs the graph.
    ///
    /// # Errors
    ///
    /// Any failure, which fails the graph (or triggers the permissive
    /// fallback).
    fn compress(&self, cx: &mut GraphContext<'_>, inputs: &[Edge]) -> Result<(), GraphError>;
}

/// A segmenter (`ZL_SegmenterFn`): cuts the frame's inputs into chunks.
pub trait Segmenter: Send + Sync + 'static {
    /// Cuts the inputs, consuming all of them.
    ///
    /// # Errors
    ///
    /// Any failure of the segmenter or of a chunk.
    fn segment(&self, cx: &mut SegmenterContext<'_>) -> Result<(), GraphError>;
}

trait SelectorApi {
    fn level(&self) -> i32;
    fn format_version(&self) -> u32;
    fn depth(&self) -> u32;
    fn node_supported(&self, node: reg::NodeId) -> bool;
    fn local_int(&self, id: i32) -> Option<i32>;
    fn set_params(&mut self, params: CoreParams);
    fn try_graph(&mut self, input: &InputView<'_>, graph: reg::GraphId)
    -> Result<usize, ExecError>;
}

impl SelectorApi for SelectorCx<'_, '_, '_> {
    fn level(&self) -> i32 {
        SelectorCx::level(self)
    }
    fn format_version(&self) -> u32 {
        SelectorCx::format_version(self)
    }
    fn depth(&self) -> u32 {
        SelectorCx::depth(self)
    }
    fn node_supported(&self, node: reg::NodeId) -> bool {
        self.is_node_supported(node)
    }
    fn local_int(&self, id: i32) -> Option<i32> {
        self.params.int(id)
    }
    fn set_params(&mut self, params: CoreParams) {
        self.set_successor_params(params);
    }
    fn try_graph(
        &mut self,
        input: &InputView<'_>,
        graph: reg::GraphId,
    ) -> Result<usize, ExecError> {
        SelectorCx::try_graph(self, &input.to_stream(), graph)
    }
}

/// What a [`Selector`] can ask of the engine.
pub struct SelectorContext<'c> {
    api: &'c mut dyn SelectorApi,
}

impl fmt::Debug for SelectorContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SelectorContext")
    }
}

impl SelectorContext<'_> {
    /// The compression level.
    #[must_use]
    pub fn level(&self) -> i32 {
        self.api.level()
    }

    /// The frame's format version.
    #[must_use]
    pub fn format_version(&self) -> u32 {
        self.api.format_version()
    }

    /// How deep in the graph the selector runs (1 at the top).
    #[must_use]
    pub fn depth(&self) -> u32 {
        self.api.depth()
    }

    /// Whether `node` exists at the frame's format version.
    #[must_use]
    pub fn is_node_supported(&self, node: impl Into<NodeId>) -> bool {
        self.api.node_supported(node.into().0)
    }

    /// An integer parameter of the selector graph.
    #[must_use]
    pub fn local_int(&self, id: i32) -> Option<i32> {
        self.api.local_int(id)
    }

    /// Makes the chosen successor run with these local parameters.
    pub fn set_successor_params(&mut self, params: LocalParams) {
        self.api.set_params(params.0);
    }

    /// The size of the complete frame `graph` would produce for `input`
    /// (`ZL_Selector_tryGraph`).
    ///
    /// # Errors
    ///
    /// Whatever makes the trial compression fail.
    pub fn try_graph(
        &mut self,
        input: &InputView<'_>,
        graph: impl Into<GraphId>,
    ) -> Result<usize, GraphError> {
        Ok(self.api.try_graph(input, graph.into().0)?)
    }
}

trait GraphApi {
    fn view(&self, edge: CoreEdge) -> Result<InputView<'_>, ExecError>;
    fn run_node(
        &mut self,
        node: reg::NodeId,
        inputs: &[CoreEdge],
        params: Option<&CoreParams>,
    ) -> Result<Vec<CoreEdge>, ExecError>;
    fn set_destination(
        &mut self,
        edges: &[CoreEdge],
        graph: reg::GraphId,
        params: Option<RuntimeParams>,
    ) -> Result<(), ExecError>;
    fn set_metadata(&mut self, edge: CoreEdge, id: i32, value: i32) -> Result<(), ExecError>;
    fn custom_graphs(&self) -> Vec<reg::GraphId>;
    fn custom_nodes(&self) -> Vec<reg::NodeId>;
    fn local_int(&self, id: i32) -> Option<i32>;
    fn level(&self) -> i32;
    fn format_version(&self) -> u32;
    fn depth(&self) -> u32;
    fn node_supported(&self, node: reg::NodeId) -> bool;
    fn try_graph(&mut self, input: CoreEdge, graph: reg::GraphId) -> Result<usize, ExecError>;
    fn run_split(&mut self, edge: CoreEdge, sizes: &[usize]) -> Result<Vec<CoreEdge>, ExecError>;
    fn run_dispatch(
        &mut self,
        edge: CoreEdge,
        instructions: &DispatchInstructions,
    ) -> Result<Vec<CoreEdge>, ExecError>;
    fn run_dispatch_string(
        &mut self,
        edge: CoreEdge,
        nb_outputs: i32,
        indices: &[u16],
    ) -> Result<Vec<CoreEdge>, ExecError>;
}

impl GraphApi for GraphCx<'_, '_, '_> {
    fn view(&self, edge: CoreEdge) -> Result<InputView<'_>, ExecError> {
        Ok(InputView::new(self.data(edge)?))
    }
    fn run_node(
        &mut self,
        node: reg::NodeId,
        inputs: &[CoreEdge],
        params: Option<&CoreParams>,
    ) -> Result<Vec<CoreEdge>, ExecError> {
        GraphCx::run_node(self, node, inputs, params)
    }
    fn set_destination(
        &mut self,
        edges: &[CoreEdge],
        graph: reg::GraphId,
        params: Option<RuntimeParams>,
    ) -> Result<(), ExecError> {
        GraphCx::set_destination(self, edges, graph, params)
    }
    fn set_metadata(&mut self, edge: CoreEdge, id: i32, value: i32) -> Result<(), ExecError> {
        GraphCx::set_metadata(self, edge, id, value)
    }
    fn custom_graphs(&self) -> Vec<reg::GraphId> {
        self.eff.custom_graphs.to_vec()
    }
    fn custom_nodes(&self) -> Vec<reg::NodeId> {
        self.eff.custom_nodes.to_vec()
    }
    fn local_int(&self, id: i32) -> Option<i32> {
        self.eff.params.int(id)
    }
    fn level(&self) -> i32 {
        self.ex.p.level
    }
    fn format_version(&self) -> u32 {
        self.ex.p.version
    }
    fn depth(&self) -> u32 {
        self.depth
    }
    fn node_supported(&self, node: reg::NodeId) -> bool {
        self.is_node_supported(node)
    }
    fn try_graph(&mut self, input: CoreEdge, graph: reg::GraphId) -> Result<usize, ExecError> {
        let stream = self.data(input)?.clone();
        self.ex
            .try_graph(core::slice::from_ref(&stream), graph, None)
    }
    fn run_split(&mut self, edge: CoreEdge, sizes: &[usize]) -> Result<Vec<CoreEdge>, ExecError> {
        engine::graphs::run_split_node(self, edge, sizes)
    }
    fn run_dispatch(
        &mut self,
        edge: CoreEdge,
        instructions: &DispatchInstructions,
    ) -> Result<Vec<CoreEdge>, ExecError> {
        engine::graphs::run_dispatch_node(self, edge, &instructions.clone().into())
    }
    fn run_dispatch_string(
        &mut self,
        edge: CoreEdge,
        nb_outputs: i32,
        indices: &[u16],
    ) -> Result<Vec<CoreEdge>, ExecError> {
        engine::graphs::run_dispatch_string_node(self, edge, nb_outputs, indices)
    }
}

/// What a [`FunctionGraph`] can ask of the engine.
pub struct GraphContext<'c> {
    api: &'c mut dyn GraphApi,
}

impl fmt::Debug for GraphContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GraphContext")
    }
}

fn core_edges(edges: &[Edge]) -> Vec<CoreEdge> {
    edges.iter().map(|edge| edge.0).collect()
}

impl GraphContext<'_> {
    /// The stream behind `edge`.
    ///
    /// # Errors
    ///
    /// An edge of another invocation.
    pub fn data(&self, edge: Edge) -> Result<InputView<'_>, GraphError> {
        Ok(self.api.view(edge.0)?)
    }

    /// Sets integer metadata on the stream behind `edge`; downstream graphs
    /// read it (`ZL_Edge_setIntMetadata`). It is never written to the frame.
    ///
    /// # Errors
    ///
    /// An edge of another invocation.
    pub fn set_int_metadata(&mut self, edge: Edge, id: i32, value: i32) -> Result<(), GraphError> {
        Ok(self.api.set_metadata(edge.0, id, value)?)
    }

    /// Runs `node` on `inputs` now (`ZL_Edge_runMultiInputNode`) and returns
    /// the edges of its outputs, in output order. Nodes do no implicit type
    /// conversion.
    ///
    /// # Errors
    ///
    /// A failing node, or an input edge that already has a destination.
    pub fn run_node(
        &mut self,
        node: impl Into<NodeId>,
        inputs: &[Edge],
    ) -> Result<Vec<Edge>, GraphError> {
        let edges = self
            .api
            .run_node(node.into().0, &core_edges(inputs), None)?;
        Ok(edges.into_iter().map(Edge).collect())
    }

    /// [`Self::run_node`] with local parameters replacing the node's own.
    ///
    /// # Errors
    ///
    /// As [`Self::run_node`].
    pub fn run_node_with(
        &mut self,
        node: impl Into<NodeId>,
        inputs: &[Edge],
        params: &LocalParams,
    ) -> Result<Vec<Edge>, GraphError> {
        let edges = self
            .api
            .run_node(node.into().0, &core_edges(inputs), Some(&params.0))?;
        Ok(edges.into_iter().map(Edge).collect())
    }

    /// Sends `edges` to `graph`. Successors run after this function returns,
    /// in the order of their first edge.
    ///
    /// # Errors
    ///
    /// An unknown graph, a wrong number of edges, or an edge that already
    /// has a destination.
    pub fn set_destination(
        &mut self,
        edges: &[Edge],
        graph: impl Into<GraphId>,
    ) -> Result<(), GraphError> {
        Ok(self
            .api
            .set_destination(&core_edges(edges), graph.into().0, None)?)
    }

    /// [`Self::set_destination`] with runtime overrides for the successor.
    ///
    /// # Errors
    ///
    /// As [`Self::set_destination`].
    pub fn set_destination_with(
        &mut self,
        edges: &[Edge],
        graph: impl Into<GraphId>,
        overrides: &GraphParams,
    ) -> Result<(), GraphError> {
        let runtime = RuntimeParams {
            params: overrides.params.as_ref().map(|params| params.0.clone()),
            custom_graphs: overrides
                .custom_graphs
                .as_ref()
                .map(|graphs| graphs.iter().map(|graph| graph.0).collect()),
            custom_nodes: overrides
                .custom_nodes
                .as_ref()
                .map(|nodes| nodes.iter().map(|node| node.0).collect()),
        };
        Ok(self
            .api
            .set_destination(&core_edges(edges), graph.into().0, Some(runtime))?)
    }

    /// The graph's successor graphs.
    #[must_use]
    pub fn custom_graphs(&self) -> Vec<GraphId> {
        self.api.custom_graphs().into_iter().map(GraphId).collect()
    }

    /// The graph's nodes.
    #[must_use]
    pub fn custom_nodes(&self) -> Vec<NodeId> {
        self.api.custom_nodes().into_iter().map(NodeId).collect()
    }

    /// An integer parameter of the graph.
    #[must_use]
    pub fn local_int(&self, id: i32) -> Option<i32> {
        self.api.local_int(id)
    }

    /// The compression level.
    #[must_use]
    pub fn level(&self) -> i32 {
        self.api.level()
    }

    /// The frame's format version.
    #[must_use]
    pub fn format_version(&self) -> u32 {
        self.api.format_version()
    }

    /// How deep in the graph this invocation runs (1 at the top).
    #[must_use]
    pub fn depth(&self) -> u32 {
        self.api.depth()
    }

    /// Whether `node` exists at the frame's format version.
    #[must_use]
    pub fn is_node_supported(&self, node: impl Into<NodeId>) -> bool {
        self.api.node_supported(node.into().0)
    }

    /// The size of the complete frame `graph` would produce for the stream
    /// behind `input`.
    ///
    /// # Errors
    ///
    /// Whatever makes the trial compression fail.
    pub fn try_graph(
        &mut self,
        input: Edge,
        graph: impl Into<GraphId>,
    ) -> Result<usize, GraphError> {
        Ok(self.api.try_graph(input.0, graph.into().0)?)
    }
}

/// How `dispatchN_byTag` cuts its serial input and routes the pieces
/// (`ZL_DispatchInstructions`).
///
/// The input is cut into consecutive segments of `segment_sizes` bytes;
/// segment `i` goes to output stream `tags[i]`, out of `tag_count` streams.
/// The sizes must add up to the input size and every tag must be below
/// `tag_count`.
///
/// # Examples
///
/// ```
/// use mopenzl::graph::DispatchInstructions;
///
/// // Alternate 3-byte and 1-byte segments between two streams.
/// let instructions = DispatchInstructions {
///     segment_sizes: vec![3, 1, 3, 1],
///     tags: vec![0, 1, 0, 1],
///     tag_count: 2,
/// };
/// assert_eq!(instructions.tag_count, 2);
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DispatchInstructions {
    /// The size in bytes of every segment, in input order.
    pub segment_sizes: Vec<usize>,
    /// The output stream of every segment.
    pub tags: Vec<u32>,
    /// The number of output streams.
    pub tag_count: usize,
}

impl From<DispatchInstructions> for engine::codecs::structure::DispatchInstructions {
    fn from(value: DispatchInstructions) -> Self {
        Self {
            segment_sizes: value.segment_sizes,
            tags: value.tags,
            nb_tags: value.tag_count,
        }
    }
}

impl GraphContext<'_> {
    /// Cuts the stream behind `edge` into consecutive segments of
    /// `segment_sizes` elements (`ZL_Edge_runSplitNode`) and returns the
    /// edge of every segment. A last size of 0 takes whatever is left; the
    /// sizes must otherwise add up to the length of the stream. Serial,
    /// struct and numeric streams can be split.
    ///
    /// # Errors
    ///
    /// A string stream, sizes that do not cover the stream, or a failing
    /// node (for example when the `split` feature is off).
    pub fn run_split_node(
        &mut self,
        edge: Edge,
        segment_sizes: &[usize],
    ) -> Result<Vec<Edge>, GraphError> {
        let edges = self.api.run_split(edge.0, segment_sizes)?;
        Ok(edges.into_iter().map(Edge).collect())
    }

    /// Runs `dispatchN_byTag` on the serial stream behind `edge`
    /// (`ZL_Edge_runDispatchNode`). The edges returned are the tag stream,
    /// the segment-size stream, and one serial stream per tag.
    ///
    /// # Errors
    ///
    /// Instructions that do not describe the stream, or a failing node.
    pub fn run_dispatch_node(
        &mut self,
        edge: Edge,
        instructions: &DispatchInstructions,
    ) -> Result<Vec<Edge>, GraphError> {
        let edges = self.api.run_dispatch(edge.0, instructions)?;
        Ok(edges.into_iter().map(Edge).collect())
    }

    /// Runs `dispatch_string` on the string stream behind `edge`
    /// (`ZL_Edge_runDispatchStringNode`): string `i` goes to output
    /// `indices[i]` of `output_count`. The edges returned are the index
    /// stream, then one string stream per output.
    ///
    /// # Errors
    ///
    /// More than 2048 outputs, an index out of range, fewer indices than
    /// strings, or a failing node.
    pub fn run_dispatch_string_node(
        &mut self,
        edge: Edge,
        output_count: usize,
        indices: &[u16],
    ) -> Result<Vec<Edge>, GraphError> {
        let count = i32::try_from(output_count).unwrap_or(i32::MAX);
        let edges = self.api.run_dispatch_string(edge.0, count, indices)?;
        Ok(edges.into_iter().map(Edge).collect())
    }
}

trait SegmenterApi {
    fn n_inputs(&self) -> usize;
    fn remaining(&self) -> Vec<usize>;
    fn custom_graphs(&self) -> Vec<reg::GraphId>;
    fn local_int(&self, id: i32) -> Option<i32>;
    fn format_version(&self) -> u32;
    fn process_chunk(&mut self, n_elts: &[usize], graph: reg::GraphId) -> Result<(), ExecError>;
}

impl SegmenterApi for SegmenterCx<'_, '_, '_> {
    fn n_inputs(&self) -> usize {
        SegmenterCx::n_inputs(self)
    }
    fn remaining(&self) -> Vec<usize> {
        SegmenterCx::remaining(self)
    }
    fn custom_graphs(&self) -> Vec<reg::GraphId> {
        self.custom_graphs.to_vec()
    }
    fn local_int(&self, id: i32) -> Option<i32> {
        self.params.int(id)
    }
    fn format_version(&self) -> u32 {
        SegmenterCx::format_version(self)
    }
    fn process_chunk(&mut self, n_elts: &[usize], graph: reg::GraphId) -> Result<(), ExecError> {
        SegmenterCx::process_chunk(self, n_elts, graph, None)
    }
}

/// What a [`Segmenter`] can ask of the engine.
pub struct SegmenterContext<'c> {
    api: &'c mut dyn SegmenterApi,
}

impl fmt::Debug for SegmenterContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SegmenterContext")
    }
}

impl SegmenterContext<'_> {
    /// Number of inputs of the frame.
    #[must_use]
    pub fn n_inputs(&self) -> usize {
        self.api.n_inputs()
    }

    /// Elements left in every input.
    #[must_use]
    pub fn remaining(&self) -> Vec<usize> {
        self.api.remaining()
    }

    /// The segmenter's successor graphs.
    #[must_use]
    pub fn custom_graphs(&self) -> Vec<GraphId> {
        self.api.custom_graphs().into_iter().map(GraphId).collect()
    }

    /// An integer parameter of the segmenter.
    #[must_use]
    pub fn local_int(&self, id: i32) -> Option<i32> {
        self.api.local_int(id)
    }

    /// The frame's format version.
    #[must_use]
    pub fn format_version(&self) -> u32 {
        self.api.format_version()
    }

    /// Compresses the next `n_elts[i]` elements of input `i` as one chunk
    /// through `graph`.
    ///
    /// # Errors
    ///
    /// More elements than are left, or any failure of the chunk.
    pub fn process_chunk(
        &mut self,
        n_elts: &[usize],
        graph: impl Into<GraphId>,
    ) -> Result<(), GraphError> {
        Ok(self.api.process_chunk(n_elts, graph.into().0)?)
    }
}

struct UserSelector<S>(S);

impl<S: Selector> reg::SelectFn for UserSelector<S> {
    fn select(
        &self,
        cx: &mut SelectorCx<'_, '_, '_>,
        input: &Stream<'_>,
        successors: &[reg::GraphId],
    ) -> reg::GraphId {
        let view = InputView::new(input);
        let successors: Vec<GraphId> = successors.iter().map(|&graph| GraphId(graph)).collect();
        let mut context = SelectorContext { api: cx };
        self.0.select(&mut context, view, &successors).0
    }
}

struct UserGraph<F>(F);

impl<F: FunctionGraph> reg::GraphFn for UserGraph<F> {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[CoreEdge]) -> Result<(), ExecError> {
        let edges: Vec<Edge> = inputs.iter().map(|&edge| Edge(edge)).collect();
        let mut context = GraphContext { api: cx };
        self.0
            .compress(&mut context, &edges)
            .map_err(|error| match error {
                GraphError::User(message) => ExecError::User(message),
                GraphError::Invalid(message) => ExecError::Parameter(message),
                _ => ExecError::User("a function graph failed"),
            })
    }
}

struct UserSegmenter<S>(S);

impl<S: Segmenter> reg::SegmentFn for UserSegmenter<S> {
    fn segment(&self, cx: &mut SegmenterCx<'_, '_, '_>) -> Result<(), ExecError> {
        let mut context = SegmenterContext { api: cx };
        self.0.segment(&mut context).map_err(|error| match error {
            GraphError::User(message) => ExecError::User(message),
            _ => ExecError::User("a segmenter failed"),
        })
    }
}

static NEXT_OWNER: AtomicU32 = AtomicU32::new(1);

/// Builds the graphs of a compressor.
///
/// Every `GraphId` and `NodeId` a builder returns belongs to it; the
/// standard ones belong to every builder.
///
/// # Examples
///
/// ```
/// use mopenzl::graph::{GraphBuilder, LocalParams, StandardGraph, StandardNode};
///
/// let mut builder = GraphBuilder::new();
/// // Interpret serial bytes as 32-bit numbers, then store them.
/// let graph = builder.static_graph(StandardNode::ConvertSerialToNumLe32, &[StandardGraph::Store.into()])?;
/// let compressor_graph = builder.build(graph)?;
/// # let _ = compressor_graph;
/// # Ok::<(), mopenzl::graph::GraphError>(())
/// ```
#[derive(Debug)]
pub struct GraphBuilder {
    registry: Registry,
}

impl Default for GraphBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphBuilder {
    /// An empty builder: only the standard graphs and nodes.
    #[must_use]
    pub fn new() -> Self {
        let owner = NEXT_OWNER.fetch_add(1, Ordering::Relaxed).max(1);
        Self {
            registry: Registry::new(owner, reg::gid::COMPRESS_GENERIC),
        }
    }

    fn graph_id(&self, id: GraphId) -> Result<reg::GraphId, GraphError> {
        self.registry
            .graph(id.0)
            .map(|_| id.0)
            .map_err(|_| GraphError::UnknownId("graph"))
    }

    fn node_id(&self, id: NodeId) -> Result<reg::NodeId, GraphError> {
        self.registry
            .node(id.0)
            .map(|_| id.0)
            .map_err(|_| GraphError::UnknownId("node"))
    }

    fn masks(inputs: &[TypeSet]) -> Result<Cow<'static, [u8]>, GraphError> {
        if inputs.is_empty() {
            return Err(GraphError::Invalid("a graph needs at least one input"));
        }
        Ok(Cow::Owned(inputs.iter().map(|set| set.0).collect()))
    }

    fn add(&mut self, def: GraphDef) -> GraphId {
        GraphId(self.registry.add_graph(def))
    }

    /// A graph that runs `head` and sends its outputs to `successors`, one
    /// graph per output (`ZL_Compressor_registerStaticGraph_fromNode`).
    ///
    /// # Errors
    ///
    /// Unknown IDs, or a number of successors that does not match the node's
    /// outputs.
    pub fn static_graph(
        &mut self,
        head: impl Into<NodeId>,
        successors: &[GraphId],
    ) -> Result<GraphId, GraphError> {
        let head = self.node_id(head.into())?;
        let spec = self
            .registry
            .node(head)
            .map_err(|_| GraphError::UnknownId("node"))?
            .spec;
        let outcomes = spec.singletons.len() + spec.variable.len();
        let expected_ok = if spec.variable.is_empty() {
            successors.len() == outcomes
        } else {
            successors.len() >= spec.singletons.len() && successors.len() == outcomes
        };
        if !expected_ok {
            return Err(GraphError::Invalid(
                "the successors do not match the node's outputs",
            ));
        }
        let successors = successors
            .iter()
            .map(|&graph| self.graph_id(graph))
            .collect::<Result<Vec<_>, _>>()?;
        let inputs: Vec<u8> = spec.inputs.iter().map(|&ty| type_mask(ty)).collect();
        Ok(self.add(GraphDef {
            name: Cow::Owned(alloc::format!("static({})", spec.name)),
            kind: GraphKind::Static,
            inputs: Cow::Owned(inputs),
            last_variable: spec.variable_input,
            custom_graphs: Cow::Owned(successors),
            custom_nodes: Cow::Owned(alloc::vec![head]),
            params: CoreParams::default(),
        }))
    }

    /// Nodes run in order, each feeding the next, the last feeding `last`
    /// (`ZL_Compressor_registerStaticGraph_fromPipelineNodes1o`).
    ///
    /// # Errors
    ///
    /// As [`Self::static_graph`], or an empty pipeline.
    pub fn pipeline(
        &mut self,
        nodes: &[NodeId],
        last: impl Into<GraphId>,
    ) -> Result<GraphId, GraphError> {
        if nodes.is_empty() {
            return Err(GraphError::Invalid("a pipeline needs at least one node"));
        }
        let mut next = last.into();
        for &node in nodes.iter().rev() {
            next = self.static_graph(node, &[next])?;
        }
        Ok(next)
    }

    /// A copy of `node` with replacement parameters
    /// (`ZL_Compressor_parameterizeNode`).
    ///
    /// # Errors
    ///
    /// An unknown node.
    pub fn parameterize_node(
        &mut self,
        node: impl Into<NodeId>,
        params: LocalParams,
    ) -> Result<NodeId, GraphError> {
        self.parameterize_node_with(node, NodeParams::default().with_params(params))
    }

    /// A copy of `node` with the overrides `params` sets, local parameters
    /// and a dictionary reference (`ZL_Compressor_parameterizeNode`).
    ///
    /// The dictionary is looked up when [`Self::build`] freezes the graphs.
    ///
    /// # Errors
    ///
    /// An unknown node.
    pub fn parameterize_node_with(
        &mut self,
        node: impl Into<NodeId>,
        params: NodeParams,
    ) -> Result<NodeId, GraphError> {
        let node = self.node_id(node.into())?;
        let view = self
            .registry
            .node(node)
            .map_err(|_| GraphError::UnknownId("node"))?;
        let (spec, own, dict_id) = (view.spec, view.params.cloned(), view.dict_id);
        Ok(NodeId(self.registry.add_node(NodeDef {
            spec,
            params: params.params.map(|params| params.0).or(own),
            dict_id: params.dictionary.or(dict_id),
            dict_index: None,
        })))
    }

    /// Attaches the dictionary bundle frames are compressed with
    /// (`ZL_Compressor_loadDictBundle`). A compressor holds one bundle.
    ///
    /// Every dictionary must already be referenced by a Zstandard node
    /// parameterized with its ID ([`NodeParams::with_dictionary`]); a
    /// dictionary nothing uses cannot be materialized. From format version 25
    /// frames then record the bundle's ID and every chunk a dictionary lane.
    ///
    /// # Errors
    ///
    /// [`GraphError::BundleAlreadyLoaded`] on a second call, and
    /// [`GraphError::Dictionary`] for a dictionary no node references, one
    /// the codec rejects, or two contents sharing an ID.
    pub fn load_dictionaries(&mut self, bundle: &DictBundle) -> Result<(), GraphError> {
        self.registry
            .load_bundle(bundle)
            .map_err(|error| match error {
                LoadError::AlreadyLoaded => GraphError::BundleAlreadyLoaded,
                LoadError::Dict(error) => GraphError::Dictionary(error),
                LoadError::NotFound(id) => GraphError::DictionaryNotFound(id),
            })
    }

    /// A copy of `graph` with overrides (`ZL_Compressor_parameterizeGraph`).
    ///
    /// # Errors
    ///
    /// Unknown IDs.
    pub fn parameterize_graph(
        &mut self,
        graph: impl Into<GraphId>,
        overrides: GraphParams,
    ) -> Result<GraphId, GraphError> {
        self.parameterize_graph_with(graph.into(), overrides, None)
    }

    /// [`parameterize_graph`](Self::parameterize_graph) that also sets the
    /// graph's materialized MParam object. A graph keeps the MParam of the
    /// graph it was derived from unless a new one is given, even when its
    /// local parameters are replaced.
    fn parameterize_graph_with(
        &mut self,
        graph: GraphId,
        overrides: GraphParams,
        mparam: Option<RefParam>,
    ) -> Result<GraphId, GraphError> {
        let base = self.graph_id(graph)?;
        let (name, kind, inputs, last_variable, custom_graphs, custom_nodes, params) = {
            let def = self
                .registry
                .graph(base)
                .map_err(|_| GraphError::UnknownId("graph"))?;
            let kind = match &def.kind {
                GraphKind::Selector(Callback::Static(f)) => {
                    GraphKind::Selector(Callback::Static(*f))
                }
                GraphKind::Selector(Callback::Shared(f)) => {
                    GraphKind::Selector(Callback::Shared(Arc::clone(f)))
                }
                GraphKind::Function(Callback::Static(f)) => {
                    GraphKind::Function(Callback::Static(*f))
                }
                GraphKind::Function(Callback::Shared(f)) => {
                    GraphKind::Function(Callback::Shared(Arc::clone(f)))
                }
                GraphKind::Segmenter(Callback::Static(f)) => {
                    GraphKind::Segmenter(Callback::Static(*f))
                }
                GraphKind::Segmenter(Callback::Shared(f)) => {
                    GraphKind::Segmenter(Callback::Shared(Arc::clone(f)))
                }
                GraphKind::Static => GraphKind::Static,
                GraphKind::Store => GraphKind::Store,
                GraphKind::Illegal => GraphKind::Illegal,
            };
            (
                def.name.clone(),
                kind,
                def.inputs.clone(),
                def.last_variable,
                def.custom_graphs.clone(),
                def.custom_nodes.clone(),
                def.params.clone(),
            )
        };
        let custom_graphs = match overrides.custom_graphs {
            Some(graphs) => Cow::Owned(
                graphs
                    .iter()
                    .map(|&graph| self.graph_id(graph))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            None => custom_graphs,
        };
        let custom_nodes = match overrides.custom_nodes {
            Some(nodes) => Cow::Owned(
                nodes
                    .iter()
                    .map(|&node| self.node_id(node))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            None => custom_nodes,
        };
        let inherited = params
            .refs
            .iter()
            .find(|(id, _)| *id == engine::exec::MPARAM_REF_ID)
            .map(|(_, object)| object.clone());
        let mut params = overrides.params.map_or(params, |params| params.0);
        if let Some(mparam) = mparam.or(inherited) {
            params
                .refs
                .retain(|(id, _)| *id != engine::exec::MPARAM_REF_ID);
            params.refs.push((engine::exec::MPARAM_REF_ID, mparam));
        }
        Ok(self.add(GraphDef {
            name: overrides.name.map_or(name, Cow::Owned),
            kind,
            inputs,
            last_variable,
            custom_graphs,
            custom_nodes,
            params,
        }))
    }

    /// A graph whose single input a selector routes
    /// (`ZL_Compressor_registerSelectorGraph`).
    ///
    /// # Errors
    ///
    /// Unknown successors.
    pub fn selector<S: Selector>(
        &mut self,
        name: &str,
        selector: S,
        input: TypeSet,
        successors: &[GraphId],
    ) -> Result<GraphId, GraphError> {
        let successors = successors
            .iter()
            .map(|&graph| self.graph_id(graph))
            .collect::<Result<Vec<_>, _>>()?;
        let callback: Arc<dyn reg::SelectFn> = Arc::new(UserSelector(selector));
        Ok(self.add(GraphDef {
            name: Cow::Owned(name.to_string()),
            kind: GraphKind::Selector(Callback::Shared(callback)),
            inputs: Cow::Owned(alloc::vec![input.0]),
            last_variable: false,
            custom_graphs: Cow::Owned(successors),
            custom_nodes: Cow::Borrowed(&[]),
            params: CoreParams::default(),
        }))
    }

    /// A graph written as a function (`ZL_Compressor_registerFunctionGraph`).
    ///
    /// `inputs` are the accepted types of each input port; with
    /// `last_input_variable` the last port takes any number of inputs.
    ///
    /// # Errors
    ///
    /// Unknown IDs, or no input port.
    pub fn function_graph<F: FunctionGraph>(
        &mut self,
        name: &str,
        function: F,
        inputs: &[TypeSet],
        last_input_variable: bool,
        successors: &[GraphId],
        nodes: &[NodeId],
    ) -> Result<GraphId, GraphError> {
        let masks = Self::masks(inputs)?;
        let successors = successors
            .iter()
            .map(|&graph| self.graph_id(graph))
            .collect::<Result<Vec<_>, _>>()?;
        let nodes = nodes
            .iter()
            .map(|&node| self.node_id(node))
            .collect::<Result<Vec<_>, _>>()?;
        let callback: Arc<dyn reg::GraphFn> = Arc::new(UserGraph(function));
        Ok(self.add(GraphDef {
            name: Cow::Owned(name.to_string()),
            kind: GraphKind::Function(Callback::Shared(callback)),
            inputs: masks,
            last_variable: last_input_variable,
            custom_graphs: Cow::Owned(successors),
            custom_nodes: Cow::Owned(nodes),
            params: CoreParams::default(),
        }))
    }

    /// A segmenter graph (`ZL_Compressor_registerSegmenter`).
    ///
    /// # Errors
    ///
    /// Unknown IDs, or no input port.
    pub fn segmenter<S: Segmenter>(
        &mut self,
        name: &str,
        segmenter: S,
        inputs: &[TypeSet],
        successors: &[GraphId],
    ) -> Result<GraphId, GraphError> {
        let masks = Self::masks(inputs)?;
        let successors = successors
            .iter()
            .map(|&graph| self.graph_id(graph))
            .collect::<Result<Vec<_>, _>>()?;
        let callback: Arc<dyn reg::SegmentFn> = Arc::new(UserSegmenter(segmenter));
        Ok(self.add(GraphDef {
            name: Cow::Owned(name.to_string()),
            kind: GraphKind::Segmenter(Callback::Shared(callback)),
            inputs: masks,
            last_variable: false,
            custom_graphs: Cow::Owned(successors),
            custom_nodes: Cow::Borrowed(&[]),
            params: CoreParams::default(),
        }))
    }

    /// The serial segmenter: cuts serial input every `chunk` bytes and
    /// compresses each piece with `successor`
    /// (`ZL_Compressor_buildSerialSegmenter`).
    ///
    /// # Errors
    ///
    /// A chunk size outside 32 KiB to `i32::MAX`, or an unknown successor.
    pub fn serial_segmenter(
        &mut self,
        chunk: crate::ChunkSize,
        successor: impl Into<GraphId>,
    ) -> Result<GraphId, GraphError> {
        let params = GraphParams::default()
            .with_graphs([successor.into()])
            .with_params(LocalParams::default().with_int(
                engine::graphs::SERIAL_CHUNK_BYTE_SIZE,
                i32::try_from(usize::from(chunk)).unwrap_or(i32::MAX),
            ));
        self.parameterize_graph(StandardGraph::SegmentSerial, params)
    }

    /// Cuts serial input into chunks aligned to `width`-byte integers and
    /// compresses each chunk with `successor`
    /// (`ZL_Compressor_buildNumFromSerialSegmenter`); `None` takes the
    /// default successor of the width.
    ///
    /// Each chunk reaches the successor as *serial* data, as in the
    /// reference: the segmenter only aligns the cuts to the element width, it
    /// does not convert the chunk. The default successor
    /// (`interpret_num8_compress` to `interpret_num64_compress`) converts the
    /// chunk to numbers; a custom successor must accept serial input and do
    /// that itself, for example a graph that starts with a
    /// `convert_serial_to_num` node (see the example). One that accepts only
    /// numeric input fails when it runs.
    ///
    /// # Errors
    ///
    /// A width other than 1, 2, 4 or 8, a bad chunk size, or an unknown
    /// successor.
    ///
    /// # Examples
    ///
    /// Little-endian 32-bit integers, converted to numbers by the successor:
    ///
    /// ```
    /// use mopenzl::graph::{GraphBuilder, StandardGraph, StandardNode};
    /// use mopenzl::{ChunkSize, Compressor, EncoderConfig};
    ///
    /// let mut builder = GraphBuilder::new();
    /// // serial chunk -> numbers -> the default numeric graph
    /// let numbers = builder.static_graph(
    ///     StandardNode::ConvertSerialToNumLe32,
    ///     &[StandardGraph::NumericCompress.into()],
    /// )?;
    /// let segmenter = builder.num_from_serial_segmenter(4, ChunkSize::MIN, Some(numbers))?;
    /// let graph = builder.build(segmenter)?;
    ///
    /// let input: Vec<u8> = (0..20_000u32).flat_map(|i| i.to_le_bytes()).collect();
    /// let mut compressor = Compressor::builder(EncoderConfig::default())
    ///     .with_graph(graph)
    ///     .build()?;
    /// let frame = compressor.compress(&input)?;
    /// # #[cfg(feature = "decompression")]
    /// assert_eq!(mopenzl::Decompressor::default().decompress(&frame)?, input);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn num_from_serial_segmenter(
        &mut self,
        width: usize,
        chunk: crate::ChunkSize,
        successor: Option<GraphId>,
    ) -> Result<GraphId, GraphError> {
        let (base, default) = match width {
            1 => (
                StandardGraph::SegmentNum8FromSerial,
                StandardGraph::InterpretNum8Compress,
            ),
            2 => (
                StandardGraph::SegmentNum16FromSerial,
                StandardGraph::InterpretNum16Compress,
            ),
            4 => (
                StandardGraph::SegmentNum32FromSerial,
                StandardGraph::InterpretNum32Compress,
            ),
            8 => (
                StandardGraph::SegmentNum64FromSerial,
                StandardGraph::InterpretNum64Compress,
            ),
            _ => {
                return Err(GraphError::Invalid(
                    "the element width must be 1, 2, 4 or 8",
                ));
            }
        };
        let params = GraphParams::default()
            .with_graphs([successor.unwrap_or_else(|| default.into())])
            .with_params(
                LocalParams::default()
                    .with_int(engine::graphs::NUM_FROM_SERIAL_ELT_WIDTH, width as i32)
                    .with_int(
                        engine::graphs::NUM_FROM_SERIAL_CHUNK_BYTE_SIZE,
                        i32::try_from(usize::from(chunk)).unwrap_or(i32::MAX),
                    ),
            );
        self.parameterize_graph(base, params)
    }

    /// The SDDL graph: parses serial data with a compiled description and
    /// sends the fields it cuts out to `successor`
    /// (`ZL_Compressor_buildSDDLGraph`).
    ///
    /// The description is the CBOR program the SDDL compiler writes. The
    /// successor receives every output of the dispatch node (tags, segment
    /// sizes, then one stream per destination of the description) as one
    /// multi-input destination.
    ///
    /// # Errors
    ///
    /// A description that is not a loadable SDDL program, or an unknown
    /// successor.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::graph::{GraphBuilder, StandardGraph};
    ///
    /// // `{"exprs": []}`: a program that consumes nothing.
    /// let description = [0xA1, 0x65, b'e', b'x', b'p', b'r', b's', 0x80];
    /// let mut builder = GraphBuilder::new();
    /// let graph = builder.sddl(&description, StandardGraph::Store)?;
    /// let compressor_graph = builder.build(graph)?;
    /// # let _ = compressor_graph;
    /// # Ok::<(), mopenzl::graph::GraphError>(())
    /// ```
    pub fn sddl(
        &mut self,
        description: &[u8],
        successor: impl Into<GraphId>,
    ) -> Result<GraphId, GraphError> {
        if !engine::graphs::validate_sddl(description) {
            return Err(GraphError::Invalid(
                "the description is not a loadable SDDL program",
            ));
        }
        let params = GraphParams::default()
            .with_graphs([successor.into()])
            .with_params(
                LocalParams::default()
                    .with_bytes(engine::graphs::SDDL_DESCRIPTION_PID, description.to_vec()),
            );
        self.parameterize_graph(StandardGraph::SimpleDataDescriptionLanguage, params)
    }

    /// The SDDL2 segmenter: runs `bytecode` over serial data once, cuts the
    /// data into chunks of about `chunk_size` bytes along the segments the
    /// program defines, and compresses each chunk's typed fields with
    /// `successor` (`ZL_Compressor_registerSDDL2Graph_advanced`).
    ///
    /// A `chunk_size` of 0 takes the default of 16 MiB; `successor` defaults
    /// to `compress_generic`. The bytecode is validated when the data is
    /// compressed, not here.
    ///
    /// # Errors
    ///
    /// A bytecode whose length is not a multiple of 4, a chunk size above
    /// `i32::MAX`, or an unknown successor.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::graph::GraphBuilder;
    ///
    /// // push.i32 4, segment.create_unspecified, halt
    /// let words: [u32; 4] = [0x0001_0003, 4, 0x000C_0001, 0x0005_0001];
    /// let bytecode: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    /// let mut builder = GraphBuilder::new();
    /// let graph = builder.sddl2(&bytecode, 0, None)?;
    /// let compressor_graph = builder.build(graph)?;
    /// # let _ = compressor_graph;
    /// # Ok::<(), mopenzl::graph::GraphError>(())
    /// ```
    pub fn sddl2(
        &mut self,
        bytecode: &[u8],
        chunk_size: usize,
        successor: Option<GraphId>,
    ) -> Result<GraphId, GraphError> {
        if !bytecode.len().is_multiple_of(4) {
            return Err(GraphError::Invalid(
                "SDDL2 bytecode must be a whole number of 32-bit words",
            ));
        }
        let chunk_size = i32::try_from(chunk_size)
            .map_err(|_| GraphError::Invalid("the SDDL2 chunk size exceeds i32::MAX"))?;
        let mut local = LocalParams::default()
            .with_bytes(engine::graphs::SDDL2_BYTECODE_PARAM, bytecode.to_vec());
        if chunk_size != 0 {
            local = local.with_int(engine::graphs::SDDL2_CHUNK_BYTE_SIZE_PARAM, chunk_size);
        }
        let mut params = GraphParams::default().with_params(local);
        if let Some(successor) = successor {
            params = params.with_graphs([successor]);
        }
        self.parameterize_graph(StandardGraph::SimpleDataDescriptionLanguageV2, params)
    }

    /// `brute_force`: tries every successor on the stream and keeps the one
    /// with the smallest frame, or `store` (`ZL_Compressor_buildBruteForceSelectorGraph`).
    ///
    /// # Errors
    ///
    /// No successors, or unknown ones.
    pub fn brute_force(&mut self, successors: &[GraphId]) -> Result<GraphId, GraphError> {
        if successors.is_empty() {
            return Err(GraphError::Invalid(
                "brute force needs at least one successor",
            ));
        }
        self.parameterize_graph(
            StandardGraph::BruteForce,
            GraphParams::default().with_graphs(successors),
        )
    }

    /// The ML selector graph: sends its numeric input to the successor a
    /// gradient-boosted-tree model picks from statistics of the input
    /// (`ZL_MLSelector_registerGraph`).
    ///
    /// The number of `successors` is not checked here: as in the reference, a
    /// model whose successor count differs makes the graph fail when it runs,
    /// and a permissive compression then falls back to `compress_generic`.
    ///
    /// # Errors
    ///
    /// Unknown successors.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::graph::{GraphBuilder, MlSelectorConfig, StandardGraph};
    ///
    /// let mut builder = GraphBuilder::new();
    /// let config = MlSelectorConfig::untrained(2)?;
    /// let store = StandardGraph::Store.into();
    /// let zstd = StandardGraph::Zstd.into();
    /// let selector = builder.ml_selector(&config, &[store, zstd])?;
    /// let graph = builder.build(selector)?;
    /// # let _ = graph;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn ml_selector(
        &mut self,
        config: &MlSelectorConfig,
        successors: &[GraphId],
    ) -> Result<GraphId, GraphError> {
        self.parameterize_graph_with(
            StandardGraph::MlSelector.into(),
            GraphParams::default().with_graphs(successors),
            Some(RefParam(config.materialized())),
        )
    }

    /// The ML selector of `ZL_Compressor_buildUntrainedMLSelector`: a model
    /// that always picks the first of `successors`.
    ///
    /// # Errors
    ///
    /// Fewer than 2 successors, or unknown ones.
    pub fn untrained_ml_selector(&mut self, successors: &[GraphId]) -> Result<GraphId, GraphError> {
        let config = MlSelectorConfig::untrained(successors.len())
            .map_err(|_| GraphError::Invalid("an ML selector needs at least two successors"))?;
        self.ml_selector(&config, successors)
    }

    /// The clustering graph with the four standard concat nodes as
    /// clustering codecs (`ZL_Clustering_registerGraph`).
    ///
    /// Every input of the graph must carry the integer metadata
    /// [`CLUSTERING_TAG_METADATA_ID`] (see
    /// [`Input::with_metadata`](crate::input::Input::with_metadata)).
    ///
    /// # Errors
    ///
    /// Unknown successors, or a configuration that names a clustering codec
    /// outside the four.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::DataType;
    /// use mopenzl::graph::{ClusteringConfig, GraphBuilder, StandardGraph};
    ///
    /// let mut builder = GraphBuilder::new();
    /// let config = ClusteringConfig::default().with_cluster(DataType::Numeric, 4, 0, 0, [1, 2]);
    /// let clustering = builder.clustering(&config, &[StandardGraph::Store.into()])?;
    /// let graph = builder.build(clustering)?;
    /// # let _ = graph;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn clustering(
        &mut self,
        config: &ClusteringConfig,
        successors: &[GraphId],
    ) -> Result<GraphId, GraphError> {
        let codecs: Vec<NodeId> = [
            StandardNode::ConcatSerial,
            StandardNode::ConcatStruct,
            StandardNode::ConcatNum,
            StandardNode::ConcatString,
        ]
        .into_iter()
        .map(NodeId::from)
        .collect();
        self.clustering_with_codecs(config, successors, &codecs)
    }

    /// The clustering graph with custom clustering codecs: nodes with one
    /// variable input and an optional numeric output followed by a typed
    /// output (`ZL_Clustering_registerGraphWithCustomClusteringCodecs`).
    ///
    /// # Errors
    ///
    /// Unknown successors or codecs, or a configuration that names a
    /// clustering codec index past `codecs`.
    pub fn clustering_with_codecs(
        &mut self,
        config: &ClusteringConfig,
        successors: &[GraphId],
        codecs: &[NodeId],
    ) -> Result<GraphId, GraphError> {
        if config.needs().1 > codecs.len() {
            return Err(GraphError::Invalid(
                "a cluster names a clustering codec that does not exist",
            ));
        }
        let blob: Vec<u8> = config.into();
        self.parameterize_graph(
            StandardGraph::Clustering,
            GraphParams::default()
                .with_graphs(successors)
                .with_nodes(codecs)
                .with_params(LocalParams::default().with_bytes(CLUSTERING_CONFIG_PARAM, blob)),
        )
    }

    /// Freezes the graphs, compressing with `start`.
    ///
    /// # Errors
    ///
    /// An unknown start graph, or [`GraphError::DictionaryNotFound`] for a
    /// node that references a dictionary the loaded bundle (if any) lacks
    /// (`CGraph_resolveDictIndices`).
    pub fn build(mut self, start: impl Into<GraphId>) -> Result<CompressorGraph, GraphError> {
        self.registry.start = self.graph_id(start.into())?;
        self.registry
            .resolve_dict_indices()
            .map_err(|error| match error {
                LoadError::NotFound(id) => GraphError::DictionaryNotFound(id),
                LoadError::Dict(error) => GraphError::Dictionary(error),
                LoadError::AlreadyLoaded => GraphError::BundleAlreadyLoaded,
            })?;
        Ok(CompressorGraph(Arc::new(self.registry)))
    }
}

/// The frozen graphs of a compressor, cheap to clone and share between
/// threads.
#[derive(Clone, Debug)]
pub struct CompressorGraph(pub(crate) Arc<Registry>);

impl Default for CompressorGraph {
    /// Standard `compress_generic`.
    fn default() -> Self {
        Self(Arc::new(Registry::new(0, reg::gid::COMPRESS_GENERIC)))
    }
}

impl CompressorGraph {
    /// A compressor graph that starts at a standard graph.
    #[must_use]
    pub fn standard(start: StandardGraph) -> Self {
        Self(Arc::new(Registry::new(
            0,
            reg::GraphId::std(u32::from(start)),
        )))
    }
}

impl From<StandardGraph> for CompressorGraph {
    fn from(start: StandardGraph) -> Self {
        Self::standard(start)
    }
}
