//! Decoders of the `bitpack` feature: `bitpack_serial` (27) and
//! `bitpack_int` (28).
//!
//! Ports `codecs/bitpack/decode_bitpack_binding.c` (header parsing and
//! the whole-source-consumed check) and the decode side of
//! `codecs/bitpack/common_bitpack_kernel.c`.
//!
//! The packed stream holds fixed-width fields of `nbBits` bits, least
//! significant bit first, back to back. The header is one or two bytes:
//! `[log2(eltWidth) << 6 | (nbBits - 1)] [extraElts]`. The element count is
//! the number of whole fields the stream could hold, minus the optional
//! `extraElts` (fields of padding the encoder had to add).
//!
//! The C kernel specialises per width and bit count (BMI2/AVX2 unpackers
//! for 1 to 16 bits, byte copies for multiples of 8, a bit-stream reader
//! otherwise). Here one scalar kernel serves every case: a 64-bit window
//! for fields of up to 56 bits and a 128-bit window above. Nothing here is
//! vectorised, because the scalar loop has no per-element branches beyond
//! the final partial window.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_bitpack_serial`: bytes packed into `nbBits`
/// fields, regenerated as serial bytes.
pub(crate) const BITPACK_SERIAL: Desc = Desc {
    id: 27,
    min_version: 3,
    regens: &[Type::Serial],
    variable_regens: false,
    inputs: &[Type::Serial],
    variable_inputs: 0,
    decode: decode_serial,
};

/// `ZL_StandardTransformID_bitpack_int`: integers packed into `nbBits`
/// fields, regenerated as numbers of the header's width.
pub(crate) const BITPACK_INT: Desc = Desc {
    id: 28,
    min_version: 3,
    regens: &[Type::Numeric],
    variable_regens: false,
    inputs: &[Type::Serial],
    variable_inputs: 0,
    decode: decode_numeric,
};

/// The parsed codec header (`ZL_BitpackHeader`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Header {
    /// Output element width in bytes: 1, 2, 4 or 8.
    width: usize,
    /// Bits per packed element: 1 to 64.
    bits: u32,
    /// Number of output elements.
    count: usize,
}

/// Reads the header for a packed stream of `packed_size` bytes
/// (`ZL_BitpackHeader_parse`).
fn parse_header(header: &[u8], packed_size: usize) -> Result<Header, DecodeError> {
    let (&first, extra) = header.split_first().ok_or_else(corrupt)?;
    if extra.len() > 1 {
        return Err(corrupt());
    }
    let width = 1usize << (first >> 6);
    let bits = u32::from(first & 0x3f) + 1;
    if bits as usize > width * 8 {
        return Err(corrupt());
    }
    let extra = extra.first().map_or(0, |&n| usize::from(n));
    let capacity = packed_size
        .checked_mul(8)
        .ok_or_else(corrupt)?
        .checked_div(bits as usize)
        .ok_or_else(corrupt)?;
    let count = capacity.checked_sub(extra).ok_or_else(corrupt)?;
    Ok(Header { width, bits, count })
}

/// `DI_bitpack_serialized`.
fn decode_serial(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    decode(ctx, inputs, regens, true)
}

/// `DI_bitpack_numeric`.
fn decode_numeric(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    decode(ctx, inputs, regens, false)
}

/// `DI_bitpack_typed`.
fn decode(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
    serial: bool,
) -> Result<(), DecodeError> {
    let packed = inputs[0].bytes;
    let header = parse_header(ctx.header, packed.len())?;
    if serial && header.width != 1 {
        return Err(corrupt());
    }
    let out = regens[0].fixed(header.width, header.count)?;
    // The reference compares the bytes the kernel consumed with the source:
    // all of the fields' bits, rounded up to a byte, must be the stream.
    let used = header
        .count
        .checked_mul(header.bits as usize)
        .map(|bits| bits.div_ceil(8))
        .ok_or_else(corrupt)?;
    if used != packed.len() {
        return Err(corrupt());
    }
    match header.width {
        1 => unpack::<1>(out, packed, header.count, header.bits),
        2 => unpack::<2>(out, packed, header.count, header.bits),
        4 => unpack::<4>(out, packed, header.count, header.bits),
        _ => unpack::<8>(out, packed, header.count, header.bits),
    }
    Ok(())
}

/// Loads the 8 bytes at `at`, zero padded past the end of `src`.
#[inline]
fn load_u64(src: &[u8], at: usize) -> u64 {
    let rest = src.get(at..).unwrap_or_default();
    match rest.first_chunk::<8>() {
        Some(chunk) => u64::from_le_bytes(*chunk),
        None => {
            let mut buf = [0u8; 8];
            buf[..rest.len()].copy_from_slice(rest);
            u64::from_le_bytes(buf)
        }
    }
}

/// Loads the 16 bytes at `at`, zero padded past the end of `src`.
#[inline]
fn load_u128(src: &[u8], at: usize) -> u128 {
    let rest = src.get(at..).unwrap_or_default();
    match rest.first_chunk::<16>() {
        Some(chunk) => u128::from_le_bytes(*chunk),
        None => {
            let mut buf = [0u8; 16];
            buf[..rest.len()].copy_from_slice(rest);
            u128::from_le_bytes(buf)
        }
    }
}

/// Appends `count` fields of `bits` bits (1 to `8 * W`) from `src` to
/// `out` as little-endian numbers of `W` bytes (`ZS_bitpackDecode*`).
///
/// `src` must hold at least `count * bits` bits; bits past it read as 0.
fn unpack<const W: usize>(out: &mut alloc::vec::Vec<u8>, src: &[u8], count: usize, bits: u32) {
    if bits as usize == W * 8 {
        // Full-width fields are the bytes themselves.
        out.extend_from_slice(src.get(..count * W).unwrap_or(src));
        return;
    }
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let step = bits as usize;
    let mut position = 0usize;
    for _ in 0..count {
        let (byte, shift) = (position >> 3, position & 7);
        let value = if bits <= 56 {
            (load_u64(src, byte) >> shift) & mask
        } else {
            ((load_u128(src, byte) >> shift) as u64) & mask
        };
        out.extend_from_slice(&value.to_le_bytes()[..W]);
        position += step;
    }
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn run(
        desc: &Desc,
        header: &[u8],
        packed: &[u8],
        budget: usize,
    ) -> Result<Stream, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, header);
        let mut regens = [Stream::default()];
        regens[0].prepare(desc.regen_type(0), budget);
        (desc.decode)(&mut ctx, &[Input::serial(packed)], &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream)
    }

    /// Bit-by-bit reference packer.
    fn pack(values: &[u64], bits: u32) -> Vec<u8> {
        let mut out = alloc::vec![0u8; (values.len() * bits as usize).div_ceil(8)];
        for (i, &value) in values.iter().enumerate() {
            for b in 0..bits as usize {
                if value >> b & 1 == 1 {
                    let at = i * bits as usize + b;
                    out[at / 8] |= 1 << (at % 8);
                }
            }
        }
        out
    }

    fn header(width: usize, bits: u32, extra: Option<u8>) -> Vec<u8> {
        let mut header = alloc::vec![((width.trailing_zeros() as u8) << 6) | (bits - 1) as u8];
        header.extend(extra);
        header
    }

    fn values(width: usize, bits: u32, count: usize) -> Vec<u64> {
        let mask = if bits == 64 {
            u64::MAX
        } else {
            (1 << bits) - 1
        };
        let mut x = 0x9e37_79b9_7f4a_7c15u64 ^ (bits as u64) << 8 ^ width as u64;
        (0..count)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x & mask
            })
            .collect()
    }

    #[test]
    fn every_width_and_bit_count_round_trips() {
        for width in [1usize, 2, 4, 8] {
            for bits in 1..=(width as u32 * 8) {
                for count in [0usize, 1, 2, 7, 8, 9, 33] {
                    let values = values(width, bits, count);
                    let packed = pack(&values, bits);
                    let stream = run(&BITPACK_INT, &header(width, bits, None), &packed, 1 << 12);
                    let Ok(stream) = stream else {
                        panic!("{width} {bits} {count}: {stream:?}");
                    };
                    let expected: Vec<u8> = values
                        .iter()
                        .flat_map(|v| v.to_le_bytes()[..width].to_vec())
                        .collect();
                    // Packed bits that do not fill a last field are padding
                    // the header must account for.
                    let padding = (packed.len() * 8 / bits as usize) - count;
                    if padding == 0 {
                        assert_eq!(stream.bytes(), expected, "{width} {bits} {count}");
                        assert_eq!(stream.as_input().width, width);
                    }
                }
            }
        }
    }

    #[test]
    fn extra_elements_are_dropped_from_the_end() {
        // 5 bits: 2 bytes hold 3 fields; the header drops the third.
        let values = values(1, 5, 2);
        let packed = pack(&values, 5);
        assert_eq!(packed.len(), 2);
        let out = run(&BITPACK_SERIAL, &header(1, 5, Some(1)), &packed, 64).unwrap();
        assert_eq!(
            out.bytes(),
            values.iter().map(|&v| v as u8).collect::<Vec<_>>()
        );
        // Without the extra, the third field is decoded too.
        let all = run(&BITPACK_SERIAL, &header(1, 5, None), &packed, 64).unwrap();
        assert_eq!(all.bytes().len(), 3);
        // An extra of 2 leaves a byte unconsumed.
        assert!(run(&BITPACK_SERIAL, &header(1, 5, Some(2)), &packed, 64).is_err());
        // More extra fields than the stream holds.
        assert!(run(&BITPACK_SERIAL, &header(1, 5, Some(4)), &packed, 64).is_err());
    }

    #[test]
    fn full_width_fields_are_copied() {
        let packed: Vec<u8> = (0u8..32).collect();
        let out = run(&BITPACK_INT, &header(8, 64, None), &packed, 64).unwrap();
        assert_eq!(out.bytes(), packed);
        let out = run(&BITPACK_INT, &header(4, 32, None), &packed, 64).unwrap();
        assert_eq!((out.bytes(), out.as_input().width), (&packed[..], 4));
    }

    #[test]
    fn empty_streams_decode_to_nothing() {
        let out = run(&BITPACK_SERIAL, &header(1, 5, None), &[], 0).unwrap();
        assert!(out.bytes().is_empty());
        let out = run(&BITPACK_INT, &header(2, 9, None), &[], 0).unwrap();
        assert!(out.bytes().is_empty());
    }

    #[test]
    fn malformed_headers_and_streams_are_rejected() {
        let packed = [0xffu8; 4];
        for bad in [&[][..], &[0, 0, 0], &[0x07 | 0x40, 1, 2]] {
            assert!(run(&BITPACK_INT, bad, &packed, 64).is_err(), "{bad:?}");
        }
        // More bits than the width holds.
        assert!(run(&BITPACK_INT, &[0x08], &packed, 64).is_err());
        assert!(run(&BITPACK_INT, &[0x40 | 0x10], &packed, 64).is_err());
        // Serial packing is for bytes only.
        assert!(run(&BITPACK_SERIAL, &header(2, 9, None), &packed, 64).is_err());
        // More extra fields than the stream holds.
        assert!(run(&BITPACK_INT, &header(1, 8, Some(5)), &packed, 64).is_err());
        // A stream with a whole unused byte.
        assert!(run(&BITPACK_INT, &header(1, 8, Some(1)), &packed, 64).is_err());
        // Output beyond the budget.
        assert!(run(&BITPACK_INT, &header(1, 8, None), &packed, 3).is_err());
    }

    #[test]
    fn short_windows_read_zero_past_the_end() {
        assert_eq!(load_u64(&[1, 2], 0), 0x0201);
        assert_eq!(load_u64(&[1, 2], 5), 0);
        assert_eq!(load_u128(&[1, 2, 3], 1), 0x0302);
        assert_eq!(load_u128(&[0xff; 20], 2), u128::MAX);
    }
}
