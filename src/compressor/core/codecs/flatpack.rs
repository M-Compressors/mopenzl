//! The `flatpack` node (`EI_flatpack`, `encode_flatpack_binding.c` and
//! `encode_flatpack_kernel.c`), node 65, codec 29.
//!
//! The alphabet (the sorted distinct bytes) is output 0, and every byte
//! replaced by its index in `ceil(log2(alphabet))` bits, least significant
//! bit first, followed by a single terminating one bit, is output 1. An
//! empty input gives two empty outputs. The reference's BMI2 `pext` path
//! writes the same bytes as the generic one ported here.

use alloc::vec::Vec;

use super::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;

/// The node encoder.
pub(crate) static FLATPACK: FnEncoder = FnEncoder(encode);

/// `ZS_FlatPack_nbBits`.
const fn nb_bits(alphabet_size: usize) -> usize {
    if alphabet_size <= 1 {
        alphabet_size
    } else {
        1 + (alphabet_size as u32 - 1).ilog2() as usize
    }
}

/// `ZS_flatpackEncode`: the alphabet and the packed indices.
fn flatpack(src: &[u8]) -> (Vec<u8>, Vec<u8>) {
    if src.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut present = [false; 256];
    for &byte in src {
        present[usize::from(byte)] = true;
    }
    let mut symbol_map = [0u8; 256];
    let mut alphabet = Vec::new();
    for (symbol, _) in present.iter().enumerate().filter(|&(_, &p)| p) {
        symbol_map[symbol] = alphabet.len() as u8;
        alphabet.push(symbol as u8);
    }
    let bits = nb_bits(alphabet.len());
    let packed_size = 1 + bits * src.len() / 8;
    let mut packed = Vec::with_capacity(packed_size);
    let mut state = 0usize;
    let mut pending = 0usize;
    for &byte in src {
        state |= usize::from(symbol_map[usize::from(byte)]) << pending;
        pending += bits;
        if pending >= 8 {
            packed.push(state as u8);
            pending -= 8;
            state >>= 8;
        }
    }
    state |= 1 << pending;
    packed.push(state as u8);
    (alphabet, packed)
}

fn encode<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let (alphabet, packed) = flatpack(inputs[0].bytes());
    cx.emit(0, 1, alphabet)?;
    cx.emit(1, 1, packed)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{flatpack, nb_bits};
    use alloc::vec;

    #[test]
    fn bit_counts_follow_the_alphabet_size() {
        assert_eq!(
            [0, 1, 2, 3, 4, 5, 128, 129, 256].map(nb_bits),
            [0, 1, 1, 2, 2, 3, 7, 8, 8]
        );
    }

    #[test]
    fn empty_input_has_empty_outputs() {
        assert_eq!(flatpack(&[]), (vec![], vec![]));
    }

    #[test]
    fn one_symbol_takes_a_bit_per_byte_before_the_terminator() {
        assert_eq!(flatpack(&[7, 7, 7]), (vec![7], vec![8]));
    }

    #[test]
    fn indices_are_packed_low_bits_first_with_a_terminator() {
        // Alphabet {1, 2, 3}: two bits per index; 3, 1, 2 -> indices 2, 0, 1.
        let (alphabet, packed) = flatpack(&[3, 1, 2]);
        assert_eq!(alphabet, [1, 2, 3]);
        assert_eq!(packed, [0b0101_0010]);
    }

    #[test]
    fn a_full_alphabet_uses_a_byte_per_symbol() {
        let src: alloc::vec::Vec<u8> = (0..=255).rev().collect();
        let (alphabet, packed) = flatpack(&src);
        assert_eq!(alphabet.len(), 256);
        assert_eq!(packed.len(), 257);
        assert_eq!(&packed[..3], &[255, 254, 253]);
        assert_eq!(packed[256], 1);
    }
}
