#![no_main]

use libfuzzer_sys::fuzz_target;
use tpt_press_tensors::Manifest;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = Manifest::parse(text);
    }
});
