//! Packs JSON Lines into a logs archive, then queries one time range and
//! two columns.
//!
//! Run with `cargo run -p tpt-press-logs --example query_time_range`.

use std::io::Cursor;

use tpt_press_logs::{pack, parse_timestamp, query, Format, PackOptions, Query};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // One record a minute for a day, so every timestamp is valid.
    let lines: String = (0..1_440)
        .map(|i| {
            format!(
                "{{\"timestamp\":\"2026-03-01T{:02}:{:02}:00Z\",\"level\":\"{}\",\"ms\":{}}}\n",
                i / 60,
                i % 60,
                if i % 10 == 0 { "warn" } else { "info" },
                i * 3 % 500
            )
        })
        .collect();

    let mut archive = Vec::new();
    let stats = pack(
        Cursor::new(lines.into_bytes()),
        &mut archive,
        &PackOptions {
            format: Format::Jsonl,
            timestamp_field: "timestamp".to_owned(),
            ..PackOptions::default()
        },
    )?;
    println!(
        "packed {} records in {} blocks",
        stats.records, stats.blocks
    );

    let mut rows = Vec::new();
    let query_stats = query(
        Cursor::new(&archive),
        &mut rows,
        &Query {
            equals: Vec::new(),
            from: Some(parse_timestamp("2026-03-01T01:00:00Z")?),
            to: Some(parse_timestamp("2026-03-01T01:04:59Z")?),
            columns: Some(vec!["timestamp".to_owned(), "level".to_owned()]),
        },
    )?;
    print!("{}", String::from_utf8_lossy(&rows));
    println!(
        "matched {} records, read {} of {} blocks",
        query_stats.records_matched, query_stats.blocks_read, query_stats.blocks_total
    );
    Ok(())
}
