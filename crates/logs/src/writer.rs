//! Packs JSON Lines or CSV logs into a time-indexed archive.

use std::io::{self, BufRead, Seek, SeekFrom, Write};

use rayon::prelude::*;
use tpt_press_core::codec;

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

/// Fewest sample records needed before a dictionary is trained.
const MIN_SAMPLE_RECORDS: usize = 8;

/// Fraction of the input, in percent, used as the dictionary sample.
const SAMPLE_PERCENT: u64 = 10;

/// Options for [`pack`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackOptions {
    /// Record format of the input.
    pub format: Format,
    /// Field (JSONL) or column (CSV) holding the RFC 3339 timestamp.
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
/// first 10% before the records are read again. Blank lines are dropped, and
/// CRLF line endings are accepted. Records must be single lines, so a CSV
/// field cannot contain a newline.
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

    let dictionary = train_dictionary(&mut input, input_bytes, data_start)?;

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
    loop {
        let mut batch = Vec::with_capacity(batch_len);
        while batch.len() < batch_len {
            match next_block(&mut input, &schema, opts.block_size)? {
                Some(block) => batch.push(block),
                None => break,
            }
        }
        if batch.is_empty() {
            break;
        }

        let packed: Vec<Vec<u8>> = batch
            .par_iter()
            .map(|block| format::compress_block(&block.data, opts.level, &dictionary))
            .collect::<Result<_>>()?;

        for (block, payload) in batch.iter().zip(&packed) {
            entries.push(Entry {
                offset,
                compressed_len: payload.len() as u32,
                records: block.records,
                has_time: block.min.is_some(),
                untimed: block.untimed,
                min_time: block.min.unwrap_or(0),
                max_time: block.max.unwrap_or(0),
            });
            offset += format::write_frame(&mut output, payload)?;
            stats.records += u64::from(block.records);
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

/// Trains a dictionary from the records in the first 10% of the input, then
/// seeks back to the first record. Returns no dictionary if the sample is too
/// small for zstd to train on.
fn train_dictionary<R: BufRead + Seek>(
    input: &mut R,
    input_bytes: u64,
    data_start: u64,
) -> Result<Vec<u8>> {
    let limit = input_bytes * SAMPLE_PERCENT / 100;
    let mut samples = Vec::new();
    let mut consumed = 0u64;
    while consumed < limit {
        let Some(line) = read_line(input)? else {
            break;
        };
        consumed += line.len() as u64 + 1;
        if !line.is_empty() {
            samples.push(line);
        }
    }
    input.seek(SeekFrom::Start(data_start))?;

    if samples.len() < MIN_SAMPLE_RECORDS {
        return Ok(Vec::new());
    }
    Ok(codec::train_dictionary(&samples, DICTIONARY_SIZE).unwrap_or_default())
}

/// One block being assembled, before compression.
#[derive(Default)]
struct Block {
    data: Vec<u8>,
    records: u32,
    min: Option<i64>,
    max: Option<i64>,
    untimed: bool,
}

/// Reads records until the block reaches `block_size`. Returns `None` at end of input.
fn next_block<R: BufRead>(
    input: &mut R,
    schema: &Schema,
    block_size: usize,
) -> Result<Option<Block>> {
    let mut block = Block::default();
    while block.data.len() < block_size {
        let Some(line) = read_line(input)? else {
            break;
        };
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_RECORD_SIZE {
            return Err(Error::RecordTooLarge(line.len(), MAX_RECORD_SIZE));
        }
        match schema.timestamp(&line) {
            Some(t) => {
                block.min = Some(block.min.map_or(t, |m| m.min(t)));
                block.max = Some(block.max.map_or(t, |m| m.max(t)));
            }
            None => block.untimed = true,
        }
        block.data.extend_from_slice(&line);
        block.data.push(b'\n');
        block.records += 1;
    }
    Ok((block.records > 0).then_some(block))
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
