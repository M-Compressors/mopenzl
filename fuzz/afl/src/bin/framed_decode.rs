//! Arbitrary framed-container parser input.
fn main() {
    afl::fuzz!(|data: &[u8]| mopenzl_afl::framed_targets::framed_decode(data));
}
