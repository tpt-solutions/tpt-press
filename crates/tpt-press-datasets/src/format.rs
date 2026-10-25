//! On-disk layout of a `.tpds` dataset archive, version 1.
//!
//! ```text
//! header: magic "TPDS" | version u16 | flags u16 (reserved, must be 0)
//! members: each member's stored bytes, back to back
//! index:   one zstd frame holding the index body, which is
//!            sample_count u32, then per sample:
//!            key u16 len + bytes | member_count u16
//!            then per member: name u16 len + bytes | offset u64 | stored_len u32
//!                             | raw_len u32 | crc32 u32 | codec u8
//! footer:  index_offset u64 | index_len u32 (stored) | index_raw_len u32
//!          | index_crc32 u32 (of the stored index) | magic "TPDX"
//! ```
//!
//! The index is compressed because names and offsets repeat a lot, and for
//! datasets of small members the index would otherwise be a large share of the file.
//!
//! A sample is one training example: a key and its named members, such as
//! `image.jpg` and `label.cls`. Samples keep the order they were written in, so
//! sample `i` is found from the index without reading the others. A member is
//! stored as is (codec 0) or as one zstd frame (codec 1). The writer uses zstd only
//! for members that are not already compressed media, and only when it is smaller.

use tpt_press_core::codec;

use crate::error::{Error, Result};

/// Magic bytes at the start of every dataset archive.
pub const MAGIC: [u8; 4] = *b"TPDS";

/// Magic bytes that end every dataset archive.
pub const FOOTER_MAGIC: [u8; 4] = *b"TPDX";

/// Version written by this build and the only one it reads.
pub const VERSION: u16 = 1;

/// Size of the header in bytes.
pub const HEADER_LEN: u64 = 8;

/// Size of the footer in bytes.
pub const FOOTER_LEN: u64 = 24;

/// Largest index accepted when reading, stored or decoded.
pub const MAX_INDEX_LEN: u64 = 256 * 1024 * 1024;

/// zstd level for the index. The index is small, so a high level costs little.
const INDEX_LEVEL: i32 = 19;

/// Largest member, in bytes, accepted when writing or reading.
pub const MAX_MEMBER_BYTES: u64 = 1 << 30;

/// Extensions of media that is already compressed. These members are stored as
/// they are, without trying zstd.
const COMPRESSED_MEDIA: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "avif", "heic", "mp3", "mp4", "m4a", "webm", "ogg",
    "opus", "flac", "aac", "wav", "zip", "gz", "tgz", "zst", "bz2", "xz", "7z", "parquet",
];

/// How a member's bytes are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// The bytes as they are.
    Stored,
    /// One zstd frame.
    Zstd,
}

impl Codec {
    fn code(self) -> u8 {
        match self {
            Codec::Stored => 0,
            Codec::Zstd => 1,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Codec::Stored),
            1 => Some(Codec::Zstd),
            _ => None,
        }
    }
}

/// Whether a member name is media that is already compressed.
pub(crate) fn is_compressed_media(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or(name);
    COMPRESSED_MEDIA.iter().any(|m| ext.eq_ignore_ascii_case(m))
}

/// One member's entry in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberEntry {
    /// Member name, such as `jpg` or `cls`.
    pub name: String,
    /// Byte offset of the stored bytes in the file.
    pub offset: u64,
    /// Length of the stored bytes.
    pub stored_len: u32,
    /// Length of the member's bytes once decoded.
    pub raw_len: u32,
    /// CRC32 of the stored bytes.
    pub crc32: u32,
    /// How the stored bytes are encoded.
    pub codec: Codec,
}

/// One sample's entry in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleEntry {
    /// The sample's key.
    pub key: String,
    /// The sample's members, in the order they were written.
    pub members: Vec<MemberEntry>,
}

/// Writes the header.
pub fn header_bytes() -> [u8; HEADER_LEN as usize] {
    let mut h = [0u8; HEADER_LEN as usize];
    h[..4].copy_from_slice(&MAGIC);
    h[4..6].copy_from_slice(&VERSION.to_le_bytes());
    h
}

/// Checks the header bytes.
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

/// The footer's fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Footer {
    /// Where the stored index starts in the file.
    pub index_offset: u64,
    /// Length of the stored index.
    pub index_len: u32,
    /// Length of the index once decompressed.
    pub index_raw_len: u32,
    /// CRC32 of the stored index.
    pub index_crc32: u32,
}

/// Encodes the index and the footer. The returned bytes are the compressed index
/// followed by the footer. `index_offset` is where the compressed index starts in
/// the file.
pub fn encode_index_and_footer(samples: &[SampleEntry], index_offset: u64) -> Result<Vec<u8>> {
    let body = encode_index_body(samples)?;
    let stored = codec::compress(&body, INDEX_LEVEL)?;
    let index_len = u32::try_from(stored.len())
        .map_err(|_| Error::InvalidInput("index is too large".to_owned()))?;
    let index_raw_len = u32::try_from(body.len())
        .map_err(|_| Error::InvalidInput("index is too large".to_owned()))?;

    let mut out = Vec::with_capacity(stored.len() + FOOTER_LEN as usize);
    out.extend_from_slice(&stored);
    out.extend_from_slice(&index_offset.to_le_bytes());
    out.extend_from_slice(&index_len.to_le_bytes());
    out.extend_from_slice(&index_raw_len.to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(&stored).to_le_bytes());
    out.extend_from_slice(&FOOTER_MAGIC);
    Ok(out)
}

/// Parses the footer bytes.
pub fn parse_footer(bytes: &[u8]) -> Result<Footer> {
    if bytes.len() != FOOTER_LEN as usize {
        return Err(Error::Truncated);
    }
    if bytes[20..24] != FOOTER_MAGIC {
        return Err(Error::Corrupt("missing footer"));
    }
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    Ok(Footer {
        index_offset: u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
        index_len: u32_at(8),
        index_raw_len: u32_at(12),
        index_crc32: u32_at(16),
    })
}

/// Checks that the footer's index lies between the header and the footer, so
/// that the file length is consistent with it. Also checks the index size limit.
pub(crate) fn check_footer_location(footer: &Footer, file_len: u64) -> Result<()> {
    if u64::from(footer.index_len) > MAX_INDEX_LEN {
        return Err(Error::Corrupt("index is larger than the limit"));
    }
    if footer.index_offset < HEADER_LEN
        || footer.index_offset + u64::from(footer.index_len) + FOOTER_LEN != file_len
    {
        return Err(Error::Corrupt("footer does not match the file size"));
    }
    Ok(())
}

/// Checks a member's stored bytes against its CRC32, then decodes them to the
/// member's raw bytes. `key` names the sample in the error.
pub(crate) fn decode_member(key: &str, m: &MemberEntry, stored: Vec<u8>) -> Result<Vec<u8>> {
    if crc32fast::hash(&stored) != m.crc32 {
        return Err(Error::ChecksumMismatch {
            sample: key.to_owned(),
            member: m.name.clone(),
        });
    }
    let raw = match m.codec {
        Codec::Stored => stored,
        Codec::Zstd => codec::decompress(&stored, m.raw_len as usize)?,
    };
    if raw.len() != m.raw_len as usize {
        return Err(Error::Corrupt("member length does not match its index"));
    }
    Ok(raw)
}

/// Checks the stored index against its footer, decompresses it, and decodes it.
pub fn read_index(stored: &[u8], footer: &Footer) -> Result<Vec<SampleEntry>> {
    if crc32fast::hash(stored) != footer.index_crc32 {
        return Err(Error::Corrupt("index checksum mismatch"));
    }
    if u64::from(footer.index_raw_len) > MAX_INDEX_LEN {
        return Err(Error::Corrupt("index is larger than the limit"));
    }
    let body = codec::decompress(stored, footer.index_raw_len as usize)?;
    if body.len() != footer.index_raw_len as usize {
        return Err(Error::Corrupt("index length does not match its footer"));
    }
    decode_index(&body, footer.index_offset)
}

fn encode_index_body(samples: &[SampleEntry]) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let count = u32::try_from(samples.len())
        .map_err(|_| Error::InvalidInput("too many samples".to_owned()))?;
    body.extend_from_slice(&count.to_le_bytes());
    for sample in samples {
        push_str(&mut body, &sample.key)?;
        let members = u16::try_from(sample.members.len())
            .map_err(|_| Error::InvalidInput("too many members in one sample".to_owned()))?;
        body.extend_from_slice(&members.to_le_bytes());
        for m in &sample.members {
            push_str(&mut body, &m.name)?;
            body.extend_from_slice(&m.offset.to_le_bytes());
            body.extend_from_slice(&m.stored_len.to_le_bytes());
            body.extend_from_slice(&m.raw_len.to_le_bytes());
            body.extend_from_slice(&m.crc32.to_le_bytes());
            body.push(m.codec.code());
        }
    }

    Ok(body)
}

/// Decodes the index body. Member offsets and lengths are checked against
/// `index_offset`, which is where the member data ends.
fn decode_index(bytes: &[u8], index_offset: u64) -> Result<Vec<SampleEntry>> {
    let mut cur = Cursor { buf: bytes, pos: 0 };
    let count = cur.u32()? as usize;
    let mut samples = Vec::new();
    for _ in 0..count {
        let key = cur.string()?;
        let member_count = cur.u16()? as usize;
        let mut members = Vec::new();
        for _ in 0..member_count {
            let name = cur.string()?;
            let offset = cur.u64()?;
            let stored_len = cur.u32()?;
            let raw_len = cur.u32()?;
            let crc32 = cur.u32()?;
            let codec =
                Codec::from_code(cur.u8()?).ok_or(Error::Corrupt("unknown member codec"))?;
            if offset < HEADER_LEN
                || offset
                    .checked_add(u64::from(stored_len))
                    .is_none_or(|end| end > index_offset)
            {
                return Err(Error::Corrupt("member lies outside the data section"));
            }
            if u64::from(raw_len) > MAX_MEMBER_BYTES || stored_len > raw_len {
                return Err(Error::Corrupt("member sizes are inconsistent"));
            }
            if codec == Codec::Stored && stored_len != raw_len {
                return Err(Error::Corrupt(
                    "stored member length does not match its raw length",
                ));
            }
            members.push(MemberEntry {
                name,
                offset,
                stored_len,
                raw_len,
                crc32,
                codec,
            });
        }
        samples.push(SampleEntry { key, members });
    }
    if cur.pos != bytes.len() {
        return Err(Error::Corrupt("index has trailing bytes"));
    }
    Ok(samples)
}

fn push_str(out: &mut Vec<u8>, text: &str) -> Result<()> {
    let len = u16::try_from(text.len())
        .map_err(|_| Error::InvalidInput(format!("name is longer than 65535 bytes: {text:.40}")))?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(text.as_bytes());
    Ok(())
}

/// Bounds-checked reader over the index bytes.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn take(&mut self, len: usize) -> Result<&[u8]> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|&end| end <= self.buf.len())
            .ok_or(Error::Corrupt("index is truncated"))?;
        let bytes = &self.buf[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
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

    fn string(&mut self) -> Result<String> {
        let len = self.u16()? as usize;
        String::from_utf8(self.take(len)?.to_vec())
            .map_err(|_| Error::Corrupt("index name is not UTF-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_extensions_are_recognized() {
        assert!(is_compressed_media("jpg"));
        assert!(is_compressed_media("JPEG"));
        assert!(!is_compressed_media("cls"));
        assert!(!is_compressed_media("json"));
    }

    #[test]
    fn index_round_trips() {
        let samples = vec![SampleEntry {
            key: "a/0001".to_owned(),
            members: vec![MemberEntry {
                name: "txt".to_owned(),
                offset: HEADER_LEN,
                stored_len: 5,
                raw_len: 5,
                crc32: 7,
                codec: Codec::Stored,
            }],
        }];
        let bytes = encode_index_and_footer(&samples, HEADER_LEN + 5).unwrap();
        let stored_len = bytes.len() - FOOTER_LEN as usize;
        let footer = parse_footer(&bytes[stored_len..]).unwrap();
        assert_eq!(footer.index_offset, HEADER_LEN + 5);
        assert_eq!(footer.index_len as usize, stored_len);
        assert_eq!(read_index(&bytes[..stored_len], &footer).unwrap(), samples);
    }

    #[test]
    fn damaged_index_fails_its_checksum() {
        let samples = vec![SampleEntry {
            key: "k".to_owned(),
            members: vec![],
        }];
        let bytes = encode_index_and_footer(&samples, HEADER_LEN).unwrap();
        let stored_len = bytes.len() - FOOTER_LEN as usize;
        let footer = parse_footer(&bytes[stored_len..]).unwrap();
        let mut stored = bytes[..stored_len].to_vec();
        stored[0] ^= 1;
        assert!(read_index(&stored, &footer).is_err());
    }

    #[test]
    fn member_outside_data_section_is_rejected() {
        let samples = vec![SampleEntry {
            key: "k".to_owned(),
            members: vec![MemberEntry {
                name: "x".to_owned(),
                offset: HEADER_LEN,
                stored_len: 100,
                raw_len: 100,
                crc32: 0,
                codec: Codec::Stored,
            }],
        }];
        // Checked at decode time against the end of the data section.
        let body = encode_index_body(&samples).unwrap();
        assert!(decode_index(&body, HEADER_LEN + 5).is_err());
    }
}
