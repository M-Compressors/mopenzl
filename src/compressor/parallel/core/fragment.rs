//! Independent OpenZL chunks obtained from a validated serial frame.
use crate::graph::CompressorGraph;
use crate::{Backend, Compressor, EncodeError, EncoderConfig};

pub(super) struct FragmentEncoder {
    encoder: Compressor,
    bytes: Vec<u8>,
    header: Vec<u8>,
}
pub(super) struct Fragment<'a> {
    pub(super) bytes: &'a [u8],
}
impl FragmentEncoder {
    pub(super) fn new(
        config: EncoderConfig,
        backend: Backend,
        graph: Option<&CompressorGraph>,
    ) -> Result<Self, crate::ConfigError> {
        Ok(Self {
            encoder: super::serial_compressor(config, backend, graph)?,
            bytes: Vec::new(),
            header: Vec::new(),
        })
    }
    pub(super) fn retained_bytes(&self) -> usize {
        self.encoder.retained_bytes() + self.bytes.capacity() + self.header.capacity()
    }
    #[cfg_attr(all(feature = "hotpath", not(feature = "no_std")), hotpath::measure)]
    pub(super) fn encode(
        &mut self,
        _history: &[u8],
        input: &[u8],
        _first: bool,
    ) -> Result<Fragment<'_>, EncodeError> {
        self.bytes.clear();
        self.encoder.compress_into(input, &mut self.bytes)?;
        self.header.clear();
        self.encoder
            .frame_header_into(input.len() as u64, &mut self.header)?;
        let start = self.header.len();
        let end = self
            .bytes
            .len()
            .checked_sub(1)
            .ok_or(EncodeError::Internal)?;
        let bytes = self.bytes.get(start..end).ok_or(EncodeError::Internal)?;
        Ok(Fragment { bytes })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn baseline_and_every_available_backend_produce_identical_chunks() {
        let input = b"independent chunks ".repeat(4000);
        let config = EncoderConfig::default();
        let mut baseline = FragmentEncoder::new(config, Backend::SCALAR, None).unwrap();
        let expected = baseline.encode(&[], &input, false).unwrap().bytes.to_vec();
        for backend in Backend::available() {
            let mut encoder = FragmentEncoder::new(config, backend, None).unwrap();
            assert_eq!(encoder.encode(&[], &input, false).unwrap().bytes, expected);
        }
    }
}
