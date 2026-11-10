//! The standard codecs a chunk can name, each behind its own feature.
//!
//! A [`Desc`] mirrors the reference's `ZL_MIGraphDesc` from the decoder's
//! side: the streams a codec regenerates (the encoder's inputs, `regens`),
//! the streams it reads (the encoder's outputs: fixed `inputs`, then
//! variable inputs of the `variable_inputs` type mask), and the function
//! that runs it. [`standard`] maps a wire ID to its descriptor; a codec
//! whose feature is off is unknown, so its chunks are `Unsupported`.
//!
//! Inputs reach a codec in the reference's order: input `i` of a codec
//! whose inputs are streams `base..base + n` is stream `base + n - 1 - i`.

use super::data::{Input, Stream, Type};
use crate::Backend;
use crate::decompressor::error::DecodeError;
use crate::dict::core::Materialized;

#[cfg(feature = "bitpack")]
pub(crate) mod bitpack;
#[cfg(feature = "bitsplit")]
pub(crate) mod bitsplit;
#[cfg(feature = "bitunpack")]
pub(crate) mod bitunpack;
#[cfg(feature = "concat")]
pub(crate) mod concat;
#[cfg(feature = "constant")]
pub(crate) mod constant;
#[cfg(feature = "conversion")]
pub(crate) mod conversion;
#[cfg(feature = "dedup")]
pub(crate) mod dedup;
#[cfg(feature = "delta")]
pub(crate) mod delta;
#[cfg(feature = "dispatch")]
pub(crate) mod dispatch;
#[cfg(feature = "divide-by")]
pub(crate) mod divide_by;
#[cfg(feature = "entropy")]
pub(crate) mod entropy;
#[cfg(feature = "field-lz")]
pub(crate) mod field_lz;
#[cfg(feature = "flatpack")]
pub(crate) mod flatpack;
#[cfg(feature = "float-deconstruct")]
pub(crate) mod float_deconstruct;
#[cfg(feature = "interleave")]
pub(crate) mod interleave;
#[cfg(feature = "lz")]
pub(crate) mod lz;
#[cfg(feature = "lz4")]
pub(crate) mod lz4;
#[cfg(any(feature = "lz", feature = "field-lz"))]
mod lz_copy;
#[cfg(feature = "merge-sorted")]
pub(crate) mod merge_sorted;
#[cfg(feature = "mux-lengths")]
pub(crate) mod mux_lengths;
#[cfg(feature = "parse-int")]
pub(crate) mod parse_int;
#[cfg(feature = "partition")]
pub(crate) mod partition;
#[cfg(feature = "pivco-huffman")]
pub(crate) mod pivco_huffman;
#[cfg(feature = "prefix")]
pub(crate) mod prefix;
#[cfg(feature = "quantize")]
pub(crate) mod quantize;
#[cfg(feature = "range-pack")]
pub(crate) mod range_pack;
#[cfg(feature = "sentinel")]
pub(crate) mod sentinel;
#[cfg(feature = "sparse-num")]
pub(crate) mod sparse_num;
#[cfg(feature = "split")]
pub(crate) mod split;
#[cfg(feature = "split-by-struct")]
pub(crate) mod split_by_struct;
#[cfg(feature = "tokenize")]
pub(crate) mod tokenize;
#[cfg(feature = "transpose")]
pub(crate) mod transpose;
#[cfg(feature = "zigzag")]
pub(crate) mod zigzag;
#[cfg(feature = "zstd")]
pub(crate) mod zstd;

/// Runs a codec: reads `inputs`, fills every stream of `regens`.
pub(crate) type DecodeFn = fn(&mut Ctx<'_>, &[Input<'_>], &mut [Stream]) -> Result<(), DecodeError>;

/// What a standard codec reads and regenerates.
#[derive(Debug)]
pub(crate) struct Desc {
    /// `ZL_StandardTransformID_*`.
    pub(crate) id: u32,
    /// First format version with this codec.
    pub(crate) min_version: u32,
    /// Types of the regenerated streams; the last repeats when
    /// `variable_regens` is set.
    pub(crate) regens: &'static [Type],
    /// Whether the codec regenerates a variable number of streams
    /// (`lastInputIsVariable`).
    pub(crate) variable_regens: bool,
    /// Types of the fixed inputs (`soTypes`).
    pub(crate) inputs: &'static [Type],
    /// `ZL_Type` bits allowed for variable inputs (`voTypes`); 0 for none.
    pub(crate) variable_inputs: u8,
    /// The decoder.
    pub(crate) decode: DecodeFn,
}

impl Desc {
    /// Whether `count` regenerated streams fit the signature
    /// (`DT_isNbRegensCompatible`).
    pub(crate) const fn regens_fit(&self, count: usize) -> bool {
        if self.variable_regens {
            count + 1 >= self.regens.len()
        } else {
            count == self.regens.len()
        }
    }

    /// The type of regenerated stream `index` (`DT_getRegenType`).
    pub(crate) fn regen_type(&self, index: usize) -> Type {
        self.regens[index.min(self.regens.len() - 1)]
    }

    /// Whether `input` may be input `index`.
    pub(crate) fn accepts(&self, index: usize, input: Type) -> bool {
        match self.inputs.get(index) {
            Some(&ty) => ty == input,
            None => self.variable_inputs & input.bit() != 0,
        }
    }
}

/// Reusable codec state, one field per codec that keeps any.
#[derive(Debug)]
pub(crate) struct State {
    #[cfg(feature = "zstd")]
    pub(crate) zstd: super::zstd::ZstdDecoder,
    /// The SIMD level the decompressor selected once; kernels dispatch on it
    /// outside their inner loops.
    #[cfg_attr(
        not(any(feature = "transpose", feature = "entropy", feature = "pivco-huffman")),
        expect(
            dead_code,
            reason = "only the transpose, entropy and PivCo-Huffman decoders dispatch on it"
        )
    )]
    pub(crate) simd: fearless_simd::Level,
}

impl State {
    pub(crate) fn new(backend: Backend) -> Self {
        Self {
            #[cfg(feature = "zstd")]
            zstd: super::zstd::ZstdDecoder::new(backend),
            simd: backend.0,
        }
    }

    /// Bytes kept allocated.
    pub(crate) fn retained_bytes(&self) -> usize {
        #[cfg(feature = "zstd")]
        let zstd = self.zstd.retained_bytes();
        #[cfg(not(feature = "zstd"))]
        let zstd = 0;
        zstd
    }

    /// Releases every buffer.
    pub(crate) fn release(&mut self) {
        #[cfg(feature = "zstd")]
        self.zstd.release();
    }
}

/// What a running codec may consult besides its streams.
#[derive(Debug)]
pub(crate) struct Ctx<'a> {
    /// The codec's private header.
    #[cfg_attr(
        not(any(
            test,
            feature = "bitpack",
            feature = "bitsplit",
            feature = "bitunpack",
            feature = "constant",
            feature = "conversion",
            feature = "delta",
            feature = "divide-by",
            feature = "entropy",
            feature = "field-lz",
            feature = "float-deconstruct",
            feature = "interleave",
            feature = "lz",
            feature = "lz4",
            feature = "merge-sorted",
            feature = "mux-lengths",
            feature = "partition",
            feature = "pivco-huffman",
            feature = "range-pack",
            feature = "sentinel",
            feature = "sparse-num",
            feature = "split"
        )),
        expect(dead_code, reason = "only these codecs read a private header")
    )]
    pub(crate) header: &'a [u8],
    /// Reusable state.
    #[cfg_attr(
        not(any(
            feature = "zstd",
            feature = "transpose",
            feature = "entropy",
            feature = "pivco-huffman"
        )),
        expect(
            dead_code,
            reason = "only the Zstandard, transpose, entropy and PivCo-Huffman decoders read the state"
        )
    )]
    pub(crate) state: &'a mut State,
    /// The codec's materialized dictionary (`ZL_Decoder_getMaterializedDict`).
    pub(crate) dict: Option<&'a Materialized>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(state: &'a mut State, header: &'a [u8]) -> Self {
        Self {
            header,
            state,
            dict: None,
        }
    }

    /// The same context with `dict` as the codec's dictionary.
    pub(crate) fn with_dictionary(mut self, dict: Option<&'a Materialized>) -> Self {
        self.dict = dict;
        self
    }
}

/// The descriptor of standard codec `id`, when its feature is enabled.
pub(crate) const fn standard(id: u32) -> Option<&'static Desc> {
    match id {
        #[cfg(feature = "delta")]
        1 => Some(&delta::DELTA_INT),
        #[cfg(feature = "transpose")]
        2 => Some(&transpose::TRANSPOSE),
        #[cfg(feature = "zigzag")]
        3 => Some(&zigzag::ZIGZAG),
        #[cfg(feature = "transpose")]
        4 => Some(&transpose::TRANSPOSE_SPLIT),
        #[cfg(feature = "conversion")]
        5 => Some(&conversion::SERIAL_TO_STRUCT),
        #[cfg(feature = "conversion")]
        6 => Some(&conversion::STRUCT_TO_SERIAL),
        #[cfg(feature = "conversion")]
        7 => Some(&conversion::STRUCT_TO_NUM_LE),
        #[cfg(feature = "conversion")]
        8 => Some(&conversion::NUM_TO_STRUCT_LE),
        #[cfg(feature = "conversion")]
        9 => Some(&conversion::SERIAL_TO_NUM_LE),
        #[cfg(feature = "conversion")]
        10 => Some(&conversion::NUM_TO_SERIAL_LE),
        #[cfg(feature = "conversion")]
        11 => Some(&conversion::SERIAL_STRING),
        #[cfg(feature = "conversion")]
        12 => Some(&conversion::SEPARATE_STRING_COMPONENTS),
        #[cfg(feature = "conversion")]
        13 => Some(&conversion::STRUCT_TO_NUM_BE),
        #[cfg(feature = "conversion")]
        14 => Some(&conversion::SERIAL_TO_NUM_BE),
        #[cfg(feature = "sentinel")]
        18 => Some(&sentinel::SENTINEL),
        #[cfg(feature = "lz")]
        19 => Some(&lz::LZ),
        #[cfg(feature = "zstd")]
        22 => Some(&zstd::ZSTD),
        #[cfg(feature = "field-lz")]
        24 => Some(&field_lz::FIELD_LZ),
        #[cfg(feature = "quantize")]
        25 => Some(&quantize::QUANTIZE_OFFSETS),
        #[cfg(feature = "quantize")]
        26 => Some(&quantize::QUANTIZE_LENGTHS),
        #[cfg(feature = "bitpack")]
        27 => Some(&bitpack::BITPACK_SERIAL),
        #[cfg(feature = "bitpack")]
        28 => Some(&bitpack::BITPACK_INT),
        #[cfg(feature = "flatpack")]
        29 => Some(&flatpack::FLATPACK),
        #[cfg(feature = "transpose")]
        30 => Some(&transpose::TRANSPOSE_SPLIT2),
        #[cfg(feature = "transpose")]
        31 => Some(&transpose::TRANSPOSE_SPLIT4),
        #[cfg(feature = "transpose")]
        32 => Some(&transpose::TRANSPOSE_SPLIT8),
        #[cfg(feature = "float-deconstruct")]
        33 => Some(&float_deconstruct::FLOAT_DECONSTRUCT),
        #[cfg(feature = "bitunpack")]
        34 => Some(&bitunpack::BITUNPACK),
        #[cfg(feature = "range-pack")]
        35 => Some(&range_pack::RANGE_PACK),
        #[cfg(feature = "tokenize")]
        36 => Some(&tokenize::TOKENIZE_FIXED),
        #[cfg(feature = "tokenize")]
        37 => Some(&tokenize::TOKENIZE_NUMERIC),
        #[cfg(feature = "tokenize")]
        38 => Some(&tokenize::TOKENIZE_STRING),
        #[cfg(feature = "split")]
        40 => Some(&split::SPLITN),
        #[cfg(feature = "split-by-struct")]
        41 => Some(&split_by_struct::SPLIT_BY_STRUCT),
        #[cfg(feature = "dispatch")]
        42 => Some(&dispatch::DISPATCH_N_BY_TAG),
        #[cfg(feature = "merge-sorted")]
        43 => Some(&merge_sorted::MERGE_SORTED),
        #[cfg(feature = "constant")]
        44 => Some(&constant::CONSTANT_SERIAL),
        #[cfg(feature = "constant")]
        45 => Some(&constant::CONSTANT_FIXED),
        #[cfg(feature = "prefix")]
        46 => Some(&prefix::PREFIX),
        #[cfg(feature = "split")]
        47 => Some(&split::SPLITN_STRUCT),
        #[cfg(feature = "split")]
        48 => Some(&split::SPLITN_NUM),
        #[cfg(feature = "entropy")]
        49 => Some(&entropy::FSE_V2),
        #[cfg(feature = "entropy")]
        50 => Some(&entropy::HUFFMAN_V2),
        #[cfg(feature = "entropy")]
        51 => Some(&entropy::HUFFMAN_STRUCT_V2),
        #[cfg(feature = "entropy")]
        52 => Some(&entropy::FSE_NCOUNT),
        #[cfg(feature = "divide-by")]
        53 => Some(&divide_by::DIVIDE_BY),
        #[cfg(feature = "dispatch")]
        54 => Some(&dispatch::DISPATCH_STRING),
        #[cfg(feature = "concat")]
        55 => Some(&concat::CONCAT_SERIAL),
        #[cfg(feature = "dedup")]
        56 => Some(&dedup::DEDUP_NUM),
        #[cfg(feature = "concat")]
        57 => Some(&concat::CONCAT_NUM),
        #[cfg(feature = "concat")]
        58 => Some(&concat::CONCAT_STRUCT),
        #[cfg(feature = "concat")]
        59 => Some(&concat::CONCAT_STRING),
        #[cfg(feature = "parse-int")]
        60 => Some(&parse_int::PARSE_INT),
        #[cfg(feature = "interleave")]
        61 => Some(&interleave::INTERLEAVE_STRING),
        #[cfg(feature = "lz4")]
        62 => Some(&lz4::LZ4),
        #[cfg(feature = "bitsplit")]
        63 => Some(&bitsplit::BITSPLIT),
        #[cfg(feature = "partition")]
        64 => Some(&partition::PARTITION),
        #[cfg(feature = "mux-lengths")]
        65 => Some(&mux_lengths::MUX_LENGTHS),
        #[cfg(feature = "sparse-num")]
        66 => Some(&sparse_num::SPARSE_NUM),
        #[cfg(feature = "pivco-huffman")]
        67 => Some(&pivco_huffman::PIVCO_HUFFMAN),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A decoder that always fails.
    fn failing(_: &mut Ctx<'_>, _: &[Input<'_>], _: &mut [Stream]) -> Result<(), DecodeError> {
        Err(DecodeError::unsupported(
            crate::decompressor::error::Unsupported::Codec { id: 0 },
        ))
    }

    #[test]
    fn every_registered_codec_carries_its_own_id() {
        for id in 0..128 {
            if let Some(desc) = standard(id) {
                assert_eq!(desc.id, id);
                assert!(!desc.regens.is_empty(), "{id}");
                assert!(desc.min_version >= 3, "{id}");
            }
        }
        let mut state = State::new(Backend::default());
        assert!(Ctx::new(&mut state, &[]).header.is_empty());
        let _ = state.retained_bytes();
        state.release();
    }

    #[test]
    fn signatures_check_counts_and_types() {
        const FIXED: Desc = Desc {
            id: 0,
            min_version: 3,
            regens: &[Type::Numeric, Type::Serial],
            variable_regens: false,
            inputs: &[Type::Serial],
            variable_inputs: 0b0110,
            decode: failing,
        };
        const VARIABLE: Desc = Desc {
            variable_regens: true,
            ..FIXED
        };
        assert!(FIXED.regens_fit(2) && !FIXED.regens_fit(1) && !FIXED.regens_fit(3));
        assert!(VARIABLE.regens_fit(1) && VARIABLE.regens_fit(5) && !VARIABLE.regens_fit(0));
        assert_eq!(VARIABLE.regen_type(0), Type::Numeric);
        assert_eq!(VARIABLE.regen_type(4), Type::Serial);
        assert!(FIXED.accepts(0, Type::Serial) && !FIXED.accepts(0, Type::Numeric));
        assert!(FIXED.accepts(1, Type::Numeric) && FIXED.accepts(3, Type::Struct));
        assert!(!FIXED.accepts(1, Type::String) && !FIXED.accepts(1, Type::Serial));
        let mut state = State::new(Backend::default());
        let mut ctx = Ctx::new(&mut state, &[]);
        assert!((FIXED.decode)(&mut ctx, &[], &mut []).is_err());
    }
}
