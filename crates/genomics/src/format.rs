//! On-disk layout of a genomics archive. The layout is also described in `docs/formats.md`.
//!
//! ```text
//! header   : magic "TPGN" | version u16 LE | kind u8 | reserved u8    (8 bytes)
//! blocks   : block payloads, back to back
//! index    : total_records u64 | total_bases u64 | count u64 | entries
//!            | lookup_raw_len u64 | lookup_comp_len u64 | lookup (zstd)
//! entry    : offset u64 | length u64 | first_record u64 | records u64 | crc32 u32 | pad u32
//! lookup   : sorted (name hash u64, record u64) pairs, as raw bytes before zstd
//! footer   : index offset u64 | index length u64 | index crc32 u32 | "TPGX"   (24 bytes)
//! ```
//!
//! Integers are little-endian. Every block payload has a CRC32 in the index, and
//! the index has one in the footer.

use std::io::{Read, Seek, SeekFrom, Write};

use crate::error::{map_io, Error, Result};
use tpt_press_core::codec;

/// Magic bytes at the start of every genomics archive.
pub const MAGIC: [u8; 4] = *b"TPGN";
/// Magic bytes at the very end, after the footer fields.
const END_MAGIC: [u8; 4] = *b"TPGX";
/// Layout version. Stays at 1 until the format is stable.
pub const VERSION: u16 = 1;
/// Length of the fixed header, in bytes.
pub const HEADER_LEN: u64 = 8;
const FOOTER_LEN: u64 = 24;
const INDEX_HEAD_LEN: u64 = 24;
const ENTRY_LEN: u64 = 40;
/// Largest stream that a block may declare when reading, to bound allocation.
pub(crate) const MAX_STREAM_LEN: u64 = 1 << 30;

/// The record format of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// FASTA: a `>` header line, then sequence lines wrapped at one width.
    Fasta,
    /// FASTQ: four lines per record, with quality scores.
    Fastq,
}

impl Kind {
    fn code(self) -> u8 {
        match self {
            Kind::Fasta => 1,
            Kind::Fastq => 2,
        }
    }

    fn from_code(code: u8) -> Result<Self> {
        match code {
            1 => Ok(Kind::Fasta),
            2 => Ok(Kind::Fastq),
            _ => Err(Error::Corrupt("unknown record kind")),
        }
    }

    /// The format's name, for messages.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Fasta => "FASTA",
            Kind::Fastq => "FASTQ",
        }
    }
}

/// One block as listed in the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entry {
    pub offset: u64,
    pub len: u64,
    pub first_record: u64,
    pub records: u64,
    pub crc: u32,
}

/// The index: totals, one entry per block, and the name lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Index {
    pub total_records: u64,
    pub total_bases: u64,
    pub entries: Vec<Entry>,
    /// `(hash of the record's name, record number)`, sorted.
    pub lookup: Vec<(u64, u64)>,
}

/// FNV-1a, 64-bit. Used for the name lookup.
pub(crate) fn name_hash(name: &[u8]) -> u64 {
    name.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Writes the fixed header.
pub(crate) fn write_header<W: Write>(w: &mut W, kind: Kind) -> Result<()> {
    w.write_all(&MAGIC)?;
    w.write_all(&VERSION.to_le_bytes())?;
    w.write_all(&[kind.code(), 0])?;
    Ok(())
}

/// Reads and checks the fixed header, and returns the record kind.
pub(crate) fn read_header<R: Read + Seek>(r: &mut R) -> Result<Kind> {
    r.seek(SeekFrom::Start(0)).map_err(map_io)?;
    let mut buf = [0u8; HEADER_LEN as usize];
    r.read_exact(&mut buf).map_err(map_io)?;
    if buf[..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u16::from_le_bytes([buf[4], buf[5]]);
    if version != VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    Kind::from_code(buf[6])
}

/// The index bytes, without the footer.
fn index_bytes(index: &Index) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    put_u64(&mut bytes, index.total_records);
    put_u64(&mut bytes, index.total_bases);
    put_u64(&mut bytes, index.entries.len() as u64);
    for e in &index.entries {
        put_u64(&mut bytes, e.offset);
        put_u64(&mut bytes, e.len);
        put_u64(&mut bytes, e.first_record);
        put_u64(&mut bytes, e.records);
        bytes.extend_from_slice(&e.crc.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
    }

    let mut raw = Vec::with_capacity(index.lookup.len() * 16);
    for &(hash, record) in &index.lookup {
        put_u64(&mut raw, hash);
        put_u64(&mut raw, record);
    }
    put_u64(&mut bytes, raw.len() as u64);
    if raw.is_empty() {
        put_u64(&mut bytes, 0);
    } else {
        let compressed = codec::compress(&raw, codec::DEFAULT_LEVEL)?;
        put_u64(&mut bytes, compressed.len() as u64);
        bytes.extend_from_slice(&compressed);
    }
    Ok(bytes)
}

/// Writes the index and footer. `index_offset` is where the index starts in the output.
pub(crate) fn write_index<W: Write>(w: &mut W, index: &Index, index_offset: u64) -> Result<()> {
    let bytes = index_bytes(index)?;
    w.write_all(&bytes)?;
    w.write_all(&index_offset.to_le_bytes())?;
    w.write_all(&(bytes.len() as u64).to_le_bytes())?;
    w.write_all(&crc32fast::hash(&bytes).to_le_bytes())?;
    w.write_all(&END_MAGIC)?;
    Ok(())
}

/// The length of the file once the index and footer are written at `index_offset`.
/// `index_len` is the length of the index bytes, from [`index_bytes`].
pub(crate) fn file_len(index_len: u64, index_offset: u64) -> u64 {
    index_offset + index_len + FOOTER_LEN
}

/// The length of the index bytes that [`write_index`] writes for `index`.
pub(crate) fn index_len(index: &Index) -> Result<u64> {
    Ok(index_bytes(index)?.len() as u64)
}

/// Reads the footer and index, and checks that they agree with the file.
pub(crate) fn read_index<R: Read + Seek>(r: &mut R) -> Result<Index> {
    let file_len = r.seek(SeekFrom::End(0)).map_err(map_io)?;
    if file_len < HEADER_LEN + FOOTER_LEN {
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
    if index_offset < HEADER_LEN || end != Some(file_len) {
        return Err(Error::Corrupt("index does not match the file size"));
    }
    if index_len < INDEX_HEAD_LEN + 16 {
        return Err(Error::Corrupt("index is too short"));
    }

    r.seek(SeekFrom::Start(index_offset)).map_err(map_io)?;
    let mut bytes = vec![0u8; index_len as usize];
    r.read_exact(&mut bytes).map_err(map_io)?;
    if crc32fast::hash(&bytes) != crc {
        return Err(Error::Corrupt("index checksum does not match"));
    }

    let total_records = u64_at(&bytes, 0);
    let total_bases = u64_at(&bytes, 8);
    let count = u64_at(&bytes, 16);
    let entries_end = count
        .checked_mul(ENTRY_LEN)
        .and_then(|e| e.checked_add(INDEX_HEAD_LEN))
        .filter(|&e| e + 16 <= index_len)
        .ok_or(Error::Corrupt(
            "index entry count does not match its length",
        ))?;

    let mut entries = Vec::with_capacity(count as usize);
    let mut next_record = 0u64;
    let mut expected_offset = HEADER_LEN;
    for chunk in bytes[INDEX_HEAD_LEN as usize..entries_end as usize]
        .as_chunks::<{ ENTRY_LEN as usize }>()
        .0
    {
        let entry = Entry {
            offset: u64_at(chunk, 0),
            len: u64_at(chunk, 8),
            first_record: u64_at(chunk, 16),
            records: u64_at(chunk, 24),
            crc: u32::from_le_bytes([chunk[32], chunk[33], chunk[34], chunk[35]]),
        };
        if entry.offset != expected_offset || entry.first_record != next_record {
            return Err(Error::Corrupt("block list is out of order"));
        }
        expected_offset = entry
            .offset
            .checked_add(entry.len)
            .ok_or(Error::Corrupt("block offset overflows"))?;
        next_record = next_record
            .checked_add(entry.records)
            .ok_or(Error::Corrupt("record count overflows"))?;
        entries.push(entry);
    }
    if expected_offset != index_offset || next_record != total_records {
        return Err(Error::Corrupt("blocks do not cover the index totals"));
    }

    let lookup = read_lookup(&bytes[entries_end as usize..], total_records)?;
    Ok(Index {
        total_records,
        total_bases,
        entries,
        lookup,
    })
}

/// Reads the name lookup section, and checks that it is sorted and in range.
fn read_lookup(section: &[u8], total_records: u64) -> Result<Vec<(u64, u64)>> {
    let raw_len = u64_at(section, 0);
    let comp_len = u64_at(section, 8);
    if raw_len > total_records.saturating_mul(16) {
        return Err(Error::Corrupt(
            "name lookup is larger than the records need",
        ));
    }
    if raw_len == 0 {
        if comp_len != 0 || section.len() != 16 {
            return Err(Error::Corrupt("name lookup has trailing bytes"));
        }
        return Ok(Vec::new());
    }
    let comp = usize::try_from(comp_len)
        .ok()
        .and_then(|len| section.get(16..).filter(|rest| rest.len() == len))
        .ok_or(Error::Corrupt("name lookup is truncated"))?;
    let raw = codec::decompress(comp, raw_len as usize)?;
    if raw.len() as u64 != raw_len || raw.len() % 16 != 0 {
        return Err(Error::Corrupt("name lookup has the wrong size"));
    }
    let lookup: Vec<(u64, u64)> = raw
        .as_chunks::<16>()
        .0
        .iter()
        .map(|c| (u64_at(c, 0), u64_at(c, 8)))
        .collect();
    if lookup.iter().any(|&(_, r)| r >= total_records) || lookup.windows(2).any(|w| w[0] > w[1]) {
        return Err(Error::Corrupt(
            "name lookup is out of order or out of range",
        ));
    }
    Ok(lookup)
}

pub(crate) fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Appends `v` as an unsigned LEB128 varint.
pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(b)
}

/// A cursor over decoded bytes. Every read is bounds-checked.
pub(crate) struct Bytes<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Bytes<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(Error::Corrupt("block is truncated"))?;
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or(Error::Corrupt("block is truncated"))?;
        self.pos = end;
        Ok(slice)
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok(u64_at(self.take(8)?, 0))
    }

    pub(crate) fn varint(&mut self) -> Result<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.u8()?;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(Error::Corrupt("varint is too long"))
    }
}
