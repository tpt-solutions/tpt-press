//! Packs JSON Lines or CSV logs into a time-indexed, column-oriented archive.

use std::io::{self, BufRead, Seek, SeekFrom, Write};

use rayon::prelude::*;
use tpt_press_core::codec;

use crate::block;
use crate::error::{Error, Result};
use crate::format::{self, Entry, Format, Header};
use crate::schema::Schema;

/// Default uncompressed size at which a block is closed.
pub const DEFAULT_BLOCK_SIZE: usize = 256 * 1024;

/// Largest block size accepted.
pub const MAX_BLOCK_SIZE: usize = 16 * 1024 * 1024;

/// Largest single record accepted. Records above this are rejected.
pub const MAX_RECORD_SIZE: usize = 64 * 1024 * 1024;

/// Largest dictionary trained from the sample.
const DICTIONARY_SIZE: usize = 64 * 1024;

/// Fewest sample cells needed before a dictionary is trained.
const MIN_SAMPLE_CELLS: usize = 8;

/// Fraction of the input, in percent, used as the dictionary sample.
const SAMPLE_PERCENT: u64 = 10;

/// Options for [`pack`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackOptions {
    /// Record format of the input.
    pub format: Format,
    /// Field (JSON) or column (CSV) holding the RFC 3339 timestamp.
    pub timestamp_field: String,
    /// Uncompressed size at which a block is closed, in bytes.
    pub block_size: usize,
    /// zstd compression level, in `1..=22`.
    pub level: i32,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            format: Format::Jsonl,
            timestamp_field: "timestamp".to_owned(),
            block_size: DEFAULT_BLOCK_SIZE,
            level: codec::DEFAULT_LEVEL,
        }
    }
}

/// Counts from a completed [`pack`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Size of the input in bytes, including any CSV header.
    pub input_bytes: u64,
    /// Size of the archive in bytes.
    pub archive_bytes: u64,
    /// Records written. Blank lines are not records.
    pub records: u64,
    /// Blocks written.
    pub blocks: u64,
    /// Size of the trained dictionary, or 0 if none was trained.
    pub dictionary_bytes: u64,
}

/// Packs `input` into a logs archive written to `output`.
///
/// The input must be seekable, because the dictionary is trained from its
/// first 10% before the records are read again. Each record is split into
/// columns. JSON input must be one object per line. CSV rows must match the
/// header's field count, and fields cannot contain newlines. Blank lines are
/// dropped, and CRLF is accepted.
pub fn pack<R: BufRead + Seek, W: Write>(
    mut input: R,
    mut output: W,
    opts: &PackOptions,
) -> Result<Stats> {
    validate(opts)?;

    let input_bytes = input.seek(SeekFrom::End(0))?;
    input.seek(SeekFrom::Start(0))?;

    let csv_header = match opts.format {
        Format::Csv => read_line(&mut input)?.ok_or(Error::MissingHeader)?,
        Format::Jsonl => Vec::new(),
    };
    let data_start = input.stream_position()?;
    let schema = Schema::new(opts.format, &opts.timestamp_field, &csv_header)?;

    let dictionary = train_dictionary(&mut input, &schema, input_bytes, data_start)?;

    let header = Header {
        format: opts.format,
        timestamp_field: opts.timestamp_field.clone(),
        dictionary: dictionary.clone(),
        csv_header,
    };
    let mut offset = format::write_header(&mut output, &header)?;
    let mut stats = Stats {
        input_bytes,
        dictionary_bytes: dictionary.len() as u64,
        ..Stats::default()
    };

    let batch_len = batch_len();
    let mut entries = Vec::new();
    let mut line_no = 0u64;
    loop {
        let mut batch = Vec::with_capacity(batch_len);
        while batch.len() < batch_len {
            match next_block(&mut input, &schema, opts.block_size, &mut line_no)? {
                Some(rows) => batch.push(rows),
                None => break,
            }
        }
        if batch.is_empty() {
            break;
        }

        let packed: Vec<(u32, Vec<u8>)> = batch
            .par_iter()
            .map(|rows| -> Result<(u32, Vec<u8>)> {
                let columns = build_columns(&schema, &rows.records);
                let payload =
                    block::encode(rows.records.len() as u32, &columns, opts.level, &dictionary)?;
                Ok((rows.records.len() as u32, payload))
            })
            .collect::<Result<_>>()?;

        for (rows, (count, payload)) in batch.iter().zip(&packed) {
            entries.push(Entry {
                offset,
                compressed_len: payload.len() as u32,
                records: *count,
                has_time: rows.min.is_some(),
                untimed: rows.untimed,
                min_time: rows.min.unwrap_or(0),
                max_time: rows.max.unwrap_or(0),
            });
            offset += format::write_frame(&mut output, payload)?;
            stats.records += u64::from(*count);
            stats.blocks += 1;
        }
    }

    let footer = format::encode_index_and_footer(&entries, offset)?;
    output.write_all(&footer)?;
    output.flush()?;

    stats.archive_bytes = offset + footer.len() as u64;
    Ok(stats)
}

fn validate(opts: &PackOptions) -> Result<()> {
    if opts.block_size == 0 || opts.block_size > MAX_BLOCK_SIZE {
        return Err(Error::InvalidOption("block size must be 1 byte to 16 MiB"));
    }
    if opts.timestamp_field.is_empty() {
        return Err(Error::InvalidOption("timestamp field must not be empty"));
    }
    codec::validate_level(opts.level)?;
    Ok(())
}

/// Trains a dictionary from the cells of the records in the first 10% of the
/// input, then seeks back to the first record. Returns no dictionary if the
/// sample is too small for zstd to train on. Lines that fail to parse are
/// skipped here. The main pass reports them.
fn train_dictionary<R: BufRead + Seek>(
    input: &mut R,
    schema: &Schema,
    input_bytes: u64,
    data_start: u64,
) -> Result<Vec<u8>> {
    let limit = input_bytes * SAMPLE_PERCENT / 100;
    let mut samples: Vec<Vec<u8>> = Vec::new();
    let mut consumed = 0u64;
    while consumed < limit {
        let Some(line) = read_line(input)? else {
            break;
        };
        consumed += line.len() as u64 + 1;
        if line.is_empty() {
            continue;
        }
        if let Ok(cells) = schema.cells(&line) {
            samples.extend(
                cells
                    .into_iter()
                    .map(|(_, cell)| cell)
                    .filter(|c| !c.is_empty()),
            );
        }
    }
    input.seek(SeekFrom::Start(data_start))?;

    if samples.len() < MIN_SAMPLE_CELLS {
        return Ok(Vec::new());
    }
    Ok(codec::train_dictionary(&samples, DICTIONARY_SIZE).unwrap_or_default())
}

/// Records gathered for one block, before it is encoded.
#[derive(Default)]
struct Rows {
    /// Each record as `(column, cell)` pairs.
    records: Vec<Vec<(String, Vec<u8>)>>,
    /// Bytes read, used to decide when the block is full.
    bytes: usize,
    min: Option<i64>,
    max: Option<i64>,
    untimed: bool,
}

/// Reads records until the block reaches `block_size`. Returns `None` at end of input.
fn next_block<R: BufRead>(
    input: &mut R,
    schema: &Schema,
    block_size: usize,
    line_no: &mut u64,
) -> Result<Option<Rows>> {
    let mut rows = Rows::default();
    while rows.bytes < block_size {
        let Some(line) = read_line(input)? else {
            break;
        };
        *line_no += 1;
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_RECORD_SIZE {
            return Err(Error::RecordTooLarge(line.len(), MAX_RECORD_SIZE));
        }

        let cells = schema.cells(&line).map_err(|e| match e {
            Error::InvalidRecord(detail) => {
                Error::InvalidRecord(format!("line {line_no}: {detail}"))
            }
            other => other,
        })?;
        let timestamp = cells
            .iter()
            .find(|(name, _)| name == schema.timestamp_field())
            .and_then(|(_, cell)| schema.cell_timestamp(cell));
        match timestamp {
            Some(t) => {
                rows.min = Some(rows.min.map_or(t, |m| m.min(t)));
                rows.max = Some(rows.max.map_or(t, |m| m.max(t)));
            }
            None => rows.untimed = true,
        }

        rows.bytes += line.len() + 1;
        rows.records.push(cells);
    }
    Ok((!rows.records.is_empty()).then_some(rows))
}

/// Turns a block's records into one raw stream per column. Each stream has one
/// cell per record, and each cell ends with `\n`.
fn build_columns(schema: &Schema, records: &[Vec<(String, Vec<u8>)>]) -> Vec<(String, Vec<u8>)> {
    let names = schema.block_columns(records);
    names
        .into_iter()
        .map(|name| {
            let mut stream = Vec::new();
            for record in records {
                if let Some((_, cell)) = record.iter().find(|(n, _)| *n == name) {
                    stream.extend_from_slice(cell);
                }
                stream.push(b'\n');
            }
            (name, stream)
        })
        .collect()
}

/// Reads one line without its line ending. Returns `None` at end of input.
fn read_line<R: BufRead>(input: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    if input.read_until(b'\n', &mut line)? == 0 {
        return Ok(None);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(Some(line))
}

/// Number of blocks compressed at once. Two per worker keeps threads busy.
fn batch_len() -> usize {
    rayon::current_num_threads().saturating_mul(2).max(1)
}
