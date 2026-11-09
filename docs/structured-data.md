# Structured data

OpenZL's advantage over a general-purpose compressor is knowing the *shape* of the
data. Tell it that a buffer is an array of 32-bit timestamps, 16-byte records or a
column of strings, and a graph can split, transform and entropy-code each part with
the codec that fits. This guide covers the typed inputs, the standard graphs that fit
each shape, clustering of related inputs, ML-selected successors and the SDDL
description languages. [Graphs](graphs.md) explains the graph machinery.

Always decode typed frames with `Decompressor::decompress_typed`: the byte-oriented
methods read one serial output.

## Numeric data

`Numbers::try_from((bytes, width))` wraps little-endian unsigned integers of width 1,
2, 4 or 8. For signed or floating-point data pass the bit patterns; the numeric
codecs treat them as unsigned (`zigzag`, `float32_deconstruct` and friends are the
bridges). Pick a graph by effort:

| Graph | Behavior |
| --- | --- |
| `SelectNumeric` (also what `CompressGeneric` uses) | Below level 7, `FieldLz`; from level 7, the numeric transformer |
| `TransformerNumeric` | The numeric transformer: scores candidate chains (delta, range pack, tokenize, GCD, sparse, ...) with a trained network and runs the best |
| `FieldLz` | LZ over fixed-width fields |
| `Entropy`, `Huffman`, `Fse` | Entropy coding of the values (width 1 and 2 directly) |
| `Bitpack`, `PartitionBitpack` | Narrow values; partitioned value ranges for 16- and 32-bit numbers |
| `Constant` | One repeated value |
| `BruteForce` | Whichever of your candidates is smallest |

Higher levels cost more time: the transformer is only used from level 7, and it is a
model's best guess, not a guarantee: measure on your data (the example below prints
the sizes).

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::{CompressorGraph, StandardGraph};
use mopenzl::input::{Input, Numbers};
use mopenzl::{Compressor, EncoderConfig, Level};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A slowly growing counter with a little noise, 64-bit.
    let values: Vec<u8> = (0..20_000u64)
        .flat_map(|i| (1_000_000 + i * 16 + (i * 2654435761) % 5).to_le_bytes())
        .collect();
    let input = Input::from(Numbers::try_from((&values[..], 8))?);

    for (name, graph, level) in [
        ("zstd", StandardGraph::Zstd, 6),
        ("field_lz", StandardGraph::FieldLz, 6),
        ("select_numeric, level 6", StandardGraph::SelectNumeric, 6),
        ("select_numeric, level 10", StandardGraph::SelectNumeric, 10),
    ] {
        let config = EncoderConfig::default().with_level(Level::try_from(level)?);
        let mut compressor = Compressor::builder(config)
            .with_graph(CompressorGraph::standard(graph))
            .build()?;
        // `Zstd` takes serial input and nodes do no implicit conversion, so it is
        // shown on the raw bytes.
        let frame = if graph == StandardGraph::Zstd {
            compressor.compress(&values)?
        } else {
            compressor.compress_inputs(&[input])?
        };
        println!("{name}: {} -> {} bytes", values.len(), frame.len());
    }
    Ok(())
}
```

## Fixed-width records

`Records::try_from((bytes, width))` wraps records of any width. Record-aware graphs
treat each byte position of a record as its own column:

| Graph | Behavior |
| --- | --- |
| `StructCompress` (what `CompressGeneric` uses) | `FieldLz` over the records |
| `FieldLz` | LZ over whole records |
| `TransposeSplit` | Column-wise bytes, but it needs five successors ([graphs that need successors](graphs.md#graphs-that-need-successors)); a function graph with the `TransposeSplit` node is simpler |
| `Entropy`, `Huffman` | Entropy coding of 16-bit records |
| `Constant` | One repeated record |

To compress each byte *column* of the records on its own, run the `TransposeSplit`
node (one serial stream per byte of the record) from a function graph and send every
column to a successor. `GraphContext::run_split_node` and the `split_by_struct` node
cut records into fields of different widths, and [SDDL](#sddl-and-sddl2) describes
whole layouts.

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::{
    Edge, FunctionGraph, GraphBuilder, GraphContext, GraphError, StandardGraph, StandardNode,
};
use mopenzl::input::{Input, Records};
use mopenzl::{Compressor, DataType, Decompressor, EncoderConfig};

/// One serial stream per byte of the record, each compressed on its own.
struct Columns;

impl FunctionGraph for Columns {
    fn compress(&self, cx: &mut GraphContext<'_>, inputs: &[Edge]) -> Result<(), GraphError> {
        let columns = cx.run_node(StandardNode::TransposeSplit, inputs)?;
        let successor = cx.custom_graphs()[0];
        for column in columns {
            cx.set_destination(&[column], successor)?;
        }
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 16-byte records: a counter, a small enum, a constant tail.
    let mut bytes = Vec::new();
    for i in 0..5_000u32 {
        bytes.extend_from_slice(&i.to_le_bytes());
        bytes.extend_from_slice(&(i % 3).to_le_bytes());
        bytes.extend_from_slice(&[0xAB; 8]);
    }
    let records = Records::try_from((&bytes[..], 16))?;

    let mut builder = GraphBuilder::new();
    let columns = builder.function_graph(
        "columns",
        Columns,
        &[DataType::Struct.into()],
        false,
        &[StandardGraph::Zstd.into()],
        &[],
    )?;
    let graph = builder.build(columns)?;
    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()?;
    let frame = compressor.compress_inputs(&[Input::from(records)])?;
    println!("{} -> {} bytes", bytes.len(), frame.len());

    let outputs = Decompressor::default().decompress_typed(&frame)?;
    assert_eq!(outputs[0].data_type(), DataType::Struct);
    assert_eq!(outputs[0].width(), 16);
    assert_eq!(outputs[0].content(), bytes);
    Ok(())
}
```

## Strings

`Strings::try_from((content, &lengths))` pairs concatenated bytes with one `u32`
length per string; the lengths must sum to the content size. `StringCompress` (what
`CompressGeneric` uses) separates the two components and compresses the content as
serial and the lengths as numeric. `TryParseInt` parses columns of decimal numbers as
integers when they are canonical decimals; it takes three successors you provide
(`GraphParams::with_graphs`): the integers, the strings that did not parse, and the
indices that tell the two apart. The `tokenize_string` nodes build an alphabet for
columns of repeated values.

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::{GraphBuilder, GraphParams, StandardGraph};
use mopenzl::input::{Input, Strings};
use mopenzl::{Compressor, DataType, Decompressor, EncoderConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A column of numbers written as text.
    let column: Vec<String> = (0..3_000u32)
        .map(|i| (10_000 + i * 7).to_string())
        .collect();
    let content: Vec<u8> = column.concat().into_bytes();
    let lens: Vec<u32> = column.iter().map(|s| s.len() as u32).collect();
    let strings = Strings::try_from((&content[..], &lens[..]))?;

    // successors: [integers, strings that did not parse, indices]
    let mut builder = GraphBuilder::new();
    let parse = builder.parameterize_graph(
        StandardGraph::TryParseInt,
        GraphParams::default().with_graphs([
            StandardGraph::NumericCompress.into(),
            StandardGraph::StringCompress.into(),
            StandardGraph::NumericCompress.into(),
        ]),
    )?;
    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(builder.build(parse)?)
        .build()?;
    let frame = compressor.compress_inputs(&[Input::from(strings)])?;
    println!("{} -> {} bytes", content.len(), frame.len());

    let outputs = Decompressor::default().decompress_typed(&frame)?;
    assert_eq!(outputs[0].data_type(), DataType::String);
    assert_eq!(outputs[0].string_lens(), lens);
    assert_eq!(outputs[0].content(), content);
    Ok(())
}
```

## Clustering related inputs with tags

A frame may carry several inputs of different types. The `Clustering` graph groups
them by `(type, element width, tag)`, concatenates each group with a concat node and
compresses it with the successor its cluster names, so inputs that look alike
compress together. Tags are integer metadata with id `CLUSTERING_TAG_METADATA_ID`
attached with `Input::with_metadata`; metadata never reaches the frame.

`ClusteringConfig` describes the clusters:

- `with_cluster(type, width, successor, codec, tags)` makes a cluster of the inputs
  of that type and width whose tag is in `tags`. `successor` and `codec` index the
  successor graphs and clustering codecs given to the builder (`clustering` uses the
  four standard concat nodes: serial 0, struct 1, numeric 2, string 3).
- `with_type_default(type, width, successor, codec)` handles inputs of that type
  whose tag no cluster lists.
- Inputs matching neither form their own cluster, compressed by `CompressGeneric`.
- The width of a serial input is 1 and that of a string input 0.

The configuration serializes to the reference's CBOR blob
(`Vec::from(&config)`, `ClusteringConfig::try_from(&bytes[..])`), so a configuration
trained elsewhere loads here.

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::{CLUSTERING_TAG_METADATA_ID, ClusteringConfig, GraphBuilder, StandardGraph};
use mopenzl::input::{Input, Numbers};
use mopenzl::{Compressor, DataType, Decompressor, EncoderConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let latency: Vec<u8> = (0..4000u32)
        .flat_map(|i| (20 + i % 9).to_le_bytes())
        .collect();
    let latency_p99: Vec<u8> = (0..4000u32)
        .flat_map(|i| (90 + i % 13).to_le_bytes())
        .collect();
    let ids: Vec<u8> = (0..4000u32)
        .flat_map(|i| (1_000_000 + i).to_le_bytes())
        .collect();
    let tags = [
        [(CLUSTERING_TAG_METADATA_ID, 1)],
        [(CLUSTERING_TAG_METADATA_ID, 2)],
        [(CLUSTERING_TAG_METADATA_ID, 3)],
    ];
    let inputs = [
        Input::from(Numbers::try_from((&latency[..], 4))?).with_metadata(&tags[0]),
        Input::from(Numbers::try_from((&latency_p99[..], 4))?).with_metadata(&tags[1]),
        Input::from(Numbers::try_from((&ids[..], 4))?).with_metadata(&tags[2]),
    ];

    // Tags 1 and 2 share a cluster (successor 0); tag 3 falls to the type default.
    let config = ClusteringConfig::default()
        .with_cluster(DataType::Numeric, 4, 0, 2, [1, 2])
        .with_type_default(DataType::Numeric, 4, 1, 2);
    let mut builder = GraphBuilder::new();
    let clustering = builder.clustering(
        &config,
        &[
            StandardGraph::SelectNumeric.into(),
            StandardGraph::FieldLz.into(),
        ],
    )?;
    let graph = builder.build(clustering)?;

    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()?;
    let frame = compressor.compress_inputs(&inputs)?;

    let outputs = Decompressor::default().decompress_typed(&frame)?;
    assert_eq!(outputs.len(), 3);
    assert_eq!(outputs[0].content(), latency);
    assert_eq!(outputs[1].content(), latency_p99);
    assert_eq!(outputs[2].content(), ids);
    Ok(())
}
```

## ML selectors

`MlSelector` sends a numeric input to the successor a gradient-boosted-tree model
picks from statistics of the data. The statistics (features) are `nbElts`,
`eltWidth`, `cardinality`, `cardinality_upper`, `cardinality_lower`, `range_size`,
`mean`, `variance`, `stddev`, `skewness` and `kurtosis`; a name outside this list
reads as missing.

`MlSelectorConfig` wraps the model:

- `MlSelectorConfig::try_from(&GbtModel)` validates a model as the reference does and
  serializes it; `MlSelectorConfig::try_from(&blob[..])` loads a trained CBOR blob
  (the model is not validated, so a bad one may fail when the graph runs).
- A model with one forest is binary (two successors; class 1 when the summed leaves
  are not negative); otherwise there is one forest per successor and the highest
  score wins. `GbtNode::split(feature, threshold, left, right, missing)` and
  `GbtNode::leaf(score)` build trees; a feature strictly below its threshold goes
  left, and children must come after their parent.
- `GraphBuilder::untrained_ml_selector(&successors)` always picks the first
  successor. The number of successors must match the model when the graph runs;
  otherwise it fails (and a permissive compression falls back to `compress_generic`).

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::{GbtModel, GbtNode, GraphBuilder, MlSelectorConfig, StandardGraph};
use mopenzl::input::{Input, Numbers};
use mopenzl::{Compressor, Decompressor, EncoderConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // One tree on `nbElts`: fewer than 64 elements -> class 0 (store), else class 1.
    let tree = vec![
        GbtNode::split(0, 64.0, 1, 2, 1),
        GbtNode::leaf(-1.0),
        GbtNode::leaf(1.0),
    ];
    let model = GbtModel {
        forests: vec![vec![tree]],
        successors: 2,
        features: vec!["nbElts".to_string()],
    };
    let config = MlSelectorConfig::try_from(&model)?;

    let mut builder = GraphBuilder::new();
    let selector = builder.ml_selector(
        &config,
        &[StandardGraph::Store.into(), StandardGraph::FieldLz.into()],
    )?;
    let graph = builder.build(selector)?;
    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()?;

    let mut decoder = Decompressor::default();
    for count in [16u32, 4096] {
        let values: Vec<u8> = (0..count).flat_map(|i| (i % 5).to_le_bytes()).collect();
        let numbers = Numbers::try_from((&values[..], 4))?;
        let frame = compressor.compress_inputs(&[Input::from(numbers)])?;
        println!("{count} values -> {} bytes", frame.len());
        assert_eq!(decoder.decompress_typed(&frame)?[0].content(), values);
    }
    Ok(())
}
```

## SDDL and SDDL2

The Simple Data Description Language describes the layout of a binary format so a
graph can cut a buffer into typed fields: the integers of a header, the records of an
array, the floats of a column. The reference compiles descriptions with C++ tools
that are not part of this crate; mopenzl takes the compiled program.

- **SDDL** (`GraphBuilder::sddl(&description, successor)`): the description is the
  CBOR program the SDDL compiler writes (`{"exprs": [...]}`). The graph runs it,
  dispatches the fields it cuts out to destinations, converts them to numeric where
  the description says so, and hands every stream (tags, segment sizes, then one per
  destination) to `successor` as one multi-input destination. An invalid program is a
  `GraphError::Invalid` at build time; a description that does not match the data
  fails when compressing (and falls back only in permissive mode).
- **SDDL2** (`GraphBuilder::sddl2(&bytecode, chunk_size, successor)`): the segmenter
  runs bytecode (32-bit little-endian words) once over the input, cuts chunks of
  about `chunk_size` bytes (0 = the 16 MiB default) along the segments the program
  defines, and compresses each chunk's typed fields with `successor` (default
  `CompressGeneric`). The bytecode is validated when the data is compressed.

The example below builds a tiny SDDL description by hand — "an array of 1000
little-endian `u32` fields, sent to destination `d0`" — and a minimal SDDL2 program.

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::{GraphBuilder, StandardGraph};
use mopenzl::{Compressor, Decompressor, EncoderConfig};

// A minimal CBOR writer: just the shapes the program needs.
fn head(major: u8, n: usize) -> Vec<u8> {
    assert!(n < 24);
    vec![major << 5 | n as u8]
}
fn text(s: &str) -> Vec<u8> {
    [head(3, s.len()), s.as_bytes().to_vec()].concat()
}
fn map1(key: &str, value: Vec<u8>) -> Vec<u8> {
    [head(5, 1), text(key), value].concat()
}
fn array(items: &[Vec<u8>]) -> Vec<u8> {
    [head(4, items.len()), items.concat()].concat()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let records = 1000u32;
    // d0 = dest; consume(array(send(atom("u4l"), d0), int(records)))
    let var_d0 = map1("var", text("d0"));
    let assign = map1("assign", array(&[var_d0.clone(), map1("dest", vec![0xF6])]));
    let count = map1(
        "int",
        [vec![0x19], (records as u16).to_be_bytes().to_vec()].concat(),
    );
    let field = map1("send", array(&[map1("atom", text("u4l")), var_d0]));
    let consume = map1("consume", map1("array", array(&[field, count])));
    let description = map1("exprs", array(&[assign, consume]));

    let mut builder = GraphBuilder::new();
    let sddl = builder.sddl(&description, StandardGraph::CompressGeneric)?;
    let graph = builder.build(sddl)?;
    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()?;
    let data: Vec<u8> = (0..records).flat_map(|i| (i * 3).to_le_bytes()).collect();
    let frame = compressor.compress(&data)?;
    assert_eq!(Decompressor::default().decompress(&frame)?, data);

    // SDDL2: push.i32 4000, segment.create_unspecified, halt (a 4000-byte segment).
    let words: [u32; 4] = [0x0001_0003, 4000, 0x000C_0001, 0x0005_0001];
    let bytecode: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
    let mut builder = GraphBuilder::new();
    let sddl2 = builder.sddl2(&bytecode, 0, None)?;
    let graph = builder.build(sddl2)?;
    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()?;
    let frame = compressor.compress(&data)?;
    assert_eq!(Decompressor::default().decompress(&frame)?, data);
    Ok(())
}
```
