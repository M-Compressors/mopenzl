//! libFuzzer adapter for [`mopenzl_afl::targets::stream_compress`],
//! the body the AFL binary of the same name runs.

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| mopenzl_afl::targets::stream_compress(data));
