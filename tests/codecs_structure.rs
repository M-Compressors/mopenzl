//! Differential tests of the structure codecs (transpose, split,
//! split-by-struct, dispatch, concat, dedup, interleave, mux-lengths and
//! sentinel decoders) against the C library: frames the C encoder writes
//! decode to the same typed outputs, and damaged frames never panic and are
//! accepted or rejected as the C decoder does.
#![cfg(feature = "decompression")]
#![allow(
    dead_code,
    reason = "each feature profile runs a different subset of the helpers"
)]

mod support;

use mopenzl::{DataType, Decompressor, TypedOutput};
use openzl_ffi::graph::{self, Input, cparam};
use openzl_ffi::structure::{self, Node, Run};

/// The format versions to write.
const VERSIONS: [i32; 3] = [21, 24, 27];

/// Global parameters: the version, without checksums so that the codecs
/// themselves see damaged streams.
fn options(version: i32) -> [(i32, i32); 3] {
    [
        (cparam::FORMAT_VERSION, version),
        (cparam::CONTENT_CHECKSUM, 2),
        (cparam::COMPRESSED_CHECKSUM, 2),
    ]
}

fn data_type(kind: u32) -> DataType {
    match kind {
        0 => DataType::Serial,
        1 => DataType::Struct,
        2 => DataType::Numeric,
        _ => DataType::String,
    }
}

/// Whether mopenzl's outputs are the C library's.
fn same(ours: &[TypedOutput], theirs: &[graph::Output]) -> bool {
    ours.len() == theirs.len()
        && ours.iter().zip(theirs).all(|(ours, theirs)| {
            ours.data_type() == data_type(theirs.kind)
                && ours.content() == theirs.bytes
                && ours.string_lens() == theirs.lens
                && ours.len() == theirs.elts
                && (ours.is_empty()
                    || ours.data_type() == DataType::String
                    || ours.width() == theirs.width)
        })
}

/// Decodes `frame` with both libraries and checks they agree.
fn agree(frame: &[u8]) -> Vec<TypedOutput> {
    let expected = graph::decompress(frame).expect("C decodes its frame");
    let outputs = Decompressor::default()
        .decompress_typed(frame)
        .expect("mopenzl decodes the frame");
    assert!(same(&outputs, &expected), "outputs differ");
    outputs
}

/// Damages `frame` byte by byte and cuts it short: mopenzl must neither
/// panic nor accept what the C library rejects (it may be stricter, which
/// is reported by the returned count) and must agree where both accept.
fn damage(frame: &[u8]) -> usize {
    let mut stricter = 0;
    let step = (frame.len() / 600).max(1);
    let mut check = |damaged: &[u8]| {
        let ours = Decompressor::default().decompress_typed(damaged);
        let theirs = graph::decompress(damaged);
        match (ours, theirs) {
            (Ok(ours), Ok(theirs)) => assert!(same(&ours, &theirs), "both accept, outputs differ"),
            (Ok(_), Err(_)) => panic!("mopenzl accepts a frame the C library rejects"),
            (Err(_), Ok(_)) => stricter += 1,
            (Err(_), Err(_)) => {}
        }
    };
    for at in (0..frame.len()).step_by(step) {
        for mask in [0x01, 0x80, 0xff] {
            let mut damaged = frame.to_vec();
            damaged[at] ^= mask;
            check(&damaged);
        }
    }
    for len in (0..frame.len()).step_by(step) {
        let ours = Decompressor::default().decompress_typed(&frame[..len]);
        assert!(ours.is_err(), "a frame cut to {len} bytes decoded");
    }
    stricter
}

/// A frame through the function graph of the family helper.
fn run(inputs: &[Input<'_>], run: &Run, version: i32) -> Vec<u8> {
    structure::compress(inputs, run, &options(version)).expect("C compresses")
}

/// Runs `node` without parameters.
fn plain(node: Node) -> Run {
    with(node, Vec::new(), Vec::new())
}

/// Runs `node` with integer and copy parameters.
fn with(node: Node, ints: Vec<(i32, i32)>, copies: Vec<(i32, Vec<u8>)>) -> Run {
    Run::Node { node, ints, copies }
}

/// Packs `values` as `usize` copy-parameter bytes.
fn sizes_param(values: &[usize]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn numbers(count: usize, width: usize, seed: u32, modulus: u64) -> Vec<u8> {
    let raw = support::noise(count * 8, seed);
    raw.as_chunks::<8>()
        .0
        .iter()
        .flat_map(|&word| (u64::from_le_bytes(word) % modulus).to_le_bytes()[..width].to_vec())
        .collect()
}

#[cfg(feature = "transpose")]
mod transpose {
    use super::*;

    fn frame(width: usize, count: usize, version: i32) -> Vec<u8> {
        let data = support::noise(width * count, (width * 31 + count) as u32);
        run(
            &[Input::Struct {
                bytes: &data,
                width,
            }],
            &plain(Node::TransposeSplit),
            version,
        )
    }

    #[test]
    #[cfg(feature = "conversion")]
    fn split_transpose_matches_c_for_every_width_and_count() {
        for version in VERSIONS {
            for width in [1, 2, 3, 4, 5, 8, 12, 33] {
                for count in [0, 1, 7, 1000] {
                    let outputs = agree(&frame(width, count, version));
                    assert_eq!(outputs[0].data_type(), DataType::Struct);
                }
            }
        }
    }

    #[test]
    #[cfg(feature = "conversion")]
    fn damaged_split_transpose_frames_are_handled() {
        for version in [21, 27] {
            damage(&frame(4, 20, version));
            damage(&frame(8, 0, version));
        }
    }
}

#[cfg(feature = "split")]
mod split {
    use super::*;

    fn splits(node: Node, input: Input<'_>, segments: &[usize], version: i32) -> Vec<u8> {
        let run_node = with(node, Vec::new(), vec![(323, sizes_param(segments))]);
        run(&[input], &run_node, version)
    }

    const SEGMENTS: [&[usize]; 6] = [
        &[0],
        &[5, 0],
        &[1, 2, 3, 0],
        &[0, 0, 0, 7],
        &[10, 10, 10, 10, 10],
        &[3, 0, 4, 0, 0],
    ];

    #[test]
    fn serial_segments_join_like_c() {
        let data = support::text(50, 3);
        for version in VERSIONS {
            for segments in SEGMENTS {
                let total: usize = segments.iter().sum();
                let input = if total == 50 || segments.last() == Some(&0) {
                    &data[..]
                } else {
                    &data[..total]
                };
                let frame = splits(Node::SplitN, Input::Serial(input), segments, version);
                let outputs = agree(&frame);
                assert_eq!(outputs[0].content(), input);
            }
        }
    }

    #[test]
    fn empty_serial_input_has_one_empty_segment() {
        for version in VERSIONS {
            agree(&splits(Node::SplitN, Input::Serial(&[]), &[0], version));
        }
    }

    #[test]
    #[cfg(feature = "conversion")]
    fn struct_segments_keep_their_width() {
        for version in [21, 27] {
            for width in [1, 3, 8, 24] {
                let data = support::noise(width * 40, width as u32);
                for segments in [&[40usize][..], &[10, 0], &[0, 5, 0]] {
                    let input = Input::Struct {
                        bytes: &data,
                        width,
                    };
                    let outputs = agree(&splits(Node::SplitNStruct, input, segments, version));
                    assert_eq!(outputs[0].width(), width);
                    assert_eq!(outputs[0].len(), 40);
                }
            }
        }
    }

    #[test]
    #[cfg(feature = "conversion")]
    fn numeric_segments_keep_their_width() {
        for version in [21, 27] {
            for width in [1, 2, 4, 8] {
                for count in [0usize, 1, 100] {
                    let data = numbers(count, width, 5, 1 << 40);
                    let input = Input::Numeric {
                        bytes: &data,
                        width,
                    };
                    let outputs = agree(&splits(Node::SplitNNum, input, &[count / 2, 0], version));
                    assert_eq!(outputs[0].content(), data);
                }
            }
        }
    }

    #[test]
    fn damaged_split_frames_are_handled() {
        let data = support::noise(60, 9);
        let frame = splits(Node::SplitN, Input::Serial(&data), &[5, 20, 0], 27);
        damage(&frame);
        let numbers = numbers(30, 4, 2, 1000);
        let input = Input::Numeric {
            bytes: &numbers,
            width: 4,
        };
        damage(&splits(Node::SplitNNum, input, &[10, 0], 24));
    }
}

#[cfg(feature = "split-by-struct")]
mod split_by_struct {
    use super::*;

    fn frame(fields: &[usize], count: usize, version: i32) -> Vec<u8> {
        let size: usize = fields.iter().sum();
        let data = support::noise(size * count, (size + count) as u32);
        let split = with(
            Node::SplitByStruct,
            Vec::new(),
            vec![(387, sizes_param(fields))],
        );
        run(&[Input::Serial(&data)], &split, version)
    }

    #[test]
    fn fields_join_like_c() {
        let layouts: [&[usize]; 7] = [
            &[1],
            &[4, 4],
            &[1, 2, 3],
            &[8, 8, 8],
            &[9, 1],
            &[16, 3],
            &[2; 12],
        ];
        for version in VERSIONS {
            for fields in layouts {
                for count in [0, 1, 9, 500] {
                    agree(&frame(fields, count, version));
                }
            }
        }
    }

    #[test]
    fn damaged_frames_are_handled() {
        damage(&frame(&[1, 2, 3], 30, 27));
        damage(&frame(&[8, 1], 0, 21));
    }
}

#[cfg(feature = "dispatch")]
mod dispatch {
    use super::*;

    /// Cuts `len` bytes into segments of at most `longest` bytes routed to
    /// `streams` streams in a fixed pattern.
    fn plan(len: usize, longest: usize, streams: u32) -> (Vec<usize>, Vec<u32>) {
        let mut sizes = Vec::new();
        let mut tags = Vec::new();
        let (mut left, mut n) = (len, 0usize);
        while left > 0 {
            let size = ((n * 37 + 11) % longest + 1).min(left);
            sizes.push(size);
            tags.push(((n * 7 + n / 3) as u32) % streams);
            left -= size;
            n += 1;
        }
        (sizes, tags)
    }

    fn by_tag(len: usize, longest: usize, streams: u32, version: i32) -> Vec<u8> {
        let data = support::text(len, len as u32);
        let (sizes, tags) = plan(len, longest, streams);
        run(
            &[Input::Serial(&data)],
            &Run::Dispatch {
                sizes,
                tags,
                tags_count: streams,
            },
            version,
        )
    }

    #[test]
    fn tagged_segments_join_like_c() {
        for version in VERSIONS {
            // 1-byte, 2-byte and 4-byte segment sizes; 1 and 2-byte tags.
            for (len, longest, streams) in [
                (1, 1, 1),
                (100, 10, 3),
                (5000, 200, 40),
                (30_000, 20_000, 2),
                (20_000, 20, 300),
                (1000, 1000, 1),
            ] {
                let frame = by_tag(len, longest, streams, version);
                assert_eq!(agree(&frame)[0].content().len(), len);
            }
        }
    }

    #[test]
    fn an_empty_input_dispatches_nothing() {
        for version in VERSIONS {
            agree(&by_tag(0, 1, 0, version));
        }
    }

    #[test]
    fn damaged_tagged_frames_are_handled() {
        damage(&by_tag(200, 20, 4, 27));
        damage(&by_tag(300, 10, 300, 21));
    }

    fn strings(count: usize, seed: u32) -> (Vec<u8>, Vec<u32>) {
        let lens: Vec<u32> = support::noise(count, seed)
            .iter()
            .map(|&b| match b % 8 {
                0 => 0,
                7 => 40 + u32::from(b) % 50,
                _ => u32::from(b) % 9,
            })
            .collect();
        let total = lens.iter().sum::<u32>() as usize;
        (support::text(total, seed + 1), lens)
    }

    fn by_index(count: usize, outputs: usize, version: i32) -> Vec<u8> {
        let (bytes, lens) = strings(count, outputs as u32 + 7);
        let indices: Vec<u8> = (0..count)
            .flat_map(|i| (((i * 5 + i / 7) % outputs.max(1)) as u16).to_le_bytes())
            .collect();
        let dispatch = with(
            Node::DispatchString,
            vec![(47, outputs as i32)],
            vec![(48, indices)],
        );
        run(
            &[Input::String {
                bytes: &bytes,
                lens: &lens,
            }],
            &dispatch,
            version,
        )
    }

    #[test]
    #[cfg(feature = "conversion")]
    fn string_dispatch_matches_c() {
        for version in VERSIONS {
            for (count, outputs) in [(0, 0), (0, 3), (1, 1), (20, 2), (500, 7), (3000, 300)] {
                let outputs = agree(&by_index(count, outputs, version));
                assert_eq!(outputs[0].data_type(), DataType::String);
                assert_eq!(outputs[0].len(), count);
            }
        }
    }

    #[test]
    #[cfg(feature = "conversion")]
    fn damaged_string_dispatch_frames_are_handled() {
        damage(&by_index(60, 3, 27));
        damage(&by_index(10, 12, 21));
    }
}

#[cfg(feature = "concat")]
mod concat {
    use super::*;

    fn pieces(lens: &[usize]) -> Vec<Vec<u8>> {
        lens.iter()
            .enumerate()
            .map(|(n, &len)| support::text(len, n as u32 + 1))
            .collect()
    }

    const LAYOUTS: [&[usize]; 5] = [&[5], &[0], &[3, 0, 9], &[100, 1, 50, 7, 0, 33], &[0, 0]];

    #[test]
    fn serial_pieces_split_back_like_c() {
        for version in VERSIONS {
            for layout in LAYOUTS {
                let data = pieces(layout);
                let inputs: Vec<Input<'_>> = data.iter().map(|d| Input::Serial(d)).collect();
                let frame = run(&inputs, &plain(Node::ConcatSerial), version);
                let outputs = agree(&frame);
                assert_eq!(outputs.len(), layout.len());
            }
        }
    }

    #[test]
    #[cfg(feature = "conversion")]
    fn numeric_and_struct_pieces_keep_their_width() {
        for version in VERSIONS {
            for width in [1, 2, 4, 8] {
                for layout in LAYOUTS {
                    let data: Vec<Vec<u8>> = layout
                        .iter()
                        .enumerate()
                        .map(|(n, &len)| numbers(len, width, n as u32 + 9, 1 << 30))
                        .collect();
                    for (node, make) in [
                        (
                            Node::ConcatNumeric,
                            (|bytes, width| Input::Numeric { bytes, width })
                                as fn(&'static [u8], usize) -> Input<'static>,
                        ),
                        (Node::ConcatStruct, |bytes, width| Input::Struct {
                            bytes,
                            width,
                        }),
                    ] {
                        // SAFETY of lifetimes: the inputs borrow `data`
                        // only for this iteration.
                        let inputs: Vec<Input<'_>> = data
                            .iter()
                            .map(|d| match make(&[], width) {
                                Input::Numeric { .. } => Input::Numeric { bytes: d, width },
                                _ => Input::Struct { bytes: d, width },
                            })
                            .collect();
                        let frame = run(&inputs, &plain(node), version);
                        let outputs = agree(&frame);
                        assert_eq!(outputs.len(), layout.len());
                    }
                }
            }
        }
    }

    #[test]
    #[cfg(feature = "conversion")]
    fn string_pieces_split_back_like_c() {
        for version in VERSIONS {
            let layouts: [&[&[u32]]; 4] = [
                &[&[1, 2, 3]],
                &[&[], &[4, 0, 4]],
                &[&[0], &[], &[7, 7, 7, 7], &[1]],
                &[&[40; 30], &[2; 100]],
            ];
            for layout in layouts {
                let contents: Vec<Vec<u8>> = layout
                    .iter()
                    .map(|lens| support::text(lens.iter().sum::<u32>() as usize, 4))
                    .collect();
                let inputs: Vec<Input<'_>> = contents
                    .iter()
                    .zip(layout)
                    .map(|(bytes, lens)| Input::String { bytes, lens })
                    .collect();
                let frame = run(&inputs, &plain(Node::ConcatString), version);
                assert_eq!(agree(&frame).len(), layout.len());
            }
        }
    }

    #[test]
    fn damaged_concat_frames_are_handled() {
        let data = pieces(&[20, 0, 30]);
        let inputs: Vec<Input<'_>> = data.iter().map(|d| Input::Serial(d)).collect();
        let node = plain(Node::ConcatSerial);
        damage(&run(&inputs, &node, 27));
        let lens = [[3u32, 4], [5, 1]];
        let bytes = [support::text(7, 1), support::text(6, 2)];
        let inputs: Vec<Input<'_>> = bytes
            .iter()
            .zip(&lens)
            .map(|(bytes, lens)| Input::String { bytes, lens })
            .collect();
        let node = plain(Node::ConcatString);
        damage(&run(&inputs, &node, 21));
    }
}

#[cfg(all(feature = "dedup", feature = "conversion"))]
mod dedup {
    use super::*;

    #[test]
    fn identical_streams_are_stored_once_and_restored() {
        for version in VERSIONS {
            for width in [1, 2, 4, 8] {
                for count in [0usize, 1, 17, 1000] {
                    let data = numbers(count, width, 3, 1 << 20);
                    for copies in [1, 2, 5] {
                        let inputs = vec![
                            Input::Numeric {
                                bytes: &data,
                                width
                            };
                            copies
                        ];
                        let frame = run(&inputs, &plain(Node::DedupNumeric), version);
                        let outputs = agree(&frame);
                        assert_eq!(outputs.len(), copies);
                    }
                }
            }
        }
    }

    #[test]
    fn damaged_dedup_frames_are_handled() {
        let data = numbers(40, 4, 1, 1000);
        let inputs = vec![
            Input::Numeric {
                bytes: &data,
                width: 4
            };
            3
        ];
        let node = plain(Node::DedupNumeric);
        damage(&run(&inputs, &node, 27));
    }
}

#[cfg(all(feature = "interleave", feature = "conversion"))]
mod interleave {
    use super::*;

    fn frame(streams: usize, strings: usize, version: i32) -> Vec<u8> {
        let lens: Vec<Vec<u32>> = (0..streams)
            .map(|s| {
                support::noise(strings, s as u32 + 1)
                    .iter()
                    .map(|&b| u32::from(b) % 7)
                    .collect()
            })
            .collect();
        let bytes: Vec<Vec<u8>> = lens
            .iter()
            .map(|l| support::text(l.iter().sum::<u32>() as usize, 2))
            .collect();
        let inputs: Vec<Input<'_>> = bytes
            .iter()
            .zip(&lens)
            .map(|(bytes, lens)| Input::String { bytes, lens })
            .collect();
        run(&inputs, &plain(Node::InterleaveString), version)
    }

    #[test]
    fn interleaved_strings_split_back_like_c() {
        for version in VERSIONS {
            for (streams, strings) in [(1, 0), (1, 9), (2, 0), (2, 50), (5, 13), (16, 100), (60, 3)]
            {
                let outputs = agree(&frame(streams, strings, version));
                assert_eq!(outputs.len(), streams);
                assert!(outputs.iter().all(|o| o.len() == strings));
            }
        }
    }

    #[test]
    fn damaged_interleave_frames_are_handled() {
        damage(&frame(3, 10, 27));
        damage(&frame(2, 0, 21));
    }
}

#[cfg(all(feature = "mux-lengths", feature = "conversion"))]
mod mux_lengths {
    use super::*;

    fn frame(width: usize, count: usize, params: &[(i32, i32)], version: i32) -> Vec<u8> {
        // Literal lengths mostly small, with outliers that overflow;
        // match lengths around a typical minimum.
        let literals = numbers(count, width, 11, 5);
        let mut long = numbers(count, width, 12, 4000);
        for (n, chunk) in long.chunks_exact_mut(width).enumerate() {
            if n % 5 != 0 {
                chunk.fill(0);
                chunk[0] = 4 + (n % 9) as u8;
            }
        }
        let mut literals = literals;
        for (n, chunk) in literals.chunks_exact_mut(width).enumerate() {
            if n % 11 == 0 {
                chunk[0] = 200;
            }
        }
        run(
            &[
                Input::Numeric {
                    bytes: &literals,
                    width,
                },
                Input::Numeric {
                    bytes: &long,
                    width,
                },
            ],
            &with(Node::MuxLengths, params.to_vec(), Vec::new()),
            version,
        )
    }

    #[test]
    fn muxed_lengths_split_back_like_c() {
        // Split points 0 and 8 push one of the lengths out entirely;
        // biases up to 15 are the format's limit.
        let settings: [&[(i32, i32)]; 6] = [
            &[],
            &[(0, 0), (1, 0)],
            &[(0, 3), (1, 4)],
            &[(0, 4), (1, 15)],
            &[(0, 8), (1, 2)],
            &[(0, 6), (1, 0)],
        ];
        for version in [24, 27] {
            for width in [1, 2, 4, 8] {
                for params in settings {
                    for count in [0, 1, 7, 300] {
                        let outputs = agree(&frame(width, count, params, version));
                        assert_eq!(outputs.len(), 2);
                        assert_eq!(outputs[0].len(), count);
                    }
                }
            }
        }
    }

    #[test]
    fn damaged_mux_lengths_frames_are_handled() {
        damage(&frame(2, 60, &[(0, 4), (1, 2)], 27));
        damage(&frame(1, 30, &[], 24));
        damage(&frame(8, 20, &[(0, 0), (1, 0)], 24));
    }
}

#[cfg(all(feature = "sentinel", feature = "conversion"))]
mod sentinel {
    use super::*;

    /// Numbers of `width` bytes, a third of them past 254.
    fn values(width: usize, count: usize) -> Vec<u8> {
        let mut data = numbers(count, width, 17, 250);
        for (n, chunk) in data.chunks_exact_mut(width).enumerate() {
            if n % 3 == 0 {
                chunk[width - 1] = 1 + (n % 7) as u8;
                chunk[0] = (n % 251) as u8;
            }
        }
        data
    }

    #[test]
    fn sentinel_byte_matches_c() {
        for version in [24, 27] {
            for width in [2, 4, 8] {
                for count in [0, 1, 10, 1000] {
                    let data = values(width, count);
                    let frame = run(
                        &[Input::Numeric {
                            bytes: &data,
                            width,
                        }],
                        &plain(Node::SentinelByte),
                        version,
                    );
                    let outputs = agree(&frame);
                    assert_eq!(
                        outputs[0].width(),
                        if count == 0 {
                            outputs[0].width()
                        } else {
                            width
                        }
                    );
                    assert_eq!(outputs[0].content(), data);
                }
            }
        }
    }

    fn general(width: usize, count: usize, sentinel: u64, version: i32) -> Vec<u8> {
        let mut data = numbers(count, width, 23, 100);
        // The sentinel never occurs in the data; every fourth element is
        // moved to the exceptions.
        let mask = if width == 8 {
            u64::MAX
        } else {
            (1 << (8 * width)) - 1
        };
        let sentinel = sentinel & mask;
        for chunk in data.chunks_exact_mut(width) {
            let mut word = [0u8; 8];
            word[..width].copy_from_slice(chunk);
            if u64::from_le_bytes(word) == sentinel {
                chunk[0] ^= 1;
            }
        }
        let indices = (0..count).filter(|i| i % 4 == 0).collect();
        run(
            &[Input::Numeric {
                bytes: &data,
                width,
            }],
            &Run::Sentinel { indices, sentinel },
            version,
        )
    }

    #[test]
    fn general_sentinels_match_c() {
        for version in [24, 27] {
            for width in [1, 2, 4, 8] {
                for sentinel in [u64::MAX, 0, 7, 0xfe, 0x1234] {
                    for count in [0, 1, 5, 500] {
                        let frame = general(width, count, sentinel, version);
                        let outputs = agree(&frame);
                        assert_eq!(outputs.len(), 2 - usize::from(true));
                    }
                }
            }
        }
    }

    #[test]
    fn damaged_sentinel_frames_are_handled() {
        damage(&general(2, 40, u64::MAX, 27));
        damage(&general(4, 40, 9, 24));
        let data = values(4, 40);
        let frame = run(
            &[Input::Numeric {
                bytes: &data,
                width: 4,
            }],
            &plain(Node::SentinelByte),
            27,
        );
        damage(&frame);
    }
}

/// Hand-built frames for codecs the C encoder no longer writes at the
/// supported versions (`transpose`, `transpose_split{2,4,8}`) or cannot be
/// driven into a corner (`splitn` without segments). They follow the layout
/// documented in `architecture/frame-format.md` and are checked against the
/// C decoder.
mod built {
    use super::*;

    /// One codec of a hand-built chunk.
    pub struct Codec {
        pub id: u32,
        pub header: Vec<u8>,
        pub variable: usize,
        /// Distance of each regenerated stream past the codec's inputs.
        pub regens: Vec<u32>,
    }

    pub fn varint(out: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            out.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }

    fn pack(out: &mut Vec<u8>, values: &[u32], bits: u32) {
        let mut buffer = 0u64;
        let mut filled = 0;
        for &value in values {
            buffer |= u64::from(value) << filled;
            filled += bits;
            while filled >= 8 {
                out.push(buffer as u8);
                buffer >>= 8;
                filled -= 8;
            }
        }
        if filled > 0 {
            out.push(buffer as u8);
        }
    }

    /// A lane of one flag per codec, then a varint for each set flag.
    fn lane(out: &mut Vec<u8>, entries: &[Option<u64>]) {
        let flags: Vec<u32> = entries.iter().map(|e| u32::from(e.is_some())).collect();
        pack(out, &flags, 1);
        for entry in entries.iter().flatten() {
            varint(out, *entry);
        }
    }

    /// A chunk of `codecs` over the stored streams `stored`, at `version`.
    pub fn chunk(version: i32, codecs: &[Codec], stored: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        varint(&mut out, codecs.len() as u64 + 1);
        varint(&mut out, stored.len() as u64);
        pack(&mut out, &vec![0; codecs.len()], 1);
        let id_bits = if version < 24 { 6 } else { 7 };
        pack(
            &mut out,
            &codecs.iter().map(|c| c.id).collect::<Vec<_>>(),
            id_bits,
        );
        let sizes: Vec<_> = codecs
            .iter()
            .map(|c| (!c.header.is_empty()).then(|| c.header.len() as u64 - 1))
            .collect();
        lane(&mut out, &sizes);
        let variable: Vec<_> = codecs
            .iter()
            .map(|c| (c.variable > 0).then(|| c.variable as u64 - 1))
            .collect();
        lane(&mut out, &variable);
        let regens: Vec<_> = codecs
            .iter()
            .map(|c| (c.regens.len() != 1).then(|| c.regens.len() as u64 - 2))
            .collect();
        lane(&mut out, &regens);
        let distances: Vec<u32> = codecs.iter().flat_map(|c| c.regens.clone()).collect();
        let total = (distances.len() + stored.len()) as u64;
        let bits = if total <= 1 {
            0
        } else {
            64 - (total - 1).leading_zeros()
        };
        pack(&mut out, &distances, bits);
        for stream in stored {
            varint(&mut out, stream.len() as u64);
        }
        for codec in codecs {
            out.extend_from_slice(&codec.header);
        }
        for stream in stored {
            out.extend_from_slice(stream);
        }
        out
    }

    /// A frame of one output of type code `ty` and `size` bytes holding the
    /// single chunk `chunk`, without checksums.
    pub fn frame(version: i32, ty: u8, size: usize, chunk: &[u8]) -> Vec<u8> {
        let mut out = (0xD7B1_A5C0u32 + version as u32).to_le_bytes().to_vec();
        out.push(0);
        out.push(1 | (ty << 4));
        varint(&mut out, size as u64 + 1);
        out.extend_from_slice(chunk);
        out.push(0);
        out
    }

    /// Both libraries decode `frame` to the same output, or both fail.
    pub fn compare(frame: &[u8]) -> Option<Vec<TypedOutput>> {
        let ours = Decompressor::default().decompress_typed(frame);
        let theirs = graph::decompress(frame);
        match (ours, theirs) {
            (Ok(ours), Ok(theirs)) => {
                assert!(same(&ours, &theirs));
                Some(ours)
            }
            (Err(_), Err(_)) => None,
            (ours, theirs) => panic!("ours {:?}, theirs {:?}", ours.is_ok(), theirs.is_ok()),
        }
    }
}

#[cfg(feature = "transpose")]
mod legacy_transpose {
    use super::built::*;
    use super::*;

    /// The transposition of `count` elements of `width` bytes: column `p`
    /// holds byte `p` of every element.
    fn columns(data: &[u8], width: usize) -> Vec<Vec<u8>> {
        (0..width)
            .map(|p| data.chunks_exact(width).map(|e| e[p]).collect())
            .collect()
    }

    #[test]
    #[cfg(feature = "conversion")]
    fn transpose_matches_c() {
        for version in VERSIONS {
            for width in [1usize, 2, 3, 4, 8, 11, 32] {
                for count in [1usize, 2, 9, 300] {
                    let data = support::noise(width * count, (width * count) as u32);
                    let stored = columns(&data, width).concat();
                    // serial -> struct of `count`-byte elements -> transpose.
                    let codecs = [
                        Codec {
                            id: 6,
                            header: {
                                let mut h = Vec::new();
                                varint(&mut h, count as u64);
                                h
                            },
                            variable: 0,
                            regens: vec![0],
                        },
                        Codec {
                            id: 2,
                            header: Vec::new(),
                            variable: 0,
                            regens: vec![0],
                        },
                    ];
                    let frame = frame(version, 1, data.len(), &chunk(version, &codecs, &[stored]));
                    let outputs = compare(&frame).expect("a valid frame");
                    assert_eq!(outputs[0].content(), data, "{width} x {count}");
                    assert_eq!(outputs[0].width(), width);
                    damage(&frame);
                }
            }
        }
    }

    #[test]
    fn the_fixed_split_variants_match_c() {
        for version in VERSIONS {
            for (id, width) in [(30, 2usize), (31, 4), (32, 8)] {
                for count in [0usize, 1, 5, 400] {
                    let data = support::noise(width * count, count as u32 + 1);
                    // Input `i` is the stream before the last `i`.
                    let mut stored = columns(&data, width);
                    stored.reverse();
                    let codec = Codec {
                        id,
                        header: Vec::new(),
                        variable: 0,
                        regens: vec![0],
                    };
                    let frame = frame(version, 1, data.len(), &chunk(version, &[codec], &stored));
                    let outputs = compare(&frame).expect("a valid frame");
                    assert_eq!(outputs[0].content(), data);
                    damage(&frame);
                }
            }
        }
    }

    #[test]
    fn unequal_split_columns_are_rejected_by_both() {
        let codec = Codec {
            id: 30,
            header: Vec::new(),
            variable: 0,
            regens: vec![0],
        };
        let stored = [vec![1u8, 2, 3], vec![4u8, 5]];
        let frame = frame(27, 1, 6, &chunk(27, &[codec], &stored));
        assert!(compare(&frame).is_none());
    }

    #[test]
    fn a_split_transpose_without_columns_is_rejected_by_both() {
        let codec = Codec {
            id: 4,
            header: Vec::new(),
            variable: 0,
            regens: vec![0],
        };
        assert!(compare(&frame(27, 1, 0, &chunk(27, &[codec], &[]))).is_none());
    }
}

#[cfg(feature = "split")]
mod split_without_segments {
    use super::built::*;
    use super::*;

    #[test]
    fn the_width_of_an_empty_split_comes_from_the_header() {
        for version in VERSIONS {
            // (codec, output type code, header)
            let cases: [(u32, u8, Vec<u8>); 6] = [
                (40, 0, vec![]),
                (47, 1, vec![7]),
                (47, 1, vec![1]),
                (48, 2, vec![4]),
                (48, 2, vec![8]),
                (48, 2, vec![1, 0xff]),
            ];
            for (id, ty, header) in cases {
                let codec = Codec {
                    id,
                    header,
                    variable: 0,
                    regens: vec![0],
                };
                let frame = frame(version, ty, 0, &chunk(version, &[codec], &[]));
                compare(&frame).expect("a valid frame");
            }
        }
    }

    #[test]
    fn invalid_widths_are_rejected_by_both() {
        let cases: [(u32, u8, Vec<u8>); 4] = [
            (47, 1, vec![0]),
            (48, 2, vec![3]),
            (48, 2, vec![0x80]),
            (40, 0, vec![0x80, 0x80]),
        ];
        for (id, ty, header) in cases {
            let codec = Codec {
                id,
                header,
                variable: 0,
                regens: vec![0],
            };
            let frame = frame(27, ty, 0, &chunk(27, &[codec], &[]));
            assert!(compare(&frame).is_none(), "{id}");
        }
    }
}

#[cfg(feature = "split")]
mod stream_limits {
    use super::built::*;

    /// A `splitn` over `inputs` stored empty streams.
    fn joined(version: i32, inputs: usize) -> Vec<u8> {
        let codec = Codec {
            id: 40,
            header: Vec::new(),
            variable: inputs,
            regens: vec![0],
        };
        let stored = vec![Vec::new(); inputs];
        frame(version, 0, 0, &chunk(version, &[codec], &stored))
    }

    #[test]
    fn a_codec_may_take_a_hundred_thousand_streams_and_no_more() {
        for version in [21, 27] {
            for inputs in [1000, 99_999, 100_000] {
                assert!(compare(&joined(version, inputs)).is_some());
            }
            // `ZL_transformOutStreamsLimit` is 100 000 inputs per codec.
            assert!(compare(&joined(version, 100_001)).is_none());
        }
    }
}

/// Random single-codec chunks over stored serial streams: mopenzl and the C
/// decoder accept the same frames and decode them to the same outputs.
#[cfg(all(feature = "transpose", feature = "split"))]
mod layouts {
    use super::built::*;
    use super::*;

    #[test]
    fn random_serial_codecs_are_accepted_alike() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |bound: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % bound
        };
        let (mut accepted, mut total) = (0, 0);
        for round in 0..20_000 {
            let version = [21, 24, 27][next(3) as usize];
            // (codec, fixed inputs, variable inputs allowed, output type)
            let (id, fixed, open, ty) = [
                (4u32, 0usize, true, 1u8),
                (30, 2, false, 1),
                (31, 4, false, 1),
                (32, 8, false, 1),
                (40, 0, true, 0),
                (47, 0, true, 1),
                (48, 0, true, 2),
            ][next(7) as usize];
            let variable = if open { next(6) as usize } else { 0 };
            let streams = fixed + variable;
            // Mostly equal lengths, so that the columns can agree.
            let len = next(6) as usize;
            let stored: Vec<Vec<u8>> = (0..streams)
                .map(|_| {
                    let len = if next(8) == 0 { next(6) as usize } else { len };
                    (0..len).map(|_| next(256) as u8).collect()
                })
                .collect();
            let header: Vec<u8> = if next(2) == 0 {
                Vec::new()
            } else {
                vec![[1u8, 2, 4, 8, 3, 0, 0x80][next(7) as usize]]
            };
            let codec = Codec {
                id,
                header,
                variable,
                regens: vec![0],
            };
            let size = if next(4) == 0 {
                next(40) as usize
            } else {
                stored.concat().len()
            };
            let frame = frame(version, ty, size, &chunk(version, &[codec], &stored));
            total += 1;
            let ours = Decompressor::default().decompress_typed(&frame);
            let theirs = graph::decompress(&frame);
            match (ours, theirs) {
                (Ok(ours), Ok(theirs)) => {
                    accepted += 1;
                    assert!(same(&ours, &theirs), "round {round}");
                }
                (Err(_), Err(_)) => {}
                (ours, theirs) => panic!(
                    "round {round}: ours {:?}, theirs {:?}, frame {frame:02x?}",
                    ours.map(|_| ()),
                    theirs.map(|_| ())
                ),
            }
        }
        assert!(accepted > total / 20, "{accepted} of {total}");
    }
}
