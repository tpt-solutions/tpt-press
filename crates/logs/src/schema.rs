//! Reads timestamps from records and projects columns out of them.

use serde_json::{Map, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::error::{Error, Result};
use crate::format::Format;

/// Knows how to read records of one archive's format.
#[derive(Debug, Clone)]
pub struct Schema {
    format: Format,
    timestamp_field: String,
    columns: Vec<String>,
    timestamp_index: Option<usize>,
}

impl Schema {
    /// Builds the schema. For CSV, `csv_header` must be the header line.
    pub fn new(format: Format, timestamp_field: &str, csv_header: &[u8]) -> Result<Self> {
        match format {
            Format::Jsonl => Ok(Self {
                format,
                timestamp_field: timestamp_field.to_owned(),
                columns: Vec::new(),
                timestamp_index: None,
            }),
            Format::Csv => {
                if csv_header.is_empty() {
                    return Err(Error::MissingHeader);
                }
                let columns = parse_csv_fields(csv_header).ok_or(Error::MissingHeader)?;
                let timestamp_index = columns.iter().position(|c| c == timestamp_field);
                Ok(Self {
                    format,
                    timestamp_field: timestamp_field.to_owned(),
                    columns,
                    timestamp_index,
                })
            }
        }
    }

    /// Checks that every requested column exists. JSONL keys are not known
    /// in advance, so any name is accepted there and a missing key becomes `null`.
    pub fn validate_columns(&self, requested: &[String]) -> Result<()> {
        if self.format == Format::Csv {
            for name in requested {
                if !self.columns.contains(name) {
                    return Err(Error::UnknownColumn(name.clone()));
                }
            }
        }
        Ok(())
    }

    /// The record's timestamp in Unix nanoseconds, or `None` if it has none
    /// or it is not RFC 3339.
    pub fn timestamp(&self, line: &[u8]) -> Option<i64> {
        match self.format {
            Format::Jsonl => {
                let value: Value = serde_json::from_slice(line).ok()?;
                parse_time(value.get(&self.timestamp_field)?.as_str()?)
            }
            Format::Csv => {
                let fields = parse_csv_fields(line)?;
                parse_time(fields.get(self.timestamp_index?)?)
            }
        }
    }

    /// Returns the record with only `columns`, in that order, without a
    /// trailing newline. Columns must have been validated first.
    pub fn project(&self, line: &[u8], columns: &[String]) -> Result<Vec<u8>> {
        match self.format {
            Format::Jsonl => {
                let value: Value = serde_json::from_slice(line)
                    .map_err(|e| Error::InvalidRecord(e.to_string()))?;
                let mut out = Map::new();
                for name in columns {
                    let field = value.get(name).cloned().unwrap_or(Value::Null);
                    out.insert(name.clone(), field);
                }
                serde_json::to_vec(&Value::Object(out))
                    .map_err(|e| Error::InvalidRecord(e.to_string()))
            }
            Format::Csv => {
                let fields = parse_csv_fields(line)
                    .ok_or_else(|| Error::InvalidRecord("bad CSV row".to_owned()))?;
                let mut selected: Vec<&str> = Vec::with_capacity(columns.len());
                for name in columns {
                    let index = self
                        .columns
                        .iter()
                        .position(|c| c == name)
                        .ok_or_else(|| Error::UnknownColumn(name.clone()))?;
                    selected.push(fields.get(index).map_or("", String::as_str));
                }
                let mut writer = csv::WriterBuilder::new()
                    .has_headers(false)
                    .terminator(csv::Terminator::Any(b'\n'))
                    .from_writer(Vec::new());
                writer
                    .write_record(&selected)
                    .map_err(|e| Error::InvalidRecord(e.to_string()))?;
                let mut out = writer
                    .into_inner()
                    .map_err(|e| Error::InvalidRecord(e.to_string()))?;
                if out.last() == Some(&b'\n') {
                    out.pop();
                }
                Ok(out)
            }
        }
    }
}

/// Parses an RFC 3339 timestamp into Unix nanoseconds.
///
/// Returns an error for input that is not RFC 3339 or is outside the
/// nanosecond range (1677 to 2262).
pub fn parse_timestamp(text: &str) -> Result<i64> {
    parse_time(text).ok_or_else(|| Error::InvalidTimestamp(text.to_owned()))
}

fn parse_time(text: &str) -> Option<i64> {
    let parsed = OffsetDateTime::parse(text, &Rfc3339).ok()?;
    i64::try_from(parsed.unix_timestamp_nanos()).ok()
}

/// Splits one CSV line into fields, handling quotes. Returns `None` if it is not one row.
fn parse_csv_fields(line: &[u8]) -> Option<Vec<String>> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(line);
    let mut records = reader.records();
    let first = records.next()?.ok()?;
    if records.next().is_some() {
        return None;
    }
    Some(first.iter().map(str::to_owned).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc3339_to_nanos() {
        assert_eq!(
            parse_timestamp("1970-01-01T00:00:01Z").unwrap(),
            1_000_000_000
        );
        assert!(parse_timestamp("yesterday").is_err());
    }

    #[test]
    fn jsonl_timestamp_and_projection() {
        let schema = Schema::new(Format::Jsonl, "timestamp", b"").unwrap();
        let line = br#"{"timestamp":"1970-01-01T00:00:02Z","level":"error","msg":"x"}"#;
        assert_eq!(schema.timestamp(line), Some(2_000_000_000));
        let cols = vec!["level".to_owned(), "missing".to_owned()];
        assert_eq!(
            schema.project(line, &cols).unwrap(),
            br#"{"level":"error","missing":null}"#.to_vec()
        );
    }

    #[test]
    fn csv_timestamp_and_projection_handle_quotes() {
        let header = b"timestamp,level,msg";
        let schema = Schema::new(Format::Csv, "timestamp", header).unwrap();
        let line = b"1970-01-01T00:00:03Z,warn,\"hello, world\"";
        assert_eq!(schema.timestamp(line), Some(3_000_000_000));
        let cols = vec!["msg".to_owned(), "level".to_owned()];
        assert_eq!(
            schema.project(line, &cols).unwrap(),
            b"\"hello, world\",warn".to_vec()
        );
    }

    #[test]
    fn csv_rejects_unknown_column() {
        let schema = Schema::new(Format::Csv, "timestamp", b"timestamp,level").unwrap();
        assert!(matches!(
            schema.validate_columns(&["nope".to_owned()]),
            Err(Error::UnknownColumn(_))
        ));
    }

    #[test]
    fn csv_requires_a_header() {
        assert!(matches!(
            Schema::new(Format::Csv, "timestamp", b""),
            Err(Error::MissingHeader)
        ));
    }
}
