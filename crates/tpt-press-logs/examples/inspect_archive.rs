//! Packs a JSON Lines log, then inspects the archive without decompressing any
//! record. Inspect reports the record count, the columns, the time range and
//! the sizes, which is what `tpt-press inspect` prints.
//!
//! Run with `cargo run -p tpt-press-logs --example inspect_archive`.

use std::io::Cursor;

use tpt_press_logs::{format_timestamp, inspect, pack, Format, PackOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Services emit different fields, so the archive has a union of columns.
    let mut lines = String::new();
    for i in 0..5_000u32 {
        let minute = i / 60;
        let ts = format!(
            "2026-05-02T{:02}:{:02}:{:02}Z",
            minute / 60 % 24,
            minute % 60,
            i % 60
        );
        if i % 3 == 0 {
            lines.push_str(&format!(
                "{{\"timestamp\":\"{ts}\",\"level\":\"info\",\"msg\":\"cache hit\",\"key\":\"item:{}\"}}\n",
                i % 400
            ));
        } else {
            lines.push_str(&format!(
                "{{\"timestamp\":\"{ts}\",\"level\":\"warn\",\"msg\":\"slow query\",\"ms\":{}}}\n",
                200 + i % 90
            ));
        }
    }

    let mut archive = Vec::new();
    pack(
        Cursor::new(lines.into_bytes()),
        &mut archive,
        &PackOptions {
            format: Format::Jsonl,
            timestamp_field: "timestamp".to_owned(),
            ..PackOptions::default()
        },
    )?;

    let summary = inspect(Cursor::new(&archive))?;
    println!("format:        {:?}", summary.format);
    println!("records:       {}", summary.records);
    println!("blocks:        {}", summary.blocks);
    println!("untimed:       {} block(s)", summary.untimed_blocks);
    println!("columns:       {}", summary.columns.join(", "));
    if let Some((first, last)) = summary.time_range {
        println!(
            "time range:    {} to {}",
            format_timestamp(first),
            format_timestamp(last)
        );
    }
    println!(
        "sizes:         {} bytes of columns, {} bytes archived",
        summary.uncompressed_bytes, summary.archive_bytes
    );
    Ok(())
}
