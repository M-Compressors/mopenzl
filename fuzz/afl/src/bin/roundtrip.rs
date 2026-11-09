//! Thin AFL adapter around [`mopenzl_afl::targets::roundtrip`].

fn main() {
    afl::fuzz!(|data: &[u8]| {
        mopenzl_afl::targets::roundtrip(data);
    });
}
