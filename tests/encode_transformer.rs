//! Byte identity of the Transformer numeric graphs with the reference.
//!
//! `transformer_numeric` (30), `select_numeric` (16), `transformer_numeric1`
//! (90) and the private `transformer_*` family (70 to 89) run by graph ID on
//! numeric inputs of widths 1, 2, 4 and 8 and many data shapes (ramps, small
//! alphabets, sparse data, sorted data, multiples of a common factor, random
//! data, timestamps, floating-point bit patterns, ...) at levels 7, 10 and
//! 19 and format versions 21, 24 and 27. A pair of runs must agree: the same
//! frame, or a failure on both sides; every frame decodes back to its input
//! with this crate's decoder and the reference's.
//!
//! The decision logic itself (features, scorers, guards, static selectors)
//! is compared bit for bit with the reference by the unit tests of
//! `compressor::core::graphs::transformer`. Graph-level frames that need a
//! successor another package has not ported yet (the engine reports it as
//! "not ported" or as an invalid graph) are skipped and counted.
#![cfg(not(miri))]
#![cfg(all(
    feature = "compression",
    feature = "decompression",
    feature = "conversion",
    feature = "all-codecs"
))]

use mopenzl::graph::{GraphBuilder, StandardGraph};
use mopenzl::input::{Input, Numbers};
use mopenzl::{Compressor, Decompressor, EncoderConfig, FormatVersion, Level};
use openzl_ffi::graph::{self, Graph, Input as CInput, cparam};

/// A deterministic xorshift stream.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

fn pack(values: &[u64], width: usize) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes()[..width].to_vec())
        .collect()
}

/// Data shapes that drive the Transformer's branches, truncated to `width`
/// bytes per element.
fn shapes(width: usize, lengths: &[usize]) -> Vec<(String, Vec<u8>)> {
    let bits = (width * 8) as u32;
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let mut out = Vec::new();
    for &n in lengths {
        let mut rng = Rng::new(n as u64 * 31 + width as u64);
        let mut add = |name: &str, values: Vec<u64>| {
            let values: Vec<u64> = values.into_iter().map(|v| v & mask).collect();
            out.push((format!("{name}-w{width}-n{n}"), pack(&values, width)));
        };
        add("constant", vec![7; n]);
        add("zeros", vec![0; n]);
        add("ramp", (0..n as u64).collect());
        add("ramp-step3", (0..n as u64).map(|i| i * 3 + 5).collect());
        add("descending", (0..n as u64).rev().collect());
        add(
            "timestamps",
            (0..n as u64)
                .map(|i| 1_600_000_000_000 + i * 1000 + (i * 7919) % 13)
                .collect(),
        );
        add(
            "sorted-random",
            (0..n)
                .scan(0u64, |state, _| {
                    *state += rng.below(50);
                    Some(*state)
                })
                .collect(),
        );
        add("random", (0..n).map(|_| rng.next()).collect());
        add("random-4bit", (0..n).map(|_| rng.below(16)).collect());
        add("random-12bit", (0..n).map(|_| rng.below(4096)).collect());
        for k in [3u64, 40] {
            let alphabet: Vec<u64> = (0..k).map(|_| rng.next()).collect();
            add(
                &format!("alphabet-{k}"),
                (0..n).map(|_| alphabet[rng.below(k) as usize]).collect(),
            );
        }
        add(
            "sparse-1pct",
            (0..n)
                .map(|_| if rng.below(100) == 0 { rng.next() } else { 0 })
                .collect(),
        );
        add(
            "sparse-60pct",
            (0..n)
                .map(|_| {
                    if rng.below(10) < 4 {
                        rng.below(60) + 1
                    } else {
                        0
                    }
                })
                .collect(),
        );
        for factor in [24u64, 1000, 65536] {
            add(
                &format!("gcd-{factor}"),
                (0..n).map(|_| rng.below(5000) * factor).collect(),
            );
        }
        add(
            "signed-small",
            (0..n).map(|_| rng.below(40).wrapping_sub(20)).collect(),
        );
        add(
            "signed-wide",
            (0..n)
                .map(|_| rng.below(20000).wrapping_sub(10000))
                .collect(),
        );
        add("sawtooth", (0..n as u64).map(|i| i % 250).collect());
        add(
            "sawtooth-step200",
            (0..n as u64).map(|i| (i % 100) * 200).collect(),
        );
        add(
            "walk-wide-steps",
            (0..n)
                .scan(1u64 << 30, |state, _| {
                    *state =
                        state.wrapping_add((rng.below(601) as i64 - 300).wrapping_mul(1000) as u64);
                    Some(*state)
                })
                .collect(),
        );
        add(
            "smooth-signal",
            (0..n)
                .map(|i| (3000.0 + 2500.0 * (i as f64 / 40.0).sin()) as u64)
                .collect(),
        );
        match width {
            2 => add(
                "floats",
                (0..n)
                    .map(|i| 0x4000 + (((i as f32 / 17.0).sin() * 400.0) as u64 & 0x3FF))
                    .collect(),
            ),
            4 => add(
                "floats",
                (0..n)
                    .map(|i| u64::from(((i as f32 / 17.0).sin() * 100.0 + 200.0).to_bits()))
                    .collect(),
            ),
            8 => {
                add(
                    "floats",
                    (0..n)
                        .map(|i| ((i as f64 / 17.0).sin() * 100.0 + 200.0).to_bits())
                        .collect(),
                );
                add(
                    "floats-repeated",
                    (0..n)
                        .map(|i| (((i % 90) as f64) * 0.25 + 100.0).to_bits())
                        .collect(),
                );
            }
            _ => {}
        }
    }
    out
}

fn config(level: i32, version: u32) -> EncoderConfig {
    EncoderConfig::default()
        .with_level(Level::try_from(level).unwrap())
        .with_format_version(FormatVersion::try_from(version).unwrap())
}

fn c_params(config: &EncoderConfig) -> Vec<(i32, i32)> {
    vec![
        (cparam::LEVEL, config.level().get()),
        (
            cparam::FORMAT_VERSION,
            u32::from(config.format_version()) as i32,
        ),
        (cparam::MIN_STREAM_SIZE, config.min_stream_size() as i32),
    ]
}

/// What one comparison found.
#[derive(Debug, Eq, PartialEq)]
enum Outcome {
    Same,
    BothFailed,
    Skipped,
}

/// Compresses `bytes` (numbers of `width` bytes, 0 for serial) by graph
/// `graph_id` in C and here, and checks that the two agree.
fn compare(label: &str, graph_id: u32, cfg: EncoderConfig, bytes: &[u8], width: usize) -> Outcome {
    let c_input = if width == 0 {
        CInput::Serial(bytes)
    } else {
        CInput::Numeric { bytes, width }
    };
    let reference = graph::compress(&[c_input], &Graph::Id(graph_id), &c_params(&cfg));

    let builder = GraphBuilder::new();
    let start: mopenzl::graph::GraphId = StandardGraph::try_from(graph_id).unwrap().into();
    let mut compressor = Compressor::builder(cfg)
        .with_graph(builder.build(start).unwrap())
        .build()
        .unwrap();
    let input = if width == 0 {
        Input::from(bytes)
    } else {
        Input::from(Numbers::try_from((bytes, width)).unwrap())
    };
    let ours = compressor.compress_inputs(&[input]);

    match (reference, ours) {
        (Ok(expected), Ok(frame)) => {
            assert_eq!(frame, expected, "{label}: frames differ");
            let decoded = Decompressor::default()
                .decompress_typed(&frame)
                .unwrap_or_else(|error| panic!("{label}: mopenzl cannot decode: {error}"));
            assert_eq!(decoded.len(), 1, "{label}");
            assert_eq!(decoded[0].content(), bytes, "{label}: round trip");
            let c_decoded = graph::decompress(&frame)
                .unwrap_or_else(|error| panic!("{label}: C cannot decode: {error:?}"));
            assert_eq!(c_decoded[0].bytes, bytes, "{label}: C round trip");
            Outcome::Same
        }
        (Err(_), Err(_)) => Outcome::BothFailed,
        (Ok(_), Err(error))
            if error.to_string().contains("not ported")
                || error.to_string().contains("invalid graph") =>
        {
            Outcome::Skipped
        }
        (Ok(_), Err(error)) => panic!("{label}: C succeeds, we fail: {error}"),
        (Err(error), Ok(_)) => panic!("{label}: we succeed, C fails: {error:?}"),
    }
}

/// Counts of outcomes over a run.
#[derive(Debug, Default)]
struct Tally {
    same: usize,
    both_failed: usize,
    skipped: usize,
}

impl Tally {
    fn add(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Same => self.same += 1,
            Outcome::BothFailed => self.both_failed += 1,
            Outcome::Skipped => self.skipped += 1,
        }
    }

    fn report(&self, what: &str) {
        eprintln!(
            "{what}: {} identical, {} both failed, {} skipped (successor not ported)",
            self.same, self.both_failed, self.skipped
        );
        assert!(
            self.same > 0 && self.same >= self.skipped,
            "{what}: too few comparable frames: {self:?}"
        );
    }
}

const LENGTHS: [usize; 5] = [1, 10, 300, 1000, 2500];

fn run_graph(graph_id: u32, levels: &[i32], versions: &[u32], lengths: &[usize]) -> Tally {
    let mut tally = Tally::default();
    for width in [1usize, 2, 4, 8] {
        for (name, bytes) in shapes(width, lengths) {
            for &level in levels {
                for &version in versions {
                    let label = format!("graph {graph_id} level {level} v{version} {name}");
                    tally.add(compare(
                        &label,
                        graph_id,
                        config(level, version),
                        &bytes,
                        width,
                    ));
                }
            }
        }
    }
    tally
}

#[test]
fn transformer_numeric_matches_the_reference_at_every_level_and_version() {
    run_graph(30, &[7, 10, 19], &[21, 24, 27], &LENGTHS).report("transformer_numeric");
}

#[test]
fn transformer_numeric_matches_the_reference_on_large_inputs() {
    run_graph(30, &[7, 19], &[27], &[12_000]).report("transformer_numeric large");
}

#[test]
fn select_numeric_routes_to_the_transformer_from_level_seven() {
    // Below level 7 the reference takes the field_lz path, which skips here
    // until it is ported; from level 7 the transformer decides.
    run_graph(16, &[7, 10, 19], &[27], &[10, 300, 2500]).report("select_numeric");
}

#[test]
fn transformer_numeric1_matches_the_reference() {
    run_graph(90, &[7, 19], &[21, 27], &[10, 300, 2500]).report("transformer_numeric1");
}

#[test]
fn the_private_transformer_graphs_match_the_reference() {
    for graph_id in 70..=89 {
        run_graph(graph_id, &[7, 10], &[24, 27], &[10, 300, 2500])
            .report(&format!("graph {graph_id}"));
    }
}

#[test]
fn the_private_transformer_graphs_match_the_reference_on_old_format_versions() {
    for graph_id in [76, 82, 83, 89, 90] {
        run_graph(graph_id, &[7], &[21], &[300, 2500]).report(&format!("graph {graph_id} v21"));
    }
}

#[test]
fn transformer_graphs_reject_non_numeric_input_like_the_reference() {
    let serial: Vec<u8> = (0..400u32).map(|i| (i * 7) as u8).collect();
    for graph_id in [30, 90, 76, 82, 89] {
        let outcome = compare(
            &format!("graph {graph_id} on serial"),
            graph_id,
            config(7, 27),
            &serial,
            0,
        );
        assert!(
            matches!(outcome, Outcome::BothFailed | Outcome::Same),
            "{outcome:?}"
        );
    }
}

/// Inputs for the rare branches of the static selectors: the FSE gate's
/// rejections, the num16 rescue, the sparse num64 dictionaries and the
/// small-delta majority of tokenize indices.
fn rare_inputs() -> Vec<(String, Vec<u8>, usize)> {
    let mut rng = Rng::new(7);
    let mut out: Vec<(String, Vec<u8>, usize)> = Vec::new();

    // num8 gates.
    let mut skewed = vec![0u64; 200_000];
    skewed[100_000] = 1;
    out.push(("fse-skewed".into(), pack(&skewed, 1), 1));
    let random5: Vec<u64> = (0..4000).map(|_| rng.below(5)).collect();
    out.push(("fse-random5".into(), pack(&random5, 1), 1));
    let mut runs = Vec::new();
    while runs.len() < 6000 {
        let symbol = rng.below(4);
        let run = 6 + rng.below(12) as usize;
        runs.extend(std::iter::repeat_n(symbol, run));
    }
    out.push(("fse-runs".into(), pack(&runs, 1), 1));
    let dominant: Vec<u64> = (0..8000)
        .map(|_| {
            if rng.below(100) < 85 {
                0
            } else {
                150 + rng.below(3)
            }
        })
        .collect();
    out.push(("fse-dominant".into(), pack(&dominant, 1), 1));
    let mut state = 50u64;
    let smooth_indices: Vec<u64> = (0..20_000)
        .map(|_| {
            let step = if rng.below(10) < 9 {
                rng.below(5)
            } else {
                rng.below(100)
            };
            state = (state + step) % 100;
            state
        })
        .collect();
    out.push(("small-delta-majority".into(), pack(&smooth_indices, 1), 1));

    // num16 rescue.
    out.push((
        "dense-sawtooth".into(),
        pack(&(0..4000u64).map(|i| i % 400).collect::<Vec<_>>(), 2),
        2,
    ));
    let mut state = 1500i64;
    let mut jump = 0i64;
    let heavy_tail: Vec<u64> = (0..12_000i64)
        .map(|i| {
            let step = if i % 20 == 0 {
                jump += 1;
                (jump * 37) % 601 - 300
            } else {
                i % 11 - 5
            };
            state = (state + step).clamp(500, 2500);
            state as u64
        })
        .collect();
    out.push(("dense-heavy-tail".into(), pack(&heavy_tail, 2), 2));
    let mut grid: Vec<u64> = (0..100).map(|_| 1000 + rng.below(900)).collect();
    grid.sort_unstable();
    grid.dedup();
    let mut index = 50i64;
    let grid_walk: Vec<u64> = (0..14_000)
        .map(|_| {
            index = (index + rng.below(13) as i64 - 6).clamp(0, grid.len() as i64 - 1);
            grid[index as usize]
        })
        .collect();
    out.push(("grid-walk".into(), pack(&grid_walk, 2), 2));
    let mut big_grid: Vec<u64> = (0..2000).map(|_| rng.below(60_000)).collect();
    big_grid.sort_unstable();
    big_grid.dedup();
    let big_dictionary: Vec<u64> = (0..30_000)
        .map(|_| big_grid[rng.below(big_grid.len() as u64) as usize])
        .collect();
    out.push(("big-dictionary".into(), pack(&big_dictionary, 2), 2));
    let mut reversal: Vec<u64> = (0..3000u64).map(|i| 1000 + i).collect();
    reversal[1500] = 5;
    out.push(("rare-reversal".into(), pack(&reversal, 2), 2));

    // num64 dictionaries.
    let mut far: Vec<u64> = (0..800).map(|_| rng.next() / 2).collect();
    far.sort_unstable();
    far.dedup();
    let far_values: Vec<u64> = (0..10_000)
        .map(|_| far[rng.below(far.len() as u64) as usize])
        .collect();
    out.push(("far-dictionary".into(), pack(&far_values, 8), 8));
    let mut near: Vec<u64> = (0..800).map(|_| 1 + rng.below(1_000_000_000_000)).collect();
    near.sort_unstable();
    near.dedup();
    let mut near_runs = Vec::new();
    while near_runs.len() < 10_000 {
        let value = near[rng.below(near.len() as u64) as usize];
        near_runs.extend(std::iter::repeat_n(value, 1 + rng.below(14) as usize));
    }
    out.push(("near-runs".into(), pack(&near_runs, 8), 8));
    out
}

#[test]
fn the_static_selectors_match_the_reference_on_rare_branch_inputs() {
    let mut tally = Tally::default();
    for (name, bytes, width) in rare_inputs() {
        for graph_id in [76, 82, 89, 90] {
            for version in [21, 27] {
                let label = format!("graph {graph_id} v{version} {name}");
                tally.add(compare(&label, graph_id, config(7, version), &bytes, width));
            }
        }
    }
    tally.report("rare branches");
}
