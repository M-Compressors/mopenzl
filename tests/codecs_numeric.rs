//! The numeric-family decoders against frames made by the C library:
//! `delta_int`, `zigzag`, `divide_by`, `quantize_*`, `range_pack`,
//! `bitSplit`, `float_deconstruct`, `sparse_num`, `merge_sorted`,
//! `constant_*` and `prefix`.
// The shared helpers serve the whole family, so the file needs all of it.
#![cfg(all(
    feature = "decompression",
    feature = "conversion",
    feature = "delta",
    feature = "zigzag",
    feature = "divide-by",
    feature = "quantize",
    feature = "range-pack",
    feature = "bitsplit",
    feature = "float-deconstruct",
    feature = "sparse-num",
    feature = "merge-sorted",
    feature = "constant",
    feature = "prefix"
))]

use mopenzl::{DataType, Decompressor, TypedOutput};
#[cfg(any(feature = "divide-by", feature = "bitsplit", feature = "sparse-num"))]
use openzl_ffi::graph::Params;
use openzl_ffi::graph::{self, Graph, Input, cparam};
#[cfg(any(feature = "divide-by", feature = "bitsplit", feature = "sparse-num"))]
use openzl_ffi::numeric;
use openzl_ffi::numeric::Node;

/// Checksums off, so the codec itself sees damaged bytes.
const NO_CHECKSUMS: [(i32, i32); 2] = [
    (cparam::CONTENT_CHECKSUM, 2),
    (cparam::COMPRESSED_CHECKSUM, 2),
];

const VERSIONS: [i32; 3] = [21, 24, 27];

fn id(node: Node) -> u32 {
    node.into()
}

fn data_type(kind: u32) -> DataType {
    match kind {
        0 => DataType::Serial,
        1 => DataType::Struct,
        2 => DataType::Numeric,
        _ => DataType::String,
    }
}

/// A deterministic xorshift stream.
#[cfg(any(
    feature = "delta",
    feature = "zigzag",
    feature = "divide-by",
    feature = "quantize",
    feature = "range-pack",
    feature = "bitsplit",
    feature = "float-deconstruct",
    feature = "sparse-num",
    feature = "merge-sorted"
))]
struct Rng(u64);

#[cfg(any(
    feature = "delta",
    feature = "zigzag",
    feature = "divide-by",
    feature = "quantize",
    feature = "range-pack",
    feature = "bitsplit",
    feature = "float-deconstruct",
    feature = "sparse-num",
    feature = "merge-sorted"
))]
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[cfg(any(
    feature = "delta",
    feature = "zigzag",
    feature = "divide-by",
    feature = "quantize",
    feature = "range-pack",
    feature = "bitsplit",
    feature = "float-deconstruct",
    feature = "sparse-num",
    feature = "merge-sorted"
))]
fn numbers(width: usize, values: impl IntoIterator<Item = u64>) -> Vec<u8> {
    values
        .into_iter()
        .flat_map(|value| value.to_le_bytes()[..width].to_vec())
        .collect()
}

#[cfg(any(
    feature = "delta",
    feature = "zigzag",
    feature = "range-pack",
    feature = "bitsplit",
    feature = "sparse-num"
))]
fn random(width: usize, count: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed);
    numbers(width, (0..count).map(|_| rng.next()))
}

/// Compresses at `version`; `None` when the C library rejects the graph.
fn compress(inputs: &[Input<'_>], graph: &Graph, version: i32) -> Option<Vec<u8>> {
    let mut params = NO_CHECKSUMS.to_vec();
    params.push((cparam::FORMAT_VERSION, version));
    match graph::compress(inputs, graph, &params) {
        Ok(frame) => Some(frame),
        Err(error) => {
            let _ = error;
            None
        }
    }
}

/// Decodes `frame` with both libraries and checks they agree.
fn agree(frame: &[u8]) -> Vec<TypedOutput> {
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

/// Truncations must fail and flipped bytes must never panic or disagree
/// with C when both succeed.
fn damaged(frame: &[u8]) {
    let step = (frame.len() / 24).max(1);
    for cut in (0..frame.len()).step_by(step) {
        assert!(
            Decompressor::default()
                .decompress_typed(&frame[..cut])
                .is_err(),
            "cut at {cut}"
        );
    }
    for at in (0..frame.len()).step_by(step) {
        for flip in [0x01, 0x80, 0xff] {
            let mut bad = frame.to_vec();
            bad[at] ^= flip;
            let ours = Decompressor::default().decompress_typed(&bad);
            if let (Ok(ours), Ok(theirs)) = (ours, graph::decompress(&bad)) {
                assert_eq!(ours.len(), theirs.len());
                for (ours, theirs) in ours.iter().zip(&theirs) {
                    assert_eq!(ours.content(), theirs.bytes, "flip {flip:#x} at {at}");
                }
            }
        }
    }
}

/// Round-trips `inputs` through `graph` at the listed versions. `min` is the
/// first version with the codec: older ones must be rejected by C.
fn round_trip(inputs: &[Input<'_>], graph: &Graph, min: i32) -> Vec<Vec<TypedOutput>> {
    let mut all = Vec::new();
    // The C encoders refuse empty streams.
    let empty = inputs.iter().any(|input| match *input {
        Input::Serial(bytes) | Input::Struct { bytes, .. } | Input::Numeric { bytes, .. } => {
            bytes.is_empty()
        }
        Input::String { lens, .. } => lens.is_empty(),
    });
    for version in VERSIONS {
        match compress(inputs, graph, version) {
            Some(frame) => {
                all.push(agree(&frame));
                if version == 27 {
                    damaged(&frame);
                }
            }
            None => assert!(empty || version < min, "C rejected version {version}"),
        }
    }
    assert!(empty || !all.is_empty());
    all
}

fn store(count: usize) -> Vec<Graph> {
    (0..count).map(|_| Graph::STORE).collect()
}

#[cfg(any(
    feature = "delta",
    feature = "zigzag",
    feature = "divide-by",
    feature = "quantize",
    feature = "range-pack",
    feature = "bitsplit",
    feature = "float-deconstruct",
    feature = "sparse-num",
    feature = "merge-sorted"
))]
fn numeric(bytes: &[u8], width: usize) -> Input<'_> {
    Input::Numeric { bytes, width }
}

#[test]
#[cfg(feature = "delta")]
fn delta_restores_every_width_and_shape() {
    let graph = Graph::node(id(Node::DeltaInt), store(1));
    for width in [1, 2, 4, 8] {
        let ramp = numbers(width, (0..1000u64).map(|i| i * 3 + 7));
        let descending = numbers(width, (0..300u64).map(|i| 1000 - i * 2));
        let shapes = [
            Vec::new(),
            random(width, 1, 1),
            random(width, 2, 2),
            ramp,
            descending,
            random(width, 777, 3),
            vec![0xff; width * 33],
        ];
        for bytes in &shapes {
            let all = round_trip(&[numeric(bytes, width)], &graph, 21);
            for outputs in all {
                assert_eq!(outputs[0].content(), bytes.as_slice());
            }
        }
    }
}

#[test]
#[cfg(feature = "zigzag")]
fn zigzag_restores_every_width_and_shape() {
    let graph = Graph::node(id(Node::Zigzag), store(1));
    for width in [1, 2, 4, 8] {
        let small = numbers(width, (0..500u64).map(|i| i.wrapping_sub(250)));
        for bytes in [
            Vec::new(),
            random(width, 1, 4),
            small,
            random(width, 999, 5),
        ] {
            for outputs in round_trip(&[numeric(&bytes, width)], &graph, 21) {
                assert_eq!(outputs[0].content(), bytes.as_slice());
            }
        }
    }
}

#[test]
#[cfg(feature = "divide-by")]
fn divide_by_restores_multiples_with_a_computed_or_given_divisor() {
    let computed = Graph::node(id(Node::DivideBy), store(1));
    let given = Graph::Node {
        node: id(Node::DivideBy),
        params: Params {
            ints: Vec::new(),
            copies: vec![(numeric::DIVIDE_BY_PID, 6u64.to_le_bytes().to_vec())],
        },
        successors: store(1),
    };
    for width in [1usize, 2, 4, 8] {
        let max = u64::MAX >> (64 - 8 * width);
        let shapes = [
            Vec::new(),
            numbers(width, (0..40u64).map(|i| (i * 6) % max.min(250))),
            numbers(width, [0, 0, 0]),
            numbers(width, [max / 5 * 5, 5, 10]),
        ];
        for bytes in &shapes {
            for outputs in round_trip(&[numeric(bytes, width)], &computed, 21) {
                assert_eq!(outputs[0].content(), bytes.as_slice());
            }
        }
        // The given divisor 6 only fits multiples of 6.
        let sixes = numbers(width, (0..40u64).map(|i| (i * 6) % max.min(250)));
        for outputs in round_trip(&[numeric(&sixes, width)], &given, 21) {
            assert_eq!(outputs[0].content(), sixes.as_slice());
        }
    }
}

#[test]
#[cfg(feature = "quantize")]
fn quantize_offsets_and_lengths_restore_u32_values() {
    let mut rng = Rng(9);
    let offsets = numbers(
        4,
        (0..2000).map(|_| (rng.next() >> (32 + rng.next() % 32)).max(1)),
    );
    let lengths = numbers(4, (0..2000).map(|_| rng.next() >> (32 + rng.next() % 32)));
    let tiny = numbers(4, [1]);
    let wide = numbers(4, [1, u64::from(u32::MAX), 0x8000_0000, 3]);
    for (node, data) in [
        (Node::QuantizeOffsets, &offsets),
        (Node::QuantizeOffsets, &tiny),
        (Node::QuantizeOffsets, &wide),
        (Node::QuantizeLengths, &lengths),
        (
            Node::QuantizeLengths,
            &numbers(4, [0, 15, 16, 17, 31, 32, u64::from(u32::MAX)]),
        ),
        (Node::QuantizeLengths, &Vec::new()),
    ] {
        let graph = Graph::node(id(node), store(2));
        for outputs in round_trip(&[numeric(data, 4)], &graph, 21) {
            assert_eq!(outputs[0].content(), data.as_slice());
        }
    }
}

#[test]
#[cfg(feature = "range-pack")]
fn range_pack_widens_and_offsets() {
    let graph = Graph::node(id(Node::RangePack), store(1));
    for width in [2usize, 4, 8] {
        for minimum in [0u64, 1000, 1 << 20] {
            if width == 2 && minimum > 60_000 {
                continue;
            }
            let mut rng = Rng(minimum + width as u64);
            let bytes = numbers(width, (0..600).map(|_| minimum + rng.next() % 200));
            for outputs in round_trip(&[numeric(&bytes, width)], &graph, 21) {
                assert_eq!(outputs[0].content(), bytes.as_slice());
            }
        }
    }
    // Spanning the whole range keeps the width.
    let wide = random(8, 100, 6);
    round_trip(&[numeric(&wide, 8)], &graph, 21);
    round_trip(&[numeric(&[], 4)], &graph, 21);
}

#[test]
#[cfg(feature = "bitsplit")]
fn bitsplit_joins_full_and_partial_coverage() {
    let split = |widths: &[u8]| Graph::Node {
        node: id(Node::BitSplit),
        params: Params {
            ints: Vec::new(),
            copies: vec![(numeric::BITSPLIT_WIDTHS, widths.to_vec())],
        },
        successors: store(1),
    };
    let cases: [(usize, &[u8], u64); 6] = [
        (2, &[7, 8, 1], u64::MAX),
        (2, &[10, 5, 1], u64::MAX),
        (4, &[23, 8, 1], u64::MAX),
        (8, &[52, 11, 1], u64::MAX),
        (4, &[5, 6], (1 << 11) - 1),
        (1, &[3, 5], u64::MAX),
    ];
    for (width, widths, mask) in cases {
        let mut rng = Rng(width as u64);
        for count in [0usize, 1, 5, 511, 512, 513, 1500] {
            let bytes = numbers(width, (0..count).map(|_| rng.next() & mask));
            let graph = split(widths);
            for outputs in round_trip(&[numeric(&bytes, width)], &graph, 24) {
                assert_eq!(outputs[0].content(), bytes.as_slice());
            }
        }
    }
    // The presets of the public nodes.
    let words = random(4, 300, 7);
    let halves = random(2, 300, 8);
    let doubles = random(8, 300, 9);
    for (node, data, width) in [
        (Node::BitSplitFp, &words, 4),
        (Node::BitSplitFp, &halves, 2),
        (Node::BitSplitFp, &doubles, 8),
        (Node::BitSplitBf16, &halves, 2),
        (Node::BitSplitTop8, &words, 4),
        (Node::BitSplitTop8, &halves, 2),
    ] {
        let graph = Graph::node(id(node), store(1));
        for out in round_trip(&[numeric(data, width)], &graph, 24) {
            assert_eq!(out[0].content(), data.as_slice());
        }
    }
}

#[test]
#[cfg(feature = "float-deconstruct")]
fn float_deconstruct_restores_each_float_type() {
    let mut rng = Rng(11);
    for (node, width) in [
        (Node::Float32Deconstruct, 4),
        (Node::Bfloat16Deconstruct, 2),
        (Node::Float16Deconstruct, 2),
    ] {
        let graph = Graph::node(id(node), store(2));
        for count in [0usize, 1, 3, 1000] {
            let bytes = numbers(width, (0..count).map(|_| rng.next()));
            for outputs in round_trip(&[numeric(&bytes, width)], &graph, 21) {
                assert_eq!(outputs[0].content(), bytes.as_slice());
            }
        }
        // Smooth data: slowly varying exponents.
        let smooth = numbers(width, (0..500u64).map(|i| 0x3f80_0000 + i * 17));
        round_trip(&[numeric(&smooth, width)], &graph, 21);
    }
}

#[test]
#[cfg(feature = "sparse-num")]
fn sparse_num_restores_zero_and_chosen_dominant_symbols() {
    let zero = Graph::node(id(Node::SparseNum), store(2));
    for width in [1usize, 2, 4, 8] {
        let mut rng = Rng(width as u64 + 100);
        let sparse = numbers(
            width,
            (0..3000).map(|_| {
                if rng.next().is_multiple_of(9) {
                    rng.next() | 1
                } else {
                    0
                }
            }),
        );
        let dense = random(width, 100, 12);
        // Runs past 255 and, for bytes, past 65535 need wider distances.
        let zeros = numbers(width, vec![0; if width == 1 { 70_000 } else { 5_000 }]);
        let literal_end = numbers(width, [0, 0, 5]);
        let zero_end = numbers(width, [5, 0, 0]);
        for bytes in [Vec::new(), sparse, dense, zeros, literal_end, zero_end] {
            for outputs in round_trip(&[numeric(&bytes, width)], &zero, 26) {
                assert_eq!(outputs[0].content(), bytes.as_slice());
            }
        }
        // A dominant symbol of every header length that fits the width.
        for dominant in [7u64, 0x1234, 0x12_3456, 0x1234_5678, 0x0123_4567_89ab_cdef] {
            if width < 8 && dominant >> (8 * width) != 0 {
                continue;
            }
            let auto = Graph::Node {
                node: id(Node::SparseNumAuto),
                params: Params {
                    ints: Vec::new(),
                    copies: vec![(
                        numeric::SPARSE_NUM_DOMINANT_PID,
                        dominant.to_le_bytes().to_vec(),
                    )],
                },
                successors: store(2),
            };
            let mut rng = Rng(dominant);
            let bytes = numbers(
                width,
                (0..2000).map(|_| {
                    if rng.next().is_multiple_of(11) {
                        rng.next()
                    } else {
                        dominant
                    }
                }),
            );
            for outputs in round_trip(&[numeric(&bytes, width)], &auto, 26) {
                assert_eq!(outputs[0].content(), bytes.as_slice());
            }
        }
    }
}

#[test]
#[cfg(feature = "merge-sorted")]
fn merge_sorted_splits_up_to_sixty_four_lists() {
    let graph = Graph::node(id(Node::MergeSorted), store(2));
    for lists in [1usize, 2, 7, 8, 9, 16, 17, 33, 64] {
        let mut rng = Rng(lists as u64);
        let mut values = Vec::new();
        for _ in 0..lists {
            let mut next = rng.next() % 50;
            for _ in 0..(rng.next() % 40) {
                next += 1 + rng.next() % 5;
                values.push(next);
            }
            // A run must end before the next starts a smaller value; keep
            // each run strictly increasing and restart low.
        }
        let bytes = numbers(4, values);
        for outputs in round_trip(&[numeric(&bytes, 4)], &graph, 21) {
            assert_eq!(outputs[0].content(), bytes.as_slice());
        }
    }
    round_trip(&[numeric(&[], 4)], &graph, 21);
}

#[test]
#[cfg(feature = "constant")]
fn constant_streams_repeat_their_element() {
    let serial = Graph::node(id(Node::ConstantSerial), store(1));
    for count in [1usize, 2, 31, 32, 33, 100_000] {
        let bytes = vec![0xab; count];
        for outputs in round_trip(&[Input::Serial(&bytes)], &serial, 21) {
            assert_eq!(outputs[0].content(), bytes.as_slice());
        }
    }
    let fixed = Graph::node(id(Node::ConstantFixed), store(1));
    for width in [1usize, 2, 3, 8, 16, 33, 100] {
        for count in [1usize, 2, 5, 1000] {
            let record: Vec<u8> = (0..width as u8)
                .map(|i| i.wrapping_mul(7).wrapping_add(1))
                .collect();
            let bytes = record.repeat(count);
            let input = Input::Struct {
                bytes: &bytes,
                width,
            };
            for outputs in round_trip(&[input], &fixed, 21) {
                assert_eq!(outputs[0].content(), bytes.as_slice());
                assert_eq!(outputs[0].data_type(), DataType::Struct);
            }
        }
    }
}

#[test]
#[cfg(feature = "prefix")]
fn prefix_rebuilds_sorted_strings() {
    let graph = Graph::node(id(Node::Prefix), store(2));
    let words = [
        "apple", "apply", "apt", "aptly", "banana", "bandana", "band", "bandit",
    ];
    let mut sorted: Vec<String> = (0..400)
        .map(|i| format!("key/{:05}/{}", i / 3, i % 3))
        .collect();
    sorted.extend(words.iter().map(|w| w.to_string()));
    sorted.sort();
    let cases: Vec<Vec<String>> = vec![
        Vec::new(),
        vec!["".into(), "a".into(), "ab".into(), "ab".into(), "b".into()],
        vec!["alone".into()],
        vec!["same".into(); 5],
        words.iter().map(|w| w.to_string()).collect(),
        sorted,
    ];
    for strings in cases {
        let bytes: Vec<u8> = strings.iter().flat_map(|s| s.bytes()).collect();
        let lens: Vec<u32> = strings.iter().map(|s| s.len() as u32).collect();
        let input = Input::String {
            bytes: &bytes,
            lens: &lens,
        };
        for outputs in round_trip(&[input], &graph, 21) {
            assert_eq!(outputs[0].content(), bytes.as_slice());
            assert_eq!(outputs[0].string_lens(), lens.as_slice());
        }
    }
}
