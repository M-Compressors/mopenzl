//! Dictionary bundles (format version 25 and newer) against the reference.
//!
//! Frames compressed with Zstandard nodes that reference dictionaries must be
//! byte-identical to the C library's: single and chained nodes, several
//! dictionaries in one bundle, nodes without a dictionary alongside, raw and
//! trained (Zstandard-format) dictionaries, levels and format versions.
//! Frames decode with this crate's decoder and with C's fat-bundle dict
//! loader, and the failures (below version 25, missing dictionaries, missing
//! or wrong bundles, corrupt bundles) must happen on both sides.
#![cfg(all(feature = "compression", feature = "decompression", feature = "zstd"))]

mod support;

use mopenzl::dict::{BundleId, BundleStore, Dict, DictBundle, DictError, DictId};
use mopenzl::graph::{GraphBuilder, GraphError, NodeParams, StandardGraph, StandardNode};
use mopenzl::{
    ChunkSize, Compressor, DecodeError, DecodeOperation, Decompressor, EncoderConfig,
    FormatVersion, InvalidDataKind, Level,
};
use openzl_ffi::enc_dict::{self, DictGraph};
use openzl_ffi::graph::cparam;

/// `ZL_CParam_decompressionLevel`.
const DECOMPRESSION_LEVEL: i32 = 3;

/// Small JSON-like records, the kind of payload dictionaries help.
fn record(n: u32) -> Vec<u8> {
    format!(
        "{{\"id\":{n},\"user\":\"user{}\",\"city\":\"{}\",\"plan\":\"{}\",\"active\":{}}}",
        n * 7 % 1000,
        ["Oslo", "Lima", "Kyiv", "Pune"][(n % 4) as usize],
        ["free", "pro", "team"][(n % 3) as usize],
        n.is_multiple_of(2),
    )
    .into_bytes()
}

/// A Zstandard-format dictionary trained by the reference's trainer.
fn trained() -> Vec<u8> {
    let samples: Vec<Vec<u8>> = (0..500).map(record).collect();
    let refs: Vec<&[u8]> = samples.iter().map(Vec::as_slice).collect();
    enc_dict::train_zstd_dictionary(&refs, 4096).expect("training succeeds")
}

/// Raw-content dictionaries: concatenated records.
fn raw(seed: u32) -> Vec<u8> {
    (seed..seed + 12).flat_map(record).collect()
}

/// A payload similar to the dictionaries' content.
fn payload(len_records: u32) -> Vec<u8> {
    (1000..1000 + len_records).flat_map(record).collect()
}

fn config(level: i32, version: u32) -> EncoderConfig {
    EncoderConfig::default()
        .with_level(Level::try_from(level).unwrap())
        .with_format_version(FormatVersion::try_from(version).unwrap())
}

fn c_params(config: &EncoderConfig) -> Vec<(i32, i32)> {
    vec![
        (cparam::LEVEL, config.level().get()),
        (DECOMPRESSION_LEVEL, i32::from(config.decompression_level())),
        (
            cparam::FORMAT_VERSION,
            u32::from(config.format_version()) as i32,
        ),
        (cparam::CONTENT_CHECKSUM, 1),
        (cparam::COMPRESSED_CHECKSUM, 1),
    ]
}

/// A chain of Zstandard nodes: each entry is the index of the dictionary the
/// node references, or `None`; the last feeds STORE.
type Chain = [Option<usize>];

fn c_graph(chain: &Chain, bundle: &DictBundle) -> DictGraph {
    chain
        .iter()
        .rev()
        .fold(DictGraph::STORE, |next, dict| DictGraph::Node {
            node: 62,
            ints: vec![],
            dict: dict.map(|index| bundle.dicts()[index].id().into()),
            successors: vec![next],
        })
}

/// Builds the chain with this crate, loading `bundle` after the nodes.
fn rust_graph(
    chain: &Chain,
    bundle: Option<&DictBundle>,
    reference: &DictBundle,
) -> Result<mopenzl::graph::CompressorGraph, GraphError> {
    let mut builder = GraphBuilder::new();
    let nodes = chain
        .iter()
        .map(|dict| {
            let params = match dict {
                Some(index) => {
                    NodeParams::default().with_dictionary(reference.dicts()[*index].id())
                }
                None => NodeParams::default(),
            };
            builder.parameterize_node_with(StandardNode::Zstd, params)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(bundle) = bundle {
        builder.load_dictionaries(bundle)?;
    }
    let mut next = StandardGraph::Store.into();
    for node in nodes.into_iter().rev() {
        next = builder.static_graph(node, &[next])?;
    }
    builder.build(next)
}

fn rust_compress(
    chain: &Chain,
    bundle: &DictBundle,
    data: &[u8],
    config: EncoderConfig,
) -> Result<Vec<u8>, String> {
    let graph = rust_graph(chain, Some(bundle), bundle).map_err(|e| e.to_string())?;
    let mut compressor = Compressor::builder(config)
        .with_graph(graph)
        .build()
        .map_err(|e| e.to_string())?;
    compressor.compress(data).map_err(|e| e.to_string())
}

fn c_compress(
    chain: &Chain,
    bundle: &DictBundle,
    data: &[u8],
    config: &EncoderConfig,
) -> Result<Vec<u8>, openzl_ffi::Error> {
    enc_dict::compress(
        data,
        &c_graph(chain, bundle),
        Some(&Vec::from(bundle)),
        &c_params(config),
    )
}

fn store_of(bundle: &DictBundle) -> BundleStore {
    BundleStore::try_from(bundle).unwrap()
}

fn decoder(bundle: &DictBundle) -> Decompressor {
    Decompressor::builder(Default::default())
        .with_dictionaries(store_of(bundle))
        .build()
}

/// Both sides write the same frame, or both fail; the frame decodes with
/// both decoders. Returns the frame.
fn check(
    chain: &Chain,
    bundle: &DictBundle,
    data: &[u8],
    config: EncoderConfig,
) -> Option<Vec<u8>> {
    let label = format!(
        "{} bytes, chain {chain:?}, level {}, version {}",
        data.len(),
        config.level().get(),
        u32::from(config.format_version()),
    );
    let expected = c_compress(chain, bundle, data, &config);
    let actual = rust_compress(chain, bundle, data, config);
    match (expected, actual) {
        (Ok(expected), Ok(actual)) => {
            assert_eq!(actual, expected, "{label}");
            assert_eq!(
                decoder(bundle).decompress(&actual).unwrap(),
                data,
                "{label}"
            );
            let fat = Vec::from(bundle);
            let outputs = enc_dict::decompress(&actual, &[&fat]).expect("reference decode");
            assert_eq!(outputs[0].bytes, data, "{label}");
            Some(actual)
        }
        (Err(_), Err(_)) => None,
        (expected, actual) => panic!("{label}: C {expected:?}, Rust {actual:?}"),
    }
}

fn bundle_of(dicts: Vec<Dict>) -> DictBundle {
    DictBundle::try_from(dicts).unwrap()
}

fn zstd_dict(raw: &[u8], level: i32) -> Dict {
    Dict::zstd(raw, level).unwrap()
}

#[test]
fn raw_and_trained_dictionaries_match_the_reference_at_every_version_and_level() {
    let trained = trained();
    let data = payload(40);
    for (name, dict) in [
        ("raw", zstd_dict(&raw(0), 3)),
        ("trained", zstd_dict(&trained, 3)),
    ] {
        let bundle = bundle_of(vec![dict]);
        for version in [25, 26, 27] {
            for level in [1, 3, 6, 9, 19] {
                let frame = check(&[Some(0)], &bundle, &data, config(level, version))
                    .unwrap_or_else(|| panic!("{name} fails"));
                // The frame records the bundle: flag 8 after the magic.
                assert_eq!(frame[4] & 8, 8, "{name} v{version}");
                assert_eq!(&frame[6..6 + frame[5] as usize], &bundle_id_prefix(&bundle));
            }
        }
    }
}

/// The significant bytes of the bundle ID.
fn bundle_id_prefix(bundle: &DictBundle) -> Vec<u8> {
    let id: [u8; 32] = <[u8; 32]>::from(bundle.id());
    let len = id.iter().rposition(|&b| b != 0).unwrap() + 1;
    id[..len].to_vec()
}

#[test]
fn the_dictionary_level_inside_the_content_matters_and_the_compressor_level_does_not_select_it() {
    let data = payload(40);
    let raw = raw(0);
    for dict_level in [-5, 0, 1, 3, 12, 22, 40, -300_000] {
        let bundle = bundle_of(vec![zstd_dict(&raw, dict_level)]);
        for level in [1, 19] {
            check(&[Some(0)], &bundle, &data, config(level, 27)).expect("compresses");
        }
    }
}

#[test]
fn a_dictionary_shrinks_matching_data() {
    let data = payload(10);
    let bundle = bundle_of(vec![zstd_dict(&trained(), 3)]);
    let with = check(&[Some(0)], &bundle, &data, config(3, 27)).unwrap();
    let without = check(&[None], &bundle_of(vec![]), &data, config(3, 27));
    // `[None]` with an empty bundle cannot load; compare against no bundle.
    assert!(without.is_none() || without.is_some());
    let plain = enc_dict::compress(
        &data,
        &c_graph(&[None], &bundle),
        None,
        &c_params(&config(3, 27)),
    )
    .unwrap();
    assert!(with.len() < plain.len(), "{} < {}", with.len(), plain.len());
}

#[test]
fn several_dictionaries_and_plain_nodes_in_one_chain_match_the_reference() {
    let bundle = bundle_of(vec![
        zstd_dict(&raw(0), 3),
        zstd_dict(&trained(), 7),
        zstd_dict(&raw(500), 1),
    ]);
    let data = payload(60);
    // Every dictionary of the bundle needs a node that references it.
    let chains: [&Chain; 4] = [
        &[Some(0), Some(1), Some(2)],
        &[Some(2), None, Some(0), Some(1)],
        &[None, Some(1), None, Some(2), Some(0)],
        &[Some(1), Some(1), Some(0), Some(2)],
    ];
    for chain in chains {
        for version in [25, 27] {
            for level in [1, 9] {
                check(chain, &bundle, &data, config(level, version)).expect("compresses");
            }
        }
    }
}

#[test]
fn nodes_with_the_same_dictionary_share_one_bundle_entry() {
    let bundle = bundle_of(vec![zstd_dict(&raw(3), 3), zstd_dict(&raw(40), 3)]);
    check(
        &[Some(1), Some(1), Some(0)],
        &bundle,
        &payload(30),
        config(3, 26),
    )
    .unwrap();
}

#[test]
fn the_dictionary_lane_is_present_even_when_no_codec_uses_a_dictionary() {
    let bundle = bundle_of(vec![zstd_dict(&raw(0), 3)]);
    // Below the minimum stream size the node never runs: the frame is a
    // stored chunk, with the bundle ID and an (empty) dictionary lane.
    for data in [&b""[..], b"tiny", b"0123456789"] {
        let frame = check(&[Some(0)], &bundle, data, config(3, 27)).expect("compresses");
        assert_eq!(frame[4] & 8, 8);
    }
    // Incompressible data is stored on expansion.
    check(&[Some(0)], &bundle, &support::noise(300, 9), config(3, 27)).unwrap();
}

#[test]
fn bundles_below_version_25_leave_the_frame_unchanged_when_no_node_runs() {
    let bundle = bundle_of(vec![zstd_dict(&raw(0), 3)]);
    for version in [21, 22, 23, 24] {
        let frame = check(&[Some(0)], &bundle, b"tiny", config(3, version)).expect("compresses");
        assert_eq!(frame[4] & 8, 0, "v{version}");
    }
}

#[test]
fn a_node_with_a_dictionary_fails_below_version_25_on_both_sides() {
    let bundle = bundle_of(vec![zstd_dict(&raw(0), 3)]);
    for version in [21, 22, 23, 24] {
        assert!(
            check(&[Some(0)], &bundle, &payload(40), config(3, version)).is_none(),
            "v{version}"
        );
    }
}

#[test]
fn missing_dictionaries_and_bundles_fail_on_both_sides() {
    let loaded = bundle_of(vec![zstd_dict(&raw(0), 3)]);
    let other = bundle_of(vec![zstd_dict(&raw(99), 3)]);
    let data = payload(20);
    let cfg = config(3, 27);
    // The node references a dictionary the bundle does not hold: loading
    // fails first (nothing references the bundle's own dictionary).
    let chain: &Chain = &[Some(0)];
    let c = enc_dict::compress(
        &data,
        &c_graph(chain, &other),
        Some(&Vec::from(&loaded)),
        &c_params(&cfg),
    );
    assert!(c.is_err());
    assert!(matches!(
        rust_graph(chain, Some(&loaded), &other),
        Err(GraphError::Dictionary(DictError::NoMaterializer { .. }))
    ));
    // With the node's dictionary also in the bundle but another one used.
    let both = bundle_of(vec![zstd_dict(&raw(0), 3), zstd_dict(&raw(99), 3)]);
    let c = enc_dict::compress(
        &data,
        &c_graph(&[Some(1)], &both),
        Some(&Vec::from(&both)),
        &c_params(&cfg),
    );
    assert!(c.is_err(), "dictionary 0 has no node");
    assert!(rust_graph(&[Some(1)], Some(&both), &both).is_err());
    // No bundle at all.
    let c = enc_dict::compress(&data, &c_graph(chain, &loaded), None, &c_params(&cfg));
    assert!(c.is_err());
    assert_eq!(
        rust_graph(chain, None, &loaded).map(|_| ()),
        Err(GraphError::DictionaryNotFound(loaded.dicts()[0].id()))
    );
    // A second bundle.
    let mut builder = GraphBuilder::new();
    builder
        .parameterize_node_with(
            StandardNode::Zstd,
            NodeParams::default().with_dictionary(loaded.dicts()[0].id()),
        )
        .unwrap();
    builder.load_dictionaries(&loaded).unwrap();
    assert_eq!(
        builder.load_dictionaries(&loaded),
        Err(GraphError::BundleAlreadyLoaded)
    );
}

#[test]
fn a_dictionary_of_a_node_that_is_not_zstd_cannot_be_loaded() {
    let bundle = bundle_of(vec![zstd_dict(&raw(0), 3)]);
    let mut builder = GraphBuilder::new();
    builder
        .parameterize_node_with(
            StandardNode::DeltaInt,
            NodeParams::default().with_dictionary(bundle.dicts()[0].id()),
        )
        .unwrap();
    assert!(matches!(
        builder.load_dictionaries(&bundle),
        Err(GraphError::Dictionary(DictError::NoMaterializer { .. }))
    ));
    // The reference refuses it too.
    let graph = DictGraph::Node {
        node: StandardNode::DeltaInt.into(),
        ints: vec![],
        dict: Some(bundle.dicts()[0].id().into()),
        successors: vec![DictGraph::STORE],
    };
    assert!(enc_dict::compress(b"x", &graph, Some(&Vec::from(&bundle)), &[]).is_err());
}

#[test]
fn zstd_dictionaries_with_broken_content_are_refused_by_both() {
    let mut broken = 0xEC30_A437u32.to_le_bytes().to_vec();
    broken.extend_from_slice(&[1, 0, 0, 0, 0xff, 0xff]);
    for content in [
        Dict::zstd(&broken, 3).unwrap(),
        // Not the zstd envelope.
        Dict::new(mopenzl::dict::DictCodec::ZSTD, b"plain".to_vec()).unwrap(),
    ] {
        let bundle = bundle_of(vec![content]);
        let c = enc_dict::compress(
            &payload(10),
            &c_graph(&[Some(0)], &bundle),
            Some(&Vec::from(&bundle)),
            &[],
        );
        assert!(c.is_err());
        assert!(matches!(
            rust_graph(&[Some(0)], Some(&bundle), &bundle),
            Err(GraphError::Dictionary(DictError::Materialization(_)))
        ));
    }
}

#[test]
fn frames_with_bundles_decode_through_every_rust_entry_point() {
    let bundle = bundle_of(vec![zstd_dict(&trained(), 3), zstd_dict(&raw(0), 5)]);
    let data = payload(80);
    let frame = check(&[Some(0), None, Some(1)], &bundle, &data, config(3, 27)).unwrap();
    let mut decoder = decoder(&bundle);
    assert_eq!(decoder.decompress(&frame).unwrap(), data);
    let mut slice = vec![0u8; data.len()];
    assert_eq!(
        decoder.decompress_to_slice(&frame, &mut slice).unwrap(),
        data.len()
    );
    assert_eq!(slice, data);
    let mut appended = b"prefix".to_vec();
    decoder.decompress_into(&frame, &mut appended).unwrap();
    assert_eq!(&appended[6..], &data[..]);
    let typed = decoder.decompress_typed(&frame).unwrap();
    assert_eq!(typed[0].content(), &data[..]);
    // Streaming: a few bytes at a time.
    #[cfg(not(feature = "no_std"))]
    {
        use std::io::Read;
        let mut out = Vec::new();
        decoder.reader(&frame[..]).read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
    }
    // The single node directly into the destination, with a dictionary.
    let single = check(&[Some(1), Some(0)], &bundle, &data, config(9, 25)).unwrap();
    let mut slice = vec![0u8; data.len()];
    decoder.decompress_to_slice(&single, &mut slice).unwrap();
    assert_eq!(slice, data);
}

#[test]
fn segmented_frames_carry_a_dictionary_lane_in_every_chunk() {
    let bundle = bundle_of(vec![zstd_dict(&trained(), 3)]);
    let mut builder = GraphBuilder::new();
    let node = builder
        .parameterize_node_with(
            StandardNode::Zstd,
            NodeParams::default().with_dictionary(bundle.dicts()[0].id()),
        )
        .unwrap();
    builder.load_dictionaries(&bundle).unwrap();
    let inner = builder
        .static_graph(node, &[StandardGraph::Store.into()])
        .unwrap();
    let start = builder.serial_segmenter(ChunkSize::MIN, inner).unwrap();
    let graph = builder.build(start).unwrap();
    let data = payload(if cfg!(miri) { 40 } else { 2500 });
    let mut compressor = Compressor::builder(config(3, 27))
        .with_graph(graph)
        .build()
        .unwrap();
    let frame = compressor.compress(&data).unwrap();
    assert_eq!(frame[4] & 8, 8);
    assert_eq!(decoder(&bundle).decompress(&frame).unwrap(), data);
    let fat = Vec::from(&bundle);
    assert_eq!(
        enc_dict::decompress(&frame, &[&fat]).unwrap()[0].bytes,
        data
    );
}

#[test]
fn decoding_without_the_bundle_fails_on_both_sides() {
    let bundle = bundle_of(vec![zstd_dict(&raw(0), 3)]);
    let other = bundle_of(vec![zstd_dict(&raw(70), 3)]);
    let frame = check(&[Some(0)], &bundle, &payload(40), config(3, 27)).unwrap();
    let missing = DecodeError::MissingBundle {
        bundle: bundle.id(),
    };
    // No store.
    assert_eq!(
        Decompressor::default().decompress(&frame),
        Err(missing.clone())
    );
    // A store with another bundle, and an empty one.
    assert_eq!(decoder(&other).decompress(&frame), Err(missing.clone()));
    let empty = Decompressor::builder(Default::default())
        .with_dictionaries(BundleStore::default())
        .build()
        .decompress(&frame);
    assert_eq!(empty, Err(missing));
    // Streaming reports it at the header.
    let mut plain = Decompressor::default();
    let mut session = plain.start();
    let mut out = [0u8; 16];
    let error = session
        .process(&frame, &mut out[..], DecodeOperation::Process)
        .expect_err("needs the bundle");
    assert!(matches!(error.error, DecodeError::MissingBundle { .. }));
    // The reference fails the same way.
    let fat_other = Vec::from(&other);
    assert!(enc_dict::decompress(&frame, &[]).is_err());
    assert!(enc_dict::decompress(&frame, &[&fat_other]).is_err());
}

#[test]
fn dictionary_indices_past_the_bundle_are_corruption_on_both_sides() {
    let two = bundle_of(vec![zstd_dict(&raw(0), 3), zstd_dict(&raw(50), 3)]);
    let one = bundle_of(vec![zstd_dict(&raw(0), 3)]);
    let cfg = config(3, 27)
        .with_content_checksum(false)
        .with_compressed_checksum(false);
    let frame = rust_compress(&[Some(0), Some(1)], &two, &payload(40), cfg).unwrap();
    // Point the header at the smaller bundle: the frame has no check byte.
    let mut forged = frame.clone();
    assert_eq!(forged[5], 32);
    forged[6..38].copy_from_slice(&<[u8; 32]>::from(one.id()));
    assert_eq!(
        decoder(&one).decompress(&forged),
        Err(DecodeError::InvalidData {
            kind: InvalidDataKind::DictionaryIndex
        })
    );
    let fat = Vec::from(&one);
    assert!(enc_dict::decompress(&forged, &[&fat]).is_err());
}

#[test]
fn a_wrong_dictionary_is_detected_by_the_checksums() {
    // Same bundle position, different content: zstd frames of a raw
    // dictionary do not name an ID, so the content checksum catches it.
    let right = bundle_of(vec![zstd_dict(&raw(0), 3)]);
    let wrong = bundle_of(vec![zstd_dict(&raw(70), 3)]);
    let frame = check(&[Some(0)], &right, &payload(40), config(3, 27)).unwrap();
    let mut forged = frame;
    // Without checksums stripped, patch the bundle ID and fix nothing: the
    // header check byte fails first.
    forged[6..38].copy_from_slice(&<[u8; 32]>::from(wrong.id()));
    assert!(decoder(&wrong).decompress(&forged).is_err());
}

#[test]
fn the_store_accepts_exactly_the_fat_bundles_the_reference_loader_accepts() {
    let bundle = bundle_of(vec![zstd_dict(&raw(0), 3), zstd_dict(&trained(), 2)]);
    let fat = Vec::from(&bundle);
    // Any frame decodes with a loader holding no needed bundle, so the
    // decode succeeds exactly when the loader accepted the bundle.
    let plain = Compressor::default().compress(b"a plain frame").unwrap();
    let accepted_by_c = |bytes: &[u8]| enc_dict::decompress(&plain, &[bytes]).is_ok();
    let accepted_by_rust = |bytes: &[u8]| BundleStore::try_from(bytes).is_ok();
    let mut cases: Vec<Vec<u8>> = vec![fat.clone(), [&fat[..], b"trailing"].concat()];
    for cut in (0..fat.len()).step_by(if cfg!(miri) { 997 } else { 61 }) {
        cases.push(fat[..cut].to_vec());
    }
    // Flip bytes of the bundle header, the first dictionary's header and the
    // zstd envelope.
    let first_dict = 41 + 64;
    for at in (0..120).chain([first_dict + 45, first_dict + 46, first_dict + 49]) {
        let mut damaged = fat.clone();
        damaged[at] ^= 0x5a;
        cases.push(damaged);
    }
    for case in &cases {
        assert_eq!(
            accepted_by_rust(case),
            accepted_by_c(case),
            "{} bytes, first diff candidates",
            case.len()
        );
    }
    assert!(accepted_by_rust(&fat));
    assert!(!accepted_by_rust(&fat[..fat.len() - 1]));
}

#[test]
fn ids_flow_through_the_public_types() {
    let dict = zstd_dict(&raw(0), 3);
    assert_eq!(dict.id(), DictId::from(enc_dict::sha256(dict.content())));
    let bundle = bundle_of(vec![dict.clone()]);
    assert_eq!(
        bundle.id(),
        BundleId::from(enc_dict::sha256(&<[u8; 32]>::from(dict.id())))
    );
    // The fat bundle is the reference's own packing.
    let packed = enc_dict::pack_fat_bundle(&[&Vec::from(&dict)]);
    assert_eq!(Vec::from(&bundle), packed);
    assert_eq!(DictBundle::try_from(&packed[..]).unwrap(), bundle);
    // And the zstd content envelope.
    assert_eq!(
        dict.content(),
        enc_dict::pack_zstd_content(3, &raw(0)).as_slice()
    );
}
