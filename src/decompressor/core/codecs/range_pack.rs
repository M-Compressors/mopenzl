//! The range-pack codec (`DI_rangePack`, `decode_range_pack_kernel.c`),
//! feature `range-pack`.
//!
//! Widens a numeric stream and adds back the minimum that the encoder
//! subtracted. The header is the destination width (1, 2, 4 or 8, at least
//! the source width), optionally followed by that minimum as a
//! destination-width little-endian number (zero when absent). Sums wrap at
//! the destination width.

use super::{Ctx, Desc};
use crate::decompressor::core::data::{Input, Stream, Type, corrupt};
use crate::decompressor::error::DecodeError;

/// `ZL_StandardTransformID_range_pack`.
pub(crate) const RANGE_PACK: Desc = Desc {
    id: 35,
    min_version: 8,
    regens: &[Type::Numeric],
    variable_regens: false,
    inputs: &[Type::Numeric],
    variable_inputs: 0,
    decode,
};

/// Reads a little-endian number of up to eight bytes.
fn load<const W: usize>(bytes: &[u8; W]) -> u64 {
    let mut value = [0u8; 8];
    for (to, from) in value.iter_mut().zip(bytes) {
        *to = *from;
    }
    u64::from_le_bytes(value)
}

/// Reads a little-endian number of up to eight bytes.
fn load_dynamic(bytes: &[u8]) -> u64 {
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
    let input = &inputs[0];
    let Some((&dst_width, minimum)) = ctx.header.split_first() else {
        return Err(corrupt());
    };
    let dst_width = usize::from(dst_width);
    if dst_width < input.width || !matches!(dst_width, 1 | 2 | 4 | 8) {
        return Err(corrupt());
    }
    let minimum = if minimum.is_empty() {
        0
    } else if minimum.len() == dst_width {
        load_dynamic(minimum)
    } else {
        return Err(corrupt());
    };
    let out = regens[0].fixed(dst_width, input.len())?;
    out.resize(input.len() * dst_width, 0);
    macro_rules! widen {
        ($($src:literal => ($($dst:literal),+)),+) => {
            match (input.width, dst_width) {
                $($(($src, $dst) => widen::<$src, $dst>(out, input.bytes, minimum),)+)+
                _ => return Err(corrupt()),
            }
        };
    }
    widen!(1 => (1, 2, 4, 8), 2 => (2, 4, 8), 4 => (4, 8), 8 => (8));
    Ok(())
}

/// `rangePackDecode` for one pair of widths.
fn widen<const S: usize, const D: usize>(dst: &mut [u8], src: &[u8], minimum: u64) {
    for (to, from) in dst
        .as_chunks_mut::<D>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<S>().0)
    {
        let value = load(from).wrapping_add(minimum);
        to.copy_from_slice(&value.to_le_bytes()[..D]);
    }
}

#[cfg(test)]
mod tests {
    const DESC: Desc = RANGE_PACK;
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
    fn values_widen_and_add_the_minimum() {
        let out = run(&[4, 100, 0, 0, 0], &[num(1, &[0, 1, 255])]).unwrap();
        assert_eq!(out.bytes(), le(4, &[100, 101, 355]));
        assert_eq!(out.as_input().width, 4);
        let out = run(&[8, 1, 0, 0, 0, 0, 0, 0, 0x80], &[num(2, &le(2, &[0, 7]))]).unwrap();
        assert_eq!(out.bytes(), le(8, &[(1 << 63) + 1, (1 << 63) + 8]));
        let out = run(&[2, 0xff, 0xff], &[num(2, &le(2, &[1]))]).unwrap();
        assert_eq!(out.bytes(), le(2, &[0]));
    }

    #[test]
    fn a_header_without_minimum_just_widens() {
        for (from, to) in [
            (1, 2),
            (1, 4),
            (1, 8),
            (2, 4),
            (2, 8),
            (4, 8),
            (1, 1),
            (8, 8),
        ] {
            let src = le(from, &[1, 2, 3]);
            let out = run(&[to as u8], &[num(from, &src)]).unwrap();
            assert_eq!(out.bytes(), le(to, &[1, 2, 3]), "{from} to {to}");
        }
        assert!(run(&[4], &[num(2, &[])]).unwrap().bytes().is_empty());
    }

    #[test]
    fn malformed_headers_are_corrupt() {
        let src = le(2, &[1]);
        // Missing, narrower than the source, illegal, or the wrong length.
        for header in [
            &[][..],
            &[1],
            &[3],
            &[16],
            &[4, 1],
            &[4, 1, 2, 3, 4, 5],
            &[2, 1],
        ] {
            assert_eq!(
                run(header, &[num(2, &src)]).err(),
                Some(corrupt()),
                "{header:?}"
            );
        }
    }

    #[test]
    fn output_beyond_the_budget_is_corrupt() {
        assert_eq!(
            run(&[8], &[num(4, &le(4, &[0; 9000]))]).err(),
            Some(corrupt())
        );
    }
}
