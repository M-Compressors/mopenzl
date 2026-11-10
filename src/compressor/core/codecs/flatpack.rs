//! The `flatpack` node (`EI_flatpack`, `encode_flatpack_binding.c` and
//! `encode_flatpack_kernel.c`), node 65, codec 29.
//!
//! The alphabet (the sorted distinct bytes) is output 0, and every byte
//! replaced by its index in `ceil(log2(alphabet))` bits, least significant
//! bit first, followed by a single terminating one bit, is output 1. An
//! empty input gives two empty outputs. The reference's BMI2 `pext` path
//! writes the same bytes as the generic one ported here.

use alloc::vec;
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
    // Eight indices are exactly `bits` bytes: compose them in a `u64` and
    // store it whole. Each store spills at most 7 bytes past its group, to
    // be overwritten by the next one, hence the slack.
    let mut packed = vec![0u8; packed_size + 8];
    let (groups, tail) = src.as_chunks::<8>();
    let mut at = 0usize;
    for group in groups {
        let mut word = 0u64;
        for (i, &byte) in group.iter().enumerate() {
            word |= u64::from(symbol_map[usize::from(byte)]) << (i * bits);
        }
        packed[at..at + 8].copy_from_slice(&word.to_le_bytes());
        at += bits;
    }
    // At most 7 indices of at most 8 bits, then the terminating one bit.
    let mut state = 0u64;
    let mut pending = 0usize;
    for &byte in tail {
        state |= u64::from(symbol_map[usize::from(byte)]) << pending;
        pending += bits;
    }
    state |= 1 << pending;
    let last = pending / 8 + 1;
    packed[at..at + last].copy_from_slice(&state.to_le_bytes()[..last]);
    packed.truncate(packed_size);
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

    /// `ZS_FlatPack_pack_generic`, one index at a time.
    fn reference(src: &[u8]) -> alloc::vec::Vec<u8> {
        let mut present = [false; 256];
        for &byte in src {
            present[usize::from(byte)] = true;
        }
        let mut map = [0u8; 256];
        let mut size = 0u8;
        for symbol in 0..256 {
            if present[symbol] {
                map[symbol] = size;
                size = size.wrapping_add(1);
            }
        }
        let distinct = present.iter().filter(|&&p| p).count();
        let bits = nb_bits(distinct);
        let (mut state, mut pending) = (0usize, 0usize);
        let mut out = alloc::vec::Vec::new();
        for &byte in src {
            state |= usize::from(map[usize::from(byte)]) << pending;
            pending += bits;
            if pending >= 8 {
                out.push(state as u8);
                pending -= 8;
                state >>= 8;
            }
        }
        state |= 1 << pending;
        out.push(state as u8);
        out
    }

    #[test]
    fn grouped_packing_matches_the_one_index_reference() {
        for distinct in [1usize, 2, 3, 4, 5, 16, 17, 100, 128, 129, 256] {
            for len in (0..40).chain([63, 64, 65, 200]) {
                let src: alloc::vec::Vec<u8> = (0..len)
                    .map(|i| ((i * 7 + i / 3) % distinct) as u8)
                    .collect();
                if src.is_empty() {
                    continue;
                }
                assert_eq!(flatpack(&src).1, reference(&src), "{distinct} {len}");
            }
        }
    }
}
