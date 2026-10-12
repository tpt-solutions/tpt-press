//! Summarizes a logs archive without decompressing its records.

use std::io::{Read, Seek, SeekFrom};

use crate::block;
use crate::error::{Error, Result};
use crate::format::{self, Format};

/// What a logs archive contains, read from its header, index and block directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// Record format.
    pub format: Format,
    /// Field or column holding the timestamp.
    pub timestamp_field: String,
    /// Records in the archive.
    pub records: u64,
    /// Blocks in the archive.
    pub blocks: u64,
    /// Blocks holding at least one record without a parseable timestamp.
    pub untimed_blocks: u64,
    /// Every column in the archive. CSV keeps header order. JSON is sorted.
    pub columns: Vec<String>,
    /// Earliest and latest timestamps, in Unix nanoseconds. `None` if no record has one.
    pub time_range: Option<(i64, i64)>,
    /// Size of the trained dictionary in bytes, or 0 if there is none.
    pub dictionary_bytes: u64,
    /// Size of the column data before compression, in bytes.
    pub uncompressed_bytes: u64,
    /// Size of the whole archive in bytes.
    pub archive_bytes: u64,
}

impl Summary {
    /// Uncompressed bytes divided by archive bytes.
    pub fn ratio(&self) -> f64 {
        self.uncompressed_bytes as f64 / self.archive_bytes.max(1) as f64
    }
}

/// Reads an archive's structure. Block payloads are read for their directories
/// and checksums, but no column is decompressed.
pub fn inspect<R: Read + Seek>(mut input: R) -> Result<Summary> {
    input.seek(SeekFrom::Start(0))?;
    let header = format::read_header(&mut input)?;
    let entries = format::read_index(&mut input)?;
    let archive_bytes = input.seek(SeekFrom::End(0))?;

    let mut columns: Vec<String> = Vec::new();
    let mut summary = Summary {
        format: header.format,
        timestamp_field: header.timestamp_field,
        records: 0,
        blocks: entries.len() as u64,
        untimed_blocks: 0,
        columns: Vec::new(),
        time_range: None,
        dictionary_bytes: header.dictionary.len() as u64,
        uncompressed_bytes: 0,
        archive_bytes,
    };

    for entry in &entries {
        summary.records += u64::from(entry.records);
        if entry.untimed {
            summary.untimed_blocks += 1;
        }
        if entry.has_time {
            summary.time_range = Some(match summary.time_range {
                None => (entry.min_time, entry.max_time),
                Some((lo, hi)) => (lo.min(entry.min_time), hi.max(entry.max_time)),
            });
        }

        let payload = format::read_payload(&mut input, entry)?;
        let dir = block::decode_directory(&payload)?;
        if dir.records != entry.records {
            return Err(Error::Corrupt("block record count does not match index"));
        }
        for column in &dir.columns {
            if !columns.contains(&column.name) {
                columns.push(column.name.clone());
            }
            summary.uncompressed_bytes += column.raw_len() as u64;
        }
    }

    if summary.format == Format::Jsonl {
        columns.sort();
    }
    summary.columns = columns;
    Ok(summary)
}
