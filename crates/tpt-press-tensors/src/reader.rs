//! Reads tensors from a `.tptn` archive, one piece at a time.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

use tpt_press_core::codec;

use crate::dtype;
use crate::error::{map_io, Error, Result};
use crate::format::{self, Compression, Index, Piece, TensorEntry, HEADER_LEN, MAX_PIECE_BYTES};
use crate::{shuffle, split};

/// A reader that can be used behind a trait object, for base archives.
trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// Metadata key that holds the identity of the base a delta archive was packed against.
pub(crate) const BASE_ID_KEY: &str = "base_id";

impl<R: Read + Seek + 'static> Archive<R> {
    /// Turns this archive into one that reads through a trait object, so that a
    /// delta can name it as its base whatever type the input is.
    fn into_boxed(self) -> Archive<Box<dyn ReadSeek>> {
        Archive {
            input: Box::new(self.input) as Box<dyn ReadSeek>,
            index: self.index,
            base: self.base,
        }
    }
}

/// An open tensor archive. Opening reads only the header, footer and index.
pub struct Archive<R> {
    input: R,
    index: Index,
    /// The base this archive is a delta against, if it was packed with one.
    base: Option<Box<Archive<Box<dyn ReadSeek>>>>,
}

impl<R: Read + Seek> Archive<R> {
    /// Opens and checks an archive.
    ///
    /// An archive packed as a delta needs its base: use [`Archive::open_with_base`].
    /// Opening it here fails when a piece is read.
    pub fn open(mut input: R) -> Result<Self> {
        input.seek(SeekFrom::Start(0)).map_err(map_io)?;
        let mut header = [0u8; HEADER_LEN as usize];
        input.read_exact(&mut header).map_err(map_io)?;
        format::check_header(&header)?;
        let index = format::read_index_from(&mut input)?;
        Ok(Self {
            input,
            index,
            base: None,
        })
    }

    /// Opens an archive that was packed as a delta against `base`. The base's
    /// identity must match the one recorded in the archive.
    pub fn open_with_base<B>(input: R, base: Archive<B>) -> Result<Self>
    where
        B: Read + Seek + 'static,
    {
        let mut archive = Self::open(input)?;
        let expected = archive
            .metadata()
            .get(BASE_ID_KEY)
            .cloned()
            .ok_or(Error::InvalidOption(
                "this archive was not packed against a base",
            ))?;
        let got = base.identity();
        if got != expected {
            return Err(Error::BaseMismatch { expected, got });
        }
        archive.base = Some(Box::new(base.into_boxed()));
        Ok(archive)
    }

    /// A short identity for this archive's tensors and pieces. Two archives with
    /// the same tensors, layout and stored bytes have the same identity. It is
    /// what a delta records to name its base.
    pub fn identity(&self) -> String {
        let mut hasher = crc32fast::Hasher::new();
        for tensor in &self.index.tensors {
            hasher.update(tensor.name.as_bytes());
            hasher.update(&tensor.byte_len.to_le_bytes());
        }
        for piece in &self.index.pieces {
            hasher.update(&piece.crc32.to_le_bytes());
            hasher.update(&piece.raw_len.to_le_bytes());
        }
        format!("{:08x}-{:x}", hasher.finalize(), self.index.pieces.len())
    }

    /// The metadata carried over from the source file.
    pub fn metadata(&self) -> &BTreeMap<String, String> {
        &self.index.metadata
    }

    /// Every tensor, sorted by name. Their metadata is available without reading any data.
    pub fn tensors(&self) -> &[TensorEntry] {
        &self.index.tensors
    }

    /// The tensor named `name`, if the archive has one.
    pub fn tensor(&self, name: &str) -> Option<&TensorEntry> {
        self.index
            .tensors
            .binary_search_by(|t| t.name.as_str().cmp(name))
            .ok()
            .map(|i| &self.index.tensors[i])
    }

    /// Passes a tensor's bytes to `sink` one piece at a time, in order.
    ///
    /// Each call receives a whole piece, and pieces are at most 16 MiB and end on
    /// element boundaries. A GPU upload, for example, can copy each piece into a
    /// device buffer as it arrives, so the whole tensor never sits in host memory.
    /// Each piece is checked against its checksum before `sink` sees it.
    pub fn read_tensor_into<F>(&mut self, name: &str, mut sink: F) -> Result<()>
    where
        F: FnMut(&[u8]) -> Result<()>,
    {
        let tensor = self
            .tensor(name)
            .ok_or_else(|| Error::NoSuchTensor(name.to_owned()))?
            .clone();
        let width = dtype::block_bytes(&tensor.dtype)?;
        let first = tensor.first_piece as usize;
        for k in 0..tensor.piece_count as usize {
            let raw = self.read_piece_at(name, first, k, width)?;
            sink(&raw)?;
        }
        Ok(())
    }

    /// Reads a whole tensor into memory.
    pub fn read_tensor(&mut self, name: &str) -> Result<Vec<u8>> {
        let len = self
            .tensor(name)
            .ok_or_else(|| Error::NoSuchTensor(name.to_owned()))?
            .byte_len;
        let mut out = Vec::with_capacity(len as usize);
        self.read_tensor_into(name, |piece| {
            out.extend_from_slice(piece);
            Ok(())
        })?;
        Ok(out)
    }

    /// Checks every piece's checksum, and decodes every compressed piece to check its length.
    pub fn verify(&mut self) -> Result<()> {
        for tensor in self.index.tensors.clone() {
            let width = dtype::block_bytes(&tensor.dtype)?;
            let first = tensor.first_piece as usize;
            for k in 0..tensor.piece_count as usize {
                self.read_piece_at(&tensor.name, first, k, width)?;
            }
        }
        Ok(())
    }

    /// Reads piece `k` of the tensor whose first piece is `first`.
    fn read_piece_at(&mut self, name: &str, first: usize, k: usize, width: u64) -> Result<Vec<u8>> {
        let piece = self.index.pieces[first + k];
        let base = match piece.compression {
            Compression::Delta => Some(self.base_piece(name, k, width)?),
            _ => None,
        };
        self.read_piece(&piece, name, width, base.as_deref())
    }

    /// Reads piece `k` of the tensor `name` from the base archive.
    fn base_piece(&mut self, name: &str, k: usize, width: u64) -> Result<Vec<u8>> {
        let id = self
            .metadata()
            .get(BASE_ID_KEY)
            .cloned()
            .unwrap_or_default();
        let base = self.base.as_mut().ok_or(Error::BaseNeeded(id))?;
        let tensor = base
            .tensor(name)
            .ok_or_else(|| Error::NoSuchTensor(name.to_owned()))?
            .clone();
        base.read_piece_at(name, tensor.first_piece as usize, k, width)
    }

    /// Piece `k` of the tensor `name`, decoded, for use as a base by a delta. `None`
    /// if the tensor is missing or has fewer than `k + 1` pieces.
    pub(crate) fn piece_of_tensor(
        &mut self,
        name: &str,
        k: usize,
        width: u64,
    ) -> Result<Option<Vec<u8>>> {
        let Some(tensor) = self.tensor(name).cloned() else {
            return Ok(None);
        };
        if k >= tensor.piece_count as usize {
            return Ok(None);
        }
        self.read_piece_at(name, tensor.first_piece as usize, k, width)
            .map(Some)
    }

    /// Reads one piece, checks its checksum, and decodes it. `base` is the same
    /// piece of the base archive, for a delta piece.
    fn read_piece(
        &mut self,
        piece: &Piece,
        name: &str,
        width: u64,
        base: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        check_piece_bounds(piece)?;
        self.input
            .seek(SeekFrom::Start(piece.offset))
            .map_err(map_io)?;
        let mut stored = vec![0u8; piece.stored_len as usize];
        self.input.read_exact(&mut stored).map_err(map_io)?;
        decode_piece(piece, stored, name, width, base)
    }
}

/// Checks a piece's declared sizes before any buffer is allocated for it.
/// A piece is at most `MAX_PIECE_BYTES`, and never stored larger than its raw
/// bytes, since the writer only compresses a piece when that makes it smaller.
pub(crate) fn check_piece_bounds(piece: &Piece) -> Result<()> {
    if piece.raw_len as u64 > MAX_PIECE_BYTES {
        return Err(Error::Corrupt("piece is larger than the limit"));
    }
    if piece.stored_len > piece.raw_len {
        return Err(Error::Corrupt("stored piece is larger than its raw data"));
    }
    Ok(())
}

/// Checks a piece's stored bytes against its checksum, then decodes them.
/// Shared by the blocking and async readers.
pub(crate) fn decode_piece(
    piece: &Piece,
    stored: Vec<u8>,
    name: &str,
    width: u64,
    base: Option<&[u8]>,
) -> Result<Vec<u8>> {
    check_piece_bounds(piece)?;
    if crc32fast::hash(&stored) != piece.crc32 {
        return Err(Error::ChecksumMismatch(name.to_owned()));
    }
    match piece.compression {
        Compression::Delta => {
            let base = base.ok_or(Error::Corrupt("delta piece has no base piece"))?;
            let raw = codec::decompress(&stored, piece.raw_len as usize)?;
            if raw.len() != piece.raw_len as usize || base.len() != raw.len() {
                return Err(Error::Corrupt(
                    "delta piece and its base piece differ in length",
                ));
            }
            Ok(raw.iter().zip(base).map(|(a, b)| a ^ b).collect())
        }
        Compression::Shuffled => {
            if !shuffle::supports(width) {
                return Err(Error::Corrupt(
                    "shuffled piece for an unsupported element width",
                ));
            }
            let raw = codec::decompress(&stored, piece.raw_len as usize)?;
            if raw.len() != piece.raw_len as usize || raw.len() % width as usize != 0 {
                return Err(Error::Corrupt(
                    "decoded piece length does not match its index",
                ));
            }
            Ok(shuffle::unshuffle(&raw, width as usize))
        }
        Compression::Split => split::decode_payload(&stored, piece.raw_len as usize),
        Compression::Stored => {
            if stored.len() != piece.raw_len as usize {
                return Err(Error::Corrupt(
                    "stored piece length does not match its index",
                ));
            }
            Ok(stored)
        }
        Compression::Zstd => {
            let raw = codec::decompress(&stored, piece.raw_len as usize)?;
            if raw.len() != piece.raw_len as usize {
                return Err(Error::Corrupt(
                    "decoded piece length does not match its index",
                ));
            }
            Ok(raw)
        }
    }
}
