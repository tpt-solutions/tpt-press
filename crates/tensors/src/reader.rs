//! Reads tensors from a `.tptn` archive, one piece at a time.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

use tpt_press_core::codec;

use crate::error::{map_io, Error, Result};
use crate::format::{self, Compression, Index, Piece, TensorEntry, HEADER_LEN, MAX_PIECE_BYTES};

/// An open tensor archive. Opening reads only the header, footer and index.
pub struct Archive<R> {
    input: R,
    index: Index,
}

impl<R: Read + Seek> Archive<R> {
    /// Opens and checks an archive.
    pub fn open(mut input: R) -> Result<Self> {
        input.seek(SeekFrom::Start(0)).map_err(map_io)?;
        let mut header = [0u8; HEADER_LEN as usize];
        input.read_exact(&mut header).map_err(map_io)?;
        format::check_header(&header)?;
        let index = format::read_index_from(&mut input)?;
        Ok(Self { input, index })
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
        let first = tensor.first_piece as usize;
        for i in first..first + tensor.piece_count as usize {
            let piece = self.index.pieces[i];
            let raw = self.read_piece(&piece, name)?;
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
            let first = tensor.first_piece as usize;
            for i in first..first + tensor.piece_count as usize {
                let piece = self.index.pieces[i];
                self.read_piece(&piece, &tensor.name)?;
            }
        }
        Ok(())
    }

    /// Reads one piece, checks its checksum, and decodes it.
    fn read_piece(&mut self, piece: &Piece, name: &str) -> Result<Vec<u8>> {
        if piece.raw_len as u64 > MAX_PIECE_BYTES {
            return Err(Error::Corrupt("piece is larger than the limit"));
        }
        self.input
            .seek(SeekFrom::Start(piece.offset))
            .map_err(map_io)?;
        let mut stored = vec![0u8; piece.stored_len as usize];
        self.input.read_exact(&mut stored).map_err(map_io)?;
        if crc32fast::hash(&stored) != piece.crc32 {
            return Err(Error::ChecksumMismatch(name.to_owned()));
        }
        match piece.compression {
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
}
