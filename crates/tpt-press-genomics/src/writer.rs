//! Packs FASTA or FASTQ into a genomics archive.
//!
//! The parser is strict. It refuses input that the archive could not give back
//! byte for byte: CRLF line endings, a missing final newline, sequence lines
//! with mixed widths, blank lines, and a FASTQ `+` line that is neither bare
//! nor the record id. Each refusal names the line.

use std::io::{BufRead, Write};

use tpt_press_core::codec;

use crate::block::{self, Columns, Record};
use crate::error::{Error, Result};
use crate::format::{self, Entry, Index, Kind, HEADER_LEN};

/// Bases per block when none is given. A block is closed once its sequence
/// reaches this size. A single record longer than this stays in one block.
pub const DEFAULT_BLOCK_BASES: usize = 1 << 20;

/// Options for [`pack`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackOptions {
    /// Sequence bytes per block, before compression. Smaller blocks make
    /// [`crate::fetch`] read less, and compress a little worse.
    pub block_bases: usize,
    /// zstd compression level, in `1..=22`.
    pub level: i32,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            block_bases: DEFAULT_BLOCK_BASES,
            level: codec::DEFAULT_LEVEL,
        }
    }
}

/// Counts from a completed [`pack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// The record format of the input.
    pub kind: Kind,
    /// Records packed.
    pub records: u64,
    /// Sequence bases packed, summed over records.
    pub bases: u64,
    /// Blocks written.
    pub blocks: u64,
    /// Bytes read from the input.
    pub input_bytes: u64,
    /// Bytes written to the archive.
    pub output_bytes: u64,
}

/// Reads FASTA or FASTQ from `input` and writes a genomics archive to `output`.
///
/// The format is taken from the first byte: `>` for FASTA, `@` for FASTQ.
pub fn pack<R: BufRead, W: Write>(
    mut input: R,
    mut output: W,
    options: &PackOptions,
) -> Result<Stats> {
    if options.block_bases == 0 {
        return Err(Error::InvalidOption("block size must be at least one base"));
    }
    codec::validate_level(options.level)?;

    let kind = match input.fill_buf()?.first() {
        Some(b'>') => Kind::Fasta,
        Some(b'@') => Kind::Fastq,
        Some(b'#') => return crate::vcf::pack(input, output, options.level),
        Some(_) => {
            return Err(Error::Format {
                line: 1,
                message: "input must start with '>' (FASTA) or '@' (FASTQ)".to_owned(),
            })
        }
        None => {
            return Err(Error::Format {
                line: 1,
                message: "input is empty".to_owned(),
            })
        }
    };

    format::write_header(&mut output, kind)?;
    let mut parser = Parser::new(input, kind);
    let mut offset = HEADER_LEN;
    let mut index = Index {
        total_records: 0,
        total_bases: 0,
        entries: Vec::new(),
        lookup: Vec::new(),
    };
    let mut block = Columns::new(kind);

    while let Some(record) = parser.next_record()? {
        let name = record
            .id
            .split(|b| b.is_ascii_whitespace())
            .next()
            .unwrap_or(&[]);
        index
            .lookup
            .push((format::name_hash(name), index.total_records));
        index.total_bases += record.seq.len() as u64;
        index.total_records += 1;
        block.push(&record);
        if block.seq.len() >= options.block_bases {
            offset = flush(&mut output, &mut index, &block, offset, options.level)?;
            block = Columns::new(kind);
        }
    }
    if block.records() > 0 {
        offset = flush(&mut output, &mut index, &block, offset, options.level)?;
    }

    index.lookup.sort_unstable();
    let blocks = index.entries.len() as u64;
    let records = index.total_records;
    let bases = index.total_bases;
    let index_len = format::index_len(&index)?;
    format::write_index(&mut output, &index, offset)?;
    output.flush()?;
    Ok(Stats {
        kind,
        records,
        bases,
        blocks,
        input_bytes: parser.lines.bytes,
        output_bytes: format::file_len(index_len, offset),
    })
}

/// Encodes one block, writes it, and records it in the index. Returns the offset after it.
fn flush<W: Write>(
    output: &mut W,
    index: &mut Index,
    block: &Columns,
    offset: u64,
    level: i32,
) -> Result<u64> {
    let payload = block::encode(block, level)?;
    output.write_all(&payload)?;
    let len = payload.len() as u64;
    let first_record = index
        .entries
        .last()
        .map_or(0, |e| e.first_record + e.records);
    index.entries.push(Entry {
        offset,
        len,
        first_record,
        records: block.records() as u64,
        crc: crc32fast::hash(&payload),
    });
    Ok(offset + len)
}

/// Reads lines and refuses anything the format cannot store.
pub(crate) struct Lines<R> {
    pub(crate) input: R,
    pub(crate) buf: Vec<u8>,
    pub(crate) line: u64,
    pub(crate) bytes: u64,
}

impl<R: BufRead> Lines<R> {
    /// Loads the next line into the buffer, without its newline. Returns
    /// `false` at the end of the input.
    pub(crate) fn advance(&mut self) -> Result<bool> {
        self.buf.clear();
        let n = self.input.read_until(b'\n', &mut self.buf)?;
        if n == 0 {
            return Ok(false);
        }
        self.bytes += n as u64;
        self.line += 1;
        if self.buf.last() != Some(&b'\n') {
            return Err(self.error("the file must end with a newline"));
        }
        self.buf.pop();
        if self.buf.contains(&b'\r') {
            return Err(Error::Unsupported {
                line: self.line,
                message: "CR characters are not supported; use LF line endings",
            });
        }
        Ok(true)
    }

    /// The line loaded by the last successful [`Lines::advance`].
    pub(crate) fn line(&self) -> &[u8] {
        &self.buf
    }

    pub(crate) fn error(&self, message: impl Into<String>) -> Error {
        Error::Format {
            line: self.line,
            message: message.into(),
        }
    }
}

/// Parses records one at a time.
struct Parser<R> {
    lines: Lines<R>,
    kind: Kind,
    /// A FASTA header that was read while finishing the previous record.
    pending_header: Option<Vec<u8>>,
}

impl<R: BufRead> Parser<R> {
    fn new(input: R, kind: Kind) -> Self {
        Self {
            lines: Lines {
                input,
                buf: Vec::new(),
                line: 0,
                bytes: 0,
            },
            kind,
            pending_header: None,
        }
    }

    fn next_record(&mut self) -> Result<Option<Record>> {
        match self.kind {
            Kind::Fasta => self.next_fasta(),
            Kind::Vcf => Err(Error::Corrupt("a VCF file is not read as FASTA or FASTQ")),
            Kind::Fastq => self.next_fastq(),
        }
    }

    fn next_fasta(&mut self) -> Result<Option<Record>> {
        let id = match self.pending_header.take() {
            Some(id) => id,
            None => {
                if !self.lines.advance()? {
                    return Ok(None);
                }
                match self.lines.line().strip_prefix(b">") {
                    Some(id) => id.to_vec(),
                    None => return Err(self.lines.error("expected a '>' header line")),
                }
            }
        };

        let mut seq = Vec::new();
        // Every sequence line but the last must have the width of the first.
        // The last may be shorter, but not empty.
        let mut width: Option<usize> = None;
        let mut short_seen = false;
        while self.lines.advance()? {
            let line = self.lines.line();
            if let Some(next_id) = line.strip_prefix(b">") {
                self.pending_header = Some(next_id.to_vec());
                break;
            }
            let len = line.len();
            if len == 0 {
                return Err(self.lines.error("blank line inside a record"));
            }
            let width_error = Error::Unsupported {
                line: self.lines.line,
                message: "line widths change inside a record; only the last line may be shorter",
            };
            if short_seen {
                return Err(width_error);
            }
            match width {
                None => width = Some(len),
                Some(w) if len == w => {}
                Some(w) if len < w => short_seen = true,
                Some(_) => return Err(width_error),
            }
            seq.extend_from_slice(line);
        }

        Ok(Some(Record {
            id,
            seq,
            width: width.unwrap_or(0) as u64,
            plus_repeats_id: false,
            quality: Vec::new(),
        }))
    }

    fn next_fastq(&mut self) -> Result<Option<Record>> {
        if !self.lines.advance()? {
            return Ok(None);
        }
        let id = match self.lines.line().strip_prefix(b"@") {
            Some(id) => id.to_vec(),
            None => return Err(self.lines.error("expected an '@' header line")),
        };
        let seq = self.required_line("record ends before its sequence line")?;

        let plus = self.required_line("record ends before its '+' line")?;
        let plus_repeats_id = match plus.strip_prefix(b"+") {
            Some([]) => false,
            Some(rest) if rest == id.as_slice() => true,
            _ => {
                return Err(self
                    .lines
                    .error("the '+' line must be empty or repeat the record id"))
            }
        };

        let quality = self.required_line("record ends before its quality line")?;
        if quality.len() != seq.len() {
            return Err(self
                .lines
                .error("the quality line has a different length from the sequence"));
        }

        Ok(Some(Record {
            id,
            seq,
            width: 0,
            plus_repeats_id,
            quality,
        }))
    }

    /// The next line, copied out. A missing line is an error, with `missing` as the message.
    fn required_line(&mut self, missing: &'static str) -> Result<Vec<u8>> {
        if !self.lines.advance()? {
            return Err(self.lines.error(missing));
        }
        Ok(self.lines.line().to_vec())
    }
}
