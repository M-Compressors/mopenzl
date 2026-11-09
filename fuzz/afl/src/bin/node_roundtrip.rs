//! Thin AFL adapter around [`mopenzl_afl::targets::node_roundtrip`].

fn main() {
    afl::fuzz!(|data: &[u8]| {
        mopenzl_afl::targets::node_roundtrip(data);
    });
}
