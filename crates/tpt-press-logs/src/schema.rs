//! Splits records into column cells, and turns cells back into records.

use std::collections::BTreeSet;

use serde_json::{Map, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::error::{Error, Result};
use crate::format::Format;
use crate::otlp;
use crate::prometheus;

const EMPTY: &[u8] = b"";

/// Knows how to read records of one archive's format.
#[derive(Debug, Clone)]
pub struct Schema {
    format: Format,
    timestamp_field: String,
    /// CSV header names, in order. Empty for JSONL.
    header: Vec<String>,
}

impl Schema {
    /// Builds the schema. For CSV, `csv_header` must be the header line.
    pub fn new(format: Format, timestamp_field: &str, csv_header: &[u8]) -> Result<Self> {
        let header = match format {
            Format::Jsonl | Format::Prometheus | Format::Otlp => Vec::new(),
            Format::Csv => {
                if csv_header.is_empty() {
                    return Err(Error::MissingHeader);
                }
                let names = parse_csv_fields(csv_header).ok_or(Error::MissingHeader)?;
                let unique: BTreeSet<&String> = names.iter().collect();
                if unique.len() != names.len() {
                    return Err(Error::InvalidRecord(
                        "duplicate column name in CSV header".to_owned(),
                    ));
                }
                names
            }
        };
        Ok(Self {
            format,
            timestamp_field: timestamp_field.to_owned(),
            header,
        })
    }

    /// Checks that every requested column exists. JSONL keys are not known
    /// in advance, so any name is accepted there, and a missing key becomes `null`.
    pub fn validate_columns(&self, requested: &[String]) -> Result<()> {
        if self.format == Format::Csv {
            for name in requested {
                if !self.header.contains(name) {
                    return Err(Error::UnknownColumn(name.clone()));
                }
            }
        }
        Ok(())
    }

    /// Whether a line is not a record at all, and is dropped without an error.
    /// For Prometheus, that covers `#` comment lines.
    pub fn skips(&self, line: &[u8]) -> bool {
        self.format == Format::Prometheus && prometheus::is_comment(line)
    }

    /// Splits one line into its records, each as `(column, cell)` pairs. Most
    /// formats have one record per line. OTLP lines can hold several.
    pub fn records(&self, line: &[u8]) -> Result<Vec<Vec<(String, Vec<u8>)>>> {
        if self.format != Format::Otlp {
            return Ok(vec![self.cells(line)?]);
        }
        otlp::parse_request(line, &self.timestamp_field)?
            .into_iter()
            .map(|fields| {
                fields
                    .into_iter()
                    .map(|(name, value)| {
                        serde_json::to_vec(&value)
                            .map(|cell| (name, cell))
                            .map_err(|e| Error::InvalidRecord(e.to_string()))
                    })
                    .collect()
            })
            .collect()
    }

    /// Splits one record into `(column, cell)` pairs. Raises an error if the record
    /// does not fit the format.
    ///
    /// For JSON, each cell is the value's JSON text, and absent keys have no
    /// pair. For CSV, each cell is the field text, and every header column has one.
    pub fn cells(&self, line: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
        match self.format {
            Format::Otlp => Err(Error::InvalidRecord(
                "an OTLP line can hold several records, so it is read with records()".to_owned(),
            )),
            Format::Prometheus => prometheus::parse_sample(line, &self.timestamp_field)?
                .into_iter()
                .map(|(k, v)| {
                    serde_json::to_vec(&v)
                        .map(|cell| (k, cell))
                        .map_err(|e| Error::InvalidRecord(e.to_string()))
                })
                .collect(),
            Format::Jsonl => {
                let value: Value = serde_json::from_slice(line)
                    .map_err(|e| Error::InvalidRecord(e.to_string()))?;
                let Value::Object(map) = value else {
                    return Err(Error::InvalidRecord(
                        "record is not a JSON object".to_owned(),
                    ));
                };
                map.iter()
                    .map(|(k, v)| {
                        serde_json::to_vec(v)
                            .map(|cell| (k.clone(), cell))
                            .map_err(|e| Error::InvalidRecord(e.to_string()))
                    })
                    .collect()
            }
            Format::Csv => {
                let fields = parse_csv_fields(line)
                    .ok_or_else(|| Error::InvalidRecord("bad CSV row".to_owned()))?;
                if fields.len() != self.header.len() {
                    return Err(Error::InvalidRecord(format!(
                        "row has {} fields but the header has {}",
                        fields.len(),
                        self.header.len()
                    )));
                }
                Ok(self
                    .header
                    .iter()
                    .cloned()
                    .zip(fields.into_iter().map(String::into_bytes))
                    .collect())
            }
        }
    }

    /// Column names for a block. CSV keeps header order. JSON uses the sorted
    /// union of keys across the block's records.
    pub fn block_columns(&self, rows: &[Vec<(String, Vec<u8>)>]) -> Vec<String> {
        match self.format {
            Format::Csv => self.header.clone(),
            Format::Jsonl | Format::Prometheus | Format::Otlp => rows
                .iter()
                .flatten()
                .map(|(k, _)| k.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }

    /// The timestamp in a cell of the timestamp column, in Unix nanoseconds.
    /// `None` if the cell is empty, or is not RFC 3339.
    pub fn cell_timestamp(&self, cell: &[u8]) -> Option<i64> {
        match self.format {
            Format::Jsonl | Format::Prometheus | Format::Otlp => {
                if cell.is_empty() {
                    return None;
                }
                let value: Value = serde_json::from_slice(cell).ok()?;
                parse_time(value.as_str()?)
            }
            Format::Csv => parse_time(std::str::from_utf8(cell).ok()?),
        }
    }

    /// The timestamp cell for Unix nanoseconds, as the timestamp column stores
    /// it: a JSON string, or the bare text for CSV. For a cell written in the
    /// canonical form, `cell_timestamp` followed by this gives back the cell.
    pub fn render_timestamp_cell(&self, nanos: i64) -> Vec<u8> {
        let text = format_timestamp(nanos);
        match self.format {
            Format::Jsonl | Format::Prometheus | Format::Otlp => {
                serde_json::to_vec(&Value::String(text)).unwrap_or_default()
            }
            Format::Csv => text.into_bytes(),
        }
    }

    /// The record format, which decides how cells are read.
    #[cfg(feature = "strata")]
    pub fn format(&self) -> Format {
        self.format
    }

    /// The name of the column that holds timestamps.
    pub fn timestamp_field(&self) -> &str {
        &self.timestamp_field
    }

    /// Builds one output record from aligned cells. `cells[i]` is the cell for
    /// `names[i]`, or `None` if the block has no such column. With `omit_absent`,
    /// absent keys are left out. Otherwise they appear as `null`. The result has
    /// no trailing newline.
    pub fn render(
        &self,
        names: &[String],
        cells: &[Option<&[u8]>],
        omit_absent: bool,
    ) -> Result<Vec<u8>> {
        match self.format {
            Format::Jsonl | Format::Prometheus | Format::Otlp => {
                let mut out = Map::new();
                for (name, cell) in names.iter().zip(cells) {
                    match cell {
                        Some(cell) if !cell.is_empty() => {
                            let value: Value = serde_json::from_slice(cell)
                                .map_err(|_| Error::Corrupt("column cell is not valid JSON"))?;
                            out.insert(name.clone(), value);
                        }
                        _ if !omit_absent => {
                            out.insert(name.clone(), Value::Null);
                        }
                        _ => {}
                    }
                }
                serde_json::to_vec(&Value::Object(out))
                    .map_err(|e| Error::InvalidRecord(e.to_string()))
            }
            Format::Csv => {
                let mut writer = csv::WriterBuilder::new()
                    .has_headers(false)
                    .terminator(csv::Terminator::Any(b'\n'))
                    .from_writer(Vec::new());
                let mut fields = Vec::with_capacity(cells.len());
                for cell in cells {
                    let bytes: &[u8] = cell.unwrap_or(EMPTY);
                    let text = std::str::from_utf8(bytes)
                        .map_err(|_| Error::Corrupt("CSV cell is not UTF-8"))?;
                    fields.push(text);
                }
                writer
                    .write_record(&fields)
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
/// Returns an error for input that is not RFC 3339, or is outside the
/// nanosecond range (1677 to 2262).
pub fn parse_timestamp(text: &str) -> Result<i64> {
    parse_time(text).ok_or_else(|| Error::InvalidTimestamp(text.to_owned()))
}

/// Formats Unix nanoseconds as RFC 3339, in UTC.
pub fn format_timestamp(nanos: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(nanos))
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_else(|| nanos.to_string())
}

fn parse_time(text: &str) -> Option<i64> {
    let parsed = OffsetDateTime::parse(text, &Rfc3339).ok()?;
    i64::try_from(parsed.unix_timestamp_nanos()).ok()
}

/// Splits one CSV line into fields, handling quotes. Returns `None` if the line is not one row.
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
    fn jsonl_cells_and_render() {
        let schema = Schema::new(Format::Jsonl, "timestamp", b"").unwrap();
        let line = br#"{"timestamp":"1970-01-01T00:00:02Z","level":"error"}"#;
        let cells = schema.cells(line).unwrap();
        let ts = cells.iter().find(|(k, _)| k == "timestamp").unwrap();
        assert_eq!(schema.cell_timestamp(&ts.1), Some(2_000_000_000));

        let names = vec!["level".to_owned(), "missing".to_owned()];
        let aligned: Vec<Option<&[u8]>> = vec![Some(b"\"error\""), None];
        assert_eq!(
            schema.render(&names, &aligned, false).unwrap(),
            br#"{"level":"error","missing":null}"#.to_vec()
        );
        assert_eq!(
            schema.render(&names, &aligned, true).unwrap(),
            br#"{"level":"error"}"#.to_vec()
        );
    }

    #[test]
    fn jsonl_rejects_non_objects() {
        let schema = Schema::new(Format::Jsonl, "timestamp", b"").unwrap();
        assert!(schema.cells(b"[1,2,3]").is_err());
        assert!(schema.cells(b"not json").is_err());
    }

    #[test]
    fn csv_cells_and_render_handle_quotes() {
        let schema = Schema::new(Format::Csv, "timestamp", b"timestamp,level,msg").unwrap();
        let cells = schema
            .cells(b"1970-01-01T00:00:03Z,warn,\"hello, world\"")
            .unwrap();
        assert_eq!(cells[2].1, b"hello, world".to_vec());

        let names = vec!["msg".to_owned(), "level".to_owned()];
        let aligned: Vec<Option<&[u8]>> = vec![Some(b"hello, world"), Some(b"warn")];
        assert_eq!(
            schema.render(&names, &aligned, true).unwrap(),
            b"\"hello, world\",warn".to_vec()
        );
    }

    #[test]
    fn csv_rejects_wrong_field_count() {
        let schema = Schema::new(Format::Csv, "timestamp", b"timestamp,level").unwrap();
        assert!(schema.cells(b"a,b,c").is_err());
    }

    #[test]
    fn csv_rejects_duplicate_header_names() {
        assert!(Schema::new(Format::Csv, "timestamp", b"timestamp,timestamp").is_err());
    }

    #[test]
    fn csv_requires_a_header() {
        assert!(matches!(
            Schema::new(Format::Csv, "timestamp", b""),
            Err(Error::MissingHeader)
        ));
    }
}
