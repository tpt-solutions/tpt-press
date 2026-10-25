//! Packs a CSV log with a header row, then queries whole records for a time
//! window. A query with no column list returns full rows, with the header's
//! column names.
//!
//! Run with `cargo run -p tpt-press-logs --example csv_query`.

use std::io::Cursor;

use tpt_press_logs::{pack, parse_timestamp, query, Format, PackOptions, Query};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // One request a minute for two hours, from three services.
    let mut csv = String::from("time,service,status,latency_ms\n");
    for minute in 0..120u32 {
        for (i, service) in ["auth", "catalog", "checkout"].iter().enumerate() {
            let status = if (minute + i as u32).is_multiple_of(17) {
                503
            } else {
                200
            };
            let latency = 20 + (minute * 7 + i as u32 * 13) % 180;
            csv.push_str(&format!(
                "2026-05-01T{:02}:{:02}:00Z,{service},{status},{latency}\n",
                minute / 60,
                minute % 60
            ));
        }
    }

    let mut archive = Vec::new();
    let stats = pack(
        Cursor::new(csv.as_bytes()),
        &mut archive,
        &PackOptions {
            format: Format::Csv,
            timestamp_field: "time".to_owned(),
            ..PackOptions::default()
        },
    )?;
    println!(
        "packed {} records into {} blocks, {} -> {} bytes",
        stats.records, stats.blocks, stats.input_bytes, stats.archive_bytes
    );

    // Whole records for 10:00 to 10:04, with no column list.
    let mut rows = Vec::new();
    let result = query(
        Cursor::new(&archive),
        &mut rows,
        &Query {
            equals: Vec::new(),
            from: Some(parse_timestamp("2026-05-01T00:10:00Z")?),
            to: Some(parse_timestamp("2026-05-01T00:12:59Z")?),
            columns: None,
        },
    )?;
    print!("{}", String::from_utf8_lossy(&rows));
    println!(
        "matched {} records, read {} of {} blocks",
        result.records_matched, result.blocks_read, result.blocks_total
    );
    Ok(())
}
