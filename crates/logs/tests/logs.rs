//! Integration tests for logs archives: packing, time-range queries, columns,
//! and damage detection.

use std::io::Cursor;

use tpt_press_logs::{pack, parse_timestamp, query, Error, Format, PackOptions, Query, QueryStats};

/// Seconds between records in generated JSONL.
const STEP: i64 = 10;

/// Formats seconds since 2026-10-13T00:00:00Z as RFC 3339, for under one day.
fn rfc3339(seconds: i64) -> String {
    format!(
        "2026-10-13T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

fn jsonl_line(i: i64) -> String {
    let level = if i % 7 == 0 { "error" } else { "info" };
    format!(
        r#"{{"timestamp":"{}","level":"{}","msg":"request {} handled"}}"#,
        rfc3339(i * STEP),
        level,
        i
    )
}

fn jsonl_input(records: i64) -> Vec<u8> {
    (0..records)
        .map(jsonl_line)
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

fn small_blocks() -> PackOptions {
    PackOptions {
        block_size: 4096,
        ..PackOptions::default()
    }
}

fn pack_bytes(input: &[u8], opts: &PackOptions) -> Vec<u8> {
    let mut out = Vec::new();
    pack(Cursor::new(input), &mut out, opts).unwrap();
    out
}

fn run_query(archive: &[u8], q: &Query) -> tpt_press_logs::Result<(Vec<String>, QueryStats)> {
    let mut out = Vec::new();
    let stats = query(Cursor::new(archive), &mut out, q)?;
    let lines = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    Ok((lines, stats))
}

/// Unix nanoseconds for `seconds` after the base date that the records use.
fn at(seconds: i64) -> i64 {
    parse_timestamp("2026-10-13T00:00:00Z").unwrap() + seconds * 1_000_000_000
}

#[test]
fn jsonl_round_trips_every_record() {
    let input = jsonl_input(5000);
    let archive = pack_bytes(&input, &small_blocks());
    let (lines, stats) = run_query(&archive, &Query::default()).unwrap();

    let expected: Vec<String> = (0..5000).map(jsonl_line).collect();
    assert_eq!(lines, expected);
    assert_eq!(stats.records_matched, 5000);
}

#[test]
fn dictionary_is_trained_for_similar_records() {
    let input = jsonl_input(5000);
    let mut out = Vec::new();
    let stats = pack(Cursor::new(&input), &mut out, &PackOptions::default()).unwrap();
    assert!(stats.dictionary_bytes > 0);
    assert!(stats.archive_bytes < input.len() as u64 / 4);
}

#[test]
fn time_range_returns_only_matching_records() {
    let archive = pack_bytes(&jsonl_input(5000), &small_blocks());
    let q = Query {
        from: Some(at(1000)),
        to: Some(at(1500)),
        columns: None,
    };
    let (lines, stats) = run_query(&archive, &q).unwrap();

    let expected: Vec<String> = (0..5000)
        .filter(|i| (1000..=1500).contains(&(i * STEP)))
        .map(jsonl_line)
        .collect();
    assert_eq!(lines, expected);
    assert_eq!(stats.records_matched, expected.len() as u64);
}

#[test]
fn time_range_skips_blocks_it_does_not_need() {
    let archive = pack_bytes(&jsonl_input(5000), &small_blocks());
    let q = Query {
        from: Some(at(1000)),
        to: Some(at(1100)),
        columns: None,
    };
    let (_, stats) = run_query(&archive, &q).unwrap();

    assert!(stats.blocks_total > 50, "{stats:?}");
    assert!(
        stats.blocks_read * 10 < stats.blocks_total,
        "a narrow range should read few blocks: {stats:?}"
    );
}

#[test]
fn records_without_timestamps_are_always_included() {
    let mut input = jsonl_input(100);
    input.extend_from_slice(b"\n{\"level\":\"info\",\"msg\":\"no time here\"}");
    let archive = pack_bytes(&input, &small_blocks());

    let q = Query {
        from: Some(at(0)),
        to: Some(at(STEP)),
        columns: None,
    };
    let (lines, _) = run_query(&archive, &q).unwrap();
    assert!(lines.iter().any(|l| l.contains("no time here")));
}

#[test]
fn column_projection_keeps_only_requested_fields() {
    let archive = pack_bytes(&jsonl_input(50), &small_blocks());
    let q = Query {
        from: None,
        to: None,
        columns: Some(vec!["level".to_owned()]),
    };
    let (lines, _) = run_query(&archive, &q).unwrap();
    assert_eq!(lines.len(), 50);
    assert!(lines.iter().all(|l| l.starts_with(r#"{"level":"#)));
    assert!(lines.iter().all(|l| !l.contains("msg")));
}

#[test]
fn inverted_range_is_rejected() {
    let archive = pack_bytes(&jsonl_input(10), &small_blocks());
    let q = Query {
        from: Some(at(100)),
        to: Some(at(1)),
        columns: None,
    };
    assert!(matches!(
        run_query(&archive, &q),
        Err(Error::InvalidOption(_))
    ));
}

fn csv_input() -> Vec<u8> {
    let mut text = String::from("timestamp,level,msg\r\n");
    for i in 0..300 {
        let level = if i % 5 == 0 { "warn" } else { "info" };
        text.push_str(&format!(
            "{},{},\"message {}, with a comma\"\r\n",
            rfc3339(i * STEP),
            level,
            i
        ));
    }
    text.into_bytes()
}

#[test]
fn csv_round_trip_and_projection() {
    let opts = PackOptions {
        format: Format::Csv,
        block_size: 2048,
        ..PackOptions::default()
    };
    let archive = pack_bytes(&csv_input(), &opts);

    let (lines, stats) = run_query(&archive, &Query::default()).unwrap();
    assert_eq!(stats.records_matched, 300);
    assert_eq!(
        lines[0],
        "2026-10-13T00:00:00Z,warn,\"message 0, with a comma\""
    );

    let q = Query {
        from: None,
        to: None,
        columns: Some(vec!["msg".to_owned(), "level".to_owned()]),
    };
    let (projected, _) = run_query(&archive, &q).unwrap();
    assert_eq!(projected[1], "\"message 1, with a comma\",info");
}

#[test]
fn csv_time_range_uses_the_timestamp_column() {
    let opts = PackOptions {
        format: Format::Csv,
        block_size: 2048,
        ..PackOptions::default()
    };
    let archive = pack_bytes(&csv_input(), &opts);
    let q = Query {
        from: Some(at(100)),
        to: Some(at(200)),
        columns: None,
    };
    let (lines, _) = run_query(&archive, &q).unwrap();
    assert_eq!(lines.len(), 11);
}

#[test]
fn csv_unknown_column_is_rejected() {
    let opts = PackOptions {
        format: Format::Csv,
        ..PackOptions::default()
    };
    let archive = pack_bytes(&csv_input(), &opts);
    let q = Query {
        from: None,
        to: None,
        columns: Some(vec!["nope".to_owned()]),
    };
    assert!(matches!(
        run_query(&archive, &q),
        Err(Error::UnknownColumn(_))
    ));
}

#[test]
fn csv_without_header_is_rejected() {
    let opts = PackOptions {
        format: Format::Csv,
        ..PackOptions::default()
    };
    let mut out = Vec::new();
    assert!(matches!(
        pack(Cursor::new(b"".as_slice()), &mut out, &opts),
        Err(Error::MissingHeader)
    ));
}

#[test]
fn blank_lines_are_dropped() {
    let input = b"\n\n{\"timestamp\":\"2026-10-13T00:00:01Z\",\"msg\":\"a\"}\n\n";
    let archive = pack_bytes(input, &PackOptions::default());
    let (lines, stats) = run_query(&archive, &Query::default()).unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(stats.records_matched, 1);
}

#[test]
fn empty_input_makes_an_empty_archive() {
    let archive = pack_bytes(b"", &PackOptions::default());
    let (lines, stats) = run_query(&archive, &Query::default()).unwrap();
    assert!(lines.is_empty());
    assert_eq!(stats.blocks_total, 0);
}

#[test]
fn tiny_input_without_a_dictionary_still_round_trips() {
    let input = jsonl_input(3);
    let mut out = Vec::new();
    let stats = pack(Cursor::new(&input), &mut out, &PackOptions::default()).unwrap();
    assert_eq!(stats.dictionary_bytes, 0);
    let (lines, _) = run_query(&out, &Query::default()).unwrap();
    assert_eq!(lines.len(), 3);
}

#[test]
fn corrupt_block_is_detected() {
    let mut archive = pack_bytes(&jsonl_input(500), &small_blocks());
    // The index starts right after the last block's payload. Its offset is the
    // first field of the 20-byte footer. Flipping the byte just before it
    // damages that block without touching the header or the index.
    let footer_at = archive.len() - 20;
    let index_offset = u64::from_le_bytes(archive[footer_at..footer_at + 8].try_into().unwrap());
    archive[index_offset as usize - 2] ^= 0xff;
    let result = run_query(&archive, &Query::default());
    assert!(
        matches!(result, Err(Error::Corrupt(_))),
        "expected a corruption error, got {result:?}"
    );
}

#[test]
fn truncated_archive_is_detected() {
    let archive = pack_bytes(&jsonl_input(500), &small_blocks());
    let cut = &archive[..archive.len() - 1];
    assert!(matches!(
        run_query(cut, &Query::default()),
        Err(Error::Truncated)
    ));
}

#[test]
fn non_archive_is_rejected() {
    assert!(matches!(
        run_query(b"plain text, not an archive at all", &Query::default()),
        Err(Error::BadMagic)
    ));
}
