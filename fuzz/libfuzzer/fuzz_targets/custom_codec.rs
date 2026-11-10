#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    mopenzl_afl::targets::custom_codec(data);
});
