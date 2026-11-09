//! Every host backend writes and reads the same bytes.
#![cfg(all(feature = "compression", feature = "decompression"))]

mod support;

use mopenzl::{Backend, Compressor, Decompressor, EncoderConfig};

#[test]
fn every_backend_matches_the_default_backend() {
    let mut reference = Compressor::default();
    for (name, input) in support::corpus() {
        let expected = reference.compress(&input).unwrap();
        for backend in Backend::available() {
            let mut encoder = Compressor::builder(EncoderConfig::default())
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
