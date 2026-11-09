//! Unit tests of the CBOR codec: RFC 8949 vectors, error positions, limits,
//! and differential tests against the reference A1CBOR on generated and
//! mutated documents.

use alloc::borrow::Cow;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use openzl_ffi::enc_ml::{self, CborConfig};

use super::encode::{encode_to_slice, encoded_size};
use super::item::ErrorKind;
use super::{DecoderConfig, Error, Item, decode, encode_to_vec};

fn dec(bytes: &[u8]) -> Result<Item<'_>, Error> {
    decode(bytes, &DecoderConfig::default())
}

fn err(kind: ErrorKind, position: usize, depth: usize) -> Error {
    Error {
        kind,
        position,
        depth,
    }
}

/// `A1C_ErrorType` as the oracle numbers it.
fn c_kind(kind: ErrorKind) -> usize {
    match kind {
        ErrorKind::BadAlloc => 1,
        ErrorKind::Truncated => 2,
        ErrorKind::InvalidItemHeader => 3,
        ErrorKind::LargeIntegersUnsupported => 4,
        ErrorKind::IntegerOverflow => 5,
        ErrorKind::InvalidChunkedString => 6,
        ErrorKind::MaxDepthExceeded => 7,
        ErrorKind::InvalidSimpleEncoding => 8,
        ErrorKind::BreakNotAllowed => 9,
        ErrorKind::WriteFailed => 10,
        ErrorKind::InvalidSimpleValue => 11,
        ErrorKind::TrailingData => 13,
    }
}

#[test]
fn rfc_8949_integers_round_trip() {
    let cases: [(i64, &[u8]); 12] = [
        (0, &[0x00]),
        (23, &[0x17]),
        (24, &[0x18, 0x18]),
        (255, &[0x18, 0xFF]),
        (256, &[0x19, 0x01, 0x00]),
        (65535, &[0x19, 0xFF, 0xFF]),
        (65536, &[0x1A, 0, 1, 0, 0]),
        (4_294_967_296, &[0x1B, 0, 0, 0, 1, 0, 0, 0, 0]),
        (-1, &[0x20]),
        (-24, &[0x37]),
        (-25, &[0x38, 0x18]),
        (
            i64::MIN,
            &[0x3B, 0x7F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
        ),
    ];
    for (value, bytes) in cases {
        assert_eq!(encode_to_vec(&Item::Int(value)).unwrap(), bytes, "{value}");
        assert_eq!(dec(bytes).unwrap(), Item::Int(value), "{value}");
    }
    assert_eq!(
        encode_to_vec(&Item::Int(i64::MAX)).unwrap(),
        [0x1B, 0x7F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
    );
}

#[test]
fn integers_outside_i64_are_unsupported() {
    let big = [0x1B, 0x80, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(
        dec(&big),
        Err(err(ErrorKind::LargeIntegersUnsupported, 9, 1))
    );
    let neg = [0x3B, 0x80, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(
        dec(&neg),
        Err(err(ErrorKind::LargeIntegersUnsupported, 9, 1))
    );
}

#[test]
fn strings_arrays_maps_and_tags_round_trip() {
    let item = Item::map([
        (Item::text("a"), Item::array([Item::Int(1), Item::Int(2)])),
        (Item::Int(-7), Item::bytes(&[1, 2, 3])),
        (
            Item::text("tag"),
            Item::Tag(1, alloc::boxed::Box::new(Item::Int(1_363_896_240))),
        ),
    ]);
    let bytes = encode_to_vec(&item).unwrap();
    assert_eq!(
        bytes,
        [
            0xA3, 0x61, b'a', 0x82, 1, 2, 0x26, 0x43, 1, 2, 3, 0x63, b't', b'a', b'g', 0xC1, 0x1A,
            0x51, 0x4B, 0x67, 0xB0
        ]
    );
    assert_eq!(dec(&bytes).unwrap(), item);
    assert_eq!(encoded_size(&item).unwrap(), bytes.len());
}

#[test]
fn simple_values_and_floats_round_trip() {
    let items = [
        (Item::Bool(false), vec![0xF4]),
        (Item::Bool(true), vec![0xF5]),
        (Item::Null, vec![0xF6]),
        (Item::Undefined, vec![0xF7]),
        (Item::Simple(16), vec![0xF0]),
        (Item::Simple(255), vec![0xF8, 0xFF]),
        (Item::Float16(0x3C00), vec![0xF9, 0x3C, 0x00]),
        (Item::Float32(100_000.0), vec![0xFA, 0x47, 0xC3, 0x50, 0x00]),
        (
            Item::Float64(1.1),
            vec![0xFB, 0x3F, 0xF1, 0x99, 0x99, 0x99, 0x99, 0x99, 0x9A],
        ),
    ];
    for (item, bytes) in items {
        assert_eq!(encode_to_vec(&item).unwrap(), bytes);
        assert_eq!(dec(&bytes).unwrap(), item);
    }
}

#[test]
fn reserved_simple_values_cannot_be_encoded() {
    for value in 20..32 {
        let error = encode_to_vec(&Item::Simple(value)).unwrap_err();
        assert_eq!(error, err(ErrorKind::InvalidSimpleValue, 0, 1));
        assert_eq!(encoded_size(&Item::Simple(value)), Err(error));
    }
    let nested = Item::array([Item::Int(1), Item::Simple(25)]);
    assert_eq!(
        encode_to_vec(&nested).unwrap_err(),
        err(ErrorKind::InvalidSimpleValue, 2, 2)
    );
}

#[test]
fn definite_and_indefinite_containers_decode_to_the_same_items() {
    assert_eq!(
        dec(&[0x9F, 1, 2, 0xFF]).unwrap(),
        Item::array([Item::Int(1), Item::Int(2)])
    );
    assert_eq!(
        dec(&[0xBF, 0x61, b'k', 5, 0xFF]).unwrap(),
        Item::map([(Item::text("k"), Item::Int(5))])
    );
    assert_eq!(
        dec(&[0x7F, 0x62, b'a', b'b', 0x61, b'c', 0xFF]).unwrap(),
        Item::text("abc")
    );
    assert_eq!(
        dec(&[0x5F, 0x41, 1, 0x42, 2, 3, 0xFF]).unwrap(),
        Item::bytes(&[1, 2, 3])
    );
    assert_eq!(dec(&[0x5F, 0xFF]).unwrap(), Item::bytes(&[]));
}

#[test]
fn malformed_documents_fail_where_the_reference_does() {
    assert_eq!(dec(&[]), Err(err(ErrorKind::Truncated, 0, 1)));
    assert_eq!(dec(&[0x18]), Err(err(ErrorKind::Truncated, 1, 1)));
    assert_eq!(dec(&[0x1C]), Err(err(ErrorKind::InvalidItemHeader, 1, 1)));
    assert_eq!(dec(&[0x1F]), Err(err(ErrorKind::InvalidItemHeader, 1, 1)));
    assert_eq!(dec(&[0xDF]), Err(err(ErrorKind::InvalidItemHeader, 1, 1)));
    assert_eq!(dec(&[0xFF]), Err(err(ErrorKind::BreakNotAllowed, 1, 1)));
    assert_eq!(dec(&[0x00, 0x00]), Err(err(ErrorKind::TrailingData, 1, 0)));
    assert_eq!(dec(&[0x83, 1, 2]), Err(err(ErrorKind::Truncated, 1, 1)));
    assert_eq!(dec(&[0x42, 1]), Err(err(ErrorKind::Truncated, 1, 1)));
    assert_eq!(
        dec(&[0x5F, 0x61, 1, 0xFF]),
        Err(err(ErrorKind::InvalidChunkedString, 2, 1))
    );
    assert_eq!(
        dec(&[0x7F, 0x7F, 0xFF, 0xFF]),
        Err(err(ErrorKind::InvalidChunkedString, 2, 1))
    );
    assert_eq!(dec(&[0x9F, 1]), Err(err(ErrorKind::Truncated, 2, 1)));
    assert_eq!(dec(&[0xC1]), Err(err(ErrorKind::Truncated, 1, 2)));
    for floats in [
        &[0xF9, 0x3C][..],
        &[0xFA, 0, 0, 0],
        &[0xFB, 0, 0, 0, 0, 0, 0, 0],
    ] {
        assert_eq!(dec(floats), Err(err(ErrorKind::Truncated, 1, 1)));
    }
    for count in [
        &[0x19, 1][..],
        &[0x1A, 0, 0, 0],
        &[0x1B, 0, 0, 0, 0, 0, 0, 0],
    ] {
        assert_eq!(dec(count), Err(err(ErrorKind::Truncated, 1, 1)));
    }
    assert_eq!(dec(&[0xF8]), Err(err(ErrorKind::Truncated, 1, 1)));
    assert_eq!(
        dec(&[0xF8, 31]),
        Err(err(ErrorKind::InvalidSimpleEncoding, 2, 1))
    );
}

#[test]
fn unknown_simple_values_follow_the_configuration() {
    let strict = DecoderConfig::STRICT;
    assert_eq!(
        decode(&[0xF8, 40], &strict),
        Err(err(ErrorKind::InvalidSimpleEncoding, 1, 1))
    );
    assert_eq!(
        decode(&[0xF0], &strict),
        Err(err(ErrorKind::InvalidSimpleEncoding, 1, 1))
    );
    assert_eq!(decode(&[0xF4], &strict).unwrap(), Item::Bool(false));
    assert_eq!(dec(&[0xF8, 40]).unwrap(), Item::Simple(40));
}

#[test]
fn depth_is_limited_and_counts_every_level() {
    let nested = |levels: usize| {
        let mut bytes = vec![0x81; levels];
        bytes.push(0);
        bytes
    };
    // 127 arrays plus the integer is depth 128.
    assert!(dec(&nested(127)).is_ok());
    assert_eq!(
        dec(&nested(128)),
        Err(err(ErrorKind::MaxDepthExceeded, 128, 129))
    );
    let config = DecoderConfig {
        max_depth: 3,
        ..DecoderConfig::default()
    };
    assert!(decode(&nested(2), &config).is_ok());
    assert_eq!(
        decode(&nested(3), &config),
        Err(err(ErrorKind::MaxDepthExceeded, 3, 4))
    );
}

#[test]
fn the_allocation_limit_mirrors_the_reference_arena() {
    // Root item 32 bytes + array of 2 items 64 bytes.
    let bytes = [0x82, 1, 2];
    let limited = |limit| DecoderConfig {
        limit_bytes: limit,
        ..DecoderConfig::default()
    };
    assert!(decode(&bytes, &limited(96)).is_ok());
    assert_eq!(
        decode(&bytes, &limited(95)),
        Err(err(ErrorKind::BadAlloc, 1, 1))
    );
    // Copied strings are charged, referenced ones are not.
    let text = [0x63, b'a', b'b', b'c'];
    assert!(decode(&text, &limited(34)).is_err());
    assert!(decode(&text, &limited(34).with_reference()).is_ok());
}

impl DecoderConfig {
    fn with_reference(mut self) -> Self {
        self.reference_source = true;
        self
    }
}

#[test]
fn strings_borrow_the_source_only_when_asked() {
    let bytes = [0x43, 1, 2, 3];
    let borrowed = decode(&bytes, &DecoderConfig::STRICT).unwrap();
    assert!(matches!(&borrowed, Item::Bytes(Cow::Borrowed(_))));
    let copied = dec(&bytes).unwrap();
    assert!(matches!(&copied, Item::Bytes(Cow::Owned(_))));
    assert_eq!(borrowed, copied);
    let owned: Item<'static> = borrowed.into_owned();
    assert_eq!(owned.as_bytes(), Some(&[1, 2, 3][..]));
}

#[test]
fn lookups_and_accessors_behave_like_the_helpers() {
    let item = Item::map([
        (Item::text("n"), Item::Int(9)),
        (Item::text("n"), Item::Int(10)),
        (Item::Int(3), Item::Float64(0.5)),
        (Item::text("l"), Item::array([Item::Bool(true), Item::Null])),
    ]);
    assert_eq!(item.map_get_str("n"), Some(&Item::Int(9)));
    assert_eq!(item.map_get_int(3).and_then(Item::as_float64), Some(0.5));
    assert_eq!(item.map_get_str("missing"), None);
    assert_eq!(Item::Int(1).map_get_str("n"), None);
    let list = item.map_get_str("l").unwrap();
    assert_eq!(list.array_get(0).and_then(Item::as_bool), Some(true));
    assert_eq!(list.array_get(2), None);
    assert_eq!(Item::Null.array_get(0), None);
    assert_eq!(list.as_array().map(<[_]>::len), Some(2));
    assert_eq!(item.as_map().map(<[_]>::len), Some(4));
    assert_eq!(Item::Int(1).as_map(), None);
    assert_eq!(Item::Int(1).as_array(), None);
    // No implicit conversions between numeric item types.
    assert_eq!(Item::Float32(1.0).as_float64(), None);
    assert_eq!(Item::Float64(1.0).as_float32(), None);
    assert_eq!(Item::Float32(1.0).as_float32(), Some(1.0));
    assert_eq!(Item::Float16(7).as_float16(), Some(7));
    assert_eq!(Item::Int(1).as_float16(), None);
    assert_eq!(Item::Float64(1.0).as_int64(), None);
    assert_eq!(Item::Simple(5).as_simple(), Some(5));
    assert_eq!(Item::Int(5).as_simple(), None);
    assert_eq!(Item::Int(5).as_bool(), None);
    assert_eq!(Item::Int(5).as_bytes(), None);
    assert_eq!(Item::Int(5).as_text_bytes(), None);
    assert_eq!(Item::Bytes(Cow::Borrowed(b"x")).as_text(), None);
    assert_eq!(Item::text("x").as_text(), Some("x"));
    assert_eq!(Item::Text(Cow::Borrowed(&[0xFF])).as_text(), None);
    let tagged = Item::Tag(5, alloc::boxed::Box::new(Item::Null));
    assert_eq!(tagged.as_tag(), Some((5, &Item::Null)));
    assert_eq!(Item::Null.as_tag(), None);
}

#[test]
fn equality_compares_floats_by_bits_and_types_strictly() {
    assert_eq!(Item::Float64(f64::NAN), Item::Float64(f64::NAN));
    assert_ne!(Item::Float64(0.0), Item::Float64(-0.0));
    assert_ne!(Item::Float32(1.0), Item::Float64(1.0));
    assert_ne!(Item::Int(1), Item::Bool(true));
    assert_ne!(Item::Null, Item::Undefined);
    assert_ne!(Item::text("a"), Item::bytes(b"a"));
    assert_eq!(Item::default(), Item::Undefined);
}

#[test]
fn items_display_in_a_compact_notation() {
    let item = Item::array([
        Item::Int(1),
        Item::text("a"),
        Item::map([(Item::Null, Item::Bool(true))]),
        Item::Float32(1.5),
        Item::Float64(2.5),
        Item::Float16(0x3C00),
        Item::Simple(40),
        Item::bytes(&[1, 2]),
        Item::Undefined,
        Item::Tag(2, alloc::boxed::Box::new(Item::Int(3))),
    ]);
    assert_eq!(
        item.to_string(),
        "[1, \"a\", {null: true}, 1.5f, 2.5, f16(0x3c00), simple(40), h'2', undefined, 2(3)]"
    );
}

#[test]
fn a_full_destination_reports_the_bytes_written() {
    let item = Item::array([Item::Int(1000), Item::text("abc")]);
    let bytes = encode_to_vec(&item).unwrap();
    assert_eq!(bytes.len(), 1 + 3 + 1 + 3);
    let mut full = vec![0u8; bytes.len()];
    assert_eq!(encode_to_slice(&item, &mut full), Ok(bytes.len()));
    assert_eq!(full, bytes);
    for cap in 0..bytes.len() {
        let mut dst = vec![0u8; cap];
        let error = encode_to_slice(&item, &mut dst).unwrap_err();
        assert_eq!(error.kind, ErrorKind::WriteFailed, "cap {cap}");
        assert_eq!(dst[..error.position], bytes[..error.position], "cap {cap}");
    }
}

/// xorshift64*.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Appends a header with a randomly widened argument.
fn push_header(rng: &mut Rng, out: &mut Vec<u8>, major: u8, count: u64) {
    let widths: &[u8] = if count < 24 {
        &[0, 24, 25, 26, 27]
    } else if count < 256 {
        &[24, 25, 26, 27]
    } else if count < 65536 {
        &[25, 26, 27]
    } else if count < 1 << 32 {
        &[26, 27]
    } else {
        &[27]
    };
    let width = widths[rng.below(widths.len() as u64) as usize];
    if width == 0 {
        out.push(major << 5 | count as u8);
        return;
    }
    out.push(major << 5 | width);
    match width {
        24 => out.push(count as u8),
        25 => out.extend((count as u16).to_be_bytes()),
        26 => out.extend((count as u32).to_be_bytes()),
        _ => out.extend(count.to_be_bytes()),
    }
}

/// Appends a random well-formed (but not canonical) document.
fn raw_item(rng: &mut Rng, out: &mut Vec<u8>, depth: u32) {
    let kind = rng.below(if depth > 5 { 8 } else { 13 });
    match kind {
        0 | 1 => {
            let shift = rng.below(64);
            let count = rng.next() >> shift >> 1;
            push_header(rng, out, kind as u8, count);
        }
        2 | 3 => {
            let len = rng.below(6);
            let major = kind as u8;
            if rng.below(4) == 0 {
                out.push(major << 5 | 31);
                for _ in 0..rng.below(3) {
                    let chunk = rng.below(4);
                    push_header(rng, out, major, chunk);
                    out.extend((0..chunk).map(|_| rng.next() as u8));
                }
                out.push(0xFF);
            } else {
                push_header(rng, out, major, len);
                out.extend((0..len).map(|_| rng.next() as u8));
            }
        }
        4 => out.push(0xF4 + rng.below(4) as u8),
        5 => {
            out.push(0xF9);
            out.extend((rng.next() as u16).to_be_bytes());
        }
        6 => {
            out.push(0xFA);
            out.extend((rng.next() as u32).to_be_bytes());
        }
        7 => {
            out.push(0xFB);
            out.extend(rng.next().to_be_bytes());
        }
        8 | 9 => {
            let count = rng.below(4);
            let major = if kind == 8 { 4 } else { 5 };
            let indefinite = rng.below(3) == 0;
            if indefinite {
                out.push(major << 5 | 31);
            } else {
                push_header(rng, out, major, count);
            }
            for _ in 0..count * (1 + u64::from(major == 5)) {
                raw_item(rng, out, depth + 1);
            }
            if indefinite {
                out.push(0xFF);
            }
        }
        10 => {
            let tag = rng.below(100);
            push_header(rng, out, 6, tag);
            raw_item(rng, out, depth + 1);
        }
        11 => {
            out.push(0xF8);
            out.push(rng.next() as u8);
        }
        _ => out.push(0xE0 | rng.below(20) as u8),
    }
}

fn mutate(rng: &mut Rng, bytes: &mut Vec<u8>) {
    match rng.below(5) {
        0 if !bytes.is_empty() => {
            let at = rng.below(bytes.len() as u64) as usize;
            bytes.truncate(at);
        }
        1 if !bytes.is_empty() => {
            let at = rng.below(bytes.len() as u64) as usize;
            bytes[at] = rng.next() as u8;
        }
        2 => bytes.push(rng.next() as u8),
        3 if !bytes.is_empty() => {
            let at = rng.below(bytes.len() as u64) as usize;
            bytes[at] ^= 1 << rng.below(8);
        }
        _ => {}
    }
}

fn compare_with_reference(bytes: &[u8], config: DecoderConfig) {
    let oracle = CborConfig {
        max_depth: config.max_depth,
        limit_bytes: config.limit_bytes,
        reference_source: config.reference_source,
        reject_unknown_simple: config.reject_unknown_simple,
    };
    let expected = enc_ml::cbor_roundtrip(bytes, oracle);
    match (decode(bytes, &config), expected) {
        (Ok(item), Ok(c_bytes)) => {
            assert_eq!(encode_to_vec(&item).unwrap(), c_bytes, "{bytes:02x?}");
        }
        (Err(error), Err(c_error)) => {
            let ours = (c_kind(error.kind), error.position, error.depth);
            assert_eq!(
                ours,
                (c_error.kind, c_error.position, c_error.depth),
                "{bytes:02x?} {config:?}"
            );
        }
        (ours, theirs) => panic!("{bytes:02x?} {config:?}: {ours:?} vs {theirs:?}"),
    }
}

#[test]
fn decoding_matches_the_reference_on_generated_and_mutated_documents() {
    let rounds = if cfg!(miri) { 20 } else { 4000 };
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let configs = [
        DecoderConfig::default(),
        DecoderConfig::STRICT,
        DecoderConfig {
            max_depth: 3,
            ..DecoderConfig::default()
        },
        DecoderConfig {
            limit_bytes: 150,
            ..DecoderConfig::default()
        },
        DecoderConfig {
            limit_bytes: 200,
            reference_source: true,
            ..DecoderConfig::default()
        },
    ];
    for round in 0..rounds {
        let mut bytes = Vec::new();
        raw_item(&mut rng, &mut bytes, 0);
        for _ in 0..round % 3 {
            mutate(&mut rng, &mut bytes);
        }
        for config in configs {
            compare_with_reference(&bytes, config);
        }
    }
}

#[test]
fn raw_bytes_decode_like_the_reference() {
    let rounds = if cfg!(miri) { 20 } else { 3000 };
    let mut rng = Rng(0xDEAD_BEEF_1234_5678);
    for _ in 0..rounds {
        let len = rng.below(12) as usize;
        let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        compare_with_reference(&bytes, DecoderConfig::default());
        compare_with_reference(&bytes, DecoderConfig::STRICT);
    }
}

#[test]
fn encoding_to_a_capped_buffer_matches_the_reference() {
    let rounds = if cfg!(miri) { 10 } else { 300 };
    let mut rng = Rng(0x1357_9BDF_0246_8ACE);
    for _ in 0..rounds {
        let mut bytes = Vec::new();
        raw_item(&mut rng, &mut bytes, 0);
        let Ok(item) = dec(&bytes) else {
            continue;
        };
        let full = encode_to_vec(&item).unwrap();
        assert_eq!(encoded_size(&item), Ok(full.len()));
        for cap in [
            0,
            1,
            2,
            full.len() / 2,
            full.len().saturating_sub(1),
            full.len(),
        ] {
            let mut dst = vec![0u8; cap];
            let ours = encode_to_slice(&item, &mut dst);
            let theirs = enc_ml::cbor_encode_capped(&bytes, cap);
            match (ours, theirs) {
                (Ok(written), Ok((c_bytes, c_size))) => {
                    assert_eq!(&dst[..written], &c_bytes[..]);
                    assert_eq!(c_size, full.len());
                }
                (Err(error), Err(c_error)) => assert_eq!(
                    (c_kind(error.kind), error.position, error.depth),
                    (c_error.kind, c_error.position, c_error.depth),
                    "cap {cap} {bytes:02x?}"
                ),
                (ours, theirs) => panic!("cap {cap} {bytes:02x?}: {ours:?} vs {theirs:?}"),
            }
        }
    }
}

#[test]
fn reserved_simple_values_fail_like_the_reference() {
    for value in 0..=255u8 {
        let ours = encode_to_vec(&Item::Simple(value));
        let theirs = enc_ml::cbor_encode_simple(value, 8);
        match (ours, theirs) {
            (Ok(bytes), Ok(c_bytes)) => assert_eq!(bytes, c_bytes, "{value}"),
            (Err(error), Err(c_error)) => assert_eq!(
                (c_kind(error.kind), error.position, error.depth),
                (c_error.kind, c_error.position, c_error.depth),
                "{value}"
            ),
            (ours, theirs) => panic!("{value}: {ours:?} vs {theirs:?}"),
        }
    }
}
