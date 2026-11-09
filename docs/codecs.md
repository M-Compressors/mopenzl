# Codecs

A *codec* is the decoder half of a node: every standard OpenZL transform or coder has
a node (the encoder side, a `StandardNode`) and a decoder in this crate, and both live
behind one Cargo feature. This page lists them. Wire formats, invariants and the
deviations on malformed data are in [decoder codecs](../architecture/decoder-codecs.md);
how nodes are wired into graphs is in [graphs](graphs.md).

Types are `serial` (bytes), `struct` (fixed-width records), `numeric` (little-endian
integers) and `string`. "Since" is the first format version that has the node. The crate writes versions 21
to 27, so a node with a "since" of 21 or less exists at every version you can select;
nodes introduced at 23, 24, 26 or 27 (`lz4`, `lz`, the bit-split family, `partition`,
`sentinel`, `sparse_num`, `pivco_huffman`) are not available when you pick an older
format version.

## Features

`all-codecs` (in `default`) enables every feature below. Without a codec's feature
the encoder never writes it (a chunk that would use it is stored; a graph that runs
its node fails, falling back only in [permissive mode](graphs.md#permissive-mode)) and
the decoder reports frames that use it as `DecodeError::Unsupported`. The decoder
parses the chunk layout in full either way, so the error names the missing codec
before any stream is decoded.

| Feature | Nodes | Codec IDs |
| --- | --- | --- |
| `zstd` | `zstd` | 22 |
| `conversion` | the type conversions and string separation | 5-14 |
| `delta` | `delta_int` | 1 |
| `zigzag` | `zigzag` | 3 |
| `divide-by` | `divide_by` | 53 |
| `quantize` | `quantize_offsets`, `quantize_lengths` | 25, 26 |
| `range-pack` | `range_pack` | 35 |
| `bitsplit` | `bitsplit_*`, `bit_split` | 63 |
| `float-deconstruct` | `float32_`, `bfloat16_`, `float16_deconstruct` | 33 |
| `sparse-num` | `sparse_num`, `sparse_num_auto` | 66 |
| `merge-sorted` | `merge_sorted` | 43 |
| `constant` | `constant_serial`, `constant_fixed` | 44, 45 |
| `prefix` | `prefix` | 46 |
| `transpose` | `transpose_split` | 2, 4, 30-32 |
| `split` | `splitN`, `splitN_struct`, `splitN_num`, `split_byrange` | 40, 47, 48 |
| `split-by-struct` | `split_by_struct` | 41 |
| `dispatch` | `dispatchN_byTag`, `dispatch_string` | 42, 54 |
| `concat` | `concat_serial`, `_num`, `_struct`, `_string` | 55, 57-59 |
| `dedup` | `dedup_num`, `dedup_num_trusted` | 56 |
| `interleave` | `interleave_string` | 61 |
| `mux-lengths` | `mux_lengths` | 65 |
| `sentinel` | `sentinel_byte`, `sentinel_num` | 18 |
| `bitpack` | `bitpack_serial`, `bitpack_int` | 27, 28 |
| `bitunpack` | `bitunpack` | 34 |
| `flatpack` | `flatpack` | 29 |
| `tokenize` | `tokenize_*` | 36-38 |
| `parse-int` | `parse_int` | 60 |
| `partition` | `partition` | 64 |
| `entropy` | `fse_v2`, `huffman_v2`, `huffman_struct_v2`, `fse_ncount` | 49-52 |
| `pivco-huffman` | `pivco_huffman` | 67 |
| `lz4` | `lz4` | 62 |
| `lz` | `lz` | 19 |
| `field-lz` | `field_lz` | 24 |

Feature names with a hyphen are Cargo features (`field-lz`); the library selects them
independently, so `features = ["std", "decompression", "zstd", "delta"]` decodes only
those codecs. Several standard *graphs* need more than one feature (the `entropy`
graph uses most coders); a missing one makes the graph fail at run time. The
`scripts/check_codec_features.py` script builds every combination.

## Nodes

`StandardNode::X` below is the variant name for the registry ID; run a node from a
function graph with `GraphContext::run_node`, or in a chain with
`GraphBuilder::pipeline`. "Key parameters" are local parameters (see
[parameterized nodes](graphs.md#parameterized-nodes-and-graphs)) whose IDs are the
reference's; nodes not listed take none that matter.

### Numeric transforms

| ID | `StandardNode` | Feature | Since | In | Out | What it does and key parameters |
| --- | --- | --- | --- | --- | --- | --- |
| 2 | `DeltaInt` | `delta` | 3 | numeric | numeric | Replaces values with differences from their predecessor; the first value goes in the header |
| 4 | `Zigzag` | `zigzag` | 3 | numeric | numeric | Maps signed-looking values to small unsigned ones |
| 31 | `DivideBy` | `divide-by` | 16 | numeric | numeric | Divides every value by a common divisor. Copy parameter `params::divide_by::DIVIDE_BY` (112): the divisor as 8 little-endian bytes; absent or zero means the greatest common divisor |
| 28 | `RangePack` | `range-pack` | 8 | numeric | numeric | Rebases values on their minimum into a narrower width |
| 43, 44 | `QuantizeOffsets`, `QuantizeLengths` | `quantize` | 3 | numeric | numeric, serial | Splits values into a small code and extra bits (LZ offsets and lengths) |
| 45-47, 75 | `BitsplitTop8`, `BitsplitFp`, `BitsplitBf16`, `BitSplit` | `bitsplit` | 24 | numeric | variable numeric | Splits each number into bit-field streams. Copy parameter 701 sets the field widths of `BitSplit` |
| 6-8 | `Float32Deconstruct`, `Bfloat16Deconstruct`, `Float16Deconstruct` | `float-deconstruct` | 4 (5 for the 16-bit ones) | numeric | struct, serial | Splits floats into sign+fraction and exponent streams |
| 54, 55 | `SparseNum`, `SparseNumAuto` | `sparse-num` | 26 | numeric | numeric, numeric | Mostly-one-value data: the exceptions and their positions. Copy parameter `params::sparse_num::DOMINANT_VALUE` (132, 8 bytes) gives the dominant value of `SparseNumAuto` |
| 50, 51 | `SentinelByte`, `SentinelNum` | `sentinel` | 24 | numeric | numeric, numeric | Exceptional values behind a sentinel. Copy parameter `params::sentinel::INDICES` (130) holds the exception indices, `params::sentinel::VALUE` (131) the sentinel |
| 29 | `MergeSorted` | `merge-sorted` | 9 | numeric | numeric, numeric | Merges up to 64 sorted runs into one sorted stream plus a bitset |
| 48 | `Partition` | `partition` | 24 | numeric | numeric, serial | Bucket IDs plus offsets within value-range partitions. Int parameter `params::partition::PRESET` (120) picks a preset; copy parameter `params::partition::CUSTOM` (121) holds a custom start value and partition sizes |
| 53 | `MuxLengths` | `mux-lengths` | 24 | numeric, numeric | serial, numeric | Multiplexes the two length streams of `lz` |
| 63, 64 | `BitpackSerial`, `BitpackInt` | `bitpack` | 3 | serial / numeric | serial | Packs values into the fewest bits the maximum needs |
| 27 | `Bitunpack` | `bitunpack` | 6 | serial | numeric | Reads bit-packed bytes as numbers of a chosen bit width (the inverse of `BitpackInt`'s packing) |
| 38 | `ParseInt` | `parse-int` | 19 | string | numeric | Parses canonical decimal strings as 64-bit integers |

### Dictionary-style transforms

| ID | `StandardNode` | Feature | Since | In | Out | What it does and key parameters |
| --- | --- | --- | --- | --- | --- | --- |
| 40-42 | `TokenizeStruct`, `TokenizeNumeric`, `TokenizeString` | `tokenize` | 8 (11 for strings) | struct / numeric / string | alphabet, numeric indices | Alphabet plus an index per element. Int parameter `params::tokenize::SORT` (0) non-zero sorts the alphabet |
| 72, 73 | `TokenizeSorted`, `TokenizeStringSorted` | `tokenize` | 8 / 11 | numeric / string | alphabet, numeric indices | The same with sorting preset |
| 65 | `Flatpack` | `flatpack` | 3 | serial | serial, serial | Alphabet of at most 256 symbols plus packed indices |
| 30 | `Prefix` | `prefix` | 11 | string | string, numeric | Shared prefixes with the previous string, as match sizes plus suffixes |
| 70, 71 | `ConstantSerial`, `ConstantFixed` | `constant` | 11 | serial / struct | one element | A stream of one repeated element, as the element and a count |
| 37, 74 | `DedupNum`, `DedupNumTrusted` | `dedup` | 16 | variable numeric | numeric | Identical inputs stored once. Int parameter 9438 = 1 skips the verification (`DedupNumTrusted`) |

### Splitting, merging and routing

| ID | `StandardNode` | Feature | Since | In | Out | What it does and key parameters |
| --- | --- | --- | --- | --- | --- | --- |
| 66-68 | `SplitN`, `SplitNStruct`, `SplitNNum` | `split` | 9 (15 for struct and numeric) | serial / struct / numeric | variable same type | Cuts into consecutive segments. Segment sizes are a reference parameter; run it with `GraphContext::run_split_node` |
| 49 | `SplitByrange` | `split` | 24 | numeric | variable numeric | Splits by value range. Int parameter `params::split::BYRANGE_MIN_SEGMENT_SIZE` (324) is the minimum segment size |
| 69 | `SplitByStruct` | `split-by-struct` | 9 | serial | variable struct | One stream per field of a repeated record. Copy parameter 387 holds the field sizes |
| 5 | `DispatchNByTag` | `dispatch` | 9 | serial | tags, sizes, variable serial | Routes segments to streams by tag; run it with `GraphContext::run_dispatch_node` and `DispatchInstructions` |
| 32 | `DispatchString` | `dispatch` | 16 | string | indices, variable string | Routes strings to streams by index; `GraphContext::run_dispatch_string_node`. Int parameter 47 is the number of outputs |
| 33-36 | `ConcatSerial`, `ConcatNum`, `ConcatStruct`, `ConcatString` | `concat` | 16-18 | variable same type | sizes, concatenation | Concatenates inputs, remembering their sizes (used by clustering) |
| 39 | `InterleaveString` | `interleave` | 20 | variable string | string | Round-robin interleaving of string streams |
| 3 | `TransposeSplit` | `transpose` | 11 | struct | variable serial | Column-wise bytes of the records |

### Type conversions

The `conversion` feature covers IDs 10-26 and 57. They change the type of a stream,
not its bytes, and appear in frames whenever typed data meets a serial-only codec.

| ID | `StandardNode` | In | Out | Notes |
| --- | --- | --- | --- | --- |
| 10 | `ConvertStructToSerial` | struct | serial | |
| 11, 12 | `ConvertNumToStructLe`, `ConvertNumToSerialLe` | numeric | struct / serial | |
| 13-16 | `ConvertSerialToStruct`, `...Struct2`, `...Struct4`, `...Struct8` | serial | struct | Int parameter 1 is the record width; the 2/4/8 variants preset it |
| 17, 18 | `ConvertStructToNumLe`, `ConvertStructToNumBe` | struct | numeric | Big-endian from format 21 |
| 19-22 | `ConvertSerialToNum8`, `...NumLe16`, `...NumLe32`, `...NumLe64` | serial | numeric | |
| 23-25 | `ConvertSerialToNumBe16`, `...Be32`, `...Be64` | serial | numeric | Format 21 and newer |
| 26 | `SeparateStringComponents` | string | serial (content), numeric (lengths) | |
| 57 | `SetStringLens` | serial | string | Lengths come from `LocalParams::with_string_lengths` |

### Entropy coders

| ID | `StandardNode` | Feature | Since | In | Out | What it does |
| --- | --- | --- | --- | --- | --- | --- |
| 58 | `FseV2` | `entropy` | 15 | serial | numeric counts, serial bits | Finite State Entropy |
| 59 | `HuffmanV2` | `entropy` | 15 | serial | numeric weights, serial bits | Huffman (zstd's `huf`) |
| 60 | `HuffmanStructV2` | `entropy` | 15 | struct (width 2) | numeric weights, serial bits | Huffman over 16-bit symbols |
| 61 | `FseNcount` | `entropy` | 15 | numeric | serial | Serializes FSE count tables |
| 87 | `PivcoHuffman` | `pivco-huffman` | 27 | serial | numeric weights, serial bits | Pivoted-code Huffman |

These come from OpenZL's own FSE and Huffman fork, not from `mzstd`.

### LZ and general-purpose coders

| ID | `StandardNode` | Feature | Since | In | Out | What it does and key parameters |
| --- | --- | --- | --- | --- | --- | --- |
| 62 | `Zstd` | `zstd` | 3 | serial | serial | Zstandard magicless frame via `mzstd`. Integer parameters are raw `ZSTD_c_*` IDs, named by `params::zstd` (`COMPRESSION_LEVEL`, `WINDOW_LOG`, ...), applied after the global level; `params::zstd::FORMAT` and `params::zstd::CONTENT_SIZE_FLAG` are refused, as in the reference |
| 86 | `Lz4` | `lz4` | 23 | serial | serial | LZ4 blocks (fast, high-compression and optimal parsers ported from lz4 1.10.0). Int parameter `params::lz4::COMPRESSION_LEVEL_OVERRIDE` (0) overrides the level |
| 52 | `Lz` | `lz` | 24 | serial | literals, offsets, literal lengths, match lengths | The generic LZ codec; the `Lz` graph entropy-codes the four streams |
| 9 | `FieldLz` | `field-lz` | 3 | struct | literals, tokens, offsets, extra literal and match lengths | LZ over fixed-width fields. Int parameter `params::field_lz::COMPRESSION_LEVEL_OVERRIDE` (181) overrides the compression level (1-3 fast finder, 4 and up greedy) |

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::params::{divide_by, zstd};
use mopenzl::graph::{GraphBuilder, LocalParams, StandardGraph, StandardNode};
use mopenzl::input::{Input, Numbers};
use mopenzl::{Compressor, Decompressor, EncoderConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = GraphBuilder::new();
    // divide_by with an explicit divisor of 1000, then zstd at level 19.
    let divide = builder.parameterize_node(
        StandardNode::DivideBy,
        LocalParams::default().with_bytes(divide_by::DIVIDE_BY, 1000u64.to_le_bytes().to_vec()),
    )?;
    let zstd19 = builder.parameterize_node(
        StandardNode::Zstd,
        LocalParams::default().with_int(zstd::COMPRESSION_LEVEL, 19),
    )?;
    let to_bytes = builder.static_graph(zstd19, &[StandardGraph::Store.into()])?;
    let start = builder.pipeline(
        &[divide, StandardNode::ConvertNumToSerialLe.into()],
        to_bytes,
    )?;
    let graph = builder.build(start)?;

    let values: Vec<u8> = (0..4096u32)
        .flat_map(|i| (i * 1000).to_le_bytes())
        .collect();
    let numbers = Numbers::try_from((&values[..], 4))?;
    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()?;
    let frame = compressor.compress_inputs(&[Input::from(numbers)])?;
    let outputs = Decompressor::default().decompress_typed(&frame)?;
    assert_eq!(outputs[0].content(), values);
    Ok(())
}
```

## Custom codecs

Codecs registered by another application (`custom` codec IDs) are not supported:
the decoder reports `DecodeError::Unsupported { feature: Unsupported::CustomCodec { id } }`
and `GraphBuilder` cannot register new nodes, only compose standard ones.

Legacy codecs whose newest format version is below 21 (`rolz`, `fastlz`, `zstd_fixed`
and the deprecated FSE and Huffman nodes) are not registered and decode as
`Unsupported::Codec`.
