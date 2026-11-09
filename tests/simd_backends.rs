//! Every host backend writes and reads the same bytes.
#![cfg(all(feature = "compression", feature = "decompression"))]

mod support;

use mopenzl::{Backend, Compressor, Decompressor, EncoderConfig, Level};

#[test]
fn every_backend_matches_the_default_backend() {
    // Under Miri a cheaper level and the shapes that reach every checksum
    // path (short, mid-size and long inputs) keep the test under a minute.
    let config = if cfg!(miri) {
        EncoderConfig::from(Level::try_from(1).unwrap())
    } else {
        EncoderConfig::default()
    };
    let corpus = support::corpus()
        .into_iter()
        .filter(|(name, _)| !cfg!(miri) || !name.ends_with("-large"));
    let mut reference = Compressor::new(config).unwrap();
    for (name, input) in corpus {
        let expected = reference.compress(&input).unwrap();
        for backend in Backend::available() {
            let mut encoder = Compressor::builder(config)
                .with_backend(backend)
                .build()
                .unwrap();
            assert_eq!(encoder.backend(), backend);
            assert_eq!(
                encoder.compress(&input).unwrap(),
                expected,
                "{name} on {backend}"
            );
            let mut decoder = Decompressor::builder(Default::default())
                .with_backend(backend)
                .build();
            assert_eq!(
                decoder.decompress(&expected).unwrap(),
                input,
                "{name} on {backend}"
            );
        }
    }
}
