//! Packs named assets into a `.tpa` archive.

use std::collections::HashSet;
use std::io::Write;

use tpt_press_core::codec;

use crate::error::{Error, Result};
use crate::format::{self, Compression, Entry, HEADER_LEN, PAGE_SIZE};

/// Options for [`pack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackOptions {
    /// zstd compression level, in `1..=22`.
    pub level: i32,
    /// Store every asset raw, so readers can use it without copying. Otherwise
    /// each asset is compressed, and stored raw only when compression does not shrink it.
    pub store_all: bool,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            level: codec::DEFAULT_LEVEL,
            store_all: false,
        }
    }
}

/// Counts from a completed [`pack`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Assets written.
    pub assets: u64,
    /// Assets stored raw.
    pub stored: u64,
    /// Assets stored compressed.
    pub compressed: u64,
    /// Total contents size before compression, in bytes.
    pub input_bytes: u64,
    /// Total archive size, including padding and index, in bytes.
    pub archive_bytes: u64,
}

/// Packs assets into an archive written to `output`.
///
/// `assets` yields `(name, contents)` pairs, and names must be unique safe
/// relative paths. Each asset is read and written in turn, so memory use is
/// one asset at a time.
pub fn pack<I, W>(assets: I, mut output: W, opts: &PackOptions) -> Result<Stats>
where
    I: IntoIterator<Item = Result<(String, Vec<u8>)>>,
    W: Write,
{
    codec::validate_level(opts.level)?;

    let header = format::header_bytes();
    output.write_all(&header)?;
    let mut offset = HEADER_LEN;
    let mut stats = Stats::default();
    let mut entries: Vec<Entry> = Vec::new();
    let mut names = HashSet::new();

    for asset in assets {
        let (name, contents) = asset?;
        format::validate_name(&name)?;
        if !names.insert(name.clone()) {
            return Err(Error::DuplicateName(name));
        }

        let raw_len = contents.len() as u64;
        let (payload, compression) = if opts.store_all {
            (contents, Compression::Stored)
        } else {
            let packed = codec::compress(&contents, opts.level)?;
            if packed.len() < contents.len() {
                (packed, Compression::Zstd)
            } else {
                (contents, Compression::Stored)
            }
        };

        // Pad so the payload starts on a page boundary.
        let padding = (PAGE_SIZE - offset % PAGE_SIZE) % PAGE_SIZE;
        output.write_all(&vec![0u8; padding as usize])?;
        offset += padding;

        output.write_all(&payload)?;
        let stored_len = payload.len() as u64;
        entries.push(Entry {
            name,
            offset,
            stored_len,
            raw_len,
            compression,
            crc32: crc32fast::hash(&payload),
        });
        offset += stored_len;

        stats.assets += 1;
        stats.input_bytes += raw_len;
        match compression {
            Compression::Stored => stats.stored += 1,
            Compression::Zstd => stats.compressed += 1,
        }
    }

    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let footer = format::encode_index_and_footer(&entries, offset)?;
    output.write_all(&footer)?;
    output.flush()?;

    stats.archive_bytes = offset + footer.len() as u64;
    Ok(stats)
}
