//! Compatibility helpers for the parallel integration tests.
pub fn c_decompress(input: &[u8], size: usize) -> Result<Vec<u8>, openzl_ffi::Error> {
    let output = openzl_ffi::decompress(input)?;
    assert_eq!(output.len(), size);
    Ok(output)
}
pub struct Rng(u64);
impl Rng {
    pub const fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    pub fn bytes(&mut self, len: usize, alphabet: usize) -> Vec<u8> {
        (0..len)
            .map(|_| {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                (self.0 as usize % alphabet) as u8
            })
            .collect()
    }
}
