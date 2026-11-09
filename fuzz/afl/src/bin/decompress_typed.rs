//! Thin AFL adapter around [`mopenzl_afl::targets::decompress_typed`].

fn main() {
    afl::fuzz!(|data: &[u8]| {
        mopenzl_afl::targets::decompress_typed(data);
    });
}
