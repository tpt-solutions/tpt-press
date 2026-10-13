//! Packs one JSON Lines input with and without the trained zstd dictionary, and
//! prints both sizes. The dictionary is on by default, so measure it on your
//! own data before relying on it.
//!
//! On this synthetic input the dictionary makes the archive several times
//! larger, at every size tried. That is a known problem, not a fixed behaviour,
//! and it is why the numbers are printed here rather than assumed.
//!
//! Run with `cargo run -p tpt-press-logs --example dictionary_compare --release`.

use std::io::Cursor;

use tpt_press_logs::{pack, Format, PackOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut lines = String::new();
    for i in 0..200_000u32 {
        lines.push_str(&format!(
            "{{\"timestamp\":\"2026-05-03T{:02}:{:02}:{:02}Z\",\"service\":\"checkout\",\"region\":\"eu-west-{}\",\"status\":{},\"duration_ms\":{}}}\n",
            i / 3600 % 24,
            i / 60 % 60,
            i % 60,
            i % 3,
            if i % 23 == 0 { 500 } else { 200 },
            i * 31 % 900
        ));
    }
    let input = lines.into_bytes();

    for (label, dictionary) in [("without dictionary", false), ("with dictionary", true)] {
        let mut archive = Vec::new();
        let stats = pack(
            Cursor::new(&input),
            &mut archive,
            &PackOptions {
                format: Format::Jsonl,
                timestamp_field: "timestamp".to_owned(),
                block_size: 64 * 1024,
                dictionary,
                ..PackOptions::default()
            },
        )?;
        println!(
            "{label:<20} {:>9} bytes  ({} blocks, dictionary {} bytes)",
            stats.archive_bytes, stats.blocks, stats.dictionary_bytes
        );
    }
    Ok(())
}
