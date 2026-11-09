//! Thin AFL adapter around [`mopenzl_afl::targets::decompress`].

fn main() {
    afl::fuzz!(|data: &[u8]| {
        mopenzl_afl::targets::decompress(data);
    });
}
