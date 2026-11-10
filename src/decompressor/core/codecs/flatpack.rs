//! Decoder of the `flatpack` feature: `flatpack` (29).
//!
//! Ports `codecs/flatpack/decode_flatpack_binding.c` and the portable
//! decoder `ZS_flatpackDecodeEnd` of `decode_flatpack_kernel.c`; the C
//! BMI2/SSE shuffle paths are speed variants of it.
//!
//! Flatpack tokenizes bytes into an alphabet (input 0, at most 256 bytes)
//! and packs the indices at `ceil(log2(alphabetSize))` bits each, least
//! significant bit first (input 1). The packed stream ends with a single
//! `1` sentinel bit above the last index, which gives the element count
//! (`ZS_FlatPack_nbElts`) and doubles as an integrity check.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_flatpack`: alphabet and packed indices back to
/// bytes.
pub(crate) const FLATPACK: Desc = Desc {
    id: 29,
    min_version: 3,
    regens: &[Type::Serial],
    variable_regens: false,
    inputs: &[Type::Serial, Type::Serial],
    variable_inputs: 0,
    decode,
};

/// Bits per index for an alphabet of `size` symbols (`ZS_FlatPack_nbBits`).
const fn index_bits(size: usize) -> usize {
    if size <= 1 {
        size
    } else {
        (usize::BITS - (size - 1).leading_zeros()) as usize
    }
}

/// Elements in `packed` (`ZS_FlatPack_nbElts`).
const fn element_count(alphabet: usize, packed: &[u8]) -> usize {
    let Some(&last) = packed.last() else {
        return 0;
    };
    if alphabet == 0 {
        return 0;
    }
    let padding = ((last | 1) as u32).leading_zeros() as usize - 24 + 1;
    (8 * packed.len() - padding) / index_bits(alphabet)
}

/// `DI_flatpack`.
fn decode(_: &mut Ctx<'_>, inputs: &[Input<'_>], regens: &mut [Stream]) -> Result<(), DecodeError> {
    let (alphabet, packed) = (inputs[0].bytes, inputs[1].bytes);
    if alphabet.len() > 256 {
        return Err(corrupt());
    }
    let count = element_count(alphabet.len(), packed);
    let out = regens[0].fixed(1, count)?;
    if packed.is_empty() || alphabet.is_empty() {
        return Ok(());
    }
    let bits = index_bits(alphabet.len());
    // The sentinel bit sits right after the last index and alone above it
    // in the last byte.
    let end_bit = count * bits;
    let sentinel_ok = packed.len() == end_bit / 8 + 1 && packed[end_bit / 8] >> (end_bit % 8) == 1;
    let mut symbols = [0u8; 256];
    symbols[..alphabet.len()].copy_from_slice(alphabet);
    let largest = unpack_symbols(out, packed, count, bits as u32, &symbols);
    if !sentinel_ok || usize::from(largest) >= alphabet.len() {
        return Err(corrupt());
    }
    Ok(())
}

/// Appends the `count` symbols of `packed`, `bits` per index (1 to 8), to
/// `out` by table lookup in `symbols`, and returns the largest index read.
///
/// Eight indices span exactly `bits` bytes, so a whole group is one
/// unaligned 64-bit load and eight shifts; only the last groups, which
/// have fewer than eight bytes left to load, read bit by bit.
fn unpack_symbols(
    out: &mut alloc::vec::Vec<u8>,
    packed: &[u8],
    count: usize,
    bits: u32,
    symbols: &[u8; 256],
) -> u8 {
    let mask = (1u64 << bits) - 1;
    let start = out.len();
    out.resize(start + count, 0);
    let mut largest = 0u8;
    let mut at = 0usize;
    let (groups, tail) = out[start..].as_chunks_mut::<8>();
    for group in groups {
        if let Some(word) = packed.get(at..at + 8) {
            let word = u64::from_le_bytes([
                word[0], word[1], word[2], word[3], word[4], word[5], word[6], word[7],
            ]);
            for (i, symbol) in group.iter_mut().enumerate() {
                let index = ((word >> (i as u32 * bits)) & mask) as u8;
                largest = largest.max(index);
                *symbol = symbols[usize::from(index)];
            }
        } else {
            largest = largest.max(unpack_slow(group, packed, at, bits, symbols));
        }
        at += bits as usize;
    }
    largest.max(unpack_slow(tail, packed, at, bits, symbols))
}

/// [`unpack_symbols`] for `group.len()` indices starting at byte `at`, one
/// bit-offset at a time; bytes past the end of `packed` read as zero.
fn unpack_slow(group: &mut [u8], packed: &[u8], at: usize, bits: u32, symbols: &[u8; 256]) -> u8 {
    let mask = (1u16 << bits) - 1;
    let mut largest = 0u8;
    for (i, symbol) in group.iter_mut().enumerate() {
        let bit = i * bits as usize;
        let low = packed.get(at + bit / 8).copied().map_or(0, u16::from);
        let high = packed.get(at + bit / 8 + 1).copied().map_or(0, u16::from);
        let index = (((high << 8 | low) >> (bit % 8)) & mask) as u8;
        largest = largest.max(index);
        *symbol = symbols[usize::from(index)];
    }
    largest
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn run(alphabet: &[u8], packed: &[u8], budget: usize) -> Result<Vec<u8>, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[]);
        let mut regens = [Stream::default()];
        regens[0].prepare(Type::Serial, budget);
        let inputs = [Input::serial(alphabet), Input::serial(packed)];
        (FLATPACK.decode)(&mut ctx, &inputs, &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream.bytes().to_vec())
    }

    /// Reference encoder (`ZS_FlatPack_pack_generic`).
    fn pack(alphabet: &[u8], data: &[u8]) -> Vec<u8> {
        let bits = index_bits(alphabet.len());
        let mut out = Vec::new();
        let (mut state, mut held) = (0usize, 0usize);
        for byte in data {
            let index = alphabet.iter().position(|a| a == byte).unwrap();
            state |= index << held;
            held += bits;
            if held >= 8 {
                out.push(state as u8);
                state >>= 8;
                held -= 8;
            }
        }
        out.push((state | 1 << held) as u8);
        out
    }

    #[test]
    fn indices_unpack_for_every_alphabet_size() {
        for size in [1usize, 2, 3, 4, 5, 16, 17, 100, 128, 129, 256] {
            let alphabet: Vec<u8> = (0..size).map(|i| (i * 7 + 3) as u8).collect();
            for len in [1usize, 2, 7, 8, 9, 100, 1000] {
                let data: Vec<u8> = (0..len)
                    .map(|i| alphabet[(i * 31 + i / 3) % size])
                    .collect();
                let packed = pack(&alphabet, &data);
                assert_eq!(run(&alphabet, &packed, 4096).unwrap(), data, "{size} {len}");
            }
        }
    }

    #[test]
    fn empty_inputs_decode_to_nothing() {
        assert!(run(&[], &[], 0).unwrap().is_empty());
        assert!(run(&[1, 2], &[], 0).unwrap().is_empty());
        assert!(run(&[], &[1], 0).unwrap().is_empty());
        // A sentinel alone: no elements.
        assert!(run(&[1, 2], &[1], 0).unwrap().is_empty());
    }

    #[test]
    fn corrupted_streams_are_rejected() {
        let alphabet = *b"abc";
        let packed = pack(&alphabet, b"abcabcabc");
        assert_eq!(run(&alphabet, &packed, 64).unwrap(), b"abcabcabc");
        // Index 3 is outside a 3-symbol alphabet.
        let bad = pack(&[0, 1, 2, 3], &[0, 1, 2, 3]);
        assert!(run(&alphabet, &bad, 64).is_err());
        // A missing sentinel, trailing bytes, a wrong sentinel position.
        assert!(run(&alphabet, &[0], 64).is_err());
        let mut trailing = packed.clone();
        trailing.push(0);
        assert!(run(&alphabet, &trailing, 64).is_err());
        let mut padded = packed.clone();
        *padded.last_mut().unwrap() |= 0x80;
        assert!(run(&alphabet, &padded, 64).is_err());
        // Alphabets are at most 256 symbols; output is bounded.
        assert!(run(&[0; 257], &packed, 64).is_err());
        assert!(run(&alphabet, &packed, 4).is_err());
        assert_eq!(index_bits(0), 0);
    }
}
