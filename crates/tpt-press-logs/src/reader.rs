//! Queries a logs archive by time range and columns.

use std::io::{Read, Seek, SeekFrom, Write};

use crate::block::{self, Directory};
use crate::error::{Error, Result};
use crate::format::{self, Entry};
use crate::schema::Schema;

/// What to select from an archive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// Earliest timestamp to include, in Unix nanoseconds. `None` means no lower bound.
    pub from: Option<i64>,
    /// Latest timestamp to include, in Unix nanoseconds. `None` means no upper bound.
    pub to: Option<i64>,
    /// Columns to output, in order. `None` outputs whole records.
    pub columns: Option<Vec<String>>,
}

/// Counts from a completed [`query`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryStats {
    /// Blocks in the archive.
    pub blocks_total: u64,
    /// Blocks that were read. The rest were skipped using the index.
    pub blocks_read: u64,
    /// Column streams that were decompressed. Only the columns a query needs
    /// are decompressed.
    pub columns_read: u64,
    /// Records written to the output.
    pub records_matched: u64,
}

/// Writes the records that match `query` to `output`, one per line.
///
/// Blocks whose time range cannot overlap the query are skipped without being
/// read. Within a block, only the columns the query needs are decompressed.
/// Those are the requested columns, plus the timestamp column if the query has
/// a time range. Records without a parseable timestamp are always included.
/// Output records are in archive order.
pub fn query<R: Read + Seek, W: Write>(
    input: R,
    mut output: W,
    query: &Query,
) -> Result<QueryStats> {
    let omit_absent = query.columns.is_none();
    let stats = scan(input, query, |schema, names, cells| {
        let row = schema.render(names, cells, omit_absent)?;
        output.write_all(&row)?;
        output.write_all(b"\n")?;
        Ok(())
    })?;
    output.flush()?;
    Ok(stats)
}

/// Calls `visit` for each record that matches `query`, in archive order.
///
/// `visit` gets the schema, the column names and one cell per name. A cell is
/// `None` when the record's block has no such column. Cells are the raw JSON
/// text or CSV field text, as stored. This is the shared path for [`query`]
/// and for the SQL surface.
pub(crate) fn scan<R, F>(mut input: R, query: &Query, mut visit: F) -> Result<QueryStats>
where
    R: Read + Seek,
    F: FnMut(&Schema, &[String], &[Option<&[u8]>]) -> Result<()>,
{
    if let (Some(from), Some(to)) = (query.from, query.to) {
        if from > to {
            return Err(Error::InvalidOption("the start time is after the end time"));
        }
    }

    input.seek(SeekFrom::Start(0))?;
    let header = format::read_header(&mut input)?;
    let schema = Schema::new(header.format, &header.timestamp_field, &header.csv_header)?;
    if let Some(columns) = &query.columns {
        schema.validate_columns(columns)?;
    }

    let entries = format::read_index(&mut input)?;
    let mut stats = QueryStats {
        blocks_total: entries.len() as u64,
        ..QueryStats::default()
    };
    let filter_time = query.from.is_some() || query.to.is_some();

    for entry in entries.iter().filter(|e| overlaps(e, query, filter_time)) {
        stats.blocks_read += 1;
        let payload = format::read_payload(&mut input, entry)?;
        let dir = block::decode_directory(&payload)?;
        check_block(entry, &dir)?;

        let output_names = output_names(query, &dir);
        let mut wanted: Vec<&str> = output_names.iter().map(String::as_str).collect();
        if filter_time {
            wanted.push(schema.timestamp_field());
        }

        // Decompress each wanted column once, and keep it as cells per record.
        let mut decoded: Vec<(&str, Vec<Vec<u8>>)> = Vec::new();
        for name in wanted {
            if decoded.iter().any(|(n, _)| *n == name) {
                continue;
            }
            let Some(column) = dir.columns.iter().find(|c| c.name == name) else {
                continue;
            };
            let raw = block::decode_column(&payload, column, &header.dictionary)?;
            stats.columns_read += 1;
            let cells = split_cells(&raw)?;
            if cells.len() != dir.records as usize {
                return Err(Error::Corrupt("column has the wrong number of cells"));
            }
            decoded.push((name, cells));
        }

        let timestamp_cells = decoded
            .iter()
            .find(|(n, _)| *n == schema.timestamp_field())
            .map(|(_, cells)| cells);

        for i in 0..dir.records as usize {
            if filter_time {
                let timestamp = timestamp_cells.and_then(|cells| schema.cell_timestamp(&cells[i]));
                if let Some(t) = timestamp {
                    if !in_range(t, query) {
                        continue;
                    }
                }
            }

            let aligned: Vec<Option<&[u8]>> = output_names
                .iter()
                .map(|name| {
                    decoded
                        .iter()
                        .find(|(n, _)| *n == name)
                        .map(|(_, cells)| cells[i].as_slice())
                })
                .collect();
            visit(&schema, &output_names, &aligned)?;
            stats.records_matched += 1;
        }
    }

    Ok(stats)
}

/// The columns to output: the requested ones, or every column in the block.
/// The block stores CSV columns in header order, so whole CSV records keep their order.
fn output_names(query: &Query, dir: &Directory) -> Vec<String> {
    match &query.columns {
        Some(columns) => columns.clone(),
        None => dir.columns.iter().map(|c| c.name.clone()).collect(),
    }
}

/// Splits a raw column stream into its cells. Every cell is followed by `\n`,
/// so an empty stream holds no cells.
fn split_cells(raw: &[u8]) -> Result<Vec<Vec<u8>>> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let body = raw
        .strip_suffix(b"\n")
        .ok_or(Error::Corrupt("column stream is not newline-terminated"))?;
    Ok(body.split(|&b| b == b'\n').map(<[u8]>::to_vec).collect())
}

/// Checks that a block's directory agrees with the index.
fn check_block(entry: &Entry, dir: &Directory) -> Result<()> {
    if dir.records != entry.records {
        return Err(Error::Corrupt("block record count does not match index"));
    }
    Ok(())
}

/// Whether a block could hold a record in the query's range.
fn overlaps(entry: &Entry, query: &Query, filter_time: bool) -> bool {
    if !filter_time || entry.untimed || !entry.has_time {
        return true;
    }
    entry.max_time >= query.from.unwrap_or(i64::MIN)
        && entry.min_time <= query.to.unwrap_or(i64::MAX)
}

fn in_range(t: i64, query: &Query) -> bool {
    query.from.is_none_or(|from| t >= from) && query.to.is_none_or(|to| t <= to)
}
