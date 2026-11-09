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
    let mask = (1usize << bits) - 1;
    let mut rest = packed.iter();
    let (mut state, mut held) = (0usize, 0usize);
    for _ in 0..count {
        if held < bits {
            let &byte = rest.next().ok_or_else(corrupt)?;
            state |= usize::from(byte) << held;
            held += 8;
        }
        let &symbol = alphabet.get(state & mask).ok_or_else(corrupt)?;
        out.push(symbol);
        state >>= bits;
        held -= bits;
    }
    // What is left is the sentinel bit, alone, in the last byte.
    if let Some(&byte) = rest.next() {
        state |= usize::from(byte) << held;
        held += 8;
    }
    if rest.next().is_some() || state != 1 || held > 8 {
        return Err(corrupt());
    }
    Ok(())
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
