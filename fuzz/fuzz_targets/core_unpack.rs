#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use tpt_press_core::archive::{inspect, unpack, UnpackOptions};

fuzz_target!(|data: &[u8]| {
    let _ = inspect(Cursor::new(data));
    let mut out = Vec::new();
    let _ = unpack(Cursor::new(data), &mut out, &UnpackOptions::default());
});
