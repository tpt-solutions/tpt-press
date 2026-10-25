//! On-disk layout of a `.tptn` tensor archive, version 1.
//!
//! ```text
//! header: magic "TPTN" | version u16 | flags u16 (reserved, must be 0)
//! pieces: each piece's payload, back to back
//! index:  metadata   (u32 count, then key: u16 len + bytes, value: u32 len + bytes)
//!         tensors    (u32 count, then per tensor: name u16 len + bytes,
//!                     dtype u8 len + bytes, rank u8, dims u64 * rank,
//!                     byte_len u64, first_piece u32, piece_count u32)
//!         pieces     (u32 count, then per piece: offset u64, stored_len u32,
//!                     raw_len u32, crc32 u32, compression u8)
//! footer: index_offset u64 | index_len u32 | index_crc32 u32 | magic "TPTX"
//! ```
//!
//! A tensor is split into pieces only at its own element boundaries, so no
//! piece holds bytes from two tensors. Pieces of one tensor are contiguous in
//! the piece table, in order. A piece is a zstd frame when `compression` is 1,
//! a shuffled zstd frame when it is 2, a split bf16 payload when it is 3 (see
//! [`Compression::Split`]), and raw bytes when it is 0.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

use crate::dtype;
use crate::error::{map_io, Error, Result};

/// Magic bytes at the start of every tensor archive.
pub const MAGIC: [u8; 4] = *b"TPTN";

/// Magic bytes that end every tensor archive.
pub const FOOTER_MAGIC: [u8; 4] = *b"TPTX";

/// Version written by this build and the only one it reads.
pub const VERSION: u16 = 1;

/// Size of the header in bytes.
pub const HEADER_LEN: u64 = 8;

/// Size of the footer in bytes.
pub const FOOTER_LEN: u64 = 20;

/// Largest piece, in uncompressed bytes. A multiple of every element size.
pub const MAX_PIECE_BYTES: u64 = 16 * 1024 * 1024;

/// Largest index accepted when reading.
pub const MAX_INDEX_LEN: u64 = 512 * 1024 * 1024;

/// Largest number of dimensions accepted.
const MAX_RANK: u8 = 32;

/// How a piece's payload is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Raw bytes.
    Stored,
    /// One zstd frame.
    Zstd,
    /// One zstd frame of the piece with its bytes regrouped by position within
    /// each element: all first bytes, then all second bytes, and so on. For
    /// floats the exponent bytes then sit together, and compress much better.
    /// Used only for element widths of 2, 4 and 8 bytes.
    Shuffled,
    /// A bf16 piece split into an exponent plane and a sign-and-mantissa plane.
    /// The payload is `exponent_frame_len u32`, then a zstd frame of the
    /// exponent bytes, then a zstd frame of the other bytes. Used only for
    /// `BF16` tensors. See `split.rs`.
    Split,
}

impl Compression {
    fn code(self) -> u8 {
        match self {
            Compression::Stored => 0,
            Compression::Zstd => 1,
            Compression::Shuffled => 2,
            Compression::Split => 3,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Compression::Stored),
            1 => Some(Compression::Zstd),
            2 => Some(Compression::Shuffled),
            3 => Some(Compression::Split),
            _ => None,
        }
    }
}

/// One stored piece of a tensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    /// Byte offset of the payload in the file.
    pub offset: u64,
    /// Length of the payload as stored.
    pub stored_len: u32,
    /// Length of the piece's bytes once decoded.
    pub raw_len: u32,
    /// CRC32 of the payload as stored.
    pub crc32: u32,
    /// How the payload is stored.
    pub compression: Compression,
}

/// One tensor's entry in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorEntry {
    /// Tensor name.
    pub name: String,
    /// Element type.
    pub dtype: String,
    /// Dimensions.
    pub shape: Vec<u64>,
    /// Total bytes of the tensor, the sum of its pieces' raw lengths.
    pub byte_len: u64,
    /// Index of the tensor's first piece in the piece table.
    pub first_piece: u32,
    /// Number of pieces the tensor is split into.
    pub piece_count: u32,
}

/// Everything in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    /// String metadata carried over from the source file.
    pub metadata: BTreeMap<String, String>,
    /// Tensors, in archive order.
    pub tensors: Vec<TensorEntry>,
    /// Pieces, in archive order.
    pub pieces: Vec<Piece>,
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

/// Encodes the index and the footer that points to it.
pub fn encode_index_and_footer(index: &Index, index_offset: u64) -> Result<Vec<u8>> {
    let mut body = Vec::new();

    body.extend_from_slice(&count_u32(index.metadata.len())?.to_le_bytes());
    for (key, value) in &index.metadata {
        put_str16(&mut body, key)?;
        put_str32(&mut body, value)?;
    }

    body.extend_from_slice(&count_u32(index.tensors.len())?.to_le_bytes());
    for t in &index.tensors {
        put_str16(&mut body, &t.name)?;
        let dtype = t.dtype.as_bytes();
        let dtype_len = u8::try_from(dtype.len())
            .map_err(|_| Error::InvalidOption("dtype name is too long"))?;
        body.push(dtype_len);
        body.extend_from_slice(dtype);
        let rank =
            u8::try_from(t.shape.len()).map_err(|_| Error::InvalidOption("too many dimensions"))?;
        body.push(rank);
        for d in &t.shape {
            body.extend_from_slice(&d.to_le_bytes());
        }
        body.extend_from_slice(&t.byte_len.to_le_bytes());
        body.extend_from_slice(&t.first_piece.to_le_bytes());
        body.extend_from_slice(&t.piece_count.to_le_bytes());
    }

    body.extend_from_slice(&count_u32(index.pieces.len())?.to_le_bytes());
    for p in &index.pieces {
        body.extend_from_slice(&p.offset.to_le_bytes());
        body.extend_from_slice(&p.stored_len.to_le_bytes());
        body.extend_from_slice(&p.raw_len.to_le_bytes());
        body.extend_from_slice(&p.crc32.to_le_bytes());
        body.push(p.compression.code());
    }

    let index_len = u32::try_from(body.len()).map_err(|_| Error::Corrupt("index too large"))?;
    let index_crc = crc32fast::hash(&body);
    body.extend_from_slice(&index_offset.to_le_bytes());
    body.extend_from_slice(&index_len.to_le_bytes());
    body.extend_from_slice(&index_crc.to_le_bytes());
    body.extend_from_slice(&FOOTER_MAGIC);
    Ok(body)
}

/// Reads the footer and index of a seekable file, and checks everything the
/// reader relies on. Only the footer and index are read, not the tensor data.
pub fn read_index_from<R: Read + Seek>(r: &mut R) -> Result<Index> {
    let file_len = r.seek(SeekFrom::End(0)).map_err(map_io)?;
    if file_len < HEADER_LEN + FOOTER_LEN {
        return Err(Error::Truncated);
    }
    r.seek(SeekFrom::End(-(FOOTER_LEN as i64)))
        .map_err(map_io)?;
    let mut footer = [0u8; FOOTER_LEN as usize];
    r.read_exact(&mut footer).map_err(map_io)?;
    let (index_offset, index_len, index_crc) = footer_location(&footer, file_len)?;

    r.seek(SeekFrom::Start(index_offset)).map_err(map_io)?;
    let mut body = vec![0u8; index_len as usize];
    r.read_exact(&mut body).map_err(map_io)?;
    finish_index(&body, index_crc, index_offset)
}

/// Reads the index's location and checksum from the footer. `file_len` is the
/// whole file's length, so the footer must end exactly at its end.
pub fn footer_location(
    footer: &[u8; FOOTER_LEN as usize],
    file_len: u64,
) -> Result<(u64, u32, u32)> {
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
    Ok((index_offset, index_len, index_crc))
}

/// Checks an index's bytes against their checksum, then decodes and checks the index itself.
pub fn finish_index(body: &[u8], index_crc: u32, index_offset: u64) -> Result<Index> {
    if crc32fast::hash(body) != index_crc {
        return Err(Error::Corrupt("index checksum mismatch"));
    }
    let index = decode_index(body)?;
    check_index(&index, index_offset)?;
    Ok(index)
}

/// Checks that the index is consistent with itself and with the data area.
fn check_index(index: &Index, index_offset: u64) -> Result<()> {
    let mut next_piece = 0usize;
    for t in &index.tensors {
        if t.first_piece as usize != next_piece {
            return Err(Error::Corrupt("tensor pieces are not contiguous"));
        }
        let end = next_piece + t.piece_count as usize;
        let pieces = index
            .pieces
            .get(next_piece..end)
            .ok_or(Error::Corrupt("tensor refers to pieces that do not exist"))?;
        next_piece = end;

        let mut total = 0u64;
        for p in pieces {
            if u64::from(p.raw_len) > MAX_PIECE_BYTES {
                return Err(Error::Corrupt("piece is larger than the limit"));
            }
            let end = p.offset.checked_add(u64::from(p.stored_len));
            if p.offset < HEADER_LEN || end.is_none_or(|end| end > index_offset) {
                return Err(Error::Corrupt("piece lies outside the data area"));
            }
            total = total
                .checked_add(u64::from(p.raw_len))
                .ok_or(Error::Corrupt("tensor is too large"))?;
        }
        if total != t.byte_len {
            return Err(Error::Corrupt("tensor pieces do not add up to its size"));
        }

        let expected = dtype::byte_len(&t.dtype, &t.shape)
            .map_err(|_| Error::Corrupt("tensor has an unknown dtype or an invalid shape"))?;
        if expected != t.byte_len {
            return Err(Error::Corrupt(
                "tensor size does not match its shape and dtype",
            ));
        }
    }
    if index.tensors.windows(2).any(|w| w[0].name >= w[1].name) {
        return Err(Error::Corrupt("tensor names are not sorted, or repeat"));
    }
    if next_piece != index.pieces.len() {
        return Err(Error::Corrupt("pieces are not all owned by a tensor"));
    }
    Ok(())
}

fn decode_index(bytes: &[u8]) -> Result<Index> {
    let mut cur = Cursor { buf: bytes, pos: 0 };

    let meta_count = cur.u32()? as usize;
    let mut metadata = BTreeMap::new();
    for _ in 0..meta_count {
        let key = cur.str16()?;
        let value = cur.str32()?;
        metadata.insert(key, value);
    }

    let tensor_count = cur.u32()? as usize;
    let mut tensors = Vec::with_capacity(tensor_count.min(bytes.len() / 16));
    for _ in 0..tensor_count {
        let name = cur.str16()?;
        let dtype_len = cur.u8()? as usize;
        let dtype = String::from_utf8(cur.take(dtype_len)?.to_vec())
            .map_err(|_| Error::Corrupt("dtype is not UTF-8"))?;
        let rank = cur.u8()?;
        if rank > MAX_RANK {
            return Err(Error::Corrupt("too many dimensions"));
        }
        let mut shape = Vec::with_capacity(rank as usize);
        for _ in 0..rank {
            shape.push(cur.u64()?);
        }
        let byte_len = cur.u64()?;
        let first_piece = cur.u32()?;
        let piece_count = cur.u32()?;
        tensors.push(TensorEntry {
            name,
            dtype,
            shape,
            byte_len,
            first_piece,
            piece_count,
        });
    }

    let piece_count = cur.u32()? as usize;
    let mut pieces = Vec::with_capacity(piece_count.min(bytes.len() / 8));
    for _ in 0..piece_count {
        let offset = cur.u64()?;
        let stored_len = cur.u32()?;
        let raw_len = cur.u32()?;
        let crc32 = cur.u32()?;
        let compression =
            Compression::from_code(cur.u8()?).ok_or(Error::Corrupt("unknown piece compression"))?;
        pieces.push(Piece {
            offset,
            stored_len,
            raw_len,
            crc32,
            compression,
        });
    }

    if cur.pos != bytes.len() {
        return Err(Error::Corrupt("index has trailing bytes"));
    }
    Ok(Index {
        metadata,
        tensors,
        pieces,
    })
}

fn count_u32(n: usize) -> Result<u32> {
    u32::try_from(n).map_err(|_| Error::Corrupt("too many entries"))
}

fn put_str16(out: &mut Vec<u8>, s: &str) -> Result<()> {
    let len = u16::try_from(s.len()).map_err(|_| Error::InvalidOption("name is too long"))?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

fn put_str32(out: &mut Vec<u8>, s: &str) -> Result<()> {
    let len = u32::try_from(s.len()).map_err(|_| Error::InvalidOption("value is too long"))?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    Ok(())
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

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes(b.try_into().unwrap()))
    }

    fn str16(&mut self) -> Result<String> {
        let len = u16::from_le_bytes(self.take(2)?.try_into().unwrap()) as usize;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| Error::Corrupt("text is not UTF-8"))
    }

    fn str32(&mut self) -> Result<String> {
        let len = self.u32()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| Error::Corrupt("text is not UTF-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_index() -> Index {
        let mut metadata = BTreeMap::new();
        metadata.insert("format".to_owned(), "pt".to_owned());
        Index {
            metadata,
            tensors: vec![TensorEntry {
                name: "w".to_owned(),
                dtype: "F32".to_owned(),
                shape: vec![2, 2],
                byte_len: 16,
                first_piece: 0,
                piece_count: 1,
            }],
            pieces: vec![Piece {
                offset: 8,
                stored_len: 16,
                raw_len: 16,
                crc32: 0,
                compression: Compression::Stored,
            }],
        }
    }

    #[test]
    fn index_round_trips() {
        let index = sample_index();
        let footer = encode_index_and_footer(&index, 24).unwrap();
        let body_len = footer.len() - FOOTER_LEN as usize;
        let decoded = decode_index(&footer[..body_len]).unwrap();
        assert_eq!(decoded, index);
    }
}
