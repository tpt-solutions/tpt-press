#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use tpt_press_tensors::{gguf, onnx, safetensors, Archive};

fuzz_target!(|data: &[u8]| {
    let _ = safetensors::parse(&mut Cursor::new(data));
    let _ = gguf::parse(&mut Cursor::new(data));
    let _ = onnx::parse(&mut Cursor::new(data));
    if let Ok(mut archive) = Archive::open(Cursor::new(data)) {
        let names: Vec<String> = archive.tensors().iter().map(|t| t.name.clone()).collect();
        for name in names {
            let _ = archive.read_tensor(&name);
        }
        let _ = archive.verify();
    }
});
