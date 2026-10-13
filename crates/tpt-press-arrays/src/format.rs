//! On-disk layout of an arrays archive. The layout is also described in `docs/formats.md`.
//!
//! ```text
//! header : magic "TPAR" | version u16 | ndim u8 | elem u8 | descr_len u16 | descr
//!          | shape (ndim x u64) | chunk shape (ndim x u64)
//!          | raw_len u64 | raw bytes (the original .npy header, or empty)
//! chunks : chunk payloads, back to back, in C order over the chunk grid
//! index  : count u64 | entries
//! entry  : offset u64 | length u64 | crc32 u32 | filter u8 | pad [u8; 3]
//! footer : index offset u64 | index length u64 | index crc32 u32 | "TPRX"
//! ```
//!
//! Integers are little-endian.

use std::io::{Read, Seek, SeekFrom, Write};

use crate::dtype::Dtype;
use crate::error::{map_io, Error, Result};
use crate::filter::Filter;

/// Magic bytes at the start of every arrays archive.
pub const MAGIC: [u8; 4] = *b"TPAR";
const END_MAGIC: [u8; 4] = *b"TPRX";
/// Layout version. Stays at 1 until the format is stable.
pub const VERSION: u16 = 1;
const FOOTER_LEN: u64 = 24;
const ENTRY_LEN: u64 = 24;
/// Largest number of dimensions accepted.
pub const MAX_DIMS: usize = 32;

/// The fixed part of an archive: what the array is and how it is chunked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Header {
    pub dtype: Dtype,
    pub shape: Vec<u64>,
    pub chunk: Vec<u64>,
    /// The original `.npy` header bytes, or empty for raw input.
    pub raw: Vec<u8>,
}

/// One chunk as listed in the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entry {
    pub offset: u64,
    pub len: u64,
    pub crc: u32,
    pub filter: Filter,
}

/// Writes the header. Returns its length in bytes, which is where the chunks start.
pub(crate) fn write_header<W: Write>(w: &mut W, header: &Header) -> Result<u64> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.push(header.shape.len() as u8);
    out.push(header.dtype.size() as u8);
    let descr = header.dtype.descr().as_bytes();
    out.extend_from_slice(&(descr.len() as u16).to_le_bytes());
    out.extend_from_slice(descr);
    for &d in header.shape.iter().chain(&header.chunk) {
        out.extend_from_slice(&d.to_le_bytes());
    }
    out.extend_from_slice(&(header.raw.len() as u64).to_le_bytes());
    out.extend_from_slice(&header.raw);
    w.write_all(&out)?;
    Ok(out.len() as u64)
}

/// Reads and checks the header.
pub(crate) fn read_header<R: Read + Seek>(r: &mut R) -> Result<Header> {
    r.seek(SeekFrom::Start(0)).map_err(map_io)?;
    let mut fixed = [0u8; 10];
    r.read_exact(&mut fixed).map_err(map_io)?;
    if fixed[..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u16::from_le_bytes([fixed[4], fixed[5]]);
    if version != VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    let ndim = fixed[6] as usize;
    let elem = fixed[7] as usize;
    let descr_len = u16::from_le_bytes([fixed[8], fixed[9]]) as usize;
    if ndim == 0 || ndim > MAX_DIMS {
        return Err(Error::Corrupt("the number of dimensions is out of range"));
    }

    let mut descr = vec![0u8; descr_len];
    r.read_exact(&mut descr).map_err(map_io)?;
    let descr = String::from_utf8(descr).map_err(|_| Error::Corrupt("element type is not text"))?;
    let dtype =
        Dtype::parse(&descr).map_err(|_| Error::Corrupt("element type is not supported"))?;
    if dtype.size() != elem {
        return Err(Error::Corrupt(
            "element size does not match the element type",
        ));
    }

    let shape = read_u64s(r, ndim)?;
    let chunk = read_u64s(r, ndim)?;
    if chunk.contains(&0) {
        return Err(Error::Corrupt("a chunk dimension is zero"));
    }
    let raw_len = read_u64(r)?;
    if raw_len > 1 << 20 {
        return Err(Error::Corrupt("stored header is too large"));
    }
    let mut raw = vec![0u8; raw_len as usize];
    r.read_exact(&mut raw).map_err(map_io)?;
    Ok(Header {
        dtype,
        shape,
        chunk,
        raw,
    })
}

/// The length of the header that [`write_header`] writes.
pub(crate) fn header_len(header: &Header) -> u64 {
    10 + header.dtype.descr().len() as u64
        + 16 * header.shape.len() as u64
        + 8
        + header.raw.len() as u64
}

/// Writes the index and footer, with the chunks ending at `index_offset`.
pub(crate) fn write_index<W: Write>(w: &mut W, entries: &[Entry], index_offset: u64) -> Result<()> {
    let mut bytes = Vec::with_capacity(8 + ENTRY_LEN as usize * entries.len());
    bytes.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for e in entries {
        bytes.extend_from_slice(&e.offset.to_le_bytes());
        bytes.extend_from_slice(&e.len.to_le_bytes());
        bytes.extend_from_slice(&e.crc.to_le_bytes());
        bytes.push(e.filter.code());
        bytes.extend_from_slice(&[0, 0, 0]);
    }
    w.write_all(&bytes)?;
    w.write_all(&index_offset.to_le_bytes())?;
    w.write_all(&(bytes.len() as u64).to_le_bytes())?;
    w.write_all(&crc32fast::hash(&bytes).to_le_bytes())?;
    w.write_all(&END_MAGIC)?;
    Ok(())
}

/// The length of the file once the index and footer are written at `index_offset`.
pub(crate) fn file_len(entry_count: usize, index_offset: u64) -> u64 {
    index_offset + 8 + ENTRY_LEN * entry_count as u64 + FOOTER_LEN
}

/// Reads the footer and index. `data_start` is where the chunks begin, and `expected`
/// is the number of chunks the shape implies.
pub(crate) fn read_index<R: Read + Seek>(
    r: &mut R,
    data_start: u64,
    expected: u64,
) -> Result<Vec<Entry>> {
    let file_len = r.seek(SeekFrom::End(0)).map_err(map_io)?;
    if file_len < data_start + FOOTER_LEN {
        return Err(Error::Truncated);
    }
    r.seek(SeekFrom::End(-(FOOTER_LEN as i64)))
        .map_err(map_io)?;
    let mut footer = [0u8; FOOTER_LEN as usize];
    r.read_exact(&mut footer).map_err(map_io)?;
    if footer[20..24] != END_MAGIC {
        return Err(Error::Truncated);
    }
    let index_offset = u64_at(&footer, 0);
    let index_len = u64_at(&footer, 8);
    let crc = u32::from_le_bytes([footer[16], footer[17], footer[18], footer[19]]);
    let end = index_offset
        .checked_add(index_len)
        .and_then(|e| e.checked_add(FOOTER_LEN));
    if index_offset < data_start || end != Some(file_len) {
        return Err(Error::Corrupt("index does not match the file size"));
    }
    if index_len < 8 || !(index_len - 8).is_multiple_of(ENTRY_LEN) {
        return Err(Error::Corrupt("index has the wrong length"));
    }

    r.seek(SeekFrom::Start(index_offset)).map_err(map_io)?;
    let mut bytes = vec![0u8; index_len as usize];
    r.read_exact(&mut bytes).map_err(map_io)?;
    if crc32fast::hash(&bytes) != crc {
        return Err(Error::Corrupt("index checksum does not match"));
    }
    let count = u64_at(&bytes, 0);
    if count != expected || count != (index_len - 8) / ENTRY_LEN {
        return Err(Error::Corrupt("chunk count does not match the shape"));
    }

    let mut entries = Vec::with_capacity(count as usize);
    let mut expected_offset = data_start;
    for chunk in bytes[8..].as_chunks::<{ ENTRY_LEN as usize }>().0 {
        let entry = Entry {
            offset: u64_at(chunk, 0),
            len: u64_at(chunk, 8),
            crc: u32::from_le_bytes([chunk[16], chunk[17], chunk[18], chunk[19]]),
            filter: Filter::from_code(chunk[20])?,
        };
        if entry.offset != expected_offset {
            return Err(Error::Corrupt("chunks are out of order"));
        }
        expected_offset = entry
            .offset
            .checked_add(entry.len)
            .ok_or(Error::Corrupt("chunk offset overflows"))?;
        entries.push(entry);
    }
    if expected_offset != index_offset {
        return Err(Error::Corrupt("chunks do not reach the index"));
    }
    Ok(entries)
}

fn read_u64<R: Read>(r: &mut R) -> Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).map_err(map_io)?;
    Ok(u64::from_le_bytes(b))
}

fn read_u64s<R: Read>(r: &mut R, n: usize) -> Result<Vec<u64>> {
    (0..n).map(|_| read_u64(r)).collect()
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(b)
}
