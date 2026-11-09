//! The sparse-number codec (`DI_sparse_num`, `decode_sparse_num_kernel.c`),
//! feature `sparse-num`.
//!
//! Two numeric inputs: run lengths (`ins[0]`, 1, 2 or 4 bytes wide) and
//! literal values (`ins[1]`, 1, 2, 4 or 8 bytes wide, which is also the
//! output width). Each distance counts dominant symbols emitted before the
//! next literal; one extra final distance counts the trailing run. The
//! dominant symbol is the header read as a little-endian number (an empty
//! header is zero) and the header may be no longer than the value width.
//! The output holds `sum(distances) + literals` elements.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_sparse_num`.
pub(crate) const SPARSE_NUM: Desc = Desc {
    id: 66,
    min_version: 26,
    regens: &[Type::Numeric],
    variable_regens: false,
    inputs: &[Type::Numeric, Type::Numeric],
    variable_inputs: 0,
    decode,
};

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
    let (distances, values) = (&inputs[0], &inputs[1]);
    if !matches!(distances.width, 1 | 2 | 4)
        || !matches!(values.width, 1 | 2 | 4 | 8)
        || ctx.header.len() > values.width
    {
        return Err(corrupt());
    }
    let (runs, literals) = (distances.len(), values.len());
    if runs != literals && runs != literals.wrapping_add(1) {
        return Err(corrupt());
    }
    // Checked before the output is sized, like `ZL_sparseNumDecodeOutputCount`.
    let mut count = literals;
    for run in distances.bytes.chunks_exact(distances.width) {
        count = usize::try_from(load(run))
            .ok()
            .and_then(|run| count.checked_add(run))
            .ok_or_else(corrupt)?;
    }
    let width = values.width;
    let out = regens[0].fixed(width, count)?;
    // The dominant symbol at the value width.
    let mut symbol = [0u8; 8];
    symbol[..ctx.header.len()].copy_from_slice(ctx.header);
    let symbol = &symbol[..width];
    let mut runs = distances.bytes.chunks_exact(distances.width);
    for literal in values.bytes.chunks_exact(width) {
        emit(out, symbol, runs.next().map_or(0, load));
        out.extend_from_slice(literal);
    }
    if let Some(last) = runs.next() {
        emit(out, symbol, load(last));
    }
    Ok(())
}

/// Appends `count` copies of `symbol`; the caller sized the stream for them.
fn emit(out: &mut alloc::vec::Vec<u8>, symbol: &[u8], count: u64) {
    let count = count as usize;
    if symbol.iter().all(|&byte| byte == 0) || symbol.len() == 1 {
        out.resize(out.len() + count * symbol.len(), symbol[0]);
    } else {
        for _ in 0..count {
            out.extend_from_slice(symbol);
        }
    }
}

#[cfg(test)]
mod tests {
    const DESC: Desc = SPARSE_NUM;
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

    #[test]
    fn zero_runs_separate_the_literals() {
        for width in [1usize, 2, 4, 8] {
            let distances = le(1, &[2, 0, 3]);
            let values = le(width, &[7, 8, 9]);
            let out = run(&[], &[num(1, &distances), num(width, &values)]).unwrap();
            assert_eq!(out.bytes(), le(width, &[0, 0, 7, 8, 0, 0, 0, 9]));
            assert_eq!(out.as_input().width, width);
        }
    }

    #[test]
    fn a_final_distance_adds_the_trailing_run() {
        let out = run(&[], &[num(1, &[1, 0, 2]), num(2, &le(2, &[5, 6]))]).unwrap();
        assert_eq!(out.bytes(), le(2, &[0, 5, 6, 0, 0]));
        // A final zero distance changes nothing.
        let out = run(&[], &[num(1, &[0, 0]), num(1, &[5])]).unwrap();
        assert_eq!(out.bytes(), [5]);
        // No literals at all: one run.
        let out = run(&[], &[num(4, &le(4, &[300])), num(1, &[])]).unwrap();
        assert_eq!(out.bytes(), alloc::vec![0u8; 300]);
    }

    #[test]
    fn distances_of_every_width_decode() {
        let values = le(2, &[1]);
        for width in [1usize, 2, 4] {
            let distances = le(width, &[3]);
            let out = run(&[], &[num(width, &distances), num(2, &values)]).unwrap();
            assert_eq!(out.bytes(), le(2, &[0, 0, 0, 1]));
        }
    }

    #[test]
    fn the_header_names_the_dominant_symbol() {
        let out = run(&[9], &[num(1, &[2, 1]), num(1, &[5])]).unwrap();
        assert_eq!(out.bytes(), [9, 9, 5, 9]);
        // Missing high bytes are zero.
        let out = run(&[0x34, 0x12], &[num(1, &[2]), num(4, &le(4, &[1]))]).unwrap();
        assert_eq!(out.bytes(), le(4, &[0x1234, 0x1234, 1]));
        let out = run(
            &[0x56, 0x34, 0x12],
            &[num(1, &[1, 1]), num(4, &le(4, &[7]))],
        )
        .unwrap();
        assert_eq!(out.bytes(), le(4, &[0x12_3456, 7, 0x12_3456]));
        let symbol = 0x0123_4567_89ab_cdef_u64;
        let out = run(
            &symbol.to_le_bytes(),
            &[num(1, &[1, 0]), num(8, &le(8, &[1]))],
        )
        .unwrap();
        assert_eq!(out.bytes(), le(8, &[symbol, 1]));
        // A zero header written out in full is still zero.
        let out = run(&[0, 0], &[num(1, &[1]), num(2, &le(2, &[3]))]).unwrap();
        assert_eq!(out.bytes(), le(2, &[0, 3]));
    }

    #[test]
    fn empty_inputs_make_an_empty_stream() {
        let out = run(&[], &[num(1, &[]), num(4, &[])]).unwrap();
        assert!(out.bytes().is_empty());
        assert_eq!(out.as_input().width, 4);
    }

    #[test]
    fn malformed_inputs_are_corrupt() {
        let values = le(1, &[1, 2]);
        // Unsupported distance width, header longer than the value width.
        assert_eq!(
            run(&[], &[num(8, &le(8, &[1, 1])), num(1, &values)]).err(),
            Some(corrupt())
        );
        assert_eq!(
            run(&[1, 2], &[num(1, &[1, 1]), num(1, &values)]).err(),
            Some(corrupt())
        );
        // Distance counts that are neither the literal count nor one more.
        assert_eq!(
            run(&[], &[num(1, &[1]), num(1, &values)]).err(),
            Some(corrupt())
        );
        assert_eq!(
            run(&[], &[num(1, &[1, 1, 1, 1]), num(1, &values)]).err(),
            Some(corrupt())
        );
    }

    #[test]
    fn sizes_beyond_the_budget_are_corrupt() {
        assert_eq!(
            run(&[], &[num(4, &le(4, &[u64::from(u32::MAX)])), num(1, &[])]).err(),
            Some(corrupt())
        );
        assert_eq!(
            run(
                &[],
                &[
                    num(4, &le(4, &[u64::from(u32::MAX); 8])),
                    num(8, &le(8, &[1; 7]))
                ]
            )
            .err(),
            Some(corrupt())
        );
    }
}
