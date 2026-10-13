//! VCF: variant calls, packed column by column.
//!
//! A VCF file is a header of `#` lines, then one line per variant. The header is
//! kept verbatim in the first block, so unpacking gives back the same bytes.
//! Each data line is split on tabs, and each column is stored as its own stream.
//! POS is stored as the difference from the previous POS in its block, which is
//! small for sorted calls. The sample columns are stored as one stream, joined by
//! tabs, and the number of columns per line is stored too, so a line rebuilds exactly.
//!
//! Lines that could not be rebuilt exactly are refused, and the refusal names the
//! line. These are: fewer than eight columns, a POS not written in plain decimal
//! (a sign, leading zeros, or a value above `i64::MAX`), and a header line after
//! the data.

use std::io::{BufRead, Write};

use crate::block::{put_stream, take_stream};
use crate::error::{Error, Result};
use crate::format::{self, put_u64, put_varint, Bytes, Entry, Index, Kind, HEADER_LEN};
use crate::writer::{Lines, Stats};

/// Data lines per block. A block is independent of the others.
pub(crate) const RECORDS_PER_BLOCK: u64 = 4096;

/// The columns of one block, as streams before compression.
#[derive(Default)]
struct Columns {
    records: u64,
    ncols: Vec<u8>,
    pos: Vec<u8>,
    prev_pos: u64,
    chrom: Vec<u8>,
    id: Vec<u8>,
    reference: Vec<u8>,
    alt: Vec<u8>,
    qual: Vec<u8>,
    filter: Vec<u8>,
    info: Vec<u8>,
    format: Vec<u8>,
    samples: Vec<u8>,
}

/// Appends `text` and a newline, the framing every text stream uses.
fn push_text(stream: &mut Vec<u8>, text: &[u8]) {
    stream.extend_from_slice(text);
    stream.push(b'\n');
}

/// Parses a plain decimal number: digits only, no leading zeros, at most `i64::MAX`.
fn canonical_pos(text: &[u8]) -> Option<u64> {
    if text.is_empty() || !text.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if text.len() > 1 && text[0] == b'0' {
        return None;
    }
    let value: u64 = std::str::from_utf8(text).ok()?.parse().ok()?;
    (value <= i64::MAX as u64).then_some(value)
}

/// Zigzag encoding, so small negative differences stay small.
fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(u: u64) -> i64 {
    ((u >> 1) as i64) ^ -((u & 1) as i64)
}

/// Adds one data line to the block.
fn push_line(cols: &mut Columns, line: &[u8], line_no: u64) -> Result<()> {
    let fields: Vec<&[u8]> = line.split(|&b| b == b'\t').collect();
    if fields.len() < 8 {
        return Err(Error::Format {
            line: line_no,
            message: "a VCF data line has fewer than eight columns".to_owned(),
        });
    }
    let pos = canonical_pos(fields[1]).ok_or_else(|| Error::Format {
        line: line_no,
        message: "POS must be a plain decimal number, without a sign or leading zeros".to_owned(),
    })?;

    put_varint(&mut cols.ncols, fields.len() as u64);
    put_varint(&mut cols.pos, zigzag(pos as i64 - cols.prev_pos as i64));
    cols.prev_pos = pos;
    push_text(&mut cols.chrom, fields[0]);
    push_text(&mut cols.id, fields[2]);
    push_text(&mut cols.reference, fields[3]);
    push_text(&mut cols.alt, fields[4]);
    push_text(&mut cols.qual, fields[5]);
    push_text(&mut cols.filter, fields[6]);
    push_text(&mut cols.info, fields[7]);
    push_text(&mut cols.format, fields.get(8).copied().unwrap_or(b""));
    let samples = if fields.len() > 9 {
        fields[9..].join(&b'\t')
    } else {
        Vec::new()
    };
    push_text(&mut cols.samples, &samples);
    cols.records += 1;
    Ok(())
}

/// Encodes a block of data lines.
fn encode_block(cols: &Columns, level: i32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    put_u64(&mut out, cols.records);
    put_u64(&mut out, 0);
    put_u64(&mut out, 0);
    let streams: [&[u8]; 11] = [
        &cols.ncols,
        &cols.pos,
        &cols.chrom,
        &cols.id,
        &cols.reference,
        &cols.alt,
        &cols.qual,
        &cols.filter,
        &cols.info,
        &cols.format,
        &cols.samples,
    ];
    for stream in streams {
        put_stream(&mut out, stream, level)?;
    }
    Ok(out)
}

/// Splits a text stream into its `records` lines.
fn text_lines(stream: &[u8], records: usize) -> Result<Vec<&[u8]>> {
    if records == 0 {
        return Ok(Vec::new());
    }
    let body = stream
        .strip_suffix(b"\n")
        .ok_or(Error::Corrupt("VCF text stream is not newline-terminated"))?;
    let lines: Vec<&[u8]> = body.split(|&b| b == b'\n').collect();
    if lines.len() != records {
        return Err(Error::Corrupt("VCF text stream does not match the record count"));
    }
    Ok(lines)
}

/// Rebuilds the data lines of one block, each with its newline.
pub(crate) fn decode_block(payload: &[u8]) -> Result<Vec<u8>> {
    let mut b = Bytes::new(payload);
    let records = usize::try_from(b.u64()?).map_err(|_| Error::Corrupt("record count is too large"))?;
    let _ = (b.u64()?, b.u64()?);
    let ncols_raw = take_stream(&mut b)?;
    let pos_raw = take_stream(&mut b)?;
    let chrom = take_stream(&mut b)?;
    let id = take_stream(&mut b)?;
    let reference = take_stream(&mut b)?;
    let alt = take_stream(&mut b)?;
    let qual = take_stream(&mut b)?;
    let filter = take_stream(&mut b)?;
    let info = take_stream(&mut b)?;
    let format = take_stream(&mut b)?;
    let samples = take_stream(&mut b)?;
    if !b.is_empty() {
        return Err(Error::Corrupt("VCF block has trailing bytes"));
    }

    let mut ncols = Bytes::new(&ncols_raw);
    let mut deltas = Bytes::new(&pos_raw);
    let columns = [
        text_lines(&chrom, records)?,
        text_lines(&id, records)?,
        text_lines(&reference, records)?,
        text_lines(&alt, records)?,
        text_lines(&qual, records)?,
        text_lines(&filter, records)?,
        text_lines(&info, records)?,
        text_lines(&format, records)?,
        text_lines(&samples, records)?,
    ];
    let mut out = Vec::new();
    let mut pos = 0i64;
    for i in 0..records {
        let count = ncols.varint()?;
        pos = pos
            .checked_add(unzigzag(deltas.varint()?))
            .filter(|&p| p >= 0)
            .ok_or(Error::Corrupt("VCF position is out of range"))?;
        let pos_text = pos.to_string();
        let mut parts: Vec<&[u8]> = Vec::with_capacity(11);
        parts.push(columns[0][i]);
        parts.push(pos_text.as_bytes());
        for column in &columns[1..7] {
            parts.push(column[i]);
        }
        if count >= 9 {
            parts.push(columns[7][i]);
        }
        if count >= 10 {
            parts.push(columns[8][i]);
        }
        for (k, part) in parts.iter().enumerate() {
            if k > 0 {
                out.push(b'\t');
            }
            out.extend_from_slice(part);
        }
        out.push(b'\n');
    }
    Ok(out)
}

/// Encodes the header lines, verbatim, as a block with no records.
fn encode_header(header: &[u8], level: i32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    put_u64(&mut out, 0);
    put_u64(&mut out, 0);
    put_u64(&mut out, 0);
    put_stream(&mut out, header, level)?;
    Ok(out)
}

/// Rebuilds the header lines from their block.
pub(crate) fn decode_header(payload: &[u8]) -> Result<Vec<u8>> {
    let mut b = Bytes::new(payload);
    let _ = (b.u64()?, b.u64()?, b.u64()?);
    let header = take_stream(&mut b)?;
    if !b.is_empty() {
        return Err(Error::Corrupt("VCF header block has trailing bytes"));
    }
    Ok(header)
}

/// Packs a VCF file. The header goes first, then the data in blocks.
pub(crate) fn pack<R: BufRead, W: Write>(input: R, mut output: W, level: i32) -> Result<Stats> {
    let mut lines = Lines {
        input,
        buf: Vec::new(),
        line: 0,
        bytes: 0,
    };
    let mut header = Vec::new();
    let mut pending: Option<Vec<u8>> = None;
    while lines.advance()? {
        if lines.line().starts_with(b"#") {
            header.extend_from_slice(lines.line());
            header.push(b'\n');
        } else {
            pending = Some(lines.line().to_vec());
            break;
        }
    }

    format::write_header(&mut output, Kind::Vcf)?;
    let header_block = encode_header(&header, level)?;
    output.write_all(&header_block)?;
    let mut offset = HEADER_LEN + header_block.len() as u64;
    let mut index = Index {
        total_records: 0,
        total_bases: 0,
        entries: vec![Entry {
            offset: HEADER_LEN,
            len: header_block.len() as u64,
            first_record: 0,
            records: 0,
            crc: crc32fast::hash(&header_block),
        }],
        lookup: Vec::new(),
    };

    let mut cols = Columns::default();
    let mut current = pending;
    loop {
        let line = match current.take() {
            Some(line) => line,
            None => {
                if !lines.advance()? {
                    break;
                }
                lines.line().to_vec()
            }
        };
        if line.starts_with(b"#") {
            return Err(Error::Unsupported {
                line: lines.line,
                message: "header lines must all come before the data lines",
            });
        }
        push_line(&mut cols, &line, lines.line)?;
        index.total_records += 1;
        if cols.records == RECORDS_PER_BLOCK {
            offset = flush(&mut output, &mut index, &cols, offset, level)?;
            cols = Columns::default();
        }
    }
    if cols.records > 0 {
        offset = flush(&mut output, &mut index, &cols, offset, level)?;
    }

    let blocks = index.entries.len() as u64;
    let records = index.total_records;
    let index_len = format::index_len(&index)?;
    format::write_index(&mut output, &index, offset)?;
    output.flush()?;
    Ok(Stats {
        kind: Kind::Vcf,
        records,
        bases: 0,
        blocks,
        input_bytes: lines.bytes,
        output_bytes: format::file_len(index_len, offset),
    })
}

fn flush<W: Write>(
    output: &mut W,
    index: &mut Index,
    cols: &Columns,
    offset: u64,
    level: i32,
) -> Result<u64> {
    let payload = encode_block(cols, level)?;
    output.write_all(&payload)?;
    let len = payload.len() as u64;
    let first_record = index.entries.last().map_or(0, |e| e.first_record + e.records);
    index.entries.push(Entry {
        offset,
        len,
        first_record,
        records: cols.records,
        crc: crc32fast::hash(&payload),
    });
    Ok(offset + len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_must_be_plain_decimals() {
        assert_eq!(canonical_pos(b"0"), Some(0));
        assert_eq!(canonical_pos(b"12345"), Some(12345));
        assert_eq!(canonical_pos(b"012"), None);
        assert_eq!(canonical_pos(b"-1"), None);
        assert_eq!(canonical_pos(b"+1"), None);
        assert_eq!(canonical_pos(b""), None);
    }

    #[test]
    fn zigzag_round_trips() {
        for v in [0i64, 1, -1, 2, -2, 1_000_000, -1_000_000, i64::MAX, i64::MIN] {
            assert_eq!(unzigzag(zigzag(v)), v);
        }
    }

    #[test]
    fn a_block_rebuilds_its_lines() {
        let text = b"chr1\t100\trs1\tA\tG\t50\tPASS\tDP=3\tGT:DP\t0/1:3\t1/1:9\n\
                     chr1\t90\t.\tC\tT\t.\t.\t.\tGT\t0/0\t0/0\n\
                     chr2\t7\t.\tG\tA,C\t9\tq10\tAF=0.5\tGT\t.\t1|0\n";
        let mut cols = Columns::default();
        for (i, line) in text.split(|&b| b == b'\n').filter(|l| !l.is_empty()).enumerate() {
            push_line(&mut cols, line, i as u64 + 1).unwrap();
        }
        let payload = encode_block(&cols, 3).unwrap();
        let rebuilt = decode_block(&payload).unwrap();
        assert_eq!(rebuilt, text.to_vec());
    }

    #[test]
    fn a_short_or_odd_line_is_refused() {
        let mut cols = Columns::default();
        assert!(push_line(&mut cols, b"chr1\t1\t.\tA", 4).is_err());
        assert!(push_line(&mut cols, b"chr1\t01\t.\tA\tG\t.\t.\t.", 5).is_err());
    }
}
