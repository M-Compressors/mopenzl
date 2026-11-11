# Graphs

OpenZL compresses by routing typed streams through a *graph* of codecs. This guide
explains how the graphs of this crate work, which standard graphs exist, and how to
build your own with `mopenzl::graph`. Frame-level mechanics live in
[compression graphs](../architecture/compressor-graphs.md).

[Types](#typed-inputs-and-outputs) · [Standard graphs](#standard-graphs) ·
[`GraphBuilder`](#building-graphs) · [Parameters](#parameterized-nodes-and-graphs) ·
[Selectors, function graphs, segmenters](#custom-graphs-in-rust) ·
[Chunking](#chunking-and-segmenters) · [`try_graph`](#trial-compression-try_graph) ·
[Permissive mode](#permissive-mode) · [Sessions](#sessions-and-graphs)

## Typed inputs and outputs

Every stream has a `DataType`:

| Type | Holds | Input constructor | Decoded as |
| --- | --- | --- | --- |
| `Serial` | raw bytes | `Input::from(&[u8])` | `TypedOutput::content()` |
| `Struct` | fixed-width records of any width | `Input::from(Records::try_from((bytes, width))?)` | content, `width()` |
| `Numeric` | little-endian unsigned integers of width 1, 2, 4 or 8 | `Input::from(Numbers::try_from((bytes, width))?)` | content, `width()` |
| `String` | variable-length strings: concatenated bytes plus one `u32` length each | `Input::from(Strings::try_from((bytes, &lens))?)` | content, `string_lens()` |

The constructors validate the layout (widths, whole elements, lengths summing to the
content size) and return `InputError` otherwise. Numbers are always little-endian
bytes. `Input::with_metadata(&[(id, value)])` attaches integer metadata that graphs
can read, such as the tag the clustering graph groups by; metadata is never written
to the frame.

`Compressor::compress_inputs(&[Input])` writes one frame from one or more inputs
and `Decompressor::decompress_typed` returns one `TypedOutput` per input, in order.
`Compressor::compress` is `compress_inputs` with a single serial input.

A *node* is one codec invocation: it takes typed streams and produces typed
streams (`delta_int`: numeric to numeric; `float32_deconstruct`: numeric to a struct
plus a serial; `tokenize`: any to an alphabet plus numeric indices). A *graph* is
anything that accepts streams and decides what happens to them: a node plus
successor graphs, a selector, a function graph, a segmenter, or `store`. Nodes do
no implicit type conversion when you run them yourself; the engine converts
implicitly only when a stream reaches `store` (numeric and struct data are
converted to serial bytes first, which emits a conversion codec).

## Standard graphs

`StandardGraph` names the graphs of the reference's registry with their numeric
IDs. `CompressorGraph::standard(graph)` (or `CompressorGraph::from`) makes a ready
compressor graph, and `Compressor::builder(config).with_graph(..)` installs it.
Without a graph the compressor runs the default: `compress_generic`.

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::{CompressorGraph, StandardGraph};
use mopenzl::input::{Input, Records, Strings};
use mopenzl::{Compressor, DataType, Decompressor, EncoderConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let text = b"alpha beta gamma delta alpha beta gamma delta ".repeat(20);
    let lens: Vec<u32> = vec![46; 20];
    let records: Vec<u8> = (0..4000u32)
        .flat_map(|i| [(i % 7) as u8, 1, 2, 3])
        .collect();
    let inputs = [
        Input::from(&text[..]),
        Input::from(Records::try_from((&records[..], 4))?),
        Input::from(Strings::try_from((&text[..], &lens[..]))?),
    ];

    // `compress_generic` takes any number of inputs and routes each by type.
    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(CompressorGraph::standard(StandardGraph::CompressGeneric))
        .build()?;
    let frame = compressor.compress_inputs(&inputs)?;

    let outputs = Decompressor::default().decompress_typed(&frame)?;
    let types: Vec<DataType> = outputs.iter().map(|o| o.data_type()).collect();
    assert_eq!(
        types,
        [DataType::Serial, DataType::Struct, DataType::String]
    );
    assert_eq!(outputs[0].content(), text);
    assert_eq!(outputs[1].width(), 4);
    assert_eq!(outputs[2].string_lens(), lens);
    Ok(())
}
```

### Which graph when

Start with `CompressGeneric` (the default) and move to a specific graph when you know
your data. `S`, `St`, `N` and `T` below mean serial, struct, numeric and string
inputs. Graphs that take "any" accept several inputs.

| Need | Use |
| --- | --- |
| General bytes, good ratio | `CompressGeneric` (Zstandard for serial) |
| Fastest decode, or already-compressed data | `Store`, `Lz4` |
| Integers: pick automatically | `SelectNumeric` (the numeric transformer at level 7 and above) |
| Integers with structure, level 7+ | `TransformerNumeric` |
| Fixed-width records | `FieldLz`; column-wise with a function graph and the `TransposeSplit` node ([structured data](structured-data.md#fixed-width-records)) |
| Strings | `StringCompress`; `TryParseInt` for numbers in text (needs successors) |
| Choose empirically | `BruteForce` through `GraphBuilder::brute_force` |
| Data with a known layout | `SimpleDataDescriptionLanguage` (SDDL), `SimpleDataDescriptionLanguageV2` |
| Many inputs of related meaning | `Clustering` (group by tag) |
| Pick by a trained model | `MlSelector` |
| Bound memory on huge serial input | `SegmentSerial` via `GraphBuilder::serial_segmenter` or `Chunking::Fixed` |

### Public graphs

| ID | `StandardGraph` | Inputs | What it does |
| --- | --- | --- | --- |
| 2 | `Store` | any, several | Stores every input without compression. Typed inputs are converted to serial first |
| 3 | `Fse` | S | Finite State Entropy coding |
| 4 | `Huffman` | S, St, N | Huffman coding of byte or 16-bit symbols; picks the Huffman variant |
| 5 | `HuffmanHuf0` | S, St, N | Huffman with the zstd-style `huffman_v2` coder |
| 6 | `HuffmanPivco` | S, St, N | Huffman with the pivoted-code coder (format 27) |
| 7 | `Entropy` | S, St, N | Chooses per 32 KiB chunk among constant, flatpack, bitpack, Huffman, FSE, tokenize and store from statistics |
| 8 | `Constant` | S, St, N | Encodes a stream of one repeated element as that element and a count. Fails at the node when the stream is not constant |
| 9 | `Zstd` | S | The Zstandard node, then store |
| 10 | `Bitpack` | S, N | Packs values into the fewest bits they need |
| 11 | `Flatpack` | S | Alphabet plus packed indices for small alphabets |
| 12 | `FieldLz` | St, N | LZ over fixed-width fields (also for numeric data) |
| 13 | `CompressGeneric` | any, several | Default router: serial to Zstandard; struct to `FieldLz`; numeric to `SelectNumeric`; string to separated components, each compressed by type |
| 14 | `SelectGenericLzBackend` | S | Selects the generic LZ backend (Zstandard) |
| 15 | `SegmentNumeric` | N | Segmenter: cuts numeric input every 16 MiB |
| 16 | `SelectNumeric` | N | Below level 7 the struct route (`FieldLz`); from level 7 the numeric transformer |
| 17 | `MlSelector` | N | Sends the input to the successor a gradient-boosted-tree model picks from statistics of the data |
| 18 | `Clustering` | any, several | Groups inputs by (type, width, tag) into clusters compressed together |
| 19 | `TryParseInt` | T | Parses decimal strings as integers. Needs three successors: integers, strings that did not parse, indices |
| 20 | `SimpleDataDescriptionLanguage` | S | Parses serial data with a compiled SDDL description and sends the fields it cuts out to a successor |
| 21 | `SimpleDataDescriptionLanguageV2` | S | The SDDL2 segmenter: runs bytecode once, cuts chunks along its segments |
| 22 | `Lz4` | S | LZ4 blocks |
| 23 | `PartitionBitpack` | N | Splits the value range into partitions and bitpacks bucket indices (16- and 32-bit numbers) |
| 24-27 | `SegmentNum8FromSerial` ... `SegmentNum64FromSerial` | S | Segmenters that cut serial input aligned to the element width and compress each chunk as numeric |
| 28 | `Lz` | S | The `lz` codec with entropy-coded literals, offsets and lengths |
| 29 | `SegmentSerial` | S | Segmenter: cuts serial input every N bytes (default 16 MiB) |
| 30 | `TransformerNumeric` | N | The numeric transformer: scores candidate transform chains with a trained network |
| 31 | `BruteForce` | any | Tries every successor and keeps the smallest frame, or store |

IDs from 32 on are private in the reference but exist here with stable numbers; they
are useful as successors and for reading the registry:

| IDs | `StandardGraph` | What they do |
| --- | --- | --- |
| 1, 32, 33 | `SerialStore`, `Store1`, `StringStore` | The store primitives |
| 34-39 | `Compress1`, `SerialCompress`, `StructCompress`, `NumericCompress`, `StringCompress`, `StringSeparateCompress` | The routers behind `CompressGeneric` |
| 40-44 | `BitpackSerial`, `BitpackInt`, `ConstantSerial`, `ConstantFixed`, `FseNcount` | Node-plus-store graphs |
| 45, 46 | `FieldLzLiterals`, `FieldLzLiteralsChannel` | Literal handling of `FieldLz` |
| 47-56 | `DeltaHuffman`, `DeltaFlatpack`, `DeltaZstd`, `DeltaFieldLz`, `RangePack`, `RangePackZstd`, `TokenizeDeltaFieldLz` and their `*Internal` forms | Static numeric pipelines |
| 57-60, 62, 64 | `SplitSerial`, `SplitStruct`, `SplitNumeric`, `SplitString`, `NToN`, `TransposeSplit` | Split and transpose graphs |
| 61 | `Sddl2Chunk` | The SDDL2 per-chunk replay graph |
| 63 | `MergeSorted` | Merges sorted runs |
| 65-68 | `InterpretNum8Compress` ... `InterpretNum64Compress` | Serial read as numbers, then `NumericCompress` |
| 69 | `CompressSmallLengths` | Lengths that are mostly tiny with a few large values |
| 70-90 | `TransformerDeltaInt` ... `TransformerNumeric1` | The numeric transformer's static candidates and its model selector |

### Graphs that need successors

Some standard graphs route to graphs you must supply with
`GraphBuilder::parameterize_graph(graph, GraphParams::default().with_graphs([..]))`;
run bare they fail ("needs exactly three custom graphs"). Each variant's rustdoc says
what it needs; in short:

| Graph | Needs | Helper |
| --- | --- | --- |
| `BruteForce` | one or more candidates (none stores) | `GraphBuilder::brute_force` |
| `MlSelector` | one successor per class of its model, and the model | `ml_selector`, `untrained_ml_selector` |
| `Clustering` | the configuration, clustering nodes and the successors it indexes | `clustering`, `clustering_with_codecs` |
| `TryParseInt` | exactly three: integers (numeric), exceptions (strings), indices | none |
| `SimpleDataDescriptionLanguage` | exactly one, plus the compiled description | `sddl` |
| `SimpleDataDescriptionLanguageV2` | zero or one (default `CompressGeneric`), plus the bytecode | `sddl2` |
| `SegmentSerial` | one per chunk (default `CompressGeneric`) | `serial_segmenter` |
| `SegmentNum8FromSerial` ... `SegmentNum64FromSerial` | one per chunk, which receives *serial* data (default `InterpretNumNCompress`) | `num_from_serial_segmenter` |
| `SplitSerial`, `SplitStruct`, `SplitNumeric`, `SplitString` | one custom node and one successor per node output | none |
| `NToN` | one successor per input | none |
| `MergeSorted` | exactly two: the merge graph, then the fallback | none |
| `TransposeSplit` | exactly five: widths 1, 2, 4, 8, then the split graph | none |

`FieldLz` and `Lz` need none; their custom graphs are optional overrides picked by
index through `params::field_lz` and `params::lz`. `SegmentNumeric` needs none.
Every other standard graph is self-contained.

`StandardGraph` and `StandardNode` are `#[non_exhaustive]`; `TryFrom<u32>` and
`From<..> for u32` convert to and from the reference's IDs. Every standard graph
needs the codecs it uses: a graph whose codec feature is off fails like a failing
codec (see [permissive mode](#permissive-mode) and [codecs](codecs.md)).

## Building graphs

`GraphBuilder` starts with only the standard graphs and nodes and derives new ones.
Each call returns a `GraphId` or `NodeId` that is meaningful to that builder only;
`build(start)` freezes everything into a cheap-to-clone, thread-shareable
`CompressorGraph` rooted at `start`.

| Call | Creates |
| --- | --- |
| `custom_codec(id, codec)` | An application-defined node implementing [`CustomCodec`](codecs.md#custom-codecs) (`custom-codec` feature, enabled by default) |
| `static_graph(node, &[successors])` | A node whose outputs go to one successor each |
| `pipeline(&[nodes], last)` | Nodes in a chain, each feeding the next, the last feeding `last` |
| `parameterize_node(node, LocalParams)` / `parameterize_node_with(node, NodeParams)` | A node copy with new local parameters (and a dictionary) |
| `parameterize_graph(graph, GraphParams)` | A graph copy with other parameters, successors, nodes or name |
| `selector`, `function_graph`, `segmenter` | Your own Rust graphs |
| `serial_segmenter`, `num_from_serial_segmenter` | The standard segmenters, typed |
| `brute_force`, `ml_selector`, `untrained_ml_selector`, `clustering`, `clustering_with_codecs` | Configured standard graphs |
| `sddl`, `sddl2` | The SDDL graphs with a description or bytecode |
| `load_dictionaries` | Attaches a [dictionary bundle](dictionaries.md) |

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::{GraphBuilder, StandardGraph, StandardNode};
use mopenzl::input::{Input, Numbers};
use mopenzl::{Compressor, Decompressor, EncoderConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = GraphBuilder::new();
    // delta -> zigzag -> bit-pack: three nodes in a chain, then store.
    let packed = builder.pipeline(
        &[
            StandardNode::DeltaInt.into(),
            StandardNode::Zigzag.into(),
            StandardNode::BitpackInt.into(),
        ],
        StandardGraph::Store,
    )?;
    let graph = builder.build(packed)?;

    let values: Vec<u8> = (0..2000u32)
        .flat_map(|i| (500 + i % 17).to_le_bytes())
        .collect();
    let numbers = Numbers::try_from((&values[..], 4))?;
    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()?;
    let frame = compressor.compress_inputs(&[Input::from(numbers)])?;
    assert!(frame.len() < values.len() / 2);

    let outputs = Decompressor::default().decompress_typed(&frame)?;
    assert_eq!(outputs[0].content(), values);
    Ok(())
}
```

Builders report mistakes early: a successor count that does not match a node's
outputs, an unknown or foreign ID, an empty pipeline or an unknown start graph is a
`GraphError` (`Invalid`, `UnknownId`). Failures while a graph runs surface from the
compressor as `EncodeError::Graph(GraphError::Execution(..))`, or
`GraphError::User(..)` when your own graph returned the error.

## Parameterized nodes and graphs

Nodes and some graphs take *local parameters*: integer, byte-blob or object
parameters keyed by the reference's IDs. `LocalParams::default().with_int(id, v)`
and `.with_bytes(id, bytes)` build them; giving a node or graph parameters
**replaces** the ones it had. The global level reaches Zstandard unchanged, and the
Zstandard node applies its own integer parameters after it, as raw `ZSTD_c_*` IDs
(`params::zstd::COMPRESSION_LEVEL` is 100), in list order.

The IDs are the reference's, and `mopenzl::graph::params` names them: one module per
codec or graph (`zstd`, `lz`, `lz4`, `field_lz`, `entropy`, `divide_by`, `bitunpack`,
`partition`, `sparse_num`, `sentinel`, `tokenize`, `split`, `dispatch`, `mux_lengths`,
`segmenters`, `sddl`, `sddl2`, `clustering`, ...). Each constant cites the C macro or
enum it mirrors (`ZL_ENTROPY_MIN_GAIN_BYTES_PID`, `ZL_LzParam_windowLog`, ...) and its
doc says whether the parameter is an integer or a copy (bytes) parameter.

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::params::zstd;
use mopenzl::graph::{GraphBuilder, GraphParams, LocalParams, StandardGraph, StandardNode};
use mopenzl::{Compressor, Decompressor, EncoderConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = GraphBuilder::new();
    // The Zstandard node with its compression level pinned to 19.
    let zstd19 = builder.parameterize_node(
        StandardNode::Zstd,
        LocalParams::default().with_int(zstd::COMPRESSION_LEVEL, 19),
    )?;
    let strong = builder.static_graph(zstd19, &[StandardGraph::Store.into()])?;
    // A copy of `brute_force`, with its candidates replaced.
    let race = builder.parameterize_graph(
        StandardGraph::BruteForce,
        GraphParams::default()
            .with_graphs([
                strong,
                StandardGraph::Lz4.into(),
                StandardGraph::Store.into(),
            ])
            .with_name("zstd19-or-lz4"),
    )?;
    let graph = builder.build(race)?;

    let input = b"parameters replace, they do not merge ".repeat(500);
    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()?;
    let frame = compressor.compress(&input)?;
    assert!(frame.len() < input.len() / 10);
    assert_eq!(Decompressor::default().decompress(&frame)?, input);
    Ok(())
}
```

`GraphBuilder::brute_force(&[..])` is the shorthand for the `parameterize_graph` call
above. `LocalParams::with_string_lengths` supplies the lengths of the
`set_string_lens` node. The key local parameters of every codec are in
[codecs](codecs.md).

## Custom graphs in Rust

Three traits mirror the reference's callbacks. All are `Send + Sync + 'static` so
a built graph is shareable between threads.

| Trait | Registered with | Sees | Decides |
| --- | --- | --- | --- |
| `Selector` | `GraphBuilder::selector(name, selector, TypeSet, &successors)` | One input (`InputView`) and its successors | Which graph compresses it (returns a `GraphId`) |
| `FunctionGraph` | `GraphBuilder::function_graph(name, f, &input_types, last_variable, &successors, &nodes)` | `Edge` handles of its inputs | Runs nodes (`run_node`) and gives every produced edge a destination (`set_destination`) |
| `Segmenter` | `GraphBuilder::segmenter(name, s, &input_types, &successors)` | The `remaining()` elements of each input | Cuts chunks with `process_chunk(&[n_elts], graph)`, which must consume everything |

### Selector

A selector looks at a stream and returns one of its successors (or any other graph of
the compressor). `SelectorContext::try_graph` compresses the stream with a candidate
and returns the resulting frame size, so a selector can measure instead of guess.

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::{
    GraphBuilder, GraphId, InputView, Selector, SelectorContext, StandardGraph, TypeSet,
};
use mopenzl::{Compressor, DataType, Decompressor, EncoderConfig};

/// Picks the successor that produces the smallest frame.
struct Smallest;

impl Selector for Smallest {
    fn select(
        &self,
        cx: &mut SelectorContext<'_>,
        input: InputView<'_>,
        successors: &[GraphId],
    ) -> GraphId {
        let mut best = successors[0];
        let mut best_size = usize::MAX;
        for &graph in successors {
            let size = cx.try_graph(&input, graph).unwrap_or(usize::MAX);
            if size < best_size {
                (best, best_size) = (graph, size);
            }
        }
        best
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = GraphBuilder::new();
    let selector = builder.selector(
        "smallest",
        Smallest,
        TypeSet::from(DataType::Serial),
        &[
            StandardGraph::Zstd.into(),
            StandardGraph::Lz4.into(),
            StandardGraph::Store.into(),
        ],
    )?;
    let graph = builder.build(selector)?;

    let input = b"selectors measure instead of guessing ".repeat(300);
    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()?;
    let frame = compressor.compress(&input)?;
    assert_eq!(Decompressor::default().decompress(&frame)?, input);
    Ok(())
}
```

A selector cannot fail: return a graph that always works (such as `Store`) as the
fallback. `SelectorContext` also exposes `level()`, `format_version()`, `depth()`,
`is_node_supported(node)`, `local_int(id)` and `set_successor_params(..)`.

### Function graph

A function graph gets its inputs as edges, runs nodes on them and assigns every
resulting edge a destination graph. Nodes run at the moment you ask; **successors
run after the function returns, in the order of the edge handles** (the inputs
first, then node outputs in creation order), not in the order you call
`set_destination`. An edge left without a destination is an error (or triggers the
permissive fallback). `GraphContext::data(edge)` reads a stream, `custom_graphs()`
and `custom_nodes()` return the lists you registered, and `run_node_with` and
`set_destination_with` override parameters at run time. `run_split_node`,
`run_dispatch_node` and `run_dispatch_string_node` run the split and dispatch nodes
with their runtime instructions.

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::{
    Edge, FunctionGraph, GraphBuilder, GraphContext, GraphError, StandardGraph, StandardNode,
};
use mopenzl::input::{Input, Strings};
use mopenzl::{Compressor, DataType, Decompressor, EncoderConfig};

/// Separates strings into lengths and content, and compresses each its own way.
struct SplitStrings;

impl FunctionGraph for SplitStrings {
    fn compress(&self, cx: &mut GraphContext<'_>, inputs: &[Edge]) -> Result<(), GraphError> {
        let parts = cx.run_node(StandardNode::SeparateStringComponents, inputs)?;
        let graphs = cx.custom_graphs();
        cx.set_destination(&parts[0..1], graphs[0])?; // content
        cx.set_destination(&parts[1..2], graphs[1])?; // lengths
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = GraphBuilder::new();
    let function = builder.function_graph(
        "split_strings",
        SplitStrings,
        &[DataType::String.into()],
        false,
        &[
            StandardGraph::Zstd.into(),
            StandardGraph::NumericCompress.into(),
        ],
        &[],
    )?;
    let graph = builder.build(function)?;

    let text = b"red green blue red green blue ".repeat(100);
    let lens: Vec<u32> = [3, 5, 4, 3, 5, 4]
        .iter()
        .cycle()
        .take(600)
        .copied()
        .collect();
    let content_len: u32 = lens.iter().sum();
    let strings = Strings::try_from((&text[..content_len as usize], &lens[..]))?;

    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()?;
    let frame = compressor.compress_inputs(&[Input::from(strings)])?;
    let outputs = Decompressor::default().decompress_typed(&frame)?;
    assert_eq!(outputs[0].string_lens(), lens);
    assert_eq!(outputs[0].content(), &text[..content_len as usize]);
    Ok(())
}
```

### Segmenter

A segmenter cuts the whole input into chunks, each compressed independently through
a successor, and writes one frame. See [chunking](#chunking-and-segmenters).

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::{GraphBuilder, GraphError, Segmenter, SegmenterContext, StandardGraph};
use mopenzl::{Compressor, DataType, Decompressor, EncoderConfig};

/// Cuts the input in thirds.
struct Thirds;

impl Segmenter for Thirds {
    fn segment(&self, cx: &mut SegmenterContext<'_>) -> Result<(), GraphError> {
        let total = cx.remaining()[0];
        let successor = cx.custom_graphs()[0];
        let third = total.div_ceil(3).max(1);
        let mut left = total;
        while left > 0 {
            let take = third.min(left);
            cx.process_chunk(&[take], successor)?;
            left -= take;
        }
        if total == 0 {
            cx.process_chunk(&[0], successor)?;
        }
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = GraphBuilder::new();
    let thirds = builder.segmenter(
        "thirds",
        Thirds,
        &[DataType::Serial.into()],
        &[StandardGraph::CompressGeneric.into()],
    )?;
    let graph = builder.build(thirds)?;

    let input = b"three chunks, three independent graphs ".repeat(400);
    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()?;
    let frame = compressor.compress(&input)?;
    assert_eq!(Decompressor::default().decompress(&frame)?, input);
    Ok(())
}
```

## Chunking and segmenters

A frame is a sequence of chunks, each with its own codec layout. The default graph
writes the whole input as one chunk (`Chunking::Whole`, which still splits every
2 GiB). A *segmenter* cuts several. The standard ones:

- `EncoderConfig::with_chunking(Chunking::Fixed(ChunkSize))` is the serial segmenter
  of the reference CLI: chunks of `ChunkSize` input bytes (32 KiB to `i32::MAX`,
  default 16 MiB), the last one shorter. It wraps whatever graph you installed.
- `GraphBuilder::serial_segmenter(chunk, successor)` is the same as a start graph of
  your own, with a chosen successor per chunk. A start graph that is already a
  segmenter cannot be combined with `Chunking::Fixed` (an `EncodeError::Graph`).
- `GraphBuilder::num_from_serial_segmenter(width, chunk, successor)` cuts serial
  input aligned to `width`-byte integers and compresses each chunk as numeric data
  (the default successor, `InterpretNum8Compress` to `InterpretNum64Compress`, reads each *serial* chunk as numbers and compresses them; a successor you pass receives the chunk as serial data, so it must accept serial input and convert it itself, for example a static graph that starts with `ConvertSerialToNumLe32`).
- `SegmentNumeric` cuts numeric input every 16 MiB; the SDDL2 graph is a segmenter
  that cuts along the segments of its program.

Chunks cap memory: with a declared `InputSize::Exact` both the encoder session and
the decoder work one chunk at a time. Chunking needs format version 21 or newer,
which every supported version has.

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::GraphBuilder;
use mopenzl::{ChunkSize, Compressor, Decompressor, EncoderConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut builder = GraphBuilder::new();
    // 8-byte integers, 64 KiB chunks; each chunk is read as numbers and compressed.
    let segmenter = builder.num_from_serial_segmenter(8, ChunkSize::try_from(64 << 10)?, None)?;
    let graph = builder.build(segmenter)?;

    let input: Vec<u8> = (0..40_000u64)
        .flat_map(|i| (i * 1000).to_le_bytes())
        .collect();
    let mut compressor = Compressor::builder(EncoderConfig::default())
        .with_graph(graph)
        .build()?;
    let frame = compressor.compress(&input)?;
    assert_eq!(Decompressor::default().decompress(&frame)?, input);
    Ok(())
}
```

## Trial compression: `try_graph`

`SelectorContext::try_graph(&input, graph)` and `GraphContext::try_graph(edge,
graph)` compress the stream with `graph` in a throwaway context (checksums off, no
comment) and return the size of the complete frame it would produce, or an error
when the graph fails. `brute_force` is built on it. Trials cost a full compression
each; use them on the successors that matter. The [selector
example](#selector) shows the idiom.

## Permissive mode

Normally a failing node, graph or successor fails the compression. With
`EncoderConfig::with_permissive(true)`, a failing successor outside a segmenter is
rolled back and its input recompressed with `compress_generic`, as in the reference.
This is the net under graphs that cannot always apply: `Constant` on data that is not
constant, an `MlSelector` whose model does not match its successors, a codec whose
Cargo feature is off.

<!-- cfg: feature = "all-codecs" -->
```rust
use mopenzl::graph::{CompressorGraph, StandardGraph};
use mopenzl::{Compressor, Decompressor, EncoderConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let not_constant = b"this is certainly not one repeated byte".repeat(10);
    let graph = CompressorGraph::standard(StandardGraph::Constant);

    let mut strict = Compressor::builder(EncoderConfig::default())
        .with_graph(graph.clone())
        .build()?;
    assert!(strict.compress(&not_constant).is_err());

    let config = EncoderConfig::default().with_permissive(true);
    let mut permissive = Compressor::builder(config).with_graph(graph).build()?;
    let frame = permissive.compress(&not_constant)?;
    assert_eq!(Decompressor::default().decompress(&frame)?, not_constant);
    Ok(())
}
```

Two more nets always apply: a lone stream below `min_stream_size` (10) is stored,
and, with `store_on_expansion` (the default), a chunk that would not shrink is
rebuilt as `store`.

## Sessions and graphs

`Compressor::start` and the `std::io` writer work with any graph, with these
rules:

- The default graph and a serial segmenter (including `Chunking::Fixed`) compress
  incrementally, chunk by chunk, and match the one-shot bytes.
- Every other graph is *buffered*: the session holds its input until
  `Operation::End`, runs the graph once and then drains the output. A declared
  `InputSize::Exact` is checked at the end.
- Typed and multiple inputs go through `compress_inputs`; sessions, readers and
  writers take serial bytes.

See [streaming](streaming.md#sessions).
