# Framing containers

Package independent resources with metadata and a checked seek index. Each
resource contains one ordinary OpenZL frame, with its own graph and serial,
typed or multiple inputs. The outer format belongs to mopenzl; an ordinary
OpenZL decoder reads an extracted frame rather than the complete container.

## Enable framing

```toml
mopenzl = { version = "0.2", features = ["framed"] }
```

The book follows the repository's `main` branch; use this checkout or a Git
dependency for features not yet in a crates.io release. `framed` is opt-in,
outside `default` and `all-codecs`. With `compression` it exposes the framed
encoder; with `decompression`, the framed decoder. Owned results and borrowed
native sessions work with alloc; std adds writer, sequential reader and seek
reader adapters. `no_std` removes those transport adapters.

The container has one resource profile and a mandatory directory/footer.
Resource commands take complete borrowed inputs; there are no byte-oriented
resource writers, hidden resources, padding or repeated metadata. Split large
input into resources explicitly. See [format and lifecycle](../architecture/framing.md).

## One container, two resources

Use the default graph for text and Store for already encoded binary data.
Graph overrides apply only to their resource and are restored after failure.

```rust
use mopenzl::framing::{FramedCompressor, FramedDecompressor, FramedResource};
use mopenzl::graph::{CompressorGraph, StandardGraph};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let text = b"notes for a compressed resource\n".repeat(200);
    let binary = [1, 2, 3, 4, 5];
    let store = CompressorGraph::standard(StandardGraph::Store);
    let resources = [
        FramedResource::from(text.as_slice()),
        FramedResource { graph: Some(&store), ..FramedResource::from(&binary[..]) },
    ];
    let mut encoder = FramedCompressor::new(Default::default())?;
    let container = encoder.compress(resources.as_slice().into())?;
    let mut decoder = FramedDecompressor::new(Default::default())?;
    let decoded = decoder.decompress(&container)?;
    assert_eq!(decoded.resources.len(), 2);
    assert_eq!(decoded.resources[0].outputs[0].content(), text);
    assert_eq!(decoded.resources[1].outputs[0].content(), binary);
    Ok(())
}
```

`compress_into` appends transactionally to a reusable Vec and returns the
container's range. Raw slice/uninitialized output methods belong to the ordinary
codec; framed output uses Vec, a native output session or a writer.
`FramedResource::from(&inputs[..])` accepts typed/multiple `Input` values.
See [graphs and typed resources](framing.md).

## Write resources and metadata

Use a writer when complete resources arrive one after another. Attach borrowed
metadata directly to each resource command, then finish the enclosing container.
Metadata codes are uppercase ASCII pairs, `id` (unique UTF-8 name within one
resource's fields), or `mt` (unique eight-byte signed little-endian timestamp).

```rust
use mopenzl::framing::{
    FramedCompressor, FramedDecompressor, FramedEncodeConfig, FramedResource,
    FramingConfig, MetadataField,
};
use mopenzl::graph::{CompressorGraph, StandardGraph};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = FramedEncodeConfig {
        framing: FramingConfig { max_resources: 16, ..Default::default() },
        ..Default::default()
    };
    let fields = [MetadataField { code: *b"id", value: b"notes.txt" }];
    let store = CompressorGraph::standard(StandardGraph::Store);
    let mut encoder = FramedCompressor::new(config)?;
    let mut writer = encoder.framed_writer(Vec::new())?;
    writer.resource(FramedResource {
        metadata: &fields,
        ..FramedResource::from(&b"first part\nsecond part\n"[..])
    })?;
    writer.resource(FramedResource {
        graph: Some(&store),
        ..FramedResource::from(&[1, 2, 3, 4][..])
    })?;
    let container = writer.finish().map_err(|failure| failure.error)?;
    let decoded = FramedDecompressor::new(Default::default())?.decompress(&container)?;
    assert_eq!(decoded.resources[0].outputs[0].content(), b"first part\nsecond part\n");
    assert_eq!(decoded.resources[0].metadata[0].value, b"notes.txt");
    assert_eq!(decoded.resources[1].outputs[0].content(), [1, 2, 3, 4]);
    Ok(())
}
```

No Drop performs I/O. `resource` drains older output before accepting its
command; an error means this resource was not accepted, so retrying that command
does not duplicate it. `try_finish` retains the writer after an I/O error;
consuming `finish` returns `FramedFinishError` with `writer` and `error` fields.
See the [retry state machine](../architecture/framing.md#encoder-lifecycle-and-retries).

`id` is a name, not an extraction path or a globally unique lookup key.
Framing computes/verifies metadata and directory XXH3 checksums, while inner
OpenZL checksums follow encoder/decoder configuration. These detect damage and
do not authenticate content.

## Indexed resource reads

Open the footer and directory on a `Read + Seek` source, then decode only a
selected whole resource. A `File` can replace this example's `Cursor`.

```rust
use mopenzl::framing::{FramedCompressor, FramedDecompressor, FramedResource};
use std::io::Cursor;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let resources = [
        FramedResource::from(&b"first"[..]),
        FramedResource::from(&b"second"[..]),
    ];
    let container = FramedCompressor::new(Default::default())?
        .compress(resources.as_slice().into())?;
    let mut decoder = FramedDecompressor::new(Default::default())?;
    let mut reader = decoder.framed_seek_reader(Cursor::new(container))?;
    assert_eq!(reader.resources().len(), 2);
    let selected = reader.read_resource(1)?;
    assert_eq!(selected.outputs[0].content(), b"second");
    Ok(())
}
```

Indices are zero-based `usize` values, independent of metadata names.
Opening validates the directory; reading validates that resource's header,
metadata, size and inner frame. It returns owned typed outputs rather than a
`Read` adapter. The container extends from the source's current position to EOF;
offsets remain relative for a container embedded after a prefix. External
dictionary bundles come from the decoder's configured `BundleStore`.
See [indexed decoder mechanics](../architecture/framing.md#seek-and-dispatch).

## Limits and streaming decoding

`FramingConfig` bounds resources, metadata per resource, encoded frame bytes,
aggregate decoded content and total wire bytes. `FramedDecodeConfig::decoder`
adds raw decoder input/output limits and checksum policy for each frame.

For arriving input, `FramedDecompressor::start` yields owned whole-resource
events; `framed_reader` adapts a `Read` source through `read_resource`.
Continue until Finished or None to validate the directory, footer and EOF.
Earlier resources remain tentative until then. There is no automatic raw-frame
detection or dictionary reference to an earlier resource.
See [Sans I/O decoding](sans-io-decompression.md#decode-a-framing-container-as-events)
and [dictionary bundles](dictionaries.md).
