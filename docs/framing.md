# Framed resources with independent graphs

Enable the opt-in feature alongside your codec directions:

```toml
mopenzl = { version = "0.2", features = ["framed"] }
```

`framing::FramedCompressor` writes an indexed container. Each resource is one
ordinary OpenZL frame with its own graph and optional metadata. The outer
container is mopenzl-specific: an ordinary OpenZL decoder accepts an extracted
resource frame, not the complete container. The decoder reconstructs codecs
from each frame; it needs no graph selection.

```rust
use mopenzl::framing::{FramedCompressor, FramedDecompressor, FramedResource, MetadataField};
use mopenzl::graph::{CompressorGraph, StandardGraph};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = CompressorGraph::standard(StandardGraph::Store);
    let name = [MetadataField { code: *b"id", value: b"image.bin" }];
    let text = b"text text text text text text";
    let image = [0x42; 1024];
    let resources = [
        FramedResource::from(&text[..]), // default graph
        FramedResource {
            graph: Some(&store),
            metadata: &name,
            ..FramedResource::from(&image[..])
        },
    ];
    let mut encoder = FramedCompressor::new(Default::default())?;
    let bytes = encoder.compress(resources.as_slice().into())?;
    let mut decoder = FramedDecompressor::new(Default::default())?;
    let output = decoder.decompress(&bytes)?;
    assert_eq!(output.resources[0].outputs[0].content(), text);
    assert_eq!(output.resources[1].outputs[0].content(), image);
    Ok(())
}
```

For typed or multiple inputs, construct `FramedResource::from(&inputs[..])`
where `inputs` is a slice of `mopenzl::input::Input`. Decoded resources retain
the original output types, widths and string lengths. A graph override lasts
only for its resource. Convert a configured `Compressor` or `Decompressor`
with `FramedCompressor::from` / `FramedDecompressor::from` to retain its
backend, graph, retention, dictionaries or custom-codec registrations.

Without `no_std`, use `encoder.framed_writer(sink)` to add complete resources
as they arrive, then finish to write the seek index. `resource` accepts complete
borrowed inputs and queues one encoded resource. `flush` delivers queued bytes;
`finish` returns the sink or a failure containing the writer for retry. Nothing
is written on Drop. Reader methods return whole typed resources:

```rust
use mopenzl::framing::{FramedCompressor, FramedDecompressor, FramedResource};
use std::io::Cursor;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = FramedCompressor::new(Default::default())?;
    let mut writer = encoder.framed_writer(Vec::new())?;
    writer.resource(FramedResource::from(&b"first"[..]))?;
    writer.resource(FramedResource::from(&b"second"[..]))?;
    let bytes = writer.finish().map_err(|failure| failure.error)?;

    let mut decoder = FramedDecompressor::new(Default::default())?;
    let mut reader = decoder.framed_seek_reader(Cursor::new(&bytes))?;
    assert_eq!(reader.resources().len(), 2);
    assert_eq!(reader.read_resource(1)?.outputs[0].content(), b"second");
    drop(reader);
    let mut sequential = decoder.framed_reader(&bytes[..]);
    while let Some(resource) = sequential.read_resource()? {
        println!("{} content bytes", resource.info.decoded_size);
    } // None validates the directory, footer and EOF.
    Ok(())
}
```

`framed_seek_reader` borrows the decoder for as long as the reader lives. To
keep the decoder, the source and the directory together in one struct, load a
detached `FramedIndex` with `framed_index` and read through
`read_indexed_resource`. The index records where the container starts, so each
read seeks itself and the source's position does not matter. An index can be
used with any decoder and any source holding the same bytes at the same
positions, such as a second handle to the file; the reading decoder's limits
apply. A different or changed source yields an error, never a panic:

```rust
use mopenzl::framing::{
    DecodedResource, FramedCompressor, FramedDecompressor, FramedError, FramedIndex,
    FramedResource,
};
use std::io::Cursor;

struct Archive {
    decoder: FramedDecompressor,
    index: FramedIndex,
    source: Cursor<Vec<u8>>,
}

impl Archive {
    fn open(bytes: Vec<u8>) -> Result<Self, FramedError> {
        let decoder = FramedDecompressor::new(Default::default())?;
        let mut source = Cursor::new(bytes);
        let index = decoder.framed_index(&mut source)?;
        Ok(Self { decoder, index, source })
    }

    fn read(&mut self, resource: usize) -> Result<DecodedResource, FramedError> {
        self.decoder
            .read_indexed_resource(&mut self.source, &self.index, resource)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let resources = [
        FramedResource::from(&b"first"[..]),
        FramedResource::from(&b"second"[..]),
    ];
    let bytes = FramedCompressor::new(Default::default())?
        .compress(resources.as_slice().into())?;
    let mut archive = Archive::open(bytes)?;
    assert_eq!(archive.index.len(), 2);
    assert_eq!(archive.read(1)?.outputs[0].content(), b"second");
    assert_eq!(archive.read(0)?.outputs[0].content(), b"first");
    assert!(archive.read(2).is_err());
    Ok(())
}
```

With `alloc` (including `no_std`), both codecs also offer borrowed native
sessions. Drain an encoder session's header with `process(output, Process)`
until NeedsInput, submit `resource`, drain again, and finally use Finish until
Finished. Decoder sessions consume wire bytes until a resource event; process
the unconsumed suffix, and use `DecodeOperation::Finish` at EOF. Resource
events are tentative until Finished validates the directory and footer.

`FramingConfig` supplies bounded resource count, metadata, frame, aggregate
output and total wire sizes. Configure it through `FramedEncodeConfig`,
`FramedDecodeConfig`, or `reconfigure`. Inputs are complete per resource, and
each resource/frame is staged in memory. Choose resource boundaries explicitly
for large streams. This first version supports resource metadata and whole
resource seek; it does not mirror every mbrotli/mzstd container command.

See the [wire format, lifecycle and limits](../architecture/framing.md).
