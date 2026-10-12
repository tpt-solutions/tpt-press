//! Integration tests for `inspect` on logs archives.

use std::io::Cursor;

use tpt_press_logs::{inspect, pack, parse_timestamp, Error, Format, PackOptions};

fn jsonl(records: i64) -> Vec<u8> {
    (0..records)
        .map(|i| {
            let s = i * 10;
            format!(
                "{{\"timestamp\":\"2026-10-13T{:02}:{:02}:{:02}Z\",\"level\":\"info\",\"msg\":\"m{}\"}}",
                s / 3600,
                (s / 60) % 60,
                s % 60,
                i
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

fn pack_bytes(input: &[u8], opts: &PackOptions) -> Vec<u8> {
    let mut out = Vec::new();
    pack(Cursor::new(input), &mut out, opts).unwrap();
    out
}

#[test]
fn summarizes_jsonl_without_decompressing_records() {
    let input = jsonl(3000);
    let archive = pack_bytes(
        &input,
        &PackOptions {
            block_size: 4096,
            ..PackOptions::default()
        },
    );
    let summary = inspect(Cursor::new(&archive)).unwrap();

    assert_eq!(summary.format, Format::Jsonl);
    assert_eq!(summary.records, 3000);
    assert!(summary.blocks > 1);
    assert_eq!(summary.untimed_blocks, 0);
    assert_eq!(summary.columns, vec!["level", "msg", "timestamp"]);
    assert_eq!(
        summary.time_range,
        Some((
            parse_timestamp("2026-10-13T00:00:00Z").unwrap(),
            parse_timestamp("2026-10-13T08:19:50Z").unwrap(),
        ))
    );
    assert!(summary.dictionary_bytes > 0);
    assert_eq!(summary.archive_bytes, archive.len() as u64);
    assert!(summary.ratio() > 1.0);
}

#[test]
fn csv_columns_keep_header_order() {
    let mut text = String::from("timestamp,level,msg\n");
    for i in 0..50 {
        text.push_str(&format!("2026-10-13T00:00:{:02}Z,info,m{i}\n", i % 60));
    }
    let opts = PackOptions {
        format: Format::Csv,
        ..PackOptions::default()
    };
    let archive = pack_bytes(text.as_bytes(), &opts);
    let summary = inspect(Cursor::new(&archive)).unwrap();

    assert_eq!(summary.format, Format::Csv);
    assert_eq!(summary.columns, vec!["timestamp", "level", "msg"]);
    assert_eq!(summary.records, 50);
}

#[test]
fn untimed_records_are_counted_and_excluded_from_the_range() {
    let mut input = jsonl(10);
    input.extend_from_slice(b"\n{\"level\":\"warn\"}");
    let archive = pack_bytes(&input, &PackOptions::default());
    let summary = inspect(Cursor::new(&archive)).unwrap();

    assert_eq!(summary.records, 11);
    assert_eq!(summary.untimed_blocks, 1);
    assert!(summary.time_range.is_some());
}

#[test]
fn archive_without_timestamps_has_no_range() {
    let archive = pack_bytes(
        b"{\"level\":\"info\"}\n{\"level\":\"warn\"}",
        &PackOptions::default(),
    );
    let summary = inspect(Cursor::new(&archive)).unwrap();
    assert_eq!(summary.time_range, None);
}

#[test]
fn corrupt_block_is_reported() {
    let mut archive = pack_bytes(
        &jsonl(500),
        &PackOptions {
            block_size: 4096,
            ..PackOptions::default()
        },
    );
    let footer_at = archive.len() - 20;
    let index_offset = u64::from_le_bytes(archive[footer_at..footer_at + 8].try_into().unwrap());
    archive[index_offset as usize - 2] ^= 0xff;

    assert!(matches!(
        inspect(Cursor::new(&archive)),
        Err(Error::Corrupt(_))
    ));
}

#[test]
fn non_archive_is_rejected() {
    assert!(matches!(
        inspect(Cursor::new(b"hello there, not an archive".to_vec())),
        Err(Error::BadMagic)
    ));
}
