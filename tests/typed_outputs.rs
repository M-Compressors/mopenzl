//! Frames with typed and multiple outputs against the C library.
#![cfg(all(feature = "decompression", feature = "conversion"))]

use mopenzl::{DataType, DecodeError, Decompressor, Unsupported};
use openzl_ffi::graph::{self, Graph, Input};

fn data_type(kind: u32) -> DataType {
    match kind {
        0 => DataType::Serial,
        1 => DataType::Struct,
        2 => DataType::Numeric,
        _ => DataType::String,
    }
}

/// Decodes `frame` with both libraries and checks they agree.
fn agree(frame: &[u8]) -> Vec<mopenzl::TypedOutput> {
    let expected = graph::decompress(frame).expect("C decodes its frame");
    let outputs = Decompressor::default()
        .decompress_typed(frame)
        .expect("mopenzl decodes the frame");
    assert_eq!(outputs.len(), expected.len());
    for (ours, theirs) in outputs.iter().zip(&expected) {
        assert_eq!(ours.data_type(), data_type(theirs.kind));
        assert_eq!(ours.content(), theirs.bytes);
        assert_eq!(ours.string_lens(), theirs.lens);
        assert_eq!(ours.len(), theirs.elts);
        if !ours.is_empty() && ours.data_type() != DataType::String {
            assert_eq!(ours.width(), theirs.width);
        }
    }
    outputs
}

#[test]
fn stored_inputs_of_every_type_decode() {
    let numbers: Vec<u8> = (0..300u32).flat_map(u32::to_le_bytes).collect();
    let records: Vec<u8> = (0..90u8).collect();
    let inputs = [
        Input::Serial(b"serial bytes"),
        Input::Struct {
            bytes: &records,
            width: 3,
        },
        Input::Numeric {
            bytes: &numbers,
            width: 4,
        },
        Input::String {
            bytes: b"onetwothree",
            lens: &[3, 3, 0, 5],
        },
    ];
    for version in [21, 24, 27] {
        let frame = graph::compress(
            &inputs,
            &Graph::STORE,
            &[(graph::cparam::FORMAT_VERSION, version)],
        )
        .unwrap();
        let outputs = agree(&frame);
        assert_eq!(outputs[2].len(), 300);
        assert_eq!(outputs[3].string_lens(), [3, 3, 0, 5]);
        // The byte-oriented API reads one serial output only.
        assert_eq!(
            Decompressor::default().decompress(&frame),
            Err(DecodeError::Unsupported {
                feature: Unsupported::MultipleOutputs { outputs: 4 }
            })
        );
    }
}

#[test]
fn a_single_numeric_output_is_typed() {
    let numbers: Vec<u8> = (0..1000u16).flat_map(u16::to_le_bytes).collect();
    let input = [Input::Numeric {
        bytes: &numbers,
        width: 2,
    }];
    let frame = graph::compress(&input, &Graph::STORE, &[]).unwrap();
    assert_eq!(agree(&frame)[0].width(), 2);
    assert_eq!(
        Decompressor::default().decompress(&frame),
        Err(DecodeError::Unsupported {
            feature: Unsupported::TypedOutput
        })
    );
    let mut trailing = frame.clone();
    trailing.push(0);
    assert!(matches!(
        Decompressor::default().decompress_typed(&trailing),
        Err(DecodeError::TrailingData { .. })
    ));
    let mut damaged = frame;
    let last = damaged.len() - 3;
    damaged[last] ^= 1;
    assert!(Decompressor::default().decompress_typed(&damaged).is_err());
}

#[test]
fn many_outputs_decode() {
    let contents: Vec<Vec<u8>> = (0..20u8).map(|i| vec![i; usize::from(i) * 7]).collect();
    let inputs: Vec<Input<'_>> = contents.iter().map(|bytes| Input::Serial(bytes)).collect();
    let frame = graph::compress(&inputs, &Graph::STORE, &[]).unwrap();
    let outputs = agree(&frame);
    assert_eq!(outputs.len(), 20);
    assert_eq!(outputs[19].clone().into_content(), contents[19]);
}
