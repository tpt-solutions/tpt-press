//! On-disk layout of a logs archive, version 3. Version 3 adds a per-column
//! encoding byte to block directories (see `block.rs`).
//!
//! ```text
//! header: magic "TPLG" | version u16 | format u8 | reserved u8
//!         | timestamp_field (u16 length + bytes)
//!         | dictionary      (u32 length + bytes, empty if none)
//!         | csv_header      (u32 length + bytes, empty for JSONL)
//! block*: payload_len u32 | crc32 u32 | payload
//!         payload: column-oriented, see `block.rs`
//! index:  count u32 | entry * count            (see `Entry`)
//! footer: index_offset u64 | index_len u32 | index_crc32 u32 | magic "TPLX"
//! ```
//!
//! Each block's CRC32 covers its whole payload. The index lets a query find the
//! blocks that overlap a time range without reading the others.

use std::io::{Read, Seek, SeekFrom, Write};

use crate::error::{map_io, Error, Result};

/// Magic bytes at the start of every logs archive.
pub const MAGIC: [u8; 4] = *b"TPLG";

/// Magic bytes that end every logs archive.
pub const FOOTER_MAGIC: [u8; 4] = *b"TPLX";

/// Version written by this build and the only one it reads.
pub const VERSION: u16 = 3;

/// Size of the footer in bytes.
pub const FOOTER_LEN: u64 = 20;

/// Size of a block's frame header in bytes.
pub const FRAME_HEADER_LEN: u64 = 8;

/// Size of one encoded index entry in bytes.
pub const ENTRY_LEN: usize = 33;

/// Largest block payload accepted when reading. Column streams never expand
/// past their raw size by much, so twice the uncompressed limit is generous.
pub const MAX_PAYLOAD_LEN: u64 = 2 * MAX_BLOCK_OUTPUT as u64;

/// Largest uncompressed block accepted when reading.
pub const MAX_BLOCK_OUTPUT: usize = 128 * 1024 * 1024;

/// Largest dictionary accepted when reading.
const MAX_DICTIONARY: usize = 1024 * 1024;

/// Largest header string accepted when reading (field name or CSV header).
const MAX_HEADER_STRING: usize = 1024 * 1024;

/// Largest index accepted when reading.
const MAX_INDEX_LEN: u64 = 512 * 1024 * 1024;

/// The record format of the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// One JSON object per line.
    Jsonl,
    /// Comma-separated values with a header line.
    Csv,
    /// Prometheus text exposition samples, one per line. Each sample is stored
    /// as a JSON object with its metric name, labels, value and timestamp.
    /// `# HELP` and `# TYPE` comment lines are skipped.
    Prometheus,
    /// OpenTelemetry log exports in OTLP JSON. Each line is one export request,
    /// which can hold several records. See `otlp.rs`.
    Otlp,
}

impl Format {
    /// Whether records are stored as JSON cells, so output is JSON objects.
    pub fn is_json(self) -> bool {
        matches!(self, Format::Jsonl | Format::Prometheus | Format::Otlp)
    }

    fn code(self) -> u8 {
        match self {
            Format::Jsonl => 1,
            Format::Csv => 2,
            Format::Prometheus => 3,
            Format::Otlp => 4,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Format::Jsonl),
            2 => Some(Format::Csv),
            3 => Some(Format::Prometheus),
            4 => Some(Format::Otlp),
            _ => None,
        }
    }
}

/// Archive-wide settings, stored once at the start of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Record format.
    pub format: Format,
    /// Name of the field (JSONL) or column (CSV) holding the RFC 3339 timestamp.
    pub timestamp_field: String,
    /// Zstd dictionary shared by all blocks. Empty if none was trained.
    pub dictionary: Vec<u8>,
    /// The CSV header line, without its newline. Empty for JSONL.
    pub csv_header: Vec<u8>,
}

/// Where one block is, and what time range it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// Byte offset of the block's frame header.
    pub offset: u64,
    /// Length of the block's compressed payload.
    pub compressed_len: u32,
    /// Number of records in the block.
    pub records: u32,
    /// At least one record in the block has a parseable timestamp.
    pub has_time: bool,
    /// At least one record has no parseable timestamp. Such a block is
    /// always read, because its records may fall in any range.
    pub untimed: bool,
    /// Earliest timestamp in the block, in Unix nanoseconds. Valid if `has_time`.
    pub min_time: i64,
    /// Latest timestamp in the block, in Unix nanoseconds. Valid if `has_time`.
    pub max_time: i64,
}

/// Writes the header and returns the number of bytes written.
pub fn write_header<W: Write>(w: &mut W, header: &Header) -> Result<u64> {
    let field = header.timestamp_field.as_bytes();
    let field_len = u16::try_from(field.len())
        .map_err(|_| Error::InvalidOption("timestamp field name is too long"))?;
    check_len(header.dictionary.len(), MAX_DICTIONARY, "dictionary")?;
    check_len(header.csv_header.len(), MAX_HEADER_STRING, "CSV header")?;

    let mut buf = Vec::new();
    buf.extend_from_slice(&MAGIC);
    buf.extend_from_slice(&VERSION.to_le_bytes());
    buf.push(header.format.code());
    buf.push(0); // reserved
    buf.extend_from_slice(&field_len.to_le_bytes());
    buf.extend_from_slice(field);
    buf.extend_from_slice(&(header.dictionary.len() as u32).to_le_bytes());
    buf.extend_from_slice(&header.dictionary);
    buf.extend_from_slice(&(header.csv_header.len() as u32).to_le_bytes());
    buf.extend_from_slice(&header.csv_header);
    w.write_all(&buf)?;
    Ok(buf.len() as u64)
}

/// Reads and validates the header.
pub fn read_header<R: Read>(r: &mut R) -> Result<Header> {
    let fixed = read_vec(r, 8)?;
    if fixed[..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u16::from_le_bytes([fixed[4], fixed[5]]);
    if version != VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    let format = Format::from_code(fixed[6]).ok_or(Error::Corrupt("unknown record format"))?;

    let field_len = read_u16(r)? as usize;
    let timestamp_field = String::from_utf8(read_vec(r, field_len)?)
        .map_err(|_| Error::Corrupt("timestamp field name is not UTF-8"))?;

    let dict_len = read_u32(r)? as usize;
    check_read_len(dict_len, MAX_DICTIONARY, "dictionary")?;
    let dictionary = read_vec(r, dict_len)?;

    let csv_len = read_u32(r)? as usize;
    check_read_len(csv_len, MAX_HEADER_STRING, "CSV header")?;
    let csv_header = read_vec(r, csv_len)?;

    Ok(Header {
        format,
        timestamp_field,
        dictionary,
        csv_header,
    })
}

/// Writes one block frame and returns the total bytes written.
pub fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> Result<u64> {
    let len = u32::try_from(payload.len()).map_err(|_| Error::Corrupt("block too large"))?;
    let mut buf = Vec::with_capacity(FRAME_HEADER_LEN as usize + payload.len());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf)?;
    Ok(buf.len() as u64)
}

/// Reads the block that `entry` points to and checks its checksum. Returns the payload.
pub fn read_payload<R: Read + Seek>(r: &mut R, entry: &Entry) -> Result<Vec<u8>> {
    // Checked before anything is allocated, so a damaged index cannot force a huge buffer.
    if u64::from(entry.compressed_len) > MAX_PAYLOAD_LEN {
        return Err(Error::Corrupt("block declares an implausible size"));
    }
    r.seek(SeekFrom::Start(entry.offset)).map_err(map_io)?;
    let len = read_u32(r)?;
    let crc = read_u32(r)?;
    if len != entry.compressed_len {
        return Err(Error::Corrupt("block length does not match index"));
    }
    let payload = read_vec(r, len as usize)?;
    if crc32fast::hash(&payload) != crc {
        return Err(Error::Corrupt("block checksum mismatch"));
    }
    Ok(payload)
}

/// Encodes the index and the footer that points to it. Returns the bytes to
/// write after the last block.
pub fn encode_index_and_footer(entries: &[Entry], index_offset: u64) -> Result<Vec<u8>> {
    let count = u32::try_from(entries.len()).map_err(|_| Error::Corrupt("too many blocks"))?;
    let mut index = Vec::with_capacity(4 + entries.len() * ENTRY_LEN);
    index.extend_from_slice(&count.to_le_bytes());
    for e in entries {
        index.extend_from_slice(&e.offset.to_le_bytes());
        index.extend_from_slice(&e.compressed_len.to_le_bytes());
        index.extend_from_slice(&e.records.to_le_bytes());
        let flags = u8::from(e.has_time) | (u8::from(e.untimed) << 1);
        index.push(flags);
        index.extend_from_slice(&e.min_time.to_le_bytes());
        index.extend_from_slice(&e.max_time.to_le_bytes());
    }

    let index_len = u32::try_from(index.len()).map_err(|_| Error::Corrupt("index too large"))?;
    let index_crc = crc32fast::hash(&index);
    let mut out = index;
    out.extend_from_slice(&index_offset.to_le_bytes());
    out.extend_from_slice(&index_len.to_le_bytes());
    out.extend_from_slice(&index_crc.to_le_bytes());
    out.extend_from_slice(&FOOTER_MAGIC);
    Ok(out)
}

/// Reads the footer and index. Leaves the reader position undefined.
pub fn read_index<R: Read + Seek>(r: &mut R) -> Result<Vec<Entry>> {
    let file_len = r.seek(SeekFrom::End(0)).map_err(map_io)?;
    if file_len < FOOTER_LEN {
        return Err(Error::Truncated);
    }
    r.seek(SeekFrom::End(-(FOOTER_LEN as i64)))
        .map_err(map_io)?;
    let footer = read_vec(r, FOOTER_LEN as usize)?;
    if footer[16..20] != FOOTER_MAGIC {
        return Err(Error::Truncated);
    }
    let index_offset = u64::from_le_bytes(footer[0..8].try_into().unwrap());
    let index_len = u32::from_le_bytes(footer[8..12].try_into().unwrap());
    let index_crc = u32::from_le_bytes(footer[12..16].try_into().unwrap());

    if u64::from(index_len) > MAX_INDEX_LEN
        || index_offset.checked_add(u64::from(index_len) + FOOTER_LEN) != Some(file_len)
    {
        return Err(Error::Corrupt("index location is invalid"));
    }

    r.seek(SeekFrom::Start(index_offset)).map_err(map_io)?;
    let index = read_vec(r, index_len as usize)?;
    if crc32fast::hash(&index) != index_crc {
        return Err(Error::Corrupt("index checksum mismatch"));
    }
    decode_index(&index)
}

fn decode_index(bytes: &[u8]) -> Result<Vec<Entry>> {
    if bytes.len() < 4 {
        return Err(Error::Corrupt("index is too short"));
    }
    let count = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    if bytes.len() != 4 + count * ENTRY_LEN {
        return Err(Error::Corrupt("index length does not match block count"));
    }
    let (entries, _) = bytes[4..].as_chunks::<ENTRY_LEN>();
    Ok(entries
        .iter()
        .map(|c| {
            let flags = c[16];
            Entry {
                offset: u64::from_le_bytes(c[0..8].try_into().unwrap()),
                compressed_len: u32::from_le_bytes(c[8..12].try_into().unwrap()),
                records: u32::from_le_bytes(c[12..16].try_into().unwrap()),
                has_time: flags & 1 != 0,
                untimed: flags & 2 != 0,
                min_time: i64::from_le_bytes(c[17..25].try_into().unwrap()),
                max_time: i64::from_le_bytes(c[25..33].try_into().unwrap()),
            }
        })
        .collect())
}

fn check_len(len: usize, max: usize, what: &'static str) -> Result<()> {
    if len > max {
        Err(Error::InvalidOption(match what {
            "dictionary" => "dictionary is too large",
            _ => "CSV header is too long",
        }))
    } else {
        Ok(())
    }
}

fn check_read_len(len: usize, max: usize, what: &'static str) -> Result<()> {
    if len > max {
        Err(Error::Corrupt(match what {
            "dictionary" => "dictionary length is implausible",
            _ => "CSV header length is implausible",
        }))
    } else {
        Ok(())
    }
}

fn read_vec<R: Read>(r: &mut R, len: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).map_err(map_io)?;
    Ok(buf)
}

fn read_u16<R: Read>(r: &mut R) -> Result<u16> {
    let b = read_vec(r, 2)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32> {
    let b = read_vec(r, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_round_trip() {
        let entries = vec![
            Entry {
                offset: 100,
                compressed_len: 50,
                records: 3,
                has_time: true,
                untimed: false,
                min_time: -5,
                max_time: 9,
            },
            Entry {
                offset: 200,
                compressed_len: 7,
                records: 1,
                has_time: false,
                untimed: true,
                min_time: 0,
                max_time: 0,
            },
        ];
        let footer = encode_index_and_footer(&entries, 1000).unwrap();
        let index_len = footer.len() - FOOTER_LEN as usize;
        let decoded = decode_index(&footer[..index_len]).unwrap();
        assert_eq!(decoded, entries);
    }
}
