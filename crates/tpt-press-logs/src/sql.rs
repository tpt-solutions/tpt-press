//! SQL over a logs archive, through the tpt-strata engine.
//!
//! Only built with the `strata` feature. The matching records are loaded into
//! memory as one table, then the SQL runs on that table.

use std::collections::HashMap;
use std::io::{Read, Seek};

use serde_json::Value;
use tpt_strata::sql::SqlContext;
use tpt_strata::{Column, Scalar, Table};

use crate::error::{Error, Result};
use crate::reader::{scan, Query};

/// The name a query uses in its `FROM` clause.
pub const TABLE_NAME: &str = "logs";

/// Runs `sql` over the records that match `query`, and returns the result.
///
/// The records become a table named [`TABLE_NAME`]. Its columns are the
/// requested `query.columns`, or every column when that is `None`. Columns
/// with no values at all are left out, since the engine cannot type them.
///
/// Column types are inferred from the cells. A column of integers is an
/// integer column. A mix of integers and decimals is a decimal column. A
/// column of `true` and `false` is boolean. Anything else is text, and then
/// each value keeps its text. CSV fields are typed the same way, so `007`
/// becomes the integer 7. Timestamps stay as text, since the engine has no
/// timestamp type. Use `from` and `to` in the [`Query`] for time ranges.
///
/// The whole result is held in memory, as with the other logs queries.
pub fn query_sql<R: Read + Seek>(input: R, query: &Query, sql: &str) -> Result<Table> {
    let mut raw = RawTable::default();
    let mut json = false;
    scan(input, query, |schema, names, cells| {
        json = schema.format().is_json();
        raw.push_row(names, cells);
        Ok(())
    })?;

    let columns: Vec<Column> = raw
        .names
        .iter()
        .zip(&raw.cells)
        .filter_map(|(name, cells)| {
            let parsed: Vec<Option<Parsed>> = cells
                .iter()
                .map(|c| c.as_deref().map(|t| parse_cell(t, json)))
                .collect();
            typed_column(name, parsed)
        })
        .collect();

    let table = Table::try_new(columns).map_err(|e| Error::Engine(e.to_string()))?;
    let mut ctx = SqlContext::new();
    ctx.add_table(TABLE_NAME, &table);
    ctx.run(sql).map_err(|e| Error::Engine(e.to_string()))
}

/// Cells collected per column. A block may lack some columns, so a missing
/// cell is `None`, and a column can first appear partway through.
#[derive(Default)]
struct RawTable {
    names: Vec<String>,
    index: HashMap<String, usize>,
    cells: Vec<Vec<Option<Vec<u8>>>>,
    rows: usize,
}

impl RawTable {
    fn push_row(&mut self, names: &[String], cells: &[Option<&[u8]>]) {
        let row = self.rows;
        self.rows += 1;
        for column in &mut self.cells {
            column.push(None);
        }
        for (name, cell) in names.iter().zip(cells) {
            let idx = match self.index.get(name) {
                Some(&idx) => idx,
                None => {
                    let idx = self.names.len();
                    self.names.push(name.clone());
                    self.index.insert(name.clone(), idx);
                    self.cells.push(vec![None; row + 1]);
                    idx
                }
            };
            // An empty cell is an absent value, the same as in the record renderer.
            self.cells[idx][row] = cell.filter(|c| !c.is_empty()).map(<[u8]>::to_vec);
        }
    }
}

/// One cell after parsing. `Text` carries the value's text for the fallback.
#[derive(Debug, Clone, PartialEq)]
enum Parsed {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

/// Parses one cell. JSON cells are JSON text. CSV cells are field text.
fn parse_cell(text: &[u8], json: bool) -> Parsed {
    if json {
        match serde_json::from_slice::<Value>(text) {
            Ok(Value::Null) => Parsed::Null,
            Ok(Value::Bool(b)) => Parsed::Bool(b),
            Ok(Value::Number(n)) => match n.as_i64() {
                Some(i) => Parsed::Int(i),
                None => n
                    .as_f64()
                    .map_or_else(|| Parsed::Text(raw(text)), Parsed::Float),
            },
            Ok(Value::String(s)) => Parsed::Text(s),
            // Arrays and objects keep their JSON text.
            Ok(_) | Err(_) => Parsed::Text(raw(text)),
        }
    } else {
        let s = raw(text);
        if s.is_empty() {
            Parsed::Null
        } else if s == "true" {
            Parsed::Bool(true)
        } else if s == "false" {
            Parsed::Bool(false)
        } else if let Ok(i) = s.parse::<i64>() {
            Parsed::Int(i)
        } else if looks_numeric(&s) {
            match s.parse::<f64>() {
                Ok(f) => Parsed::Float(f),
                Err(_) => Parsed::Text(s),
            }
        } else {
            Parsed::Text(s)
        }
    }
}

/// Whether `s` is a plain decimal number. This rejects `NaN` and `inf`, which
/// `f64::from_str` also accepts.
fn looks_numeric(s: &str) -> bool {
    s.bytes().any(|b| b.is_ascii_digit())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'e' | b'E'))
}

fn raw(text: &[u8]) -> String {
    String::from_utf8_lossy(text).into_owned()
}

/// Builds one typed column from parsed cells. Returns `None` if every cell is null.
fn typed_column(name: &str, parsed: Vec<Option<Parsed>>) -> Option<Column> {
    let values: Vec<&Parsed> = parsed
        .iter()
        .flatten()
        .filter(|p| !matches!(p, Parsed::Null))
        .collect();
    if values.is_empty() {
        return None;
    }

    let all = |f: fn(&Parsed) -> bool| values.iter().all(|p| f(p));
    let scalars: Vec<Scalar> = if all(|p| matches!(p, Parsed::Bool(_))) {
        to_scalars(parsed, |p| match p {
            Parsed::Bool(b) => Scalar::Bool(*b),
            _ => Scalar::Null,
        })
    } else if all(|p| matches!(p, Parsed::Int(_))) {
        to_scalars(parsed, |p| match p {
            Parsed::Int(i) => Scalar::I64(*i),
            _ => Scalar::Null,
        })
    } else if all(|p| matches!(p, Parsed::Int(_) | Parsed::Float(_))) {
        to_scalars(parsed, |p| match p {
            Parsed::Int(i) => Scalar::F64(*i as f64),
            Parsed::Float(f) => Scalar::F64(*f),
            _ => Scalar::Null,
        })
    } else {
        to_scalars(parsed, |p| match p {
            Parsed::Text(s) => Scalar::Str(s.clone()),
            Parsed::Bool(b) => Scalar::Str(b.to_string()),
            Parsed::Int(i) => Scalar::Str(i.to_string()),
            Parsed::Float(f) => Scalar::Str(f.to_string()),
            Parsed::Null => Scalar::Null,
        })
    };
    Some(Column::new(name, scalars))
}

fn to_scalars(parsed: Vec<Option<Parsed>>, f: impl Fn(&Parsed) -> Scalar) -> Vec<Scalar> {
    parsed
        .iter()
        .map(|p| p.as_ref().map_or(Scalar::Null, &f))
        .collect()
}
