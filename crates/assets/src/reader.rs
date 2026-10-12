//! Reads assets from a memory-mapped `.tpa` archive.

use std::fs::File;
use std::ops::Deref;
use std::path::Path;

use memmap2::Mmap;
use tpt_press_core::codec;

use crate::error::{Error, Result};
use crate::format::{self, Compression, Entry};

/// An asset's contents. Stored assets borrow from the mapped file, and
/// compressed assets are decompressed into memory.
#[derive(Debug)]
pub enum Asset<'a> {
    /// Borrowed from the archive. No bytes were copied.
    Borrowed(&'a [u8]),
    /// Decompressed into a new buffer.
    Owned(Vec<u8>),
}

impl Asset<'_> {
    /// Whether the contents were used in place, without copying.
    pub fn is_zero_copy(&self) -> bool {
        matches!(self, Asset::Borrowed(_))
    }
}

impl Deref for Asset<'_> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Asset::Borrowed(bytes) => bytes,
            Asset::Owned(bytes) => bytes,
        }
    }
}

/// An open archive. Its file is memory-mapped, and the index is checked on open.
pub struct Archive {
    map: Mmap,
    entries: Vec<Entry>,
}

impl Archive {
    /// Opens and checks an archive.
    ///
    /// The archive file must not be changed while it is open. Changes made by
    /// another process can make reads fail, or return changed bytes.
    #[allow(unsafe_code)]
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        // SAFETY: the mapping is read-only, and this crate never writes through it.
        // The one hazard is another process changing or truncating the file while
        // it is mapped. The documentation above puts that on the caller.
        let map = unsafe { Mmap::map(&file)? };
        format::check_header(&map)?;
        let entries = format::read_index(&map)?;
        Ok(Self { map, entries })
    }

    /// Every asset, sorted by name.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Returns the asset with `name`, or `None` if there is none.
    ///
    /// Lookup is a binary search over the sorted index, so it takes O(log n)
    /// comparisons. A stored asset is returned without a checksum pass, so
    /// reads stay lazy. Use [`Archive::verify`] to check every asset.
    pub fn get(&self, name: &str) -> Result<Option<Asset<'_>>> {
        let Ok(index) = self.entries.binary_search_by(|e| e.name.as_str().cmp(name)) else {
            return Ok(None);
        };
        let entry = &self.entries[index];
        let stored = self.payload(entry);
        match entry.compression {
            Compression::Stored => Ok(Some(Asset::Borrowed(stored))),
            Compression::Zstd => Ok(Some(Asset::Owned(decode(entry, stored)?))),
        }
    }

    /// Checks every asset's checksum, and decompresses every compressed asset
    /// to check its length.
    pub fn verify(&self) -> Result<()> {
        for entry in &self.entries {
            let stored = self.payload(entry);
            if crc32fast::hash(stored) != entry.crc32 {
                return Err(Error::ChecksumMismatch(entry.name.clone()));
            }
            if entry.compression == Compression::Zstd {
                decode(entry, stored)?;
            }
        }
        Ok(())
    }

    /// Returns an asset's stored bytes. Opening checked that they lie inside the data area.
    fn payload(&self, entry: &Entry) -> &[u8] {
        let start = entry.offset as usize;
        &self.map[start..start + entry.stored_len as usize]
    }
}

/// Decompresses a zstd asset and checks its length.
fn decode(entry: &Entry, stored: &[u8]) -> Result<Vec<u8>> {
    let raw = codec::decompress(stored, entry.raw_len as usize)?;
    if raw.len() as u64 != entry.raw_len {
        return Err(Error::Corrupt("asset length does not match its index"));
    }
    Ok(raw)
}
