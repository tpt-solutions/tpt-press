//! SQL over logs archives. Only built with the `strata` feature.

#![cfg(feature = "strata")]

use std::io::Cursor;

use tpt_press_logs::{pack, query_sql, Error, Format, PackOptions, Query, TABLE_NAME};
use tpt_strata::{Scalar, Table};

fn pack_text(text: &str, format: Format) -> Vec<u8> {
    let mut archive = Vec::new();
    pack(
        Cursor::new(text.as_bytes().to_vec()),
        &mut archive,
        &PackOptions {
            format,
            ..PackOptions::default()
        },
    )
    .expect("pack");
    archive
}

/// Every row of a result table, as scalars in column order.
fn rows(table: &Table) -> Vec<Vec<Scalar>> {
    let mut out = Vec::new();
    for batch in table.batches() {
        for r in 0..batch.row_count() {
            out.push(
                batch
                    .columns
                    .iter()
                    .map(|c| c.get(r).unwrap_or(Scalar::Null))
                    .collect(),
            );
        }
    }
    out
}

fn jsonl_levels() -> String {
    [
        ("2026-03-01T10:00:00Z", "info", 10),
        ("2026-03-01T10:01:00Z", "warn", 20),
        ("2026-03-01T10:02:00Z", "info", 30),
        ("2026-03-01T10:03:00Z", "error", 40),
        ("2026-03-01T10:04:00Z", "info", 50),
        ("2026-03-01T10:05:00Z", "warn", 60),
    ]
    .iter()
    .map(|(t, level, ms)| format!("{{\"timestamp\":\"{t}\",\"level\":\"{level}\",\"ms\":{ms}}}\n"))
    .collect()
}

#[test]
fn filters_and_aggregates_records() {
    let archive = pack_text(&jsonl_levels(), Format::Jsonl);
    let table = query_sql(
        Cursor::new(archive),
        &Query::default(),
        &format!("SELECT SUM(ms) FROM {TABLE_NAME} WHERE level = 'info'"),
    )
    .expect("sql");
    let values = rows(&table);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0][0].as_f64(), Some(90.0));
}

#[test]
fn time_range_limits_the_rows_the_sql_sees() {
    let archive = pack_text(&jsonl_levels(), Format::Jsonl);
    let from = tpt_press_logs::parse_timestamp("2026-03-01T10:01:00Z").unwrap();
    let to = tpt_press_logs::parse_timestamp("2026-03-01T10:03:00Z").unwrap();
    let table = query_sql(
        Cursor::new(archive),
        &Query {
            from: Some(from),
            to: Some(to),
            columns: None,
        },
        &format!("SELECT ms FROM {TABLE_NAME}"),
    )
    .expect("sql");
    let ms: Vec<f64> = rows(&table).iter().filter_map(|r| r[0].as_f64()).collect();
    assert_eq!(ms, vec![20.0, 30.0, 40.0]);
}

#[test]
fn requested_columns_are_the_only_ones_in_the_table() {
    let archive = pack_text(&jsonl_levels(), Format::Jsonl);
    let query = Query {
        columns: Some(vec!["level".to_owned()]),
        ..Query::default()
    };
    let table = query_sql(
        Cursor::new(archive),
        &query,
        &format!("SELECT * FROM {TABLE_NAME}"),
    )
    .expect("sql");
    assert_eq!(table.column_names(), vec!["level"]);
    assert_eq!(table.row_count(), 6);
}

#[test]
fn integers_stay_integers_and_mixed_columns_become_text() {
    let text = "{\"timestamp\":\"2026-03-01T10:00:00Z\",\"n\":1,\"v\":1}\n\
                {\"timestamp\":\"2026-03-01T10:01:00Z\",\"n\":2,\"v\":\"two\"}\n";
    let archive = pack_text(text, Format::Jsonl);
    let table = query_sql(
        Cursor::new(archive),
        &Query::default(),
        &format!("SELECT n, v FROM {TABLE_NAME}"),
    )
    .expect("sql");
    let values = rows(&table);
    assert_eq!(values[0], vec![Scalar::I64(1), Scalar::Str("1".to_owned())]);
    assert_eq!(
        values[1],
        vec![Scalar::I64(2), Scalar::Str("two".to_owned())]
    );
}

#[test]
fn columns_with_no_values_are_left_out() {
    let text = "{\"timestamp\":\"2026-03-01T10:00:00Z\",\"level\":\"info\",\"note\":null}\n";
    let archive = pack_text(text, Format::Jsonl);
    let err = query_sql(
        Cursor::new(archive),
        &Query::default(),
        &format!("SELECT note FROM {TABLE_NAME}"),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Engine(_)), "got {err:?}");
}

#[test]
fn csv_fields_are_typed_per_column() {
    let text = "timestamp,code,name\n\
                2026-03-01T10:00:00Z,007,alpha\n\
                2026-03-01T10:01:00Z,12,beta\n";
    let archive = pack_text(text, Format::Csv);
    let table = query_sql(
        Cursor::new(archive),
        &Query::default(),
        &format!("SELECT code, name FROM {TABLE_NAME} WHERE name = 'beta'"),
    )
    .expect("sql");
    assert_eq!(
        rows(&table),
        vec![vec![Scalar::I64(12), Scalar::Str("beta".to_owned())]]
    );
}

#[test]
fn sql_errors_are_reported_not_hidden() {
    let archive = pack_text(&jsonl_levels(), Format::Jsonl);
    let err = query_sql(
        Cursor::new(archive),
        &Query::default(),
        &format!("SELECT nope FROM {TABLE_NAME}"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("nope"), "got {err}");
}

#[test]
fn unsupported_sql_is_refused_with_a_diagnostic() {
    let archive = pack_text(&jsonl_levels(), Format::Jsonl);
    let result = query_sql(
        Cursor::new(archive),
        &Query::default(),
        &format!("SELECT ms FROM {TABLE_NAME} WHERE level = 'info' OR level = 'warn'"),
    );
    assert!(result.is_err());
}
