# Buffers and memory reuse

Every call involves two kinds of memory: the **destination** you hand to the
codec, and the **workspace** the codec keeps for itself. You control both.

| You want to | Use |
| --- | --- |
| Get a fresh `Vec<u8>` | `compress` / `decompress` |
| Append to a Vec you already own | `compress_into` / `decompress_into` |
| Write into a fixed slice | `compress_to_slice` / `decompress_to_slice` |
| Write into uninitialized memory | `compress_to_uninit` / `decompress_to_uninit` |
| Cap what the codec keeps between calls | A [retention policy](#reusing-memory) |

## Output buffers

`compress` returns a new `Vec<u8>`. `compress_into` appends to the vector you pass
and returns the range that holds the new frame; existing bytes stay intact.
`compress_to_slice` returns the number of bytes written. Size the slice with
`Compressor::max_compressed_size`:

```rust
use mopenzl::{Compressor, Decompressor, EncodeError};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"data to compress";
    let mut encoder = Compressor::default();
    let bound = Compressor::max_compressed_size(input.len())?;
    let mut output = vec![0; bound];
    let written = encoder.compress_to_slice(input, &mut output)?;
    output.truncate(written);
    assert_eq!(output, encoder.compress(input)?);

    // A slice that is too small is an error, not a truncated frame.
    let mut cramped = [0u8; 4];
    assert!(matches!(
        encoder.compress_to_slice(input, &mut cramped),
        Err(EncodeError::OutputTooSmall { provided: 4 })
    ));

    // Decompression into a slice reports how much it wrote.
    let mut restored = [0u8; 64];
    let n = Decompressor::default().decompress_to_slice(&output, &mut restored)?;
    assert_eq!(&restored[..n], input);
    Ok(())
}
```

### Uninitialized memory

`compress_to_uninit`, `decompress_to_uninit` and the sessions' `process_uninit`
write into `&mut [MaybeUninit<u8>]`, such as `Vec::spare_capacity_mut`, without
zeroing it first. They produce the same bytes, counts and errors as the
initialized APIs; on success exactly the reported prefix is initialized and nothing
after it is written. Reading the prefix back is the caller's `unsafe` step. See
[uninitialized output](../architecture/uninit-output.md).

```rust
use mopenzl::{Compressor, Decompressor};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"uninitialized destination ".repeat(50);
    let mut encoder = Compressor::default();
    let mut frame: Vec<u8> = Vec::with_capacity(Compressor::max_compressed_size(input.len())?);
    let written = encoder.compress_to_uninit(&input, frame.spare_capacity_mut())?;
    // SAFETY: `compress_to_uninit` initialized the first `written` bytes.
    unsafe { frame.set_len(written) };
    assert_eq!(frame, encoder.compress(&input)?);

    let mut restored: Vec<u8> = Vec::with_capacity(input.len());
    let n = Decompressor::default().decompress_to_uninit(&frame, restored.spare_capacity_mut())?;
    // SAFETY: `decompress_to_uninit` initialized the first `n` bytes.
    unsafe { restored.set_len(n) };
    assert_eq!(restored, input);
    Ok(())
}
```

## Reusing memory

Keep the codec and your output buffer alive across operations when allocation
reuse matters:

```rust
use mopenzl::{Compressor, Decompressor, EncoderConfig, Level};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = Compressor::new(EncoderConfig::default().with_level(Level::try_from(3)?))?;
    let mut decoder = Decompressor::default();
    let mut frame = Vec::new();

    for input in [b"first payload".as_slice(), b"second payload".as_slice()] {
        frame.clear(); // Keep the allocation; compress_into appends.
        let written = encoder.compress_into(input, &mut frame)?;
        assert_eq!(written, 0..frame.len());
        assert_eq!(decoder.decompress(&frame)?, input);
    }
    Ok(())
}
```

A compressor or decompressor retains working state between operations: hash
tables, stream buffers and the Zstandard context. Compatible calls reuse the
allocations. Destination capacity is yours to manage.

| Retention policy | Effect |
| --- | --- |
| `Aggressive` (default) | Keeps every buffer between operations |
| `Bounded { max_bytes }` | Releases retained storage when its accounting exceeds the ceiling |
| `ReleaseAll` | Releases storage after each operation |

Set the policy with `Compressor::builder(config).with_retention(policy).build()`
(`Decompressor::builder` likewise). Use `trim(policy)` for an immediate cleanup and
`retained_bytes()` for the owned heap size; it excludes caller buffers and
allocator overhead, and a ceiling is not a peak-memory limit. The policy never
changes the bytes produced.

```rust
use mopenzl::{Compressor, EncoderConfig, RetentionPolicy};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut encoder = Compressor::builder(EncoderConfig::default())
        .with_retention(RetentionPolicy::Bounded { max_bytes: 1 << 20 })
        .build()?;
    encoder.compress(&b"warm the tables up ".repeat(1000))?;
    println!("retained {} bytes", encoder.retained_bytes());
    encoder.trim(RetentionPolicy::ReleaseAll);
    assert_eq!(encoder.retained_bytes(), 0);
    Ok(())
}
```

For simultaneous independent streams use one codec per worker; a
`CompressorGraph` is cheap to clone and share between them. A compressor
configured with a graph keeps it; `Compressor::builder(config).with_graph(graph)`.
