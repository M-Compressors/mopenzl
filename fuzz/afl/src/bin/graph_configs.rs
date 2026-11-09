//! Thin AFL adapter around [`mopenzl_afl::targets::graph_configs`].

fn main() {
    afl::fuzz!(|data: &[u8]| {
        mopenzl_afl::targets::graph_configs(data);
    });
}
