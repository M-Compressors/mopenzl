//! Unit tests of the public dictionary types.

use alloc::format;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use super::*;

fn dict(content: &[u8]) -> Dict {
    Dict::new(DictCodec::ZSTD, content.to_vec()).unwrap()
}

#[test]
fn ids_display_as_hex_and_convert_both_ways() {
    let id = DictId::from([0x0f; 32]);
    assert_eq!(id.to_string(), "0f".repeat(32));
    assert_eq!(format!("{id:?}"), format!("DictId({})", "0f".repeat(32)));
    assert_eq!(id.as_ref(), &[0x0f; 32]);
    assert_eq!(<[u8; 32]>::from(id), [0x0f; 32]);
    assert_eq!(BundleId::from([1; 32]).to_string().len(), 64);
}

#[test]
fn a_dictionary_is_identified_by_the_digest_of_its_content() {
    let first = dict(b"content");
    assert_eq!(
        first.id(),
        DictId::from(crate::shared::sha256::digest(b"content"))
    );
    assert_eq!(first.codec(), DictCodec::ZSTD);
    assert_eq!(first.content(), b"content");
    // An explicit ID sticks; zero restores the default.
    let named = first.clone().with_id(DictId::from([5; 32]));
    assert_eq!(named.id(), DictId::from([5; 32]));
    assert_eq!(named.with_id(DictId::from([0; 32])).id(), first.id());
}

#[test]
fn packed_dictionaries_round_trip_with_their_codec() {
    let custom = Dict::new(DictCodec::from((9, true)), b"abc".to_vec()).unwrap();
    let packed = Vec::from(&custom);
    let parsed = Dict::try_from(&packed[..]).unwrap();
    assert_eq!(parsed, custom);
    assert!(parsed.codec().is_custom());
    assert_eq!(parsed.codec().id(), 9);
    assert!(matches!(
        Dict::try_from(&packed[..10]),
        Err(DictError::Malformed(_))
    ));
}

#[test]
fn zstd_dictionaries_carry_their_level() {
    let dict = Dict::zstd(b"raw", 7).unwrap();
    assert_eq!(&dict.content()[..4], &1u32.to_le_bytes());
    assert_eq!(&dict.content()[4..8], &7i32.to_le_bytes());
    assert_eq!(&dict.content()[8..], b"raw");
}

#[test]
fn bundles_round_trip_through_the_fat_format() {
    let bundle = DictBundle::try_from(vec![dict(b"one"), dict(b"two"), dict(b"")]).unwrap();
    assert_eq!(bundle.dicts().len(), 3);
    let fat = Vec::from(&bundle);
    let parsed = DictBundle::try_from(&fat[..]).unwrap();
    assert_eq!(parsed, bundle);
    assert_eq!(parsed.id(), bundle.id());
    // The ID depends on the order.
    let swapped = DictBundle::try_from(vec![dict(b"two"), dict(b"one"), dict(b"")]).unwrap();
    assert_ne!(swapped.id(), bundle.id());
    // An empty bundle is valid.
    let empty = DictBundle::try_from(Vec::new()).unwrap();
    assert_eq!(DictBundle::try_from(&Vec::from(&empty)[..]).unwrap(), empty);
}

#[test]
fn malformed_bundles_are_reported() {
    let bundle = DictBundle::try_from(vec![dict(b"one")]).unwrap();
    let fat = Vec::from(&bundle);
    for cut in [0, 10, 41, 60, fat.len() - 1] {
        assert!(
            matches!(
                DictBundle::try_from(&fat[..cut]),
                Err(DictError::Malformed(_))
            ),
            "{cut}"
        );
    }
    let mut bad = fat.clone();
    bad[1] ^= 0xff;
    assert!(matches!(
        DictBundle::try_from(&bad[..]),
        Err(DictError::Malformed(_))
    ));
    let mut many = fat.clone();
    many[37..41].copy_from_slice(&70_000u32.to_le_bytes());
    assert_eq!(
        DictBundle::try_from(&many[..]),
        Err(DictError::TooManyDicts { count: 70_000 })
    );
    let too_many = vec![dict(b"x"); 0x1_0000];
    assert_eq!(
        DictBundle::try_from(too_many),
        Err(DictError::TooManyDicts { count: 0x1_0000 })
    );
}

#[test]
fn errors_describe_themselves_and_conflicts_are_found() {
    let id = DictId::from([3; 32]);
    let errors = [
        DictError::Malformed("x".into()),
        DictError::TooManyDicts { count: 1 },
        DictError::ContentTooLarge,
        DictError::ConflictingId(id),
        DictError::NoMaterializer {
            codec: DictCodec::ZSTD,
        },
        DictError::NoMaterializer {
            codec: DictCodec::from((1, true)),
        },
        DictError::Materialization("y".into()),
        DictError::StoreFull,
    ];
    assert!(errors.iter().all(|error| !error.to_string().is_empty()));
    assert!(errors[5].to_string().contains("custom"));
    #[cfg(feature = "decompression")]
    {
        let a = dict(b"a").with_id(id);
        let b = dict(b"b").with_id(id);
        assert_eq!(super::core::check_unique(&[a.clone(), a.clone()]), Ok(()));
        assert_eq!(
            super::core::check_unique(&[a, b]),
            Err(DictError::ConflictingId(id))
        );
    }
    assert_eq!(
        DictError::from(crate::shared::dictwire::WireError::ContentTooLarge),
        DictError::ContentTooLarge
    );
}

#[cfg(all(feature = "decompression", feature = "zstd"))]
mod store {
    use super::*;

    fn bundle(contents: &[&[u8]]) -> DictBundle {
        let dicts: Vec<Dict> = contents
            .iter()
            .map(|content| Dict::zstd(content, 3).unwrap())
            .collect();
        DictBundle::try_from(dicts).unwrap()
    }

    #[test]
    fn inserting_a_bundle_is_idempotent_and_shares_dictionaries() {
        let mut store = BundleStore::default();
        assert!(store.is_empty());
        let first = bundle(&[b"alpha history", b"beta history"]);
        let second = bundle(&[b"beta history"]);
        assert_eq!(store.insert(&first), Ok(first.id()));
        assert_eq!(store.insert(&first), Ok(first.id()));
        assert_eq!(store.insert(&second), Ok(second.id()));
        assert_eq!(store.len(), 2);
        assert!(store.contains(&second.id()));
        assert!(!store.contains(&BundleId::from([9; 32])));
        // The shared dictionary is one materialization.
        let a = store.get(&first.id()).unwrap();
        let b = store.get(&second.id()).unwrap();
        assert!(alloc::sync::Arc::ptr_eq(&a.dicts[1], &b.dicts[0]));
    }

    #[test]
    fn bad_bundles_leave_the_store_unchanged() {
        let mut store = BundleStore::default();
        // Not a Zstandard dictionary: no materializer.
        let other = DictBundle::try_from(vec![dict(b"x").with_id(DictId::from([1; 32]))]).unwrap();
        let wrong = Dict::new(DictCodec::from((1, false)), b"x".to_vec()).unwrap();
        let wrong = DictBundle::try_from(vec![wrong]).unwrap();
        assert!(matches!(
            store.insert(&wrong),
            Err(DictError::NoMaterializer { .. })
        ));
        // A content without the zstd envelope.
        assert!(matches!(
            store.insert(&other),
            Err(DictError::Materialization(_))
        ));
        // A Zstandard-format dictionary with a broken entropy section.
        let mut broken = 0xEC30_A437u32.to_le_bytes().to_vec();
        broken.extend_from_slice(&[1, 0, 0, 0, 0xff, 0xff]);
        let broken = DictBundle::try_from(vec![Dict::zstd(&broken, 3).unwrap()]).unwrap();
        assert!(matches!(
            store.insert(&broken),
            Err(DictError::Materialization(_))
        ));
        // One ID, two contents.
        let id = DictId::from([4; 32]);
        let conflicting = DictBundle::try_from(vec![
            Dict::zstd(b"a", 3).unwrap().with_id(id),
            Dict::zstd(b"b", 3).unwrap().with_id(id),
        ])
        .unwrap();
        assert_eq!(
            store.insert(&conflicting),
            Err(DictError::ConflictingId(id))
        );
        assert!(store.is_empty());
        // The same ID across bundles with another content is refused too.
        let first = DictBundle::try_from(vec![Dict::zstd(b"a", 3).unwrap().with_id(id)]).unwrap();
        // (A bundle with the same dictionary IDs is the same bundle.)
        let second = DictBundle::try_from(vec![
            Dict::zstd(b"b", 3).unwrap().with_id(id),
            Dict::zstd(b"c", 3).unwrap(),
        ])
        .unwrap();
        store.insert(&first).unwrap();
        assert_eq!(store.insert(&second), Err(DictError::ConflictingId(id)));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn a_store_holds_at_most_128_bundles() {
        let mut store = BundleStore::default();
        for n in 0..128u32 {
            store.insert(&bundle(&[&n.to_le_bytes()])).unwrap();
        }
        assert_eq!(
            store.insert(&bundle(&[b"one too many"])),
            Err(DictError::StoreFull)
        );
        // Re-inserting a known bundle still works.
        store.insert(&bundle(&[&0u32.to_le_bytes()])).unwrap();
    }

    #[test]
    fn a_store_holds_at_most_65535_dictionaries() {
        let mut store = BundleStore::default();
        // 1000 bundles of 66 tiny dictionaries each would exceed the bundle
        // limit first, so use the limit on dictionaries directly: three
        // bundles of 21845 distinct dictionaries.
        let make = |start: u32, count: u32| {
            let dicts: Vec<Dict> = (start..start + count)
                .map(|n| Dict::zstd(&n.to_le_bytes(), 1).unwrap())
                .collect();
            DictBundle::try_from(dicts).unwrap()
        };
        if cfg!(miri) {
            return;
        }
        store.insert(&make(0, 32_768)).unwrap();
        store.insert(&make(32_768, 32_767)).unwrap();
        assert_eq!(store.insert(&make(65_535, 1)), Err(DictError::StoreFull));
    }

    #[test]
    fn stores_build_from_bundles_and_fat_bytes() {
        let source = bundle(&[b"history"]);
        let from_bundle = BundleStore::try_from(&source).unwrap();
        assert!(from_bundle.contains(&source.id()));
        let fat = Vec::from(&source);
        let from_bytes = BundleStore::try_from(&fat[..]).unwrap();
        assert!(from_bytes.contains(&source.id()));
        assert!(matches!(
            BundleStore::try_from(&b"junk"[..]),
            Err(DictError::Malformed(_))
        ));
    }
}
