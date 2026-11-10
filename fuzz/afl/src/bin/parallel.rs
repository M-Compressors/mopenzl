//! AFL adapter for caller-scheduled compression.
fn main() {
    afl::fuzz!(|data: &[u8]| {
        mopenzl_afl::targets::parallel(data);
    });
}
