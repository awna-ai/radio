#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    radio_fuzz::join_code(data);
});
