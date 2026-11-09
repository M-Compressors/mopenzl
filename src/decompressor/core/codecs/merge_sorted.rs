//! The merge-sorted codec (`DI_mergeSorted`,
//! `decode_merge_sorted_kernel.c`), feature `merge-sorted`.
//!
//! Splits a merged list of unique 32-bit values back into up to 64 sorted
//! lists, concatenated into one numeric stream. `ins[0]` holds one bitset
//! per merged value (1, 2, 4 or 8 bytes wide); bit `b` tells whether list
//! `b` holds the value. `ins[1]` is the merged values. The header lists the
//! length of every list as a varint, so its entries also give the list
//! count (at most 64 and at most the bitset's bit width) and the output
//! length. A list that does not end up full is corrupt; set bits for a full
//! list are ignored.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;
use crate::shared::varint;

/// `ZL_StandardTransformID_merge_sorted`.
pub(crate) const MERGE_SORTED: Desc = Desc {
    id: 43,
    min_version: 9,
    regens: &[Type::Numeric],
    variable_regens: false,
    inputs: &[Type::Numeric, Type::Numeric],
    variable_inputs: 0,
    decode,
};

/// Lists the codec can split into.
const MAX_LISTS: usize = 64;

/// Reads a little-endian number of up to eight bytes.
fn load(bytes: &[u8]) -> u64 {
    let mut value = [0u8; 8];
    for (to, from) in value.iter_mut().zip(bytes) {
        *to = *from;
    }
    u64::from_le_bytes(value)
}

fn decode(
    ctx: &mut Ctx<'_>,
    inputs: &[Input<'_>],
    regens: &mut [Stream],
) -> Result<(), DecodeError> {
    let (bitsets, merged) = (&inputs[0], &inputs[1]);
    if merged.len() != bitsets.len() || merged.width != 4 {
        return Err(corrupt());
    }
    // `fillDstPtrsFromHeader`: the list sizes and their total.
    let max_total = (bitsets.width as u64 * 8).saturating_mul(bitsets.len() as u64);
    // Start and end (in elements) of every list.
    let mut spans = [(0usize, 0usize); MAX_LISTS];
    let mut lists = 0;
    let mut start = 0usize;
    let mut total = 0u64;
    let mut rest = ctx.header;
    while !rest.is_empty() {
        let (size, used) = varint::decode(rest).map_err(|_| corrupt())?;
        rest = &rest[used..];
        total = total.checked_add(size).ok_or_else(corrupt)?;
        // A size beyond `usize` fails the budget (or the total) below.
        let size = usize::try_from(size).unwrap_or(usize::MAX);
        let end = start.checked_add(size).ok_or_else(corrupt)?;
        *spans.get_mut(lists).ok_or_else(corrupt)? = (start, end);
        start = end;
        lists += 1;
    }
    if total > max_total || lists > bitsets.width * 8 {
        return Err(corrupt());
    }
    let total = usize::try_from(total).unwrap_or(usize::MAX);
    let out = regens[0].fixed(4, total)?;
    out.resize(total * 4, 0);
    let spans = &mut spans[..lists];
    for (bits, value) in bitsets
        .bytes
        .chunks_exact(bitsets.width)
        .zip(merged.bytes.as_chunks::<4>().0)
    {
        let bits = load(bits);
        for (list, (next, end)) in spans.iter_mut().enumerate() {
            if next != end {
                // Written even when the bit is clear: the next value then
                // overwrites it, as in the reference.
                out[*next * 4..*next * 4 + 4].copy_from_slice(value);
                *next += usize::from((bits >> list) & 1 != 0);
            }
        }
    }
    if spans.iter().any(|(next, end)| next != end) {
        return Err(corrupt());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    const DESC: Desc = MERGE_SORTED;
    use super::super::State;
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn run(header: &[u8], inputs: &[Input<'_>]) -> Result<Stream, DecodeError> {
        run_with(&DESC, header, inputs)
    }

    fn run_with(desc: &Desc, header: &[u8], inputs: &[Input<'_>]) -> Result<Stream, DecodeError> {
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, header);
        let mut regens = [Stream::default()];
        regens[0].prepare(desc.regen_type(0), 1 << 16);
        (desc.decode)(&mut ctx, inputs, &mut regens)?;
        let [stream] = regens;
        stream.validate()?;
        Ok(stream)
    }

    fn num(width: usize, bytes: &[u8]) -> Input<'_> {
        Input {
            ty: Type::Numeric,
            width,
            bytes,
            lens: &[],
        }
    }

    fn le(width: usize, values: &[u64]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes()[..width].to_vec())
            .collect()
    }

    fn words(values: &[u32]) -> Vec<u8> {
        le(4, &values.iter().map(|&v| u64::from(v)).collect::<Vec<_>>())
    }

    #[test]
    fn bitsets_route_each_value_to_its_lists() {
        // Lists [1, 3, 5] and [3, 4, 5, 6]: merged 1 3 4 5 6.
        let merged = words(&[1, 3, 4, 5, 6]);
        let bitsets = [0b01u8, 0b11, 0b10, 0b11, 0b10];
        let out = run(&[3, 4], &[num(1, &bitsets), num(4, &merged)]).unwrap();
        assert_eq!(out.bytes(), words(&[1, 3, 5, 3, 4, 5, 6]));
        assert_eq!(out.as_input().width, 4);
    }

    #[test]
    fn bitsets_of_every_width_decode() {
        let merged = words(&[10, 20]);
        for width in [1usize, 2, 4, 8] {
            let bitsets = le(width, &[0b01, 0b11]);
            let out = run(&[2, 1], &[num(width, &bitsets), num(4, &merged)]).unwrap();
            assert_eq!(out.bytes(), words(&[10, 20, 20]));
        }
        // The highest bit of a wide bitset.
        let bitsets = le(8, &[1 << 63, 1 << 63]);
        let mut header = alloc::vec![0u8; 63];
        header.push(2);
        let out = run(&header, &[num(8, &bitsets), num(4, &merged)]).unwrap();
        assert_eq!(out.bytes(), words(&[10, 20]));
    }

    #[test]
    fn empty_lists_and_empty_headers_decode() {
        let out = run(&[], &[num(1, &[]), num(4, &[])]).unwrap();
        assert!(out.bytes().is_empty());
        // Empty lists take nothing; the other list holds everything.
        let merged = words(&[4, 5]);
        let out = run(&[0, 2, 0], &[num(1, &[0b10, 0b10]), num(4, &merged)]).unwrap();
        assert_eq!(out.bytes(), words(&[4, 5]));
    }

    #[test]
    fn bits_for_full_lists_are_ignored() {
        let merged = words(&[1, 2, 3]);
        let out = run(&[1], &[num(1, &[1, 1, 1]), num(4, &merged)]).unwrap();
        assert_eq!(out.bytes(), words(&[1]));
    }

    #[test]
    fn lists_that_stay_short_are_corrupt() {
        let merged = words(&[1, 2]);
        assert_eq!(
            run(&[3], &[num(1, &[1, 1]), num(4, &merged)]).err(),
            Some(corrupt())
        );
        assert_eq!(
            run(&[2], &[num(1, &[1, 0]), num(4, &merged)]).err(),
            Some(corrupt())
        );
    }

    #[test]
    fn malformed_streams_and_headers_are_corrupt() {
        let merged = words(&[1, 2]);
        let bitsets = [1u8, 1];
        // Mismatched element counts, a merged stream that is not 32-bit.
        assert_eq!(
            run(&[1], &[num(1, &[1]), num(4, &merged)]).err(),
            Some(corrupt())
        );
        assert_eq!(
            run(&[1], &[num(1, &bitsets), num(2, &le(2, &[1, 2]))]).err(),
            Some(corrupt())
        );
        // A cut varint, a total beyond what the bitsets can hold, a
        // total that overflows.
        assert_eq!(
            run(&[0x80], &[num(1, &bitsets), num(4, &merged)]).err(),
            Some(corrupt())
        );
        assert_eq!(
            run(&[17], &[num(1, &bitsets), num(4, &merged)]).err(),
            Some(corrupt())
        );
        let mut huge = alloc::vec![0xff; 9];
        huge.push(1);
        huge.extend(huge.clone());
        assert_eq!(
            run(&huge, &[num(1, &bitsets), num(4, &merged)]).err(),
            Some(corrupt())
        );
        // More lists than bits in the bitset, and than the codec supports.
        assert_eq!(
            run(&[0; 9], &[num(1, &bitsets), num(4, &merged)]).err(),
            Some(corrupt())
        );
        let zeros = le(8, &[0; 2]);
        let wide = [num(8, &zeros), num(4, &merged)];
        assert!(run(&[0; 64], &wide).is_ok());
        assert_eq!(run(&[0; 65], &wide).err(), Some(corrupt()));
    }

    #[test]
    fn output_beyond_the_budget_is_corrupt() {
        let merged = words(&[0; 2000]);
        let bitsets = alloc::vec![0xffu8; 2000];
        let mut header = Vec::new();
        for _ in 0..8 {
            header.extend([0xa0, 0x1f]);
        }
        assert_eq!(
            run(&header, &[num(1, &bitsets), num(4, &merged)]).err(),
            Some(corrupt())
        );
    }
}
