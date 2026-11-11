//! Framed resource commands, graphs and backpressure.
fn main() {
    afl::fuzz!(|data: &[u8]| mopenzl_afl::framed_targets::framed_roundtrip(data));
}
