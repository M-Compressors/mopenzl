# Decompressor

## Ownership

```mermaid
classDiagram
    class Decompressor {
        DecoderConfig config
        RetentionPolicy retention
        Backend backend
        Engine engine
        Stream stream
    }
    class Engine {
        Layout layout
        ZstdDecoder zstd
        Vec~Vec~ buffers
        Level simd
    }
    class Layout {
        Vec~Node~ nodes
        Vec~Source~ streams
        lanes...
        usize len
    }
    class Stream {
        Phase phase
        FrameInfo frame, pending_frame
        Vec unit, peek, output
    }
    Decompressor *-- Engine
    Decompressor *-- Stream
    Engine *-- Layout
    Engine *-- ZstdDecoder
    DecoderSession --> Decompressor : borrows
    DecoderSessionOwned --> Decompressor : boxes
```

`DecoderConfig` selects `FrameMode` (`Single`, the default, rejects trailing
bytes like `ZL_decompress`; `Concatenated`), `DecodeLimits` (input and output
caps) and both checksum checks (on by default, as the reference).

## Parsing

`core::reader::Reader` returns `Parse::Truncated` when bytes run out and
`Parse::Failed` for invalid or unsupported data, so the same parsers serve
one-shot calls (truncation is `UnexpectedEndOfInput`) and sessions (truncation
means "wait"). `frame::parse` validates the magic, flags, single serial
output, known size, comment and check byte. `Layout::parse` reads a chunk
header's lanes, rejects unknown codecs before laying out anything, then wires
streams as `fillStoredStreams` does and computes the chunk's length:

```mermaid
flowchart TD
    A[chunk header lanes] --> B{every codec known?}
    B -- no --> U[Unsupported Codec / CustomCodec]
    B -- yes --> C[per codec: inputs after previous inputs]
    C --> D{input past last-1, regen out of range or taken,<br/>wrong variable or regen count?}
    D -- yes --> G[InvalidData Graph]
    D -- no --> E{codec input produced by a later codec?}
    E -- yes --> G
    E -- no --> F[stored streams fill the rest in order]
    F --> H{stored count matches?}
    H -- no --> G
    H -- yes --> L[len = header + private headers + stored + checksums]
```

## Chunk execution

```mermaid
sequenceDiagram
    participant E as Engine::decode_chunk
    participant L as Layout
    participant Z as ZstdDecoder
    participant O as Out (Vec or slice)
    E->>L: parse(chunk)
    E->>E: verify compressed checksum (before any codec)
    loop nodes in order
        E->>E: input = stored range or buffers[input]
        E->>E: split width varint, read zstd content size
        alt output is the last stream
            E->>O: grow(size), size <= remaining declared output
            E->>Z: decode into the output
        else intermediate
            E->>Z: decode into buffers[output] (size bounded)
        end
    end
    E->>O: copy the last stream if it is stored
    E->>E: verify content checksum over the chunk's output
```

The zstd decoder stages `magic + frame` and decodes straight into the sized
destination with `mzstd` in single-frame mode; the content size is read from
the magicless header first, and its absence is invalid, as in `DI_zstd`.
Intermediate streams may exceed the remaining output only by 1/128 plus
64 KiB, which a Zstandard chain never needs.

## Sessions

```mermaid
stateDiagram-v2
    [*] --> Frame
    Frame --> Chunk: frame header complete (output limit checked)
    Chunk --> Chunk: chunk complete -> decoded into pending output
    Chunk --> Frame: end marker, Concatenated
    Chunk --> Done: end marker, Single
    Frame --> Done: Finish between frames (Concatenated, >= 1 frame)
    Done --> [*]
```

A unit (frame header, chunk or end marker) is decoded once all its bytes are
present: in place when it lies whole in the caller's input, otherwise from the
`unit` buffer. Its length comes from its header alone. A header split across
calls is measured over the buffered prefix plus a window of new input that
doubles from 64 bytes, so the session never accepts bytes past the unit it
builds (trailing data after a `Single` frame stays with the caller) and
measuring stays linear in the header's length. The frame header found while
measuring is kept, not parsed again.

`DecoderSession`/`DecoderSessionOwned` add a sticky failure flag
(`InvalidState` afterwards), counters (`total_in`, `total_out`,
`frames_decoded`) and retention on completion. `io::DecoderReader` and
`io::DecoderWriter` wrap sessions with an 8 KiB buffer each.

## Errors

`DecodeError`: `InvalidData { kind }` (`Magic`, `FrameHeader`,
`HeaderChecksum`, `ChunkHeader`, `Graph`, `CodecStream`), `Unsupported
{ feature }` (`FormatVersion`, `Codec`, `CustomCodec`, `UnknownSize`,
`Dictionary`, `MultipleOutputs`, `TypedOutput`), `UnexpectedEndOfInput`,
`TrailingData`, `OutputTooSmall`, both checksum mismatches,
`ContentSizeMismatch`, both limits and `InvalidState`. It converts to
`std::io::Error` with matching kinds.

## Known gaps

- Only store and Zstandard chunks decode; see the module map's gaps.
- Session output is decoded into the pending buffer and copied out; one-shot
  calls decode in place (see [uninitialized output](uninit-output.md)).
- Slice and uninit destinations decode Zstandard through
  `mzstd::decompress_to_slice`, which trails the C library by about 15% on
  records-like data, while `Vec` destinations (`mzstd::decompress_into`) are
  ahead of it (see [the comparison](benchmark-comparison.md)).
