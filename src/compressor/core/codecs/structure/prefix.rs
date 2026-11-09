//! The `prefix` node 30 (`encode_prefix_binding.c`, `encode_prefix_kernel.c`).
//!
//! Every string is replaced by its suffix after the longest prefix it shares
//! with the previous string (the first string has no previous one). Outputs:
//! the suffixes as strings, and the matched prefix length of every string as
//! a `u32` numeric stream.

use alloc::vec::Vec;

use crate::compressor::core::codecs::FnEncoder;
use crate::compressor::core::data::Stream;
use crate::compressor::core::error::NodeError;
use crate::compressor::core::exec::NodeCx;

/// `EI_prefix`.
pub(crate) static PREFIX: FnEncoder = FnEncoder(prefix);

fn prefix<'a>(cx: &mut NodeCx<'_, 'a>, inputs: &[Stream<'a>]) -> Result<(), NodeError> {
    let input = &inputs[0];
    let mut suffixes = Vec::with_capacity(input.content_size());
    let mut suffix_lens = Vec::with_capacity(input.n_elts());
    let mut match_bytes = Vec::with_capacity(input.n_elts() * 4);
    let mut previous: &[u8] = &[];
    let mut rest = input.bytes();
    for &len in input.lens() {
        let (current, tail) = rest.split_at(len as usize);
        // `ZS_getPrefixMatchSize`: the common prefix, at most the shorter
        // of the two strings.
        let matched = previous
            .iter()
            .zip(current)
            .take_while(|(a, b)| a == b)
            .count();
        suffixes.extend_from_slice(&current[matched..]);
        suffix_lens.push((current.len() - matched) as u32);
        match_bytes.extend_from_slice(&(matched as u32).to_le_bytes());
        previous = current;
        rest = tail;
    }
    cx.emit_strings(0, suffixes, suffix_lens)?;
    cx.emit(1, 4, match_bytes)?;
    Ok(())
}
