//! On-disk layout of a `.tpa` assets archive, version 1.
//!
//! ```text
//! header: magic "TPAS" | version u16 | flags u16 (reserved, must be 0)
//! data:   each asset's payload, starting on a 4096-byte boundary
//! index:  count u32 | entry * count, sorted by name (see `Entry`)
//! footer: index_offset u64 | index_len u32 | index_crc32 u32 | magic "TPAX"
//! ```
//!
//! A stored asset's payload is its raw bytes, so a reader can use it straight
//! from the mapped file. A compressed asset's payload is one zstd frame. Page
//! alignment keeps stored payloads aligned for the OS and for GPU uploads.

use crate::error::{Error, Result};

/// Magic bytes at the start of every assets archive.
pub const MAGIC: [u8; 4] = *b"TPAS";

/// Magic bytes that end every assets archive.
pub const FOOTER_MAGIC: [u8; 4] = *b"TPAX";

/// Version written by this build and the only one it reads.
pub const VERSION: u16 = 1;

/// Size of the header in bytes.
pub const HEADER_LEN: u64 = 8;

/// Size of the footer in bytes.
pub const FOOTER_LEN: u64 = 20;

/// Alignment of every asset payload.
pub const PAGE_SIZE: u64 = 4096;

/// Longest asset name accepted, in bytes.
pub const MAX_NAME_LEN: usize = 1024;

/// Largest index accepted when reading.
const MAX_INDEX_LEN: u64 = 256 * 1024 * 1024;

/// Largest asset that may be stored compressed. Decompression allocates this
/// much up front, so a larger declared size could exhaust memory.
pub const MAX_COMPRESSED_ASSET_BYTES: u64 = 1024 * 1024 * 1024;

/// How an asset's payload is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Raw bytes. Readers can use them without copying.
    Stored,
    /// One zstd frame.
    Zstd,
}

impl Compression {
    fn code(self) -> u8 {
        match self {
            Compression::Stored => 0,
            Compression::Zstd => 1,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Compression::Stored),
            1 => Some(Compression::Zstd),
            _ => None,
        }
    }
}

/// One asset's place in the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Relative path inside the archive, with `/` separators.
    pub name: String,
    /// Byte offset of the payload. Always a multiple of [`PAGE_SIZE`].
    pub offset: u64,
    /// Length of the payload as stored.
    pub stored_len: u64,
    /// Length of the asset's contents.
    pub raw_len: u64,
    /// How the payload is stored.
    pub compression: Compression,
    /// CRC32 of the payload as stored.
    pub crc32: u32,
}

/// Checks that `name` is a safe relative path. Names come from untrusted
/// archives, so this stops an unpack from writing outside its output directory.
pub fn validate_name(name: &str) -> Result<()> {
    let unsafe_name = || Error::UnsafeName(name.to_owned());
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(unsafe_name());
    }
    if name.contains(['\\', '\0', ':']) || name.starts_with('/') {
        return Err(unsafe_name());
    }
    for segment in name.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(unsafe_name());
        }
    }
    Ok(())
}

/// Encodes the index and the footer. `index_offset` is where the index starts.
pub fn encode_index_and_footer(entries: &[Entry], index_offset: u64) -> Result<Vec<u8>> {
    let count = u32::try_from(entries.len()).map_err(|_| Error::Corrupt("too many assets"))?;
    let mut index = Vec::new();
    index.extend_from_slice(&count.to_le_bytes());
    for e in entries {
        let name = e.name.as_bytes();
        let name_len = u16::try_from(name.len())
            .map_err(|_| Error::InvalidOption("asset name is too long"))?;
        index.extend_from_slice(&name_len.to_le_bytes());
        index.extend_from_slice(name);
        index.extend_from_slice(&e.offset.to_le_bytes());
        index.extend_from_slice(&e.stored_len.to_le_bytes());
        index.extend_from_slice(&e.raw_len.to_le_bytes());
        index.push(e.compression.code());
        index.extend_from_slice(&e.crc32.to_le_bytes());
    }

    let index_len = u32::try_from(index.len()).map_err(|_| Error::Corrupt("index too large"))?;
    let index_crc = crc32fast::hash(&index);
    index.extend_from_slice(&index_offset.to_le_bytes());
    index.extend_from_slice(&index_len.to_le_bytes());
    index.extend_from_slice(&index_crc.to_le_bytes());
    index.extend_from_slice(&FOOTER_MAGIC);
    Ok(index)
}

/// Reads the footer and index from `bytes`, and checks everything the reader relies on.
///
/// Returns the entries sorted by name, so lookups can binary-search them.
pub fn read_index(bytes: &[u8]) -> Result<Vec<Entry>> {
    let file_len = bytes.len() as u64;
    if file_len < HEADER_LEN + FOOTER_LEN {
        return Err(Error::Truncated);
    }

    let footer = &bytes[bytes.len() - FOOTER_LEN as usize..];
    if footer[16..20] != FOOTER_MAGIC {
        return Err(Error::Truncated);
    }
    let index_offset = u64::from_le_bytes(footer[0..8].try_into().unwrap());
    let index_len = u32::from_le_bytes(footer[8..12].try_into().unwrap());
    let index_crc = u32::from_le_bytes(footer[12..16].try_into().unwrap());

    let index_end = index_offset.checked_add(u64::from(index_len));
    if u64::from(index_len) > MAX_INDEX_LEN
        || index_offset < HEADER_LEN
        || index_end.is_none_or(|end| end + FOOTER_LEN != file_len)
    {
        return Err(Error::Corrupt("index location is invalid"));
    }
    let index = &bytes[index_offset as usize..(index_offset + u64::from(index_len)) as usize];
    if crc32fast::hash(index) != index_crc {
        return Err(Error::Corrupt("index checksum mismatch"));
    }

    let entries = decode_index(index)?;
    for e in &entries {
        validate_name(&e.name)?;
        if e.compression == Compression::Zstd && e.raw_len > MAX_COMPRESSED_ASSET_BYTES {
            return Err(Error::Corrupt(
                "compressed asset declares an implausible size",
            ));
        }
        let end = e.offset.checked_add(e.stored_len);
        if e.offset % PAGE_SIZE != 0
            || e.offset < HEADER_LEN
            || end.is_none_or(|end| end > index_offset)
        {
            return Err(Error::Corrupt("asset payload lies outside the data area"));
        }
    }
    if entries.windows(2).any(|w| w[0].name >= w[1].name) {
        return Err(Error::Corrupt(
            "index is not sorted, or has duplicate names",
        ));
    }
    Ok(entries)
}

fn decode_index(bytes: &[u8]) -> Result<Vec<Entry>> {
    let mut cur = Cursor { buf: bytes, pos: 0 };
    let count = cur.u32()? as usize;
    let mut entries = Vec::with_capacity(count.min(bytes.len() / 8));
    for _ in 0..count {
        let name_len = cur.u16()? as usize;
        if name_len > MAX_NAME_LEN {
            return Err(Error::Corrupt("asset name is too long"));
        }
        let name = String::from_utf8(cur.take(name_len)?.to_vec())
            .map_err(|_| Error::Corrupt("asset name is not UTF-8"))?;
        let offset = cur.u64()?;
        let stored_len = cur.u64()?;
        let raw_len = cur.u64()?;
        let compression =
            Compression::from_code(cur.take(1)?[0]).ok_or(Error::Corrupt("unknown compression"))?;
        let crc32 = cur.u32()?;
        entries.push(Entry {
            name,
            offset,
            stored_len,
            raw_len,
            compression,
            crc32,
        });
    }
    if cur.pos != bytes.len() {
        return Err(Error::Corrupt("index has trailing bytes"));
    }
    Ok(entries)
}

/// Checks the header of a file, which must be at least `HEADER_LEN` bytes.
pub fn check_header(bytes: &[u8]) -> Result<()> {
    if bytes.len() < HEADER_LEN as usize {
        return Err(Error::Truncated);
    }
    if bytes[..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    if u16::from_le_bytes([bytes[6], bytes[7]]) != 0 {
        return Err(Error::Corrupt("reserved header flags are set"));
    }
    Ok(())
}

/// Writes the header.
pub fn header_bytes() -> [u8; HEADER_LEN as usize] {
    let mut h = [0u8; HEADER_LEN as usize];
    h[..4].copy_from_slice(&MAGIC);
    h[4..6].copy_from_slice(&VERSION.to_le_bytes());
    h
}

/// Bounds-checked reader over the index bytes.
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
            .ok_or(Error::Corrupt("index is truncated"))?;
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

    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes(b.try_into().unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_relative_paths() {
        for name in ["a.png", "textures/rock.dds", "a/b/c.bin"] {
            assert!(validate_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn rejects_paths_that_escape_or_are_ambiguous() {
        for name in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "./a",
            "a\\b",
            "C:x",
            "a\0b",
        ] {
            assert!(validate_name(name).is_err(), "{name:?}");
        }
    }
}

#[cfg(test)]
mod size_tests {
    use super::*;

    #[test]
    fn compressed_asset_with_huge_declared_size_is_rejected() {
        let entries = vec![Entry {
            name: "a".to_owned(),
            offset: PAGE_SIZE,
            stored_len: 10,
            raw_len: u64::MAX,
            compression: Compression::Zstd,
            crc32: 0,
        }];
        let mut bytes = vec![0u8; PAGE_SIZE as usize + 10];
        bytes[..4].copy_from_slice(&MAGIC);
        bytes[4..6].copy_from_slice(&VERSION.to_le_bytes());
        let index_offset = bytes.len() as u64;
        bytes.extend_from_slice(&encode_index_and_footer(&entries, index_offset).unwrap());
        assert!(matches!(read_index(&bytes), Err(Error::Corrupt(_))));
    }
}
