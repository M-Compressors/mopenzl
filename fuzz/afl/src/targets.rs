//! Target bodies, shared by the AFL binaries, the libFuzzer adapters and the
//! regression replay.

use mopenzl::dict::{BundleStore, DictBundle};
use mopenzl::graph::{
    ClusteringConfig, CompressorGraph, Edge, FunctionGraph, GraphBuilder, GraphContext, GraphError,
    LocalParams, MlSelectorConfig, NodeParams, StandardGraph, StandardNode, TypeSet,
};
use mopenzl::input::{Input, Numbers, Records, Strings};
use mopenzl::{
    Compressor, DataType, DecodeError, DecodeLimits, DecodeOperation, DecoderConfig, DecoderStatus,
    Decompressor, EncoderConfig, EncoderStatus, FormatVersion, InputSize, Level, Operation,
    TypedOutput,
};

use crate::{MAX_OUTPUT, Schedule, decode_config, take};

/// A decoder that refuses to produce more than [`MAX_OUTPUT`] bytes.
fn bounded_decoder() -> Decompressor {
    let limits = DecodeLimits::default().with_max_output_bytes(Some(MAX_OUTPUT as u64));
    Decompressor::new(DecoderConfig::default().with_limits(limits))
}

/// Whether a decoder error says the input is outside what this port decodes
/// or what the fuzzer allows, rather than that the input is wrong.
fn out_of_scope(error: &DecodeError) -> bool {
    matches!(
        error,
        DecodeError::Unsupported { .. }
            | DecodeError::OutputLimitExceeded { .. }
            | DecodeError::InputLimitExceeded { .. }
    )
}

/// The C reference's verdict on `data`, or `None` when its declared content
/// exceeds [`MAX_OUTPUT`] and the reference is not asked.
fn reference_decode(data: &[u8]) -> Option<Result<Vec<u8>, openzl_ffi::Error>> {
    match openzl_ffi::decompressed_size(data) {
        Ok(size) if size > MAX_OUTPUT => None,
        Ok(_) => Some(openzl_ffi::decompress(data)),
        Err(error) => Some(Err(error)),
    }
}

/// Decodes `src` through a session fed pieces of random size into output
/// windows of random size, checking the session's progress contract.
fn decode_in_pieces(src: &[u8], schedule: &mut Schedule) -> Result<Vec<u8>, DecodeError> {
    let mut decoder = bounded_decoder();
    let mut session = decoder.start();
    let room = schedule.length(1 << 16);
    let mut buffer = vec![0u8; room];
    let mut content = Vec::new();
    let mut pos = 0usize;
    let mut idle = 0u32;
    loop {
        let end = pos
            .saturating_add(schedule.length(src.len()))
            .min(src.len());
        let operation = if end == src.len() {
            DecodeOperation::Finish
        } else {
            DecodeOperation::Process
        };
        let offered = end - pos;
        let progress = session
            .process(&src[pos..end], &mut buffer, operation)
            .map_err(|failure| {
                assert!(
                    failure.consumed <= offered,
                    "{failure:?} over-reports input"
                );
                assert!(failure.produced <= room, "{failure:?} over-reports output");
                failure.into_error()
            })?;
        assert!(
            progress.consumed <= offered,
            "{progress:?} over-reports input"
        );
        assert!(
            progress.produced <= room,
            "{progress:?} over-reports output"
        );
        if progress.status == DecoderStatus::NeedsInput {
            assert_eq!(
                progress.consumed, offered,
                "NeedsInput with offered input left: {progress:?}"
            );
            assert_eq!(
                operation,
                DecodeOperation::Process,
                "NeedsInput after the input ended"
            );
        }
        content.extend_from_slice(&buffer[..progress.produced]);
        pos += progress.consumed;
        if progress.status == DecoderStatus::Finished {
            assert!(session.is_finished());
            // A single-frame session leaves trailing bytes unconsumed where
            // one-shot decoding rejects them.
            if pos != src.len() {
                return Err(DecodeError::TrailingData { offset: pos as u64 });
            }
            return Ok(content);
        }
        if progress.consumed == 0 && progress.produced == 0 {
            idle += 1;
            assert!(idle < 4, "the session stalls at input byte {pos}");
        } else {
            idle = 0;
        }
    }
}

/// Arbitrary bytes never panic either decoder path.
///
/// One-shot decoding, a session fed random pieces and `decompress_to_slice`
/// agree. Where mopenzl decodes a frame the reference decodes it to the same
/// content; where the reference decodes one, mopenzl does too, unless the
/// frame needs a codec or feature this port does not decode yet (or the
/// output cap). The reference is asked only when mopenzl's verdict is in
/// scope, which keeps the fuzzer on the code paths both sides implement.
pub fn decompress(data: &[u8]) {
    let mut decoder = bounded_decoder();
    let ours = decoder.decompress(data);

    let mut schedule = Schedule::from_bytes(data);
    let pieces = decode_in_pieces(data, &mut schedule);
    match (&ours, &pieces) {
        (Ok(whole), Ok(pieces)) => assert_eq!(whole, pieces, "session differs from one-shot"),
        (Err(_), Err(_)) => {}
        (whole, pieces) => panic!("one-shot {whole:?} but session {pieces:?}"),
    }

    if let Ok(content) = &ours {
        let mut exact = vec![0u8; content.len()];
        let written = decoder
            .decompress_to_slice(data, &mut exact)
            .expect("decodes into a slice of its exact size");
        assert_eq!(&exact[..written], &content[..]);
    }

    if ours.as_ref().is_err_and(out_of_scope) {
        return;
    }
    // A chunk claiming more content than the frame header declares: the
    // reference sizes its stream buffers from that claim before it notices,
    // which under AddressSanitizer or libFuzzer's malloc limit is a fatal
    // multi-gigabyte allocation in C, not a finding in mopenzl. The reference
    // rejects every such frame.
    if matches!(ours, Err(DecodeError::ContentSizeMismatch { .. })) {
        return;
    }
    let Some(theirs) = reference_decode(data) else {
        return;
    };
    match (&ours, &theirs) {
        (Ok(ours), Ok(theirs)) => assert_eq!(ours, theirs, "content differs from the reference"),
        (Err(_), Err(_)) => {}
        (Ok(ours), Err(error)) => panic!(
            "mopenzl decodes {} bytes the reference rejects ({error:?})",
            ours.len()
        ),
        (Err(error), Ok(theirs)) => panic!(
            "the reference decodes {} bytes mopenzl rejects ({error:?})",
            theirs.len()
        ),
    }
}

/// One-shot compression is byte-identical to `ZL_CCtx_compress` with the
/// same parameters, and the frame decodes back to the payload.
pub fn roundtrip(data: &[u8]) {
    let Some(decoded) = decode_config(data) else {
        return;
    };
    let mut encoder = Compressor::new(decoded.config).expect("valid configuration");
    let ours = encoder.compress(decoded.rest).expect("compression");
    let theirs = openzl_ffi::compress(decoded.rest, decoded.params).expect("reference compression");
    assert_eq!(ours, theirs, "{:?}", decoded.config);

    let mut decoder = Decompressor::default();
    assert_eq!(
        decoder.decompress(&ours).expect("decoding our frame"),
        decoded.rest
    );
    let mut schedule = Schedule::from_bytes(data);
    assert_eq!(
        decode_in_pieces(&ours, &mut schedule).expect("decoding our frame in pieces"),
        decoded.rest
    );
}

/// A session fed random pieces, flushes and output windows writes a frame
/// that decodes back to the payload, and, with no flush that can move a
/// chunk boundary, writes exactly the one-shot frame.
///
/// After the configuration header, four bytes: the output window (byte 0,
/// `1 + 97 * n` bytes), flags (byte 1: bit 0 declares the input size, bit 1
/// allows flushes, bit 2 drives an owned session) and the seed of the
/// schedule (bytes 1-3).
pub fn stream_compress(data: &[u8]) {
    let Some(decoded) = decode_config(data) else {
        return;
    };
    let mut rest = decoded.rest;
    let Some(control) = take(&mut rest, 4) else {
        return;
    };
    let input = rest;
    let room = 1 + usize::from(control[0]) * 97;
    let declared = control[1] & 1 != 0;
    let flushes = control[1] & 2 != 0;
    let owned = control[1] & 4 != 0;
    let mut schedule = Schedule::new(u32::from_le_bytes([control[1], control[2], control[3], 0]));

    let mut steps: Vec<(&[u8], Operation)> = Vec::new();
    let mut pos = 0;
    let mut flushed = false;
    while pos < input.len() {
        let len = schedule.length(input.len() - pos);
        let operation = if flushes && schedule.bits().is_multiple_of(5) {
            flushed = true;
            Operation::Flush
        } else {
            Operation::Continue
        };
        steps.push((&input[pos..pos + len], operation));
        pos += len;
    }
    if flushes && schedule.bits().is_multiple_of(2) {
        // A flush on an empty input, without taking any.
        steps.push((&[], Operation::Flush));
        flushed = true;
    }
    steps.push((&[], Operation::End));

    let size = if declared {
        InputSize::Exact(input.len() as u64)
    } else {
        InputSize::Unknown
    };
    let mut encoder = Compressor::new(decoded.config).expect("valid configuration");
    let expected = encoder.compress(input).expect("one-shot compression");
    let mut frame = Vec::new();
    let mut buffer = vec![0u8; room];
    if owned {
        let mut session = encoder.into_session(size.into());
        drive(
            &steps,
            &mut buffer,
            &mut frame,
            |chunk, output, operation| session.process(chunk, output, operation),
        );
        assert!(session.is_finished());
    } else {
        let mut session = encoder.start(size.into());
        drive(
            &steps,
            &mut buffer,
            &mut frame,
            |chunk, output, operation| session.process(chunk, output, operation),
        );
        assert!(session.is_finished());
    }

    // With an unknown size nothing is written before the end, so a flush
    // cannot cut a chunk.
    if !flushed || !declared {
        assert_eq!(frame, expected, "session differs from one-shot");
    }
    let mut decoder = Decompressor::default();
    assert_eq!(
        decoder
            .decompress(&frame)
            .expect("decoding the session's frame"),
        input
    );
}

/// Runs every step of an encoder session to completion, appending what it
/// writes to `frame`.
fn drive(
    steps: &[(&[u8], Operation)],
    buffer: &mut [u8],
    frame: &mut Vec<u8>,
    mut process: impl FnMut(
        &[u8],
        &mut [u8],
        Operation,
    ) -> Result<mopenzl::Progress, mopenzl::EncodeError>,
) {
    for &(mut chunk, operation) in steps {
        loop {
            let offered = chunk.len();
            let progress = process(chunk, buffer, operation).expect("session");
            assert!(
                progress.consumed <= offered,
                "{progress:?} over-reports input"
            );
            assert!(
                progress.produced <= buffer.len(),
                "{progress:?} over-reports output"
            );
            frame.extend_from_slice(&buffer[..progress.produced]);
            chunk = &chunk[progress.consumed..];
            let done = match operation {
                Operation::Continue => {
                    chunk.is_empty() && progress.status != EncoderStatus::NeedsOutput
                }
                Operation::Flush => progress.status != EncoderStatus::NeedsOutput,
                Operation::End => progress.status == EncoderStatus::Finished,
            };
            if done {
                break;
            }
            assert!(
                progress.consumed > 0 || progress.produced > 0,
                "the session stalls: {progress:?} on {operation:?}"
            );
        }
    }
}

/// The C reference's typed outputs for `data`, or `None` when the frame
/// declares more than [`MAX_OUTPUT`] bytes of content in total.
fn reference_typed(
    data: &[u8],
) -> Option<Result<Vec<openzl_ffi::graph::Output>, openzl_ffi::Error>> {
    match openzl_ffi::decompressed_size(data) {
        Ok(size) if size > MAX_OUTPUT => None,
        _ => Some(openzl_ffi::graph::decompress(data)),
    }
}

/// The type code the C shim reports for `data_type`.
const fn c_kind(data_type: DataType) -> u32 {
    match data_type {
        DataType::Serial => 0,
        DataType::Struct => 1,
        DataType::Numeric => 2,
        DataType::String => 3,
    }
}

/// Asserts that `ours` and the C reference decoded the same typed outputs.
fn assert_same_as_reference(ours: &[TypedOutput], theirs: &[openzl_ffi::graph::Output]) {
    assert_eq!(ours.len(), theirs.len(), "output count differs from C");
    for (index, (ours, theirs)) in ours.iter().zip(theirs).enumerate() {
        assert_eq!(c_kind(ours.data_type()), theirs.kind, "output {index} type");
        assert_eq!(ours.content(), &theirs.bytes[..], "output {index} content");
        if ours.data_type() == DataType::String {
            assert_eq!(
                ours.string_lens(),
                &theirs.lens[..],
                "output {index} lengths"
            );
        }
    }
}

/// Arbitrary bytes never panic `decompress_typed` or `frame_info`.
///
/// Both run under input (1 MiB) and output ([`MAX_OUTPUT`]) limits. Where
/// `frame_info` and `decompress_typed` both succeed, the header's output
/// count, types and element counts match the decoded outputs. Where mopenzl
/// decodes a frame, the C reference decodes it to the same typed outputs;
/// where the reference decodes one, mopenzl does too unless it reports a
/// codec this port lacks or a limit.
pub fn decompress_typed(data: &[u8]) {
    let limits = DecodeLimits::default()
        .with_max_input_bytes(Some(1 << 20))
        .with_max_output_bytes(Some(MAX_OUTPUT as u64));
    let mut decoder = Decompressor::new(DecoderConfig::default().with_limits(limits));
    let info = decoder.frame_info(data);
    let ours = decoder.decompress_typed(data);

    if let (Ok(info), Ok(outputs)) = (&info, &ours) {
        let described = info.outputs();
        assert_eq!(described.len(), outputs.len(), "frame_info output count");
        for (index, (described, decoded)) in described.iter().zip(outputs).enumerate() {
            assert_eq!(
                described.data_type(),
                decoded.data_type(),
                "output {index} type"
            );
            if let Some(elements) = described.num_elts() {
                assert_eq!(elements, decoded.len() as u64, "output {index} elements");
            }
        }
    }

    if ours.as_ref().is_err_and(out_of_scope)
        || matches!(ours, Err(DecodeError::ContentSizeMismatch { .. }))
    {
        return;
    }
    let Some(theirs) = reference_typed(data) else {
        return;
    };
    match (&ours, &theirs) {
        (Ok(ours), Ok(theirs)) => assert_same_as_reference(ours, theirs),
        (Err(_), Err(_)) => {}
        (Ok(ours), Err(error)) => panic!(
            "mopenzl decodes {} typed outputs the reference rejects ({error:?})",
            ours.len()
        ),
        (Err(error), Ok(theirs)) => panic!(
            "the reference decodes {} typed outputs mopenzl rejects ({error:?})",
            theirs.len()
        ),
    }
}

/// Graphs `typed_roundtrip` picks from: every standard graph that runs
/// without configuration. A graph that cannot take an input type makes
/// compression fail, which the target accepts.
const TYPED_GRAPHS: [StandardGraph; 24] = [
    StandardGraph::Store,
    StandardGraph::Fse,
    StandardGraph::Huffman,
    StandardGraph::HuffmanHuf0,
    StandardGraph::HuffmanPivco,
    StandardGraph::Entropy,
    StandardGraph::Constant,
    StandardGraph::Zstd,
    StandardGraph::Bitpack,
    StandardGraph::Flatpack,
    StandardGraph::FieldLz,
    StandardGraph::CompressGeneric,
    StandardGraph::SelectGenericLzBackend,
    StandardGraph::SegmentNumeric,
    StandardGraph::SelectNumeric,
    StandardGraph::TryParseInt,
    StandardGraph::Lz4,
    StandardGraph::PartitionBitpack,
    StandardGraph::Lz,
    StandardGraph::SegmentSerial,
    StandardGraph::NumericCompress,
    StandardGraph::StringCompress,
    StandardGraph::StructCompress,
    StandardGraph::CompressSmallLengths,
];

/// Element widths of numeric inputs.
const NUMBER_WIDTHS: [usize; 4] = [1, 2, 4, 8];

/// Record widths of struct inputs.
const RECORD_WIDTHS: [usize; 8] = [1, 2, 3, 4, 8, 12, 16, 24];

/// Total content a `typed_roundtrip` input may carry.
const MAX_TYPED_CONTENT: usize = 64 << 10;

/// One typed input in owned form.
#[derive(Debug)]
enum Owned {
    Serial(Vec<u8>),
    Records(Vec<u8>, usize),
    Numbers(Vec<u8>, usize),
    Strings(Vec<u8>, Vec<u32>),
}

impl Owned {
    fn input(&self) -> Input<'_> {
        match self {
            Self::Serial(bytes) => Input::from(&bytes[..]),
            Self::Records(bytes, width) => Input::from(
                Records::try_from((&bytes[..], *width)).expect("whole records by construction"),
            ),
            Self::Numbers(bytes, width) => Input::from(
                Numbers::try_from((&bytes[..], *width)).expect("whole numbers by construction"),
            ),
            Self::Strings(bytes, lens) => Input::from(
                Strings::try_from((&bytes[..], &lens[..])).expect("lengths sum by construction"),
            ),
        }
    }
}

/// Cuts `bytes` into string lengths that sum to its length, with empty
/// strings among them.
fn split_strings(bytes: Vec<u8>, schedule: &mut Schedule) -> Owned {
    let mut lens = Vec::new();
    let mut left = bytes.len();
    while left > 0 && lens.len() < 4096 {
        let len = if schedule.bits().is_multiple_of(4) {
            0
        } else {
            1 + schedule.bits() as usize % left.min(64)
        };
        lens.push(len as u32);
        left -= len;
    }
    if left > 0 {
        lens.push(left as u32);
    }
    Owned::Strings(bytes, lens)
}

/// Builds the typed inputs a `typed_roundtrip` input describes; each takes a
/// spec byte (type in bits 0-1, width index in bits 2-4) and a little-endian
/// `u16` of content bytes.
fn typed_inputs(mut data: &[u8], count: usize, schedule: &mut Schedule) -> Vec<Owned> {
    let mut inputs = Vec::new();
    let mut budget = MAX_TYPED_CONTENT;
    for _ in 0..count {
        let Some(head) = take(&mut data, 3) else {
            break;
        };
        let spec = head[0];
        let wanted = usize::from(u16::from_le_bytes([head[1], head[2]]));
        let size = wanted.min(data.len()).min(budget);
        let mut bytes = take(&mut data, size).unwrap_or_default().to_vec();
        budget -= bytes.len();
        let index = usize::from(spec >> 2);
        inputs.push(match spec & 3 {
            0 => Owned::Serial(bytes),
            1 => {
                let width = RECORD_WIDTHS[index % RECORD_WIDTHS.len()];
                bytes.truncate(bytes.len() / width * width);
                Owned::Records(bytes, width)
            }
            2 => {
                let width = NUMBER_WIDTHS[index % NUMBER_WIDTHS.len()];
                bytes.truncate(bytes.len() / width * width);
                Owned::Numbers(bytes, width)
            }
            _ => split_strings(bytes, schedule),
        });
    }
    inputs
}

/// Checks that `outputs` are exactly `inputs`: type, content, width of
/// non-empty fixed-width inputs, and string lengths.
fn assert_outputs_are(inputs: &[Owned], outputs: &[TypedOutput]) {
    assert_eq!(inputs.len(), outputs.len(), "output count");
    for (index, (input, output)) in inputs.iter().zip(outputs).enumerate() {
        match input {
            Owned::Serial(bytes) => {
                assert_eq!(output.data_type(), DataType::Serial, "output {index}");
                assert_eq!(output.content(), &bytes[..], "output {index}");
            }
            Owned::Records(bytes, width) | Owned::Numbers(bytes, width) => {
                let expected = if matches!(input, Owned::Records(..)) {
                    DataType::Struct
                } else {
                    DataType::Numeric
                };
                assert_eq!(output.data_type(), expected, "output {index}");
                assert_eq!(output.content(), &bytes[..], "output {index}");
                if !bytes.is_empty() {
                    assert_eq!(output.width(), *width, "output {index} width");
                }
            }
            Owned::Strings(bytes, lens) => {
                assert_eq!(output.data_type(), DataType::String, "output {index}");
                assert_eq!(output.content(), &bytes[..], "output {index}");
                assert_eq!(output.string_lens(), &lens[..], "output {index} lengths");
            }
        }
    }
}

/// Typed inputs compressed by a standard graph decode back to themselves.
///
/// Header: byte 0 as in [`decode_config`] (level, format version and
/// checksums); byte 1 picks the graph from [`TYPED_GRAPHS`] in bits 0-4 and
/// turns the minimum stream size down to 1 with bit 7, so tiny inputs reach
/// the codecs; byte 2 holds the input count (`1 + n % 4`). Then, for each
/// input, a spec byte, two length bytes and that many content bytes. The
/// compressor may refuse an input the graph cannot take; a frame it writes
/// must decode, in mopenzl and in C, to exactly the inputs.
pub fn typed_roundtrip(data: &[u8]) {
    let mut rest = data;
    let Some(header) = take(&mut rest, 3) else {
        return;
    };
    let Some(config) = typed_config(header[0], header[1]) else {
        return;
    };
    let graph = TYPED_GRAPHS[usize::from(header[1] & 31) % TYPED_GRAPHS.len()];
    let mut schedule = Schedule::from_bytes(data);
    let inputs = typed_inputs(rest, 1 + usize::from(header[2] % 4), &mut schedule);
    if inputs.is_empty() {
        return;
    }
    let views: Vec<Input<'_>> = inputs.iter().map(Owned::input).collect();

    let Ok(mut encoder) = Compressor::builder(config)
        .with_graph(CompressorGraph::standard(graph))
        .build()
    else {
        return;
    };
    let Ok(frame) = encoder.compress_inputs(&views) else {
        return;
    };
    let outputs = Decompressor::default()
        .decompress_typed(&frame)
        .unwrap_or_else(|error| panic!("{graph:?} frame does not decode: {error:?}"));
    assert_outputs_are(&inputs, &outputs);
    let reference = openzl_ffi::graph::decompress(&frame)
        .unwrap_or_else(|error| panic!("{graph:?} frame is rejected by C: {error:?}"));
    assert_same_as_reference(&outputs, &reference);
}

/// The encoder configuration of a `typed_roundtrip` header.
fn typed_config(first: u8, second: u8) -> Option<EncoderConfig> {
    let version = match (first >> 3) & 7 {
        7 => 27,
        n => 21 + u32::from(n),
    };
    let mut config = EncoderConfig::default()
        .with_level(Level::try_from(crate::LEVELS[usize::from(first & 7)]).ok()?)
        .with_format_version(FormatVersion::try_from(version).ok()?)
        .with_content_checksum(first & 0x40 == 0)
        .with_compressed_checksum(first & 0x80 == 0);
    if second & 0x80 != 0 {
        config = config.with_min_stream_size(1);
    }
    Some(config)
}

/// Compresses small fixed inputs through `graph` and requires a frame, when
/// one is written, to decode to them (with `store` for dictionary frames).
fn exercise_graph(graph: CompressorGraph, store: Option<std::sync::Arc<BundleStore>>) {
    let text: Vec<u8> = (0..600u32)
        .map(|i| b"openzl graph "[(i % 13) as usize])
        .collect();
    let numbers: Vec<u8> = (0..300u32)
        .flat_map(|i| (i * i % 977).to_le_bytes())
        .collect();
    let records: Vec<u8> = (0..256u32).map(|i| (i / 3) as u8).collect();
    let words = b"alphabetagammadeltaepsilon".to_vec();
    let lens = [5u32, 4, 5, 5, 7];
    let strings = Strings::try_from((&words[..], &lens[..])).expect("lengths sum to the content");
    let numbers_in = Numbers::try_from((&numbers[..], 4)).expect("whole u32 numbers");
    let records_in = Records::try_from((&records[..], 4)).expect("whole records");
    let sets: [Vec<Input<'_>>; 3] = [
        vec![Input::from(&text[..])],
        vec![Input::from(numbers_in)],
        vec![
            Input::from(&text[..]),
            Input::from(numbers_in),
            Input::from(records_in),
            Input::from(strings),
        ],
    ];
    let config = EncoderConfig::default().with_min_stream_size(1);
    let Ok(mut encoder) = Compressor::builder(config).with_graph(graph).build() else {
        return;
    };
    for inputs in &sets {
        let Ok(frame) = encoder.compress_inputs(inputs) else {
            continue;
        };
        let mut builder = Decompressor::builder(Default::default());
        if let Some(store) = &store {
            builder = builder.with_dictionaries(std::sync::Arc::clone(store));
        }
        let outputs = builder
            .build()
            .decompress_typed(&frame)
            .unwrap_or_else(|error| {
                panic!("a frame the compressor wrote does not decode: {error:?}")
            });
        assert_eq!(outputs.len(), inputs.len());
        for (input, output) in inputs.iter().zip(&outputs) {
            assert_eq!(input.data_type(), output.data_type());
            assert_eq!(input.len(), output.len());
        }
    }
}

/// Successor lists the ML selector and clustering graphs are tried with.
fn successor_lists() -> [Vec<mopenzl::graph::GraphId>; 4] {
    let store = mopenzl::graph::GraphId::from(StandardGraph::Store);
    let zstd = mopenzl::graph::GraphId::from(StandardGraph::Zstd);
    [
        vec![store, zstd],
        vec![store, zstd, StandardGraph::Fse.into()],
        vec![store; 4],
        vec![zstd; 8],
    ]
}

/// Arbitrary configuration bytes never panic their parsers or the graphs
/// built from them.
///
/// Byte 0 picks the parser: a dictionary bundle (also inserted in a
/// [`BundleStore`] and used by a Zstandard node), an ML selector blob, a
/// clustering blob, an SDDL description, or SDDL2 bytecode (its first
/// following byte picks the chunk size). A configuration that parses is built
/// into a compressor over small fixed inputs; that must not panic and any
/// frame it writes must decode to the inputs.
pub fn graph_configs(data: &[u8]) {
    let Some((&kind, blob)) = data.split_first() else {
        return;
    };
    match kind % 5 {
        0 => {
            let Ok(bundle) = DictBundle::try_from(blob) else {
                return;
            };
            let mut store = BundleStore::default();
            let stored = store.insert(&bundle).is_ok();
            let mut builder = GraphBuilder::new();
            if builder.load_dictionaries(&bundle).is_err() {
                return;
            }
            let mut node = NodeParams::default();
            if let Some(dict) = bundle.dicts().first() {
                node = node.with_dictionary(dict.id());
            }
            let Ok(node) = builder.parameterize_node_with(StandardNode::Zstd, node) else {
                return;
            };
            let Ok(start) = builder.static_graph(node, &[StandardGraph::Store.into()]) else {
                return;
            };
            if let Ok(graph) = builder.build(start) {
                exercise_graph(graph, stored.then(|| std::sync::Arc::new(store)));
            }
        }
        1 => {
            let Ok(config) = MlSelectorConfig::try_from(blob) else {
                return;
            };
            for successors in successor_lists() {
                let mut builder = GraphBuilder::new();
                let Ok(start) = builder.ml_selector(&config, &successors) else {
                    continue;
                };
                if let Ok(graph) = builder.build(start) {
                    exercise_graph(graph, None);
                }
            }
        }
        2 => {
            let Ok(config) = ClusteringConfig::try_from(blob) else {
                return;
            };
            for successors in successor_lists() {
                let mut builder = GraphBuilder::new();
                let Ok(start) = builder.clustering(&config, &successors) else {
                    continue;
                };
                if let Ok(graph) = builder.build(start) {
                    exercise_graph(graph, None);
                }
            }
        }
        3 => {
            let mut builder = GraphBuilder::new();
            let Ok(start) = builder.sddl(blob, StandardGraph::Store) else {
                return;
            };
            if let Ok(graph) = builder.build(start) {
                exercise_graph(graph, None);
            }
        }
        _ => {
            let Some((&chunk, bytecode)) = blob.split_first() else {
                return;
            };
            let chunk_size = [0, 64, 4096, 1 << 20][usize::from(chunk & 3)];
            let mut builder = GraphBuilder::new();
            let Ok(start) = builder.sddl2(bytecode, chunk_size, None) else {
                return;
            };
            if let Ok(graph) = builder.build(start) {
                exercise_graph(graph, None);
            }
        }
    }
}

/// Every standard node ID `StandardNode` has: 2 to 55, 57 to 75, the LZ4
/// node 86 and the PIVCO Huffman node 87.
pub const NODE_IDS: [u32; 75] = {
    let mut ids = [0u32; 75];
    let mut index = 0;
    let mut id = 2;
    while id <= 75 {
        if id != 56 {
            ids[index] = id;
            index += 1;
        }
        id += 1;
    }
    ids[73] = 86;
    ids[74] = 87;
    ids
};

/// The type a node takes as input (`architecture/encoder-codecs.md`).
pub const fn node_input_type(id: u32) -> DataType {
    match id {
        3 | 9 | 10 | 17 | 18 | 35 | 40 | 60 | 67 | 71 => DataType::Struct,
        26 | 30 | 32 | 36 | 38 | 39 | 42 | 73 => DataType::String,
        5 | 13..=16 | 19..=25 | 27 | 33 | 52 | 57..=59 | 62 | 63 | 65 | 66 | 69 | 70 | 86 | 87 => {
            DataType::Serial
        }
        _ => DataType::Numeric,
    }
}

/// How many inputs a node reads: variable-count nodes take one to three,
/// `mux_lengths` two.
const fn node_inputs(id: u32, wanted: usize) -> usize {
    match id {
        33..=37 | 39 | 74 => wanted,
        53 => 2,
        _ => 1,
    }
}

/// Integer parameter IDs the node reads, then a handful any node may be given.
fn node_param_ids(id: u32) -> &'static [i32] {
    use mopenzl::graph::params;
    match id {
        62 => &[
            params::zstd::COMPRESSION_LEVEL,
            params::zstd::WINDOW_LOG,
            params::zstd::HASH_LOG,
            params::zstd::CHAIN_LOG,
            params::zstd::SEARCH_LOG,
            params::zstd::MIN_MATCH,
            params::zstd::TARGET_LENGTH,
            params::zstd::STRATEGY,
            params::zstd::ENABLE_LDM,
            params::zstd::CHECKSUM_FLAG,
            params::zstd::LITERAL_COMPRESSION_MODE,
        ],
        52 => &[
            params::lz::COMPRESSION_LEVEL,
            params::lz::ACCELERATION,
            params::lz::WINDOW_LOG,
            params::lz::STRATEGY,
            params::lz::HASH_LOG1,
            params::lz::HASH_LOG2,
            params::lz::HASH_LENGTH,
            params::lz::SEARCH_LOG,
            params::lz::LITERALS_GRAPH_IDX,
            params::lz::MIN_GAIN_FOR_ENTROPY_BYTES,
            params::lz::MIN_GAIN_FOR_ENTROPY_PCT,
        ],
        86 => &[params::lz4::COMPRESSION_LEVEL_OVERRIDE],
        9 => &[params::field_lz::COMPRESSION_LEVEL_OVERRIDE],
        58..=61 | 87 => &[
            params::entropy::MIN_GAIN_BYTES,
            params::entropy::MIN_GAIN_PCT,
        ],
        31 => &[params::divide_by::DIVIDE_BY],
        27 => &[params::bitunpack::NUM_BITS],
        48 => &[params::partition::PRESET],
        54 | 55 => &[params::sparse_num::DOMINANT_VALUE],
        50 | 51 => &[params::sentinel::VALUE],
        40..=42 | 72 | 73 => &[params::tokenize::SORT],
        49 | 66..=69 => &[
            params::split::BYRANGE_MIN_SEGMENT_SIZE,
            params::split::CHANNEL_ID,
        ],
        5 | 32 => &[
            params::dispatch::CHANNEL_ID,
            params::dispatch::STRING_NUM_OUTPUTS,
        ],
        53 => &[
            params::mux_lengths::SPLIT_POINT,
            params::mux_lengths::MATCH_LENGTH_BIAS,
        ],
        13..=16 => &[params::conversion::SERIAL_TO_STRUCT_SIZE],
        37 | 74 => &[params::dedup::TRUST_IDENTICAL],
        _ => &[0, 1, 100],
    }
}

/// Copy parameter IDs: segment sizes, field sizes, dispatch indices, sentinel
/// indices and value, custom partition.
const COPY_IDS: [i32; 7] = [323, 387, 48, 130, 131, 121, 100];

/// Runs one standard node over all inputs and stores every output.
struct RunNode {
    node: StandardNode,
    params: Option<LocalParams>,
}

impl FunctionGraph for RunNode {
    fn compress(&self, cx: &mut GraphContext<'_>, inputs: &[Edge]) -> Result<(), GraphError> {
        let outputs = match &self.params {
            Some(params) => cx.run_node_with(self.node, inputs, params)?,
            None => cx.run_node(self.node, inputs)?,
        };
        for output in outputs {
            cx.set_destination(&[output], StandardGraph::Store)?;
        }
        Ok(())
    }
}

/// Largest payload `node_roundtrip` compresses.
const MAX_NODE_PAYLOAD: usize = 16 << 10;

/// The decoded `node_roundtrip` input.
struct NodeCase {
    id: u32,
    config: EncoderConfig,
    c_params: Vec<(i32, i32)>,
    ints: Vec<(i32, i32)>,
    copies: Vec<(i32, Vec<u8>)>,
    inputs: Vec<Owned>,
}

fn node_case(data: &[u8]) -> Option<NodeCase> {
    let mut rest = data;
    let head = take(&mut rest, 4)?;
    let id = NODE_IDS[usize::from(head[0]) % NODE_IDS.len()];
    let version = 21 + u32::from((head[1] >> 3) & 7) % 7;
    let level = crate::LEVELS[usize::from(head[1] & 7)];
    let config = EncoderConfig::default()
        .with_level(Level::try_from(level).ok()?)
        .with_format_version(FormatVersion::try_from(version).ok()?)
        .with_content_checksum(head[1] & 0x40 == 0)
        .with_compressed_checksum(head[1] & 0x80 == 0)
        // A lone stream below the minimum goes straight to the store.
        .with_min_stream_size(1);
    let c_params = vec![
        (openzl_ffi::graph::cparam::LEVEL, level),
        (openzl_ffi::graph::cparam::FORMAT_VERSION, version as i32),
        (
            openzl_ffi::graph::cparam::CONTENT_CHECKSUM,
            1 + i32::from(!config.content_checksum()),
        ),
        (
            openzl_ffi::graph::cparam::COMPRESSED_CHECKSUM,
            1 + i32::from(!config.compressed_checksum()),
        ),
        (
            openzl_ffi::graph::cparam::PERMISSIVE,
            1 + i32::from(!config.permissive()),
        ),
        (
            openzl_ffi::graph::cparam::MIN_STREAM_SIZE,
            config.min_stream_size() as i32,
        ),
        (
            openzl_ffi::graph::cparam::STORE_ON_EXPANSION,
            1 + i32::from(!config.store_on_expansion()),
        ),
    ];

    let mut schedule = Schedule::from_bytes(data);
    let shape = head[2];
    let data_type = if shape & 0x40 != 0 {
        [
            DataType::Serial,
            DataType::Struct,
            DataType::Numeric,
            DataType::String,
        ][usize::from(head[3] & 3)]
    } else {
        node_input_type(id)
    };
    let width = match data_type {
        DataType::Numeric => NUMBER_WIDTHS[usize::from(shape >> 4) & 3],
        DataType::Struct => {
            RECORD_WIDTHS[usize::from(shape >> 4) & 3 | usize::from(head[3] >> 4 & 1) << 2]
        }
        _ => 1,
    };

    let mut ints = Vec::new();
    for _ in 0..(shape & 3) {
        let spec = take(&mut rest, 5)?;
        let ids = node_param_ids(id);
        let param = if spec[0] & 0x80 != 0 {
            i32::from(spec[0] & 0x7F)
        } else {
            ids[usize::from(spec[0]) % ids.len()]
        };
        ints.push((
            param,
            i32::from_le_bytes([spec[1], spec[2], spec[3], spec[4]]),
        ));
    }
    let mut copies = Vec::new();
    if shape & 0x80 != 0 {
        let spec = take(&mut rest, 2)?;
        let len = usize::from(spec[1]).min(rest.len());
        let bytes = take(&mut rest, len)?.to_vec();
        copies.push((COPY_IDS[usize::from(spec[0]) % COPY_IDS.len()], bytes));
    }

    let count = node_inputs(id, 1 + usize::from((shape >> 2) & 3) % 3);
    let payload = &rest[..rest.len().min(MAX_NODE_PAYLOAD)];
    // Trusted deduplication drops later inputs without comparing them, which
    // loses data unless they are equal: every input is then the first.
    let trusting = id == 74
        || (id == 37
            && ints.iter().any(|&(param, value)| {
                param == mopenzl::graph::params::dedup::TRUST_IDENTICAL && value != 0
            }));
    let each = if trusting {
        payload.len()
    } else {
        payload.len() / count
    };
    let inputs: Vec<Owned> = (0..count)
        .map(|index| {
            let part = if trusting {
                payload
            } else if index + 1 == count {
                &payload[index * each..]
            } else {
                &payload[index * each..(index + 1) * each]
            };
            let mut bytes = part.to_vec();
            match data_type {
                DataType::Serial => Owned::Serial(bytes),
                DataType::Struct => {
                    bytes.truncate(bytes.len() / width * width);
                    Owned::Records(bytes, width)
                }
                DataType::Numeric => {
                    bytes.truncate(bytes.len() / width * width);
                    Owned::Numbers(bytes, width)
                }
                DataType::String => split_strings(bytes, &mut schedule),
            }
        })
        .collect();
    // The C `dispatch_string` encoder reads one `u16` index per string from
    // the copy parameter without checking its size, so an undersized
    // parameter makes it read past the buffer (here: 8 bytes of zeros, which
    // it accepts where mopenzl rightly refuses). The indices are padded to
    // the string count so both sides see a well-formed parameter.
    if id == 32 {
        let strings = inputs.iter().map(Owned::len).max().unwrap_or(0);
        for (param, bytes) in &mut copies {
            if *param == 48 && bytes.len() < 2 * strings {
                bytes.resize(2 * strings, 0);
            }
        }
    }
    Some(NodeCase {
        id,
        config,
        c_params,
        ints,
        copies,
        inputs,
    })
}

impl Owned {
    fn len(&self) -> usize {
        match self {
            Self::Strings(_, lens) => lens.len(),
            Self::Serial(bytes) => bytes.len(),
            Self::Records(bytes, width) | Self::Numbers(bytes, width) => bytes.len() / width,
        }
    }

    fn c_input(&self) -> openzl_ffi::graph::Input<'_> {
        use openzl_ffi::graph::Input as C;
        match self {
            Self::Serial(bytes) => C::Serial(bytes),
            Self::Records(bytes, width) => C::Struct {
                bytes,
                width: *width,
            },
            Self::Numbers(bytes, width) => C::Numeric {
                bytes,
                width: *width,
            },
            Self::Strings(bytes, lens) => C::String { bytes, lens },
        }
    }
}

/// One standard node, run over a typed input, writes the reference's frame.
///
/// Four header bytes: the node (`NODE_IDS[b0 % 75]`); the level (bits 0-2),
/// format version (bits 3-5, `21 + n % 7`) and checksum switches (bits 6-7);
/// the shape (bits 0-1 integer parameter count, bits 2-3 input count for
/// variable-count nodes, bits 4-5 element width, bit 6 a mismatched input type
/// taken from byte 3, bit 7 one copy parameter); the type override. Then five
/// bytes per integer parameter (an index into the node's parameter IDs, or a
/// raw ID with bit 7; the value), two and the bytes of a copy parameter, and
/// the payload (at most 16 KiB).
///
/// Both libraries run the node in a function graph that stores every output.
/// They must both succeed or both fail, the frames must be equal, and the
/// frame must decode, in mopenzl and in C, to the inputs.
pub fn node_roundtrip(data: &[u8]) {
    use openzl_ffi::structure::{self, Run};
    let Some(case) = node_case(data) else {
        return;
    };
    let what = format!(
        "node {} ints {:?} copies {} bytes, v{}, level {}",
        case.id,
        case.ints,
        case.copies
            .iter()
            .map(|(_, bytes)| bytes.len())
            .sum::<usize>(),
        u32::from(case.config.format_version()),
        case.config.level().get(),
    );
    let Ok(node) = StandardNode::try_from(case.id) else {
        return;
    };
    let reference = structure::compress(
        &case.inputs.iter().map(Owned::c_input).collect::<Vec<_>>(),
        &Run::RawNode {
            node: case.id,
            ints: case.ints.clone(),
            copies: case.copies.clone(),
        },
        &case.c_params,
    );

    let mut params = LocalParams::default();
    for &(id, value) in &case.ints {
        params = params.with_int(id, value);
    }
    for (id, bytes) in &case.copies {
        params = params.with_bytes(*id, bytes.clone());
    }
    let params = (!case.ints.is_empty() || !case.copies.is_empty()).then_some(params);
    let mut builder = GraphBuilder::new();
    let graph = builder
        .function_graph(
            "node",
            RunNode { node, params },
            &[TypeSet::ANY],
            true,
            &[],
            &[],
        )
        .expect("function graph registers");
    let mut encoder = Compressor::builder(case.config)
        .with_graph(builder.build(graph).expect("graph builds"))
        .build()
        .expect("valid configuration");
    let views: Vec<Input<'_>> = case.inputs.iter().map(Owned::input).collect();
    let ours = encoder.compress_inputs(&views);

    match (reference, ours) {
        (Ok(expected), Ok(frame)) => {
            assert_eq!(frame, expected, "{what}: frames differ");
            let outputs = Decompressor::default()
                .decompress_typed(&frame)
                .unwrap_or_else(|error| panic!("{what}: frame does not decode: {error:?}"));
            assert_outputs_are(&case.inputs, &outputs);
            let theirs = openzl_ffi::graph::decompress(&frame)
                .unwrap_or_else(|error| panic!("{what}: C rejects the frame: {error:?}"));
            assert_same_as_reference(&outputs, &theirs);
        }
        (Err(_), Err(_)) => {}
        (Ok(_), Err(error)) => panic!("{what}: only mopenzl failed: {error}"),
        (Err(error), Ok(_)) => panic!("{what}: only the reference failed: {error:?}"),
    }
}
