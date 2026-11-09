//! Thin AFL adapter around [`mopenzl_afl::targets::stream_compress`].

fn main() {
    afl::fuzz!(|data: &[u8]| {
        mopenzl_afl::targets::stream_compress(data);
    });
}
