#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use tpt_press_tensors::pytorch;

fuzz_target!(|data: &[u8]| {
    // Both checkpoint formats: the zip directory with its restricted pickle,
    // and the legacy run of pickles followed by storage records.
    let _ = pytorch::parse(&mut Cursor::new(data));
    let _ = pytorch::parse_legacy(&mut Cursor::new(data));
});
