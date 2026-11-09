//! Generic compression routing and `brute_force` (`selector_compress.c`,
//! `selector_genericLZ.c`, `selector_numeric.c`, `selector_brute_force.c`).

use super::super::data::{Stream, Type};
use super::super::error::ExecError;
use super::super::exec::{Edge, GraphCx, SelectorCx, conversion_compatible};
use super::super::registry::{GraphFn, GraphId, SelectFn, gid};

/// `MultiInputGraph_compress` (`ZL_GRAPH_COMPRESS_GENERIC`): every input
/// goes to `compress1`.
#[derive(Debug)]
pub(crate) struct CompressGeneric;

/// `SI_selector_compress` (`compress1`): routes by input type.
#[derive(Debug)]
pub(crate) struct Compress1;

/// `SI_selector_compress_serial`: Zstandard.
#[derive(Debug)]
pub(crate) struct SerialCompress;

/// `SI_selector_compress_struct`: `field_lz`.
#[derive(Debug)]
pub(crate) struct StructCompress;

/// `SI_selector_compress_numeric`: `select_numeric`.
#[derive(Debug)]
pub(crate) struct NumericCompress;

/// `SI_selector_compress_string`: separate the components and compress each.
#[derive(Debug)]
pub(crate) struct StringCompress;

/// `SI_selector_genericLZ`: always Zstandard.
#[derive(Debug)]
pub(crate) struct SelectGenericLz;

/// `SI_selector_numeric`: below level 7 the struct route, otherwise the
/// transformer.
#[derive(Debug)]
pub(crate) struct SelectNumeric;

/// `SI_selector_brute_force`: the successor with the smallest trial frame,
/// strictly smaller than the input, else `store`.
#[derive(Debug)]
pub(crate) struct BruteForce;

/// See [`CompressGeneric`].
pub(crate) static COMPRESS_GENERIC: CompressGeneric = CompressGeneric;
/// See [`Compress1`].
pub(crate) static COMPRESS1: Compress1 = Compress1;
/// See [`SerialCompress`].
pub(crate) static SERIAL_COMPRESS: SerialCompress = SerialCompress;
/// See [`StructCompress`].
pub(crate) static STRUCT_COMPRESS: StructCompress = StructCompress;
/// See [`NumericCompress`].
pub(crate) static NUMERIC_COMPRESS: NumericCompress = NumericCompress;
/// See [`StringCompress`].
pub(crate) static STRING_COMPRESS: StringCompress = StringCompress;
/// See [`SelectGenericLz`].
pub(crate) static SELECT_GENERIC_LZ: SelectGenericLz = SelectGenericLz;
/// See [`SelectNumeric`].
pub(crate) static SELECT_NUMERIC: SelectNumeric = SelectNumeric;
/// See [`BruteForce`].
pub(crate) static BRUTE_FORCE: BruteForce = BruteForce;

/// `TRANSFORMER_MIN_COMPRESSION_LEVEL`.
const TRANSFORMER_MIN_LEVEL: i32 = 7;

impl GraphFn for CompressGeneric {
    fn run(&self, cx: &mut GraphCx<'_, '_, '_>, inputs: &[Edge]) -> Result<(), ExecError> {
        for &input in inputs {
            cx.set_destination(&[input], gid::COMPRESS1, None)?;
        }
        Ok(())
    }
}

impl SelectFn for Compress1 {
    fn select(&self, _: &mut SelectorCx<'_, '_, '_>, input: &Stream<'_>, _: &[GraphId]) -> GraphId {
        match input.ty() {
            Type::Serial => gid::SERIAL_COMPRESS,
            Type::Struct => gid::STRUCT_COMPRESS,
            Type::Numeric => gid::NUMERIC_COMPRESS,
            Type::String => gid::STRING_COMPRESS,
        }
    }
}

macro_rules! constant_selector {
    ($($ty:ty => $graph:expr;)*) => {$(
        impl SelectFn for $ty {
            fn select(&self, _: &mut SelectorCx<'_, '_, '_>, _: &Stream<'_>, _: &[GraphId]) -> GraphId {
                $graph
            }
        }
    )*};
}

constant_selector! {
    SerialCompress => gid::ZSTD;
    StructCompress => gid::FIELD_LZ;
    NumericCompress => gid::SELECT_NUMERIC;
    StringCompress => gid::STRING_SEPARATE_COMPRESS;
    SelectGenericLz => gid::ZSTD;
}

impl SelectFn for SelectNumeric {
    fn select(&self, cx: &mut SelectorCx<'_, '_, '_>, _: &Stream<'_>, _: &[GraphId]) -> GraphId {
        if cx.level() < TRANSFORMER_MIN_LEVEL {
            gid::STRUCT_COMPRESS
        } else {
            gid::TRANSFORMER_NUMERIC1
        }
    }
}

impl SelectFn for BruteForce {
    fn select(
        &self,
        cx: &mut SelectorCx<'_, '_, '_>,
        input: &Stream<'_>,
        successors: &[GraphId],
    ) -> GraphId {
        let mut best = input.weight();
        let mut best_index = None;
        for (index, &successor) in successors.iter().enumerate() {
            // Skip successors that cannot be fed the input type.
            let compatible = cx
                .input0_mask(successor)
                .is_some_and(|mask| conversion_compatible(input.ty(), mask));
            if !compatible {
                continue;
            }
            if let Ok(size) = cx.try_graph(input, successor)
                && size < best
            {
                best = size;
                best_index = Some(index);
            }
        }
        best_index.map_or(gid::STORE, |index| successors[index])
    }
}
