//! AFL adapter for custom codec registration and execution.
fn main() {
    afl::fuzz!(|data: &[u8]| {
        mopenzl_afl::targets::custom_codec(data);
    });
}
