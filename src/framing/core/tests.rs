use super::*;
use crate::Backend;

#[test]
fn checked_sizes_reservations_and_future_versions_report_specific_failures() {
    assert!(matches!(add(u64::MAX, 1), Err(E::Overflow)));
    assert!(matches!(index_size(usize::MAX), Err(E::Overflow)));
    assert!(matches!(
        reserve(&mut Vec::<u64>::new(), usize::MAX),
        Err(E::Allocation)
    ));
    assert!(matches!(number::<8>(&[], usize::MAX), Err(E::Overflow)));
    assert!(matches!(
        number::<8>(&[0; 7], 0),
        Err(E::UnexpectedEndOfInput)
    ));
    assert!(matches!(
        check_magic(b"MOZFRM\x02\0"),
        Err(E::UnsupportedVersion(2))
    ));
    assert!(
        validate(FramingConfig {
            max_resources: usize::MAX,
            ..Default::default()
        })
        .is_err()
    );
    assert!(
        validate(FramingConfig {
            max_metadata_bytes: usize::MAX,
            ..Default::default()
        })
        .is_err()
    );
}

#[test]
fn scalar_and_host_backends_write_and_read_identical_containers() {
    use crate::framing::{FramedCompressor, FramedDecompressor, FramedResource};
    let payload = [0x5a; 1024];
    let resources = [FramedResource::from(&payload[..])];
    let mut expected = None;
    for backend in Backend::available() {
        let raw = crate::Compressor::builder(Default::default())
            .with_backend(backend)
            .build()
            .unwrap();
        let bytes = FramedCompressor::from(raw)
            .compress(resources.as_slice().into())
            .unwrap();
        if let Some(previous) = &expected {
            assert_eq!(&bytes, previous);
        }
        expected = Some(bytes.clone());
        let raw = crate::Decompressor::builder(Default::default())
            .with_backend(backend)
            .build();
        assert_eq!(
            FramedDecompressor::from(raw)
                .decompress(&bytes)
                .unwrap()
                .resources[0]
                .outputs[0]
                .content(),
            payload
        );
    }
}

#[test]
fn checksum_valid_directory_cannot_overlap_skip_or_change_aggregate_sizes() {
    let level = Backend::SCALAR.0;
    let config = FramingConfig::default();
    let entries = [ResourceInfo {
        offset: 8,
        length: 100,
        decoded_size: 7,
    }];
    let bytes = suffix(&entries, 108, level, config).unwrap();
    let len = index_size(1).unwrap();
    assert_eq!(index(&bytes[..len], 108, level, config).unwrap(), entries);
    for (at, value) in [(8, 9u64), (16, 1u64), (24, config.max_output_bytes + 1)] {
        let mut changed = bytes[..len].to_vec();
        changed[at..at + 8].copy_from_slice(&value.to_le_bytes());
        let hash = xxh3::hash32(level, &changed[..len - 4]);
        changed[len - 4..].copy_from_slice(&hash.to_le_bytes());
        assert!(index(&changed, 108, level, config).is_err());
    }
    assert!(index(&bytes[..len], 109, level, config).is_err());
    assert!(index(&bytes[..len - 1], 108, level, config).is_err());
    assert!(metadata(&[b'A', b'B', 2, 0, 0, 0, 1]).is_err());
    assert!(metadata(b"A").is_err());
}

#[test]
fn checksum_valid_record_must_match_inner_content_size_and_exact_frame_extent() {
    use crate::framing::{FramedCompressor, FramedResource};
    let mut encoder = FramedCompressor::new(Default::default()).unwrap();
    let bytes = encoder
        .compress([FramedResource::from(&b"abc"[..])].as_slice().into())
        .unwrap();
    let length = decode::record_length(&bytes[8..], FramingConfig::default()).unwrap();
    let mut record = bytes[8..8 + length].to_vec();
    record[16..24].copy_from_slice(&4u64.to_le_bytes());
    let hash = xxh3::hash32(Backend::SCALAR.0, &record[..24]);
    record[24..28].copy_from_slice(&hash.to_le_bytes());
    let mut raw = crate::Decompressor::default();
    let info = ResourceInfo {
        offset: 8,
        length: length as u64,
        decoded_size: 4,
    };
    assert!(matches!(
        decode::resource(&mut raw, &record, info, FramingConfig::default()),
        Err(E::Invalid("resource decoded size"))
    ));
    let info = ResourceInfo {
        decoded_size: 3,
        ..info
    };
    assert!(decode::resource(&mut raw, &record, info, FramingConfig::default()).is_err());
    let len = u64::from_le_bytes(number(&record, 8).unwrap());
    record[8..16].copy_from_slice(&(len + 1).to_le_bytes());
    record[16..24].copy_from_slice(&3u64.to_le_bytes());
    let hash = xxh3::hash32(Backend::SCALAR.0, &record[..24]);
    record[24..28].copy_from_slice(&hash.to_le_bytes());
    record.push(0);
    let info = ResourceInfo {
        length: record.len() as u64,
        ..info
    };
    assert!(matches!(
        decode::resource(&mut raw, &record, info, FramingConfig::default()),
        Err(E::Decode(crate::DecodeError::TrailingData { .. }))
    ));
}
