//! Decoders of the `entropy` feature: `fse_v2` (49), `huffman_v2` (50),
//! `huffman_struct_v2` (51) and `fse_ncount` (52).
//!
//! Ports of `codecs/entropy/decode_entropy_binding.c` (the bindings),
//! `codecs/entropy/decode_huffman_kernel.c` (the 16-bit Huffman) and the
//! FSE and Huffman libraries under `fse/` that OpenZL forked from
//! Zstandard. Each entropy codec reads a table stream (normalised counts or
//! Huffman weights) and a bits stream, and a private header holding the
//! decoded size:
//!
//! - `fse_v2`: header `[states: 2|4][size: LE, 1..=8 bytes]`; counts are a
//!   16-bit numeric stream; see [`fse`].
//! - `huffman_v2`: header `[x4 in bit 0][size]`; weights are a byte numeric
//!   stream; the bits are one stream or, with `x4`, a jump table of three
//!   16-bit lengths and four streams; see [`huf`].
//! - `huffman_struct_v2`: the same header over 16-bit symbols, whose bits
//!   are length-prefixed blocks.
//! - `fse_ncount`: turns a serialised count header back into the numeric
//!   counts that `fse_v2` consumes.
//!
//! Pre-version-15 entropy codecs (`fse`, `huffman`, `huffman_fixed`,
//! `zstd_fixed`, in `codecs/entropy/deprecated`) are not supported.

mod bits;
mod fse;
mod huf;

use alloc::vec::Vec;

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_fse_v2`: the numeric counts and the serial bits.
pub(crate) const FSE_V2: Desc = Desc {
    id: 49,
    min_version: 15,
    regens: &[Type::Serial],
    variable_regens: false,
    inputs: &[Type::Numeric, Type::Serial],
    variable_inputs: 0,
    decode: decode_fse,
};

/// `ZL_StandardTransformID_huffman_v2`: byte weights and the serial bits.
pub(crate) const HUFFMAN_V2: Desc = Desc {
    id: 50,
    min_version: 15,
    regens: &[Type::Serial],
    variable_regens: false,
    inputs: &[Type::Numeric, Type::Serial],
    variable_inputs: 0,
    decode: decode_huffman,
};

/// `ZL_StandardTransformID_huffman_struct_v2`: weights and bits of 16-bit
/// symbols, regenerated as 2-byte structs.
pub(crate) const HUFFMAN_STRUCT_V2: Desc = Desc {
    id: 51,
    min_version: 15,
    regens: &[Type::Struct],
    variable_regens: false,
    inputs: &[Type::Numeric, Type::Serial],
    variable_inputs: 0,
    decode: decode_huffman_struct,
};

/// `ZL_StandardTransformID_fse_ncount`: a count header back to numeric
/// 16-bit counts.
pub(crate) const FSE_NCOUNT: Desc = Desc {
    id: 52,
    min_version: 15,
    regens: &[Type::Numeric],
    variable_regens: false,
    inputs: &[Type::Serial],
    variable_inputs: 0,
    decode: decode_ncount,
};

/// Reads the codec header `[flags byte][decoded size, little-endian]`: 2 to
/// 9 bytes, a size of at least 2.
fn header(bytes: &[u8]) -> Result<(u8, usize), DecodeError> {
    let (&flags, size) = bytes.split_first().ok_or_else(corrupt)?;
    if size.is_empty() || size.len() > 8 {
        return Err(corrupt());
    }
    let mut value = [0u8; 8];
    value[..size.len()].copy_from_slice(size);
    let size = usize::try_from(u64::from_le_bytes(value)).or(Err(corrupt()))?;
    if size < 2 {
        return Err(corrupt());
    }
    Ok((flags, size))
}

/// The weights of a table stream: bytes of width 1.
fn weights<'a>(input: &Input<'a>) -> Result<&'a [u8], DecodeError> {
    if input.width != 1 {
        return Err(corrupt());
    }
    Ok(input.bytes)
}

/// `DI_fse_v2`.
fn decode_fse(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let [counts, bits] = inputs else {
        return Err(corrupt());
    };
    if counts.width != 2 {
        return Err(corrupt());
    }
    let counts: Vec<i16> = counts
        .bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| i16::from_le_bytes(pair))
        .collect();
    let table = fse::Table::new(&counts)?;
    let (states, size) = header(ctx.header)?;
    if states != 2 && states != 4 {
        return Err(corrupt());
    }
    let out = regens[0].fixed(1, size)?;
    fse::decode(
        ctx.state.simd,
        &table,
        bits.bytes,
        usize::from(states),
        size,
        out,
    )
}

/// `DI_huffman_v2`.
fn decode_huffman(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let [table, bits] = inputs else {
        return Err(corrupt());
    };
    let table = huf::ByteTable::new(weights(table)?)?;
    let (flags, size) = header(ctx.header)?;
    let out = regens[0].fixed(1, size)?;
    out.resize(size, 0);
    if flags & 1 != 0 {
        table.quad(ctx.state.simd, bits.bytes, out)
    } else {
        table.single(ctx.state.simd, bits.bytes, out)
    }
}

/// `DI_huffman_struct_v2`.
fn decode_huffman_struct(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let [table, bits] = inputs else {
        return Err(corrupt());
    };
    let table = huf::WideTable::new(weights(table)?)?;
    let (flags, size) = header(ctx.header)?;
    let out = regens[0].fixed(2, size)?;
    table.blocks(ctx.state.simd, bits.bytes, size, flags & 1 != 0, out)
}

/// `DI_fse_ncount`.
fn decode_ncount(
    _ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let (counts, used) = fse::read_ncount(inputs[0].bytes)?;
    if used != inputs[0].bytes.len() {
        return Err(corrupt());
    }
    let out = regens[0].fixed(2, counts.len())?;
    for count in counts {
        out.extend_from_slice(&count.to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::vec;

    fn numeric(bytes: &[u8], width: usize) -> Input<'_> {
        Input {
            ty: Type::Numeric,
            width,
            bytes,
            lens: &[],
        }
    }

    /// Runs `decode` as the engine would and returns the regenerated bytes.
    fn run(
        desc: &Desc,
        header: &[u8],
        inputs: &[Input<'_>],
        budget: usize,
    ) -> Result<Vec<u8>, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, header);
        let mut regens = [Stream::default()];
        regens[0].prepare(desc.regens[0], budget);
        (desc.decode)(&mut ctx, inputs, &mut regens)?;
        regens[0].validate()?;
        Ok(regens[0].bytes().to_vec())
    }

    #[test]
    fn signatures_follow_the_registry() {
        for desc in [&FSE_V2, &HUFFMAN_V2, &HUFFMAN_STRUCT_V2] {
            assert_eq!(desc.inputs, [Type::Numeric, Type::Serial]);
            assert_eq!(desc.min_version, 15);
        }
        assert_eq!(FSE_NCOUNT.inputs, [Type::Serial]);
        assert_eq!(FSE_NCOUNT.regens, [Type::Numeric]);
        assert_eq!(HUFFMAN_STRUCT_V2.regens, [Type::Struct]);
        assert_eq!(
            [
                FSE_V2.id,
                HUFFMAN_V2.id,
                HUFFMAN_STRUCT_V2.id,
                FSE_NCOUNT.id
            ],
            [49, 50, 51, 52]
        );
    }

    #[test]
    fn headers_hold_flags_and_a_size_of_at_least_two() {
        assert_eq!(header(&[1, 0x34, 0x12]), Ok((1, 0x1234)));
        assert_eq!(header(&[4, 2, 0, 0, 0, 0, 0, 0, 0]), Ok((4, 2)));
        for bad in [&[][..], &[1][..], &[1, 1][..], &[1, 0][..], &[1; 10][..]] {
            assert_eq!(header(bad), Err(corrupt()));
        }
    }

    #[test]
    fn huffman_decodes_one_and_four_streams() {
        let weights = [3u8, 2, 2];
        let table = numeric(&weights, 1);
        let single = run(&HUFFMAN_V2, &[0, 3], &[table, Input::serial(&[0x31])], 16);
        assert_eq!(single, Ok(vec![0, 1, 2]));
        let framed = [1, 0, 1, 0, 1, 0, 3, 3, 3, 3];
        let quad = run(&HUFFMAN_V2, &[1, 4], &[table, Input::serial(&framed)], 16);
        assert_eq!(quad, Ok(vec![0; 4]));
        // Errors: wrong weights width, bad header, output over budget, junk.
        assert!(
            run(
                &HUFFMAN_V2,
                &[0, 3],
                &[numeric(&weights, 2), Input::serial(&[0x31])],
                16
            )
            .is_err()
        );
        assert!(run(&HUFFMAN_V2, &[0], &[table, Input::serial(&[0x31])], 16).is_err());
        assert!(run(&HUFFMAN_V2, &[0, 3], &[table, Input::serial(&[0x31])], 2).is_err());
        assert!(run(&HUFFMAN_V2, &[0, 3], &[table, Input::serial(&[0])], 16).is_err());
        assert!(run(&HUFFMAN_V2, &[0, 3], &[table], 16).is_err());
        assert!(
            run(
                &HUFFMAN_V2,
                &[0, 3],
                &[numeric(&[1], 1), Input::serial(&[1])],
                16
            )
            .is_err()
        );
    }

    #[test]
    fn huffman_struct_decodes_sixteen_bit_symbols() {
        let weights = [1u8, 1];
        let table = numeric(&weights, 1);
        let block = [3, 0, 0, 0, 1, 0, 0, 0, 0x0d];
        let out = run(
            &HUFFMAN_STRUCT_V2,
            &[0, 3],
            &[table, Input::serial(&block)],
            16,
        );
        assert_eq!(out, Ok(vec![1, 0, 0, 0, 1, 0]));
        assert!(
            run(
                &HUFFMAN_STRUCT_V2,
                &[0, 4],
                &[table, Input::serial(&block)],
                16
            )
            .is_err()
        );
        assert!(
            run(
                &HUFFMAN_STRUCT_V2,
                &[0, 3],
                &[numeric(&weights, 2), Input::serial(&block)],
                16
            )
            .is_err()
        );
        assert!(run(&HUFFMAN_STRUCT_V2, &[0, 3], &[table], 16).is_err());
    }

    #[test]
    fn fse_rejects_malformed_inputs() {
        let counts = [16u8, 0, 16, 0];
        let table = numeric(&counts, 2);
        let bits = Input::serial(&[0, 0b100]);
        // Counts of the wrong width, a bad table, bad headers.
        assert!(run(&FSE_V2, &[2, 4], &[numeric(&counts, 1), bits], 16).is_err());
        assert!(run(&FSE_V2, &[2, 4], &[numeric(&[16, 0], 2), bits], 16).is_err());
        assert!(run(&FSE_V2, &[3, 4], &[table, bits], 16).is_err());
        assert!(run(&FSE_V2, &[2], &[table, bits], 16).is_err());
        assert!(run(&FSE_V2, &[2, 4], &[table], 16).is_err());
        // Output over the budget, and a stream without bits for the states.
        assert!(run(&FSE_V2, &[2, 40], &[table, bits], 16).is_err());
        assert!(run(&FSE_V2, &[4, 4], &[table, bits], 16).is_err());
    }

    #[test]
    fn ncount_headers_become_sixteen_bit_counts() {
        let out = run(&FSE_NCOUNT, &[], &[Input::serial(&[0x10, 0x3f])], 1024);
        assert_eq!(out, Ok(vec![16, 0, 16, 0]));
        // Trailing bytes after the header, and a malformed header.
        assert!(run(&FSE_NCOUNT, &[], &[Input::serial(&[0x10, 0x3f, 0])], 1024).is_err());
        assert!(run(&FSE_NCOUNT, &[], &[Input::serial(&[0x0f])], 1024).is_err());
        assert!(run(&FSE_NCOUNT, &[], &[Input::serial(&[0x10, 0x3f])], 2).is_err());
    }
}
