//! Column-oriented block layout.
//!
//! A block holds the records of one row group. Each column is its own zstd
//! frame, so a query decompresses only the columns it reads. Layout:
//!
//! ```text
//! payload:      records u32 | column_count u16 | column_entry * column_count | streams
//! column_entry: name_len u16 | name | offset u32 | compressed_len u32 | raw_len u32
//! ```
//!
//! `offset` counts from the start of the payload. A raw column stream holds
//! one cell per record, and each cell is followed by `\n`. A cell is never
//! empty in a JSON archive, so an empty cell there means the key is absent. In
//! CSV an empty cell is an empty field.

use tpt_press_core::codec;

use crate::error::{Error, Result};
use crate::format::MAX_BLOCK_OUTPUT;

/// Fixed size of the payload prefix: record count and column count.
const PREFIX_LEN: usize = 6;

/// Size of a column entry besides its name: offset, compressed length and raw length.
const ENTRY_FIXED_LEN: usize = 4 + 4 + 4;

/// One column's place in a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// Column name: a CSV header field, or a JSON key.
    pub name: String,
    offset: usize,
    compressed_len: usize,
    raw_len: usize,
}

impl Column {
    /// Size of the column's cells before compression, in bytes.
    pub fn raw_len(&self) -> usize {
        self.raw_len
    }
}

/// The parts of a block that come before its column streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    /// Records in the block.
    pub records: u32,
    /// Columns in the block, in storage order.
    pub columns: Vec<Column>,
}

/// Builds a block payload from raw column streams.
///
/// Every stream must hold exactly `records` cells. Each stream is compressed
/// on its own, with `dictionary` if one is given.
pub fn encode(
    records: u32,
    columns: &[(String, Vec<u8>)],
    level: i32,
    dictionary: &[u8],
) -> Result<Vec<u8>> {
    let column_count =
        u16::try_from(columns.len()).map_err(|_| Error::Corrupt("too many columns"))?;

    let compressed: Vec<Vec<u8>> = columns
        .iter()
        .map(|(_, raw)| compress(raw, level, dictionary))
        .collect::<Result<_>>()?;

    let directory_len: usize = PREFIX_LEN
        + columns
            .iter()
            .map(|(name, _)| 2 + name.len() + ENTRY_FIXED_LEN)
            .sum::<usize>();
    let mut out = Vec::with_capacity(directory_len);
    out.extend_from_slice(&records.to_le_bytes());
    out.extend_from_slice(&column_count.to_le_bytes());

    let mut offset = directory_len;
    for ((name, raw), packed) in columns.iter().zip(&compressed) {
        let name_len = u16::try_from(name.len())
            .map_err(|_| Error::InvalidOption("column name is too long"))?;
        let offset_u32 = u32::try_from(offset).map_err(|_| Error::Corrupt("block too large"))?;
        let packed_len =
            u32::try_from(packed.len()).map_err(|_| Error::Corrupt("column too large"))?;
        let raw_len = u32::try_from(raw.len()).map_err(|_| Error::Corrupt("column too large"))?;

        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&offset_u32.to_le_bytes());
        out.extend_from_slice(&packed_len.to_le_bytes());
        out.extend_from_slice(&raw_len.to_le_bytes());
        offset += packed.len();
    }
    for packed in &compressed {
        out.extend_from_slice(packed);
    }
    Ok(out)
}

/// Reads the directory of a block payload and checks that every column lies
/// inside it.
pub fn decode_directory(payload: &[u8]) -> Result<Directory> {
    let mut cur = Cursor {
        buf: payload,
        pos: 0,
    };
    let records = cur.u32()?;
    let column_count = cur.u16()? as usize;

    let mut columns = Vec::with_capacity(column_count);
    for _ in 0..column_count {
        let name_len = cur.u16()? as usize;
        let name = String::from_utf8(cur.take(name_len)?.to_vec())
            .map_err(|_| Error::Corrupt("column name is not UTF-8"))?;
        let offset = cur.u32()? as usize;
        let compressed_len = cur.u32()? as usize;
        let raw_len = cur.u32()? as usize;

        let end = offset.checked_add(compressed_len);
        if end.is_none_or(|end| end > payload.len()) || raw_len > MAX_BLOCK_OUTPUT {
            return Err(Error::Corrupt("column lies outside its block"));
        }
        columns.push(Column {
            name,
            offset,
            compressed_len,
            raw_len,
        });
    }
    Ok(Directory { records, columns })
}

/// Decompresses one column's raw stream from a block payload.
pub fn decode_column(payload: &[u8], column: &Column, dictionary: &[u8]) -> Result<Vec<u8>> {
    let packed = &payload[column.offset..column.offset + column.compressed_len];
    let raw = decompress(packed, column.raw_len, dictionary)?;
    if raw.len() != column.raw_len {
        return Err(Error::Corrupt("column length does not match its directory"));
    }
    Ok(raw)
}

fn compress(raw: &[u8], level: i32, dictionary: &[u8]) -> Result<Vec<u8>> {
    if dictionary.is_empty() {
        Ok(codec::compress(raw, level)?)
    } else {
        Ok(codec::compress_with_dict(raw, level, dictionary)?)
    }
}

fn decompress(packed: &[u8], raw_len: usize, dictionary: &[u8]) -> Result<Vec<u8>> {
    if dictionary.is_empty() {
        Ok(codec::decompress(packed, raw_len)?)
    } else {
        Ok(codec::decompress_with_dict(packed, raw_len, dictionary)?)
    }
}

/// Bounds-checked reader over a block payload.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|&end| end <= self.buf.len())
            .ok_or(Error::Corrupt("block directory is truncated"))?;
        let bytes = &self.buf[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_round_trip_independently() {
        let columns = vec![
            ("level".to_owned(), b"info\nerror\ninfo\n".to_vec()),
            ("msg".to_owned(), b"a\n\nc\n".to_vec()),
        ];
        let payload = encode(3, &columns, 3, &[]).unwrap();
        let dir = decode_directory(&payload).unwrap();
        assert_eq!(dir.records, 3);
        assert_eq!(dir.columns.len(), 2);
        assert_eq!(
            decode_column(&payload, &dir.columns[1], &[]).unwrap(),
            columns[1].1
        );
        assert_eq!(
            decode_column(&payload, &dir.columns[0], &[]).unwrap(),
            columns[0].1
        );
    }

    #[test]
    fn truncated_directory_is_rejected() {
        let columns = vec![("a".to_owned(), b"x\n".to_vec())];
        let payload = encode(1, &columns, 3, &[]).unwrap();
        assert!(decode_directory(&payload[..5]).is_err());
    }
}
