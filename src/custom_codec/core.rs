//! Validation and adapters between application callbacks and private engines.

use super::{CodecError, CustomCodec, Signature};
use crate::DataType;
use alloc::sync::Arc;

pub(crate) fn validate(signature: Signature) -> Result<(), CodecError> {
    if signature.inputs.is_empty()
        || signature.inputs.len() > 2048
        || signature.outputs.len() + signature.variable_outputs.len() > 100_000
        || signature.outputs.is_empty() && signature.variable_outputs.is_empty()
    {
        return Err(CodecError::Invalid("invalid custom codec port count"));
    }
    for (i, ty) in signature.variable_outputs.iter().enumerate() {
        if signature.variable_outputs[..i].contains(ty) {
            return Err(CodecError::Invalid(
                "variable output types must be distinct",
            ));
        }
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Registered {
    pub(crate) id: u32,
    pub(crate) signature: Signature,
    pub(crate) codec: Arc<dyn CustomCodec>,
}

impl ::core::fmt::Debug for Registered {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        f.debug_struct("CustomCodec")
            .field("id", &self.id)
            .field("signature", &self.signature)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "compression")]
mod encoding {
    use super::super::{CodecInput, EncoderContext};
    use super::*;
    use crate::compressor::core::{data::Stream, error::NodeError, exec::NodeCx};
    use alloc::vec::Vec;

    pub(crate) trait EncoderApi {
        fn level(&self) -> i32;
        fn format_version(&self) -> u32;
        fn local_int(&self, id: i32) -> Option<i32>;
        fn local_bytes(&self, id: i32) -> Option<&[u8]>;
        fn emit(
            &mut self,
            outcome: usize,
            width: usize,
            bytes: Vec<u8>,
        ) -> Result<usize, CodecError>;
        fn emit_strings(
            &mut self,
            outcome: usize,
            bytes: Vec<u8>,
            lengths: Vec<u32>,
        ) -> Result<usize, CodecError>;
        fn send_header(&mut self, header: &[u8]) -> Result<(), CodecError>;
    }

    fn invalid(_: NodeError) -> CodecError {
        CodecError::Invalid("invalid custom encoder output or header")
    }

    impl EncoderApi for NodeCx<'_, '_> {
        fn level(&self) -> i32 {
            NodeCx::level(self)
        }
        fn format_version(&self) -> u32 {
            NodeCx::format_version(self)
        }
        fn local_int(&self, id: i32) -> Option<i32> {
            self.int(id)
        }
        fn local_bytes(&self, id: i32) -> Option<&[u8]> {
            self.copy_param(id)
        }
        fn emit(
            &mut self,
            outcome: usize,
            width: usize,
            bytes: Vec<u8>,
        ) -> Result<usize, CodecError> {
            if width == 0
                || !bytes.len().is_multiple_of(width)
                || self.spec().output_type(outcome) == Some(DataType::String)
            {
                return Err(CodecError::Invalid(
                    "invalid custom encoder fixed-width stream",
                ));
            }
            NodeCx::emit(self, outcome, width, bytes).map_err(invalid)
        }
        fn emit_strings(
            &mut self,
            outcome: usize,
            bytes: Vec<u8>,
            lengths: Vec<u32>,
        ) -> Result<usize, CodecError> {
            if self.spec().output_type(outcome) != Some(DataType::String) {
                return Err(CodecError::Invalid("custom output is not a string stream"));
            }
            NodeCx::emit_strings(self, outcome, bytes, lengths).map_err(invalid)
        }
        fn send_header(&mut self, header: &[u8]) -> Result<(), CodecError> {
            NodeCx::send_header(self, header).map_err(invalid)
        }
    }

    pub(crate) fn encode(
        codec: &Registered,
        cx: &mut NodeCx<'_, '_>,
        inputs: &[Stream<'_>],
    ) -> Result<(), NodeError> {
        let inputs: alloc::boxed::Box<[_]> = inputs
            .iter()
            .map(|input| CodecInput {
                data_type: input.ty(),
                width: input.width(),
                bytes: input.bytes(),
                string_lengths: input.lens(),
            })
            .collect();
        codec
            .codec
            .encode(&mut EncoderContext { api: cx }, &inputs)
            .map_err(NodeError::Custom)
    }
}

#[cfg(feature = "compression")]
pub(crate) use encoding::{EncoderApi, encode};

#[cfg(feature = "decompression")]
mod decoding {
    use super::super::{CodecInput, DecoderContext};
    use super::*;
    use crate::DecodeError;
    use crate::decompressor::core::{
        codecs::{Ctx, Desc},
        data::{Input, Stream},
    };

    pub(crate) fn descriptor(codec: &Registered) -> Desc {
        Desc {
            id: codec.id,
            min_version: 21,
            regens: codec.signature.inputs,
            variable_regens: codec.signature.variable_input,
            inputs: codec.signature.outputs,
            variable_inputs: codec.signature.variable_outputs.iter().fold(0, |mask, ty| {
                mask | match ty {
                    DataType::Serial => 1,
                    DataType::Struct => 2,
                    DataType::Numeric => 4,
                    DataType::String => 8,
                }
            }),
            // Custom invocations are dispatched through their registered callback.
            decode: unreachable_standard,
        }
    }

    fn unreachable_standard(
        _: &mut Ctx<'_>,
        _: &[Input<'_>],
        _: &mut [Stream],
    ) -> Result<(), DecodeError> {
        Err(DecodeError::invalid(crate::InvalidDataKind::Graph))
    }

    #[cfg(test)]
    #[test]
    fn a_custom_descriptor_rejects_the_standard_execution_entry_point() {
        let mut state = crate::decompressor::core::codecs::State::new(crate::Backend::default());
        let mut cx = Ctx::new(&mut state, &[]);
        assert_eq!(
            unreachable_standard(&mut cx, &[], &mut []),
            Err(DecodeError::invalid(crate::InvalidDataKind::Graph))
        );
    }

    fn slot<'a>(
        cx: &'a mut DecoderContext<'_>,
        index: usize,
    ) -> Result<&'a mut Stream, CodecError> {
        if cx.committed.get(index) != Some(&false) {
            return Err(CodecError::Invalid(
                "invalid or duplicate custom decoder output",
            ));
        }
        Ok(&mut cx.streams[index])
    }

    pub(crate) fn emit(
        cx: &mut DecoderContext<'_>,
        index: usize,
        width: usize,
        bytes: &[u8],
    ) -> Result<(), CodecError> {
        if width == 0 || !bytes.len().is_multiple_of(width) {
            return Err(CodecError::Invalid("partial custom decoder element"));
        }
        slot(cx, index)?
            .fixed(width, bytes.len() / width)
            .map_err(|_| CodecError::Invalid("invalid custom decoder width or budget"))?
            .extend_from_slice(bytes);
        cx.committed[index] = true;
        Ok(())
    }

    pub(crate) fn emit_strings(
        cx: &mut DecoderContext<'_>,
        index: usize,
        bytes: &[u8],
        lengths: &[u32],
    ) -> Result<(), CodecError> {
        if lengths.iter().map(|&n| u64::from(n)).sum::<u64>() != bytes.len() as u64 {
            return Err(CodecError::Invalid(
                "custom string lengths do not match content",
            ));
        }
        let (out, lens) = slot(cx, index)?
            .strings(lengths.len(), bytes.len())
            .map_err(|_| CodecError::Invalid("invalid custom decoder string type or budget"))?;
        out.extend_from_slice(bytes);
        lens.extend_from_slice(lengths);
        cx.committed[index] = true;
        Ok(())
    }

    pub(crate) fn decode(
        codec: &Registered,
        header: &[u8],
        inputs: &[Input<'_>],
        streams: &mut [Stream],
        budget: usize,
    ) -> Result<(), DecodeError> {
        let inputs: alloc::boxed::Box<[_]> = inputs
            .iter()
            .map(|input| CodecInput {
                data_type: input.ty,
                width: input.width,
                bytes: input.bytes,
                string_lengths: input.lens,
            })
            .collect();
        let mut cx = DecoderContext {
            header,
            committed: alloc::vec![false; streams.len()].into_boxed_slice(),
            streams,
            budget,
        };
        codec
            .codec
            .decode(&mut cx, &inputs)
            .map_err(|source| DecodeError::CustomCodec {
                id: codec.id,
                source,
            })?;
        if cx.committed.contains(&false) {
            return Err(DecodeError::CustomCodec {
                id: codec.id,
                source: CodecError::Invalid("custom decoder left an output uncommitted"),
            });
        }
        Ok(())
    }
}

#[cfg(feature = "decompression")]
pub(crate) use decoding::{decode, descriptor, emit, emit_strings};
