//! Queries a logs archive by time range and columns.

use std::io::{Read, Seek, SeekFrom, Write};

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
    /// Columns to output, in order. `None` outputs each record unchanged.
    pub columns: Option<Vec<String>>,
}

/// Counts from a completed [`query`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryStats {
    /// Blocks in the archive.
    pub blocks_total: u64,
    /// Blocks that were read and decompressed. The rest were skipped
    /// using the index.
    pub blocks_read: u64,
    /// Records written to the output.
    pub records_matched: u64,
}

/// Writes the records that match `query` to `output`, one per line.
///
/// Blocks whose time range cannot overlap the query are skipped without being
/// read. Records without a parseable timestamp are always included. Output
/// records are in archive order.
pub fn query<R: Read + Seek, W: Write>(
    mut input: R,
    mut output: W,
    query: &Query,
) -> Result<QueryStats> {
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
        let data = format::read_block(&mut input, entry, &header.dictionary)?;

        for line in data.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            if filter_time {
                if let Some(t) = schema.timestamp(line) {
                    if !in_range(t, query) {
                        continue;
                    }
                }
            }
            match &query.columns {
                Some(columns) => output.write_all(&schema.project(line, columns)?)?,
                None => output.write_all(line)?,
            }
            output.write_all(b"\n")?;
            stats.records_matched += 1;
        }
    }

    output.flush()?;
    Ok(stats)
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
