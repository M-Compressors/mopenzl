//! Custom codec registration, wire compatibility and callback validation.
#![cfg(all(
    feature = "custom-codec",
    feature = "compression",
    feature = "decompression"
))]

use mopenzl::custom_codec::{
    CodecError, CodecInput, CustomCodec, DecoderContext, EncoderContext, Signature,
};
use mopenzl::graph::{GraphBuilder, LocalParams, StandardGraph};
use mopenzl::{
    Compressor, DataType, DecodeError, Decompressor, EncoderConfig, FormatVersion, RetentionPolicy,
    Unsupported,
};

#[derive(Clone, Copy)]
struct Xor;

impl CustomCodec for Xor {
    fn signature(&self) -> Signature {
        Signature {
            inputs: &[DataType::Serial],
            variable_input: false,
            outputs: &[DataType::Serial],
            variable_outputs: &[],
        }
    }
    fn encode(
        &self,
        cx: &mut EncoderContext<'_>,
        inputs: &[CodecInput<'_>],
    ) -> Result<(), CodecError> {
        assert!(cx.level() != 0);
        assert!((21..=27).contains(&cx.format_version()));
        assert!(format!("{cx:?}").contains("EncoderContext"));
        let key = cx.local_int(0).unwrap_or(0xa5) as u8;
        assert_eq!(cx.local_bytes(1), None);
        cx.send_header(&[key])?;
        cx.emit(0, 1, inputs[0].bytes.iter().map(|b| b ^ key).collect())?;
        Ok(())
    }
    fn decode(
        &self,
        cx: &mut DecoderContext<'_>,
        inputs: &[CodecInput<'_>],
    ) -> Result<(), CodecError> {
        assert_eq!(cx.output_count(), 1);
        assert!(cx.output_budget() >= inputs[0].bytes.len());
        assert!(format!("{cx:?}").contains("DecoderContext"));
        let [key] = cx.header() else {
            return Err(CodecError::Invalid("invalid XOR header"));
        };
        let bytes: Vec<_> = inputs[0].bytes.iter().map(|b| b ^ key).collect();
        cx.emit(0, 1, &bytes)
    }
}

fn config() -> EncoderConfig {
    EncoderConfig::default()
        .with_min_stream_size(1)
        .with_store_on_expansion(false)
}

fn encoder(id: u32, codec: impl CustomCodec) -> Compressor {
    let mut builder = GraphBuilder::new();
    let node = builder.custom_codec(id, codec).unwrap();
    let graph = builder
        .static_graph(node, &[StandardGraph::Store.into()])
        .unwrap();
    Compressor::builder(config())
        .with_graph(builder.build(graph).unwrap())
        .build()
        .unwrap()
}

fn decoder(id: u32, codec: impl CustomCodec) -> Decompressor {
    Decompressor::builder(Default::default())
        .with_custom_codec(id, codec)
        .unwrap()
        .build()
}

#[test]
fn custom_frames_are_byte_identical_to_c_and_decode_both_ways() {
    for id in [0, 22, 127, 128, 1000, u32::MAX] {
        for version in 21..=27 {
            let mut builder = GraphBuilder::new();
            let node = builder.custom_codec(id, Xor).unwrap();
            let graph = builder
                .static_graph(node, &[StandardGraph::Store.into()])
                .unwrap();
            let mut encoder = Compressor::builder(
                config().with_format_version(FormatVersion::try_from(version).unwrap()),
            )
            .with_graph(builder.build(graph).unwrap())
            .build()
            .unwrap();
            for input in [
                b"".as_slice(),
                b"x",
                b"custom payload, with a private header",
            ] {
                let frame = encoder.compress(input).unwrap();
                let reference = openzl_ffi::custom_codec::transform(
                    false,
                    id,
                    version as i32,
                    input,
                    input.len() + 1024,
                )
                .unwrap();
                assert_eq!(frame, reference, "id {id}, version {version}");
                assert_eq!(decoder(id, Xor).decompress(&reference).unwrap(), input);
                assert_eq!(
                    openzl_ffi::custom_codec::transform(
                        true,
                        id,
                        version as i32,
                        &frame,
                        input.len()
                    )
                    .unwrap(),
                    input
                );
                if input.is_empty() {
                    continue;
                }
                assert_eq!(
                    Decompressor::default().decompress(&frame),
                    Err(DecodeError::Unsupported {
                        feature: Unsupported::CustomCodec { id }
                    })
                );
            }
        }
    }
}

#[test]
fn parameterized_nodes_keep_the_custom_callback_and_signature() {
    let mut builder = GraphBuilder::new();
    let node = builder.custom_codec(1000, Xor).unwrap();
    let node = builder
        .parameterize_node(node, LocalParams::default().with_int(0, 42))
        .unwrap();
    let node = builder
        .parameterize_node(node, LocalParams::default().with_int(0, 7))
        .unwrap();
    let graph = builder
        .static_graph(node, &[StandardGraph::Store.into()])
        .unwrap();
    let graph = builder.build(graph).unwrap();
    let mut encoder = Compressor::builder(config())
        .with_graph(graph.clone())
        .build()
        .unwrap();
    let frame = encoder.compress(b"parameters survive cloning").unwrap();
    assert_eq!(
        decoder(1000, Xor).decompress(&frame).unwrap(),
        b"parameters survive cloning"
    );
}

#[test]
fn cloned_decoder_builders_and_release_all_keep_registration() {
    let builder = Decompressor::builder(Default::default())
        .with_custom_codec(1000, Xor)
        .unwrap()
        .with_retention(RetentionPolicy::ReleaseAll);
    assert!(format!("{builder:?}").contains("CustomCodec"));
    let frame = encoder(1000, Xor).compress(b"reuse registrations").unwrap();
    for mut decoder in [builder.clone().build(), builder.build()] {
        for _ in 0..2 {
            assert_eq!(decoder.decompress(&frame).unwrap(), b"reuse registrations");
            assert_eq!(decoder.retained_bytes(), 0);
        }
    }
}

#[test]
fn duplicate_decoder_ids_are_rejected_in_the_custom_namespace() {
    let error = Decompressor::builder(Default::default())
        .with_custom_codec(22, Xor)
        .unwrap()
        .with_custom_codec(22, Xor)
        .unwrap_err();
    assert_eq!(error, CodecError::DuplicateId(22));
    assert!(error.to_string().contains("22"));
    let mut builder = GraphBuilder::new();
    builder.custom_codec(22, Xor).unwrap();
    builder.custom_codec(22, Xor).unwrap();
    assert!(builder.custom_codec(22, Variable).is_err());
}

#[test]
fn fragmented_sessions_use_the_registered_codec() {
    use mopenzl::{DecodeOperation, DecoderStatus, EncoderStatus, InputSize, Operation};
    let input = b"fragmented custom codec input".repeat(40);
    let mut encoder = encoder(22, Xor);
    let expected = encoder.compress(&input).unwrap();
    let mut session = encoder.start(InputSize::Exact(input.len() as u64).into());
    let mut pos = 0;
    let mut output = [0; 11];
    let mut frame = Vec::new();
    loop {
        let end = (pos + 7).min(input.len());
        let progress = session
            .process(
                &input[pos..end],
                &mut output,
                if end == input.len() {
                    Operation::End
                } else {
                    Operation::Continue
                },
            )
            .unwrap();
        pos += progress.consumed;
        frame.extend_from_slice(&output[..progress.produced]);
        if progress.status == EncoderStatus::Finished {
            break;
        }
    }
    assert_eq!(frame, expected);
    let mut decoder = decoder(22, Xor);
    let mut session = decoder.start();
    pos = 0;
    let mut decoded = Vec::new();
    loop {
        let end = (pos + 3).min(frame.len());
        let progress = session
            .process(
                &frame[pos..end],
                &mut output,
                if end == frame.len() {
                    DecodeOperation::Finish
                } else {
                    DecodeOperation::Process
                },
            )
            .unwrap();
        pos += progress.consumed;
        decoded.extend_from_slice(&output[..progress.produced]);
        if progress.status == DecoderStatus::Finished {
            break;
        }
    }
    assert_eq!(decoded, input);
}

#[derive(Clone, Copy)]
struct InvalidSignature(Signature);
impl CustomCodec for InvalidSignature {
    fn signature(&self) -> Signature {
        self.0
    }
    fn encode(&self, _: &mut EncoderContext<'_>, _: &[CodecInput<'_>]) -> Result<(), CodecError> {
        Ok(())
    }
    fn decode(&self, _: &mut DecoderContext<'_>, _: &[CodecInput<'_>]) -> Result<(), CodecError> {
        Ok(())
    }
}

#[test]
fn malformed_signatures_fail_before_callbacks_run() {
    let valid = Xor.signature();
    for signature in [
        Signature {
            inputs: &[],
            ..valid
        },
        Signature {
            outputs: &[],
            ..valid
        },
        Signature {
            variable_outputs: &[DataType::Serial, DataType::Serial],
            ..valid
        },
    ] {
        assert!(
            GraphBuilder::new()
                .custom_codec(0, InvalidSignature(signature))
                .is_err()
        );
        assert!(
            Decompressor::builder(Default::default())
                .with_custom_codec(0, InvalidSignature(signature))
                .is_err()
        );
    }
}

#[derive(Clone, Copy)]
struct Failure(u8);
impl CustomCodec for Failure {
    fn signature(&self) -> Signature {
        Xor.signature()
    }
    fn encode(&self, cx: &mut EncoderContext<'_>, _: &[CodecInput<'_>]) -> Result<(), CodecError> {
        match self.0 {
            0 => Err(CodecError::Invalid("callback failed")),
            1 => cx.emit(0, 0, vec![0]).map(|_| ()),
            2 => cx.emit(1, 1, vec![]).map(|_| ()),
            3 => {
                cx.emit(0, 1, vec![])?;
                cx.emit(0, 1, vec![]).map(|_| ())
            }
            4 => {
                cx.send_header(&[])?;
                cx.send_header(&[])
            }
            5 => cx.emit_strings(0, vec![], vec![]).map(|_| ()),
            6 => Ok(()),
            _ => cx.emit(0, 2, vec![1]).map(|_| ()),
        }
    }
    fn decode(&self, cx: &mut DecoderContext<'_>, _: &[CodecInput<'_>]) -> Result<(), CodecError> {
        match self.0 {
            0 => Err(CodecError::Invalid("callback failed")),
            1 => cx.emit(0, 0, &[]),
            2 => cx.emit(1, 1, &[]),
            3 => {
                cx.emit(0, 1, &[])?;
                cx.emit(0, 1, &[])
            }
            4 => cx.emit(0, 2, &[0, 0]),
            5 => cx.emit_strings(0, &[], &[]),
            6 => Ok(()),
            7 => cx.emit_strings(0, &[0], &[2]),
            _ => cx.emit(0, 1, &vec![0; cx.output_budget() + 1]),
        }
    }
}

#[test]
fn encoder_callback_failures_and_invalid_outputs_return_errors() {
    use std::error::Error;
    for mode in 0..8 {
        let error = encoder(0, Failure(mode))
            .compress(b"invalid callback")
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains(if mode == 6 { "singleton" } else { "custom" })
        );
        {
            assert!(
                error
                    .source()
                    .unwrap()
                    .downcast_ref::<CodecError>()
                    .is_some()
            );
        }
    }
}

#[test]
fn decoder_callback_failures_preserve_the_source_and_rollback_appends() {
    use std::error::Error;
    let frame = encoder(0, Xor)
        .compress(b"invalid decoder callback")
        .unwrap();
    for mode in 0..9 {
        let mut decoder = decoder(0, Failure(mode));
        let mut destination = b"keep".to_vec();
        let error = decoder
            .decompress_into(&frame, &mut destination)
            .unwrap_err();
        assert!(matches!(error, DecodeError::CustomCodec { id: 0, .. }));
        assert!(error.source().is_some());
        assert_eq!(destination, b"keep");
    }
}

#[derive(Clone, Copy)]
struct Typed(DataType);
impl CustomCodec for Typed {
    fn signature(&self) -> Signature {
        Signature {
            inputs: match self.0 {
                DataType::Numeric => &[DataType::Numeric],
                DataType::Struct => &[DataType::Struct],
                _ => &[DataType::String],
            },
            ..Xor.signature()
        }
    }
    fn encode(
        &self,
        cx: &mut EncoderContext<'_>,
        inputs: &[CodecInput<'_>],
    ) -> Result<(), CodecError> {
        let input = inputs[0];
        let mut header = (input.width as u32).to_le_bytes().to_vec();
        header.extend(input.string_lengths.iter().flat_map(|n| n.to_le_bytes()));
        cx.send_header(&header)?;
        cx.emit(0, 1, input.bytes.to_vec()).map(|_| ())
    }
    fn decode(
        &self,
        cx: &mut DecoderContext<'_>,
        inputs: &[CodecInput<'_>],
    ) -> Result<(), CodecError> {
        let header = cx.header();
        let width = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        let lengths: Vec<_> = header[4..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|n| u32::from_le_bytes(*n))
            .collect();
        if self.0 == DataType::String {
            cx.emit_strings(0, inputs[0].bytes, &lengths)
        } else {
            cx.emit(0, width, inputs[0].bytes)
        }
    }
}

#[test]
fn typed_regenerated_streams_keep_widths_and_string_lengths() {
    use mopenzl::input::{Input, Numbers, Records, Strings};
    let bytes = b"abcdefghijkl";
    for (ty, input) in [
        (
            DataType::Numeric,
            Input::from(Numbers::try_from((&bytes[..], 4)).unwrap()),
        ),
        (
            DataType::Struct,
            Input::from(Records::try_from((&bytes[..], 3)).unwrap()),
        ),
        (
            DataType::String,
            Input::from(Strings::try_from((&bytes[..], &[0, 3, 4, 5][..])).unwrap()),
        ),
    ] {
        let frame = encoder(300, Typed(ty)).compress_inputs(&[input]).unwrap();
        let output = decoder(300, Typed(ty)).decompress_typed(&frame).unwrap();
        assert_eq!(output[0].data_type(), ty);
        assert_eq!(output[0].content(), bytes);
        if ty == DataType::String {
            assert_eq!(output[0].string_lens(), [0, 3, 4, 5]);
        }
    }
}

#[derive(Clone, Copy)]
struct Variable;
impl CustomCodec for Variable {
    fn signature(&self) -> Signature {
        Signature {
            inputs: &[DataType::Serial],
            variable_input: true,
            outputs: &[],
            variable_outputs: &[DataType::Serial],
        }
    }
    fn encode(
        &self,
        cx: &mut EncoderContext<'_>,
        inputs: &[CodecInput<'_>],
    ) -> Result<(), CodecError> {
        for input in inputs {
            cx.emit(0, 1, input.bytes.to_vec())?;
        }
        Ok(())
    }
    fn decode(
        &self,
        cx: &mut DecoderContext<'_>,
        inputs: &[CodecInput<'_>],
    ) -> Result<(), CodecError> {
        assert_eq!(cx.output_count(), inputs.len());
        for (index, input) in inputs.iter().enumerate() {
            cx.emit(index, 1, input.bytes)?;
        }
        Ok(())
    }
}

#[test]
fn variable_inputs_and_outputs_preserve_port_order() {
    use mopenzl::input::Input;
    let frame = encoder(7, Variable)
        .compress_inputs(&[
            Input::from(&b"first"[..]),
            Input::from(&b"second"[..]),
            Input::from(&b"third"[..]),
        ])
        .unwrap();
    let outputs = decoder(7, Variable).decompress_typed(&frame).unwrap();
    assert_eq!(
        outputs.iter().map(|out| out.content()).collect::<Vec<_>>(),
        [b"first".as_slice(), b"second", b"third"]
    );
}

#[derive(Clone, Copy)]
struct StringOutput;
impl CustomCodec for StringOutput {
    fn signature(&self) -> Signature {
        Signature {
            outputs: &[DataType::String],
            ..Xor.signature()
        }
    }
    fn encode(
        &self,
        cx: &mut EncoderContext<'_>,
        inputs: &[CodecInput<'_>],
    ) -> Result<(), CodecError> {
        assert_eq!(cx.local_bytes(9), Some(b"bytes".as_slice()));
        cx.emit_strings(
            0,
            inputs[0].bytes.to_vec(),
            vec![0, inputs[0].bytes.len() as u32],
        )
        .map(|_| ())
    }
    fn decode(
        &self,
        cx: &mut DecoderContext<'_>,
        inputs: &[CodecInput<'_>],
    ) -> Result<(), CodecError> {
        assert_eq!(inputs[0].data_type, DataType::String);
        assert_eq!(inputs[0].string_lengths[0], 0);
        cx.emit(0, 1, inputs[0].bytes)
    }
}

#[test]
fn string_outputs_feed_standard_successors_and_custom_decoders() {
    let mut builder = GraphBuilder::new();
    let node = builder.custom_codec(6, StringOutput).unwrap();
    let node = builder
        .parameterize_node(
            node,
            LocalParams::default().with_bytes(9, b"bytes".to_vec()),
        )
        .unwrap();
    let graph = builder
        .static_graph(node, &[StandardGraph::Store.into()])
        .unwrap();
    let mut encoder = Compressor::builder(config())
        .with_graph(builder.build(graph).unwrap())
        .build()
        .unwrap();
    // Storing strings requires the standard conversion codec.
    #[cfg(feature = "conversion")]
    {
        let frame = encoder.compress(b"strings from a custom node").unwrap();
        assert_eq!(
            decoder(6, StringOutput).decompress(&frame).unwrap(),
            b"strings from a custom node"
        );
    }
    #[cfg(not(feature = "conversion"))]
    assert!(encoder.compress(b"strings from a custom node").is_err());
}

#[cfg(not(feature = "no_std"))]
#[test]
fn callback_errors_map_to_invalid_data_in_io_adapters() {
    let error = DecodeError::CustomCodec {
        id: 7,
        source: CodecError::Invalid("bad stream"),
    };
    let error = std::io::Error::from(error);
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[cfg(feature = "conversion")]
#[derive(Clone, Copy)]
struct Mixed;

#[cfg(feature = "conversion")]
impl CustomCodec for Mixed {
    fn signature(&self) -> Signature {
        Signature {
            outputs: &[DataType::Serial, DataType::Numeric],
            variable_outputs: &[DataType::Struct, DataType::String],
            ..Xor.signature()
        }
    }
    fn encode(
        &self,
        cx: &mut EncoderContext<'_>,
        inputs: &[CodecInput<'_>],
    ) -> Result<(), CodecError> {
        cx.emit(1, 4, 42u32.to_le_bytes().to_vec())?;
        cx.emit_strings(3, b"tag".to_vec(), vec![0, 3])?;
        cx.emit(0, 1, inputs[0].bytes[..3].to_vec())?;
        cx.emit(2, 3, inputs[0].bytes[3..].to_vec())?;
        Ok(())
    }
    fn decode(
        &self,
        cx: &mut DecoderContext<'_>,
        inputs: &[CodecInput<'_>],
    ) -> Result<(), CodecError> {
        assert_eq!(
            inputs
                .iter()
                .map(|input| input.data_type)
                .collect::<Vec<_>>(),
            [
                DataType::Serial,
                DataType::Numeric,
                DataType::String,
                DataType::Struct
            ]
        );
        assert_eq!(inputs[1].width, 4);
        assert_eq!(inputs[1].bytes, 42u32.to_le_bytes());
        assert_eq!(inputs[2].string_lengths, [0, 3]);
        assert_eq!(inputs[3].width, 3);
        cx.emit(0, 1, &[inputs[0].bytes, inputs[3].bytes].concat())
    }
}

#[cfg(feature = "conversion")]
#[test]
fn singleton_port_order_and_variable_creation_order_are_independent() {
    let mut builder = GraphBuilder::new();
    let node = builder.custom_codec(128, Mixed).unwrap();
    let graph = builder
        .static_graph(node, &[StandardGraph::Store.into(); 4])
        .unwrap();
    let mut encoder = Compressor::builder(config())
        .with_graph(builder.build(graph).unwrap())
        .build()
        .unwrap();
    let frame = encoder.compress(b"abcdefghi").unwrap();
    assert_eq!(
        decoder(128, Mixed).decompress(&frame).unwrap(),
        b"abcdefghi"
    );
}
