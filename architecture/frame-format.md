# Frame format

Both codecs follow the reference's `src/openzl/common/wire_format.h`. This page
describes the subset the crate reads and writes (versions 21 to 27), in
`shared::wire`, `compressor::core::{frame, chunk}` and
`decompressor::core::{frame, chunk}`.

## Frame

```mermaid
flowchart LR
    M[magic LE: 0xD7B1A5C0 + version] --> F[flags]
    F --> B{v25+ and bundle flag?}
    B -- yes --> BID[bundle ID: len + bytes]
    B -- no --> T
    BID --> T[inputs token: count bits 0-3, types from bit 4]
    T --> S[varint size + 1 per input]
    S --> C{v22+ and comment flag?}
    C -- yes --> CM[varint len + comment]
    C -- no --> K
    CM --> K{compressed checksum flag?}
    K -- yes --> KB[check byte: XXH3 of header & 0xFF]
    K -- no --> CH
    KB --> CH[chunk ...]
    CH --> CH
    CH --> E[end marker 0x00]
```

- Flags: bit 0 content checksums, bit 1 compressed checksums (and the header
  check byte), bit 2 comment (v22+), bit 3 bundle ID (v25+).
- Bundle ID (flag bit 3, v25+): one length byte, then that many leading bytes of
  the 32-byte ID; trailing zero bytes are trimmed on write and restored on read
  (an all-zero ID is refused by the encoder). See [dictionaries](dictionaries.md).
- A first size byte of `0x00` means "unknown sizes", which the reference
  decoder rejects; neither codec writes or accepts it.
- The header lists any number of inputs of any type (the general form is below).
  The default graph writes one serial input, token `0x01`. The decoder rejects
  zero inputs as invalid; the byte-oriented APIs report more than one input, or a
  non-serial one, as unsupported, while `Decompressor::decompress_typed` returns
  every output.
- Magics from version 8 to 20, and up to 16 versions past 27, are OpenZL but
  unsupported; anything else is not OpenZL.

## Chunk

```mermaid
flowchart TD
    A["varint(codecs + 1)"] --> B["varint(stored streams)"]
    B --> C[1-bit lane: custom codec flags]
    C --> D["standard IDs, 6 bits (v21-23) or 7 bits (v24+)"]
    D --> E[varint custom IDs]
    E --> F["1-bit lane + varints: private header sizes - 1"]
    F --> G["1-bit lane + varints: variable inputs - 1"]
    G --> H["1-bit lane + varints: regenerated streams - 2"]
    H --> DL["v25+ with a bundle: dictionary lane = 1 bit per codec, then width byte and bit-packed bundle indices of the flagged codecs"]
    DL --> I["distances - 1, bits_for(regenerated + stored) each"]
    I --> J[varint stored stream sizes]
    J --> K[codec private headers, concatenated]
    K --> L[stored streams in stream order]
    L --> M["[content checksum: XXH3(chunk output) low 32]"]
    M --> N["[compressed checksum: XXH3(chunk bytes so far) low 32]"]
```

Codecs are listed in decoding order. Codec `t` takes `fixed + variable` input
streams starting right after codec `t - 1`'s inputs; each stream it regenerates
lies `distance` streams past the end of its inputs. Streams no codec
regenerates are stored, in index order. The last stream is the chunk's output.
The compressed checksum covers the chunk header, streams and the content
checksum; it is verified before any codec runs.

```mermaid
graph LR
    S0["stream 0: stored (zstd stream)"] -->|zstd, distance 0| S1["stream 1: output"]
```

The two chunks the encoder writes:

| Chunk | Header bytes | Streams |
| --- | --- | --- |
| Stored | `01 01 varint(n)` | the input |
| Zstandard | `02 01 00 16 00 00 00 00 varint(n)` | `01` (element width) + magicless zstd frame |

## Limits

Codec count below 20 000 (`ZL_runtimeNodeLimit`), streams below 110 000
(`ZL_runtimeStreamLimit`), at most 2 048 regenerated streams per codec, stored
streams and private headers below 4 GiB, comments of 1 to 10 000 bytes.


## Typed and multiple inputs, comments

The encoder writes the general v21+ header (`layout::write_frame_header`):

```mermaid
flowchart LR
    M[magic LE32] --> F["flags: content, compressed, comment (v22+), bundle (v25+)"]
    F --> T["inputs token: n below 15: n, type0 shifted 4, type1 shifted 6, the rest 2 bits each; n of 15 or more: ((n-15) shifted 4) OR 15, then (n-15) shifted right 4, then all types"]
    T --> S["varint(size+1) per input, then varint(numElts) per string input"]
    S --> C["varint(len) + comment (v22+)"]
    C --> K["check byte: XXH3(header) & 0xFF"]
```

Types are 2-bit codes (serial 0, struct 1, numeric 2, string 3). A string's
size is the sum of its lengths. Content checksums hash every chunk input in
order and, for strings, their little-endian length array after the content.
The chunk header and its stored-stream order are described in
[compression graphs](compressor-graphs.md). The dictionary lane
(`push_header_with` on the encoder, `dictionary_lane` on the decoder) is present
in every chunk of a frame that has a bundle (flag bit 3), including stored and
empty chunks; the decoder reads it exactly when the frame header named a bundle.
The decoder reads a multi-output chunk's outputs as its last `n` streams
(output `k` is stream `total - 1 - k`); the content checksum covers each
output's content followed, for strings, by its length array.

## Known gaps

- Formats below 21 are not read or written (the supported range is 21 to 27).
- A comment is accepted and written from v22; only a single bundle per frame
  exists in the format.
- Unknown-size frames (`0x00` first size byte) are rejected as in the reference.
