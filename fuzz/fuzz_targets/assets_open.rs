#![no_main]

use std::fs;
use std::path::PathBuf;

use libfuzzer_sys::fuzz_target;
use tpt_press_assets::Archive;

fuzz_target!(|data: &[u8]| {
    // The reader maps a file, so each input is written to a scratch file first.
    let path = PathBuf::from(std::env::temp_dir()).join("tpt-press-fuzz-assets.tpa");
    if fs::write(&path, data).is_err() {
        return;
    }
    if let Ok(archive) = Archive::open(&path) {
        let names: Vec<String> = archive.entries().iter().map(|e| e.name.clone()).collect();
        for name in names {
            let _ = archive.get(&name);
        }
        let _ = archive.verify();
    }
});
