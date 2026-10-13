//! On-disk layout of the generic tpt-press archive, format version 1.
//!
//! ```text
//! header:  magic "TPPR" (4) | version u16 LE | flags u16 LE
//!          if flags bit 0 (dictionary) is set: dict_len u32 LE | dictionary
//! frame*:  compressed_len u32 LE | uncompressed_len u32 LE | crc32 u32 LE | payload
//! end:     a frame whose compressed_len and uncompressed_len are both 0
//! ```
//!
//! The CRC32 covers the compressed payload. Frames appear in input order.
//! A missing end frame means the archive was truncated.

use std::io::{Read, Write};

use crate::error::{Error, Result};

/// Magic bytes at the start of every archive.
pub const MAGIC: [u8; 4] = *b"TPPR";

/// Version written by this build and the only one it reads.
pub const VERSION: u16 = 1;

/// Size of the archive header in bytes.
pub const HEADER_LEN: usize = 8;

/// Size of each frame header in bytes.
pub const FRAME_HEADER_LEN: usize = 12;

/// Flag: a zstd dictionary follows the header, and every chunk is compressed with it.
pub const FLAG_DICTIONARY: u16 = 1;

/// Largest dictionary the format accepts. zstd dictionaries are typically under 128 KiB.
pub const MAX_DICTIONARY_LEN: usize = 1024 * 1024;

/// Writes the archive header, followed by `dictionary` if there is one.
pub(crate) fn write_header<W: Write>(w: &mut W, dictionary: Option<&[u8]>) -> Result<()> {
    let mut buf = [0u8; HEADER_LEN];
    buf[..4].copy_from_slice(&MAGIC);
    buf[4..6].copy_from_slice(&VERSION.to_le_bytes());
    if dictionary.is_some() {
        buf[6..8].copy_from_slice(&FLAG_DICTIONARY.to_le_bytes());
    }
    w.write_all(&buf)?;
    if let Some(dictionary) = dictionary {
        w.write_all(&(dictionary.len() as u32).to_le_bytes())?;
        w.write_all(dictionary)?;
    }
    Ok(())
}

/// Reads and validates the archive header. Returns the dictionary if the
/// header carries one.
pub(crate) fn read_header<R: Read>(r: &mut R) -> Result<Option<Vec<u8>>> {
    let mut buf = [0u8; HEADER_LEN];
    r.read_exact(&mut buf)?;
    if buf[..4] != MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u16::from_le_bytes([buf[4], buf[5]]);
    if version != VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    let flags = u16::from_le_bytes([buf[6], buf[7]]);
    if flags & !FLAG_DICTIONARY != 0 {
        return Err(Error::UnsupportedFlags(flags));
    }
    if flags & FLAG_DICTIONARY == 0 {
        return Ok(None);
    }
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > MAX_DICTIONARY_LEN {
        return Err(Error::CorruptHeader("dictionary size is out of range"));
    }
    let mut dictionary = vec![0u8; len];
    r.read_exact(&mut dictionary)?;
    Ok(Some(dictionary))
}

/// Metadata that precedes each compressed chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameHeader {
    pub(crate) compressed_len: u32,
    pub(crate) uncompressed_len: u32,
    pub(crate) crc32: u32,
}

impl FrameHeader {
    /// The end-of-archive marker.
    pub(crate) const END: Self = Self {
        compressed_len: 0,
        uncompressed_len: 0,
        crc32: 0,
    };

    pub(crate) fn write<W: Write>(&self, w: &mut W) -> Result<()> {
        let mut buf = [0u8; FRAME_HEADER_LEN];
        buf[0..4].copy_from_slice(&self.compressed_len.to_le_bytes());
        buf[4..8].copy_from_slice(&self.uncompressed_len.to_le_bytes());
        buf[8..12].copy_from_slice(&self.crc32.to_le_bytes());
        w.write_all(&buf)?;
        Ok(())
    }

    pub(crate) fn read<R: Read>(r: &mut R) -> Result<Self> {
        let mut buf = [0u8; FRAME_HEADER_LEN];
        r.read_exact(&mut buf)?;
        Ok(Self {
            compressed_len: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            uncompressed_len: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            crc32: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let mut buf = Vec::new();
        write_header(&mut buf, None).unwrap();
        assert_eq!(buf.len(), HEADER_LEN);
        read_header(&mut buf.as_slice()).unwrap();
    }

    #[test]
    fn frame_round_trip() {
        let frame = FrameHeader {
            compressed_len: 123,
            uncompressed_len: 456,
            crc32: 0xDEAD_BEEF,
        };
        let mut buf = Vec::new();
        frame.write(&mut buf).unwrap();
        assert_eq!(buf.len(), FRAME_HEADER_LEN);
        assert_eq!(FrameHeader::read(&mut buf.as_slice()).unwrap(), frame);
    }
}
