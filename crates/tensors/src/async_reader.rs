//! Async reader for `.tptn` archives, for sources that are read by range, such
//! as a file opened with tokio or a network object store.
//!
//! Opening reads only the header, the footer and the index. Each tensor is then
//! fetched one piece at a time, with a seek and a read per piece. The whole file
//! is never needed, so a loader can start on the first piece while the rest is
//! still in flight.

use std::collections::{BTreeMap, BTreeSet};
use std::io::SeekFrom;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt};

use crate::error::{map_io, Error, Result};
use crate::format::{
    check_header, finish_index, footer_location, Index, Piece, TensorEntry, FOOTER_LEN, HEADER_LEN,
};
use crate::reader::{check_piece_bounds, decode_piece};
use crate::sharded::Manifest;

/// An open tensor archive whose bytes are read asynchronously.
pub struct AsyncArchive<R> {
    input: R,
    index: Index,
}

impl<R: AsyncRead + AsyncSeek + Unpin> AsyncArchive<R> {
    /// Opens and checks an archive, reading only its header, footer and index.
    pub async fn open(mut input: R) -> Result<Self> {
        let file_len = input.seek(SeekFrom::End(0)).await.map_err(map_io)?;
        if file_len < HEADER_LEN + FOOTER_LEN {
            return Err(Error::Truncated);
        }

        input.seek(SeekFrom::Start(0)).await.map_err(map_io)?;
        let mut header = [0u8; HEADER_LEN as usize];
        input.read_exact(&mut header).await.map_err(map_io)?;
        check_header(&header)?;

        input
            .seek(SeekFrom::End(-(FOOTER_LEN as i64)))
            .await
            .map_err(map_io)?;
        let mut footer = [0u8; FOOTER_LEN as usize];
        input.read_exact(&mut footer).await.map_err(map_io)?;
        let (index_offset, index_len, index_crc) = footer_location(&footer, file_len)?;

        input
            .seek(SeekFrom::Start(index_offset))
            .await
            .map_err(map_io)?;
        let mut body = vec![0u8; index_len as usize];
        input.read_exact(&mut body).await.map_err(map_io)?;
        let index = finish_index(&body, index_crc, index_offset)?;
        Ok(Self { input, index })
    }

    /// The metadata carried over from the source file.
    pub fn metadata(&self) -> &BTreeMap<String, String> {
        &self.index.metadata
    }

    /// Every tensor, sorted by name.
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

    /// Passes a tensor's bytes to `sink` one piece at a time, in order. Each
    /// piece is checked against its checksum before `sink` sees it.
    pub async fn read_tensor_into<F>(&mut self, name: &str, mut sink: F) -> Result<()>
    where
        F: FnMut(&[u8]) -> Result<()>,
    {
        let tensor = self
            .tensor(name)
            .ok_or_else(|| Error::NoSuchTensor(name.to_owned()))?
            .clone();
        let width = crate::dtype::block_bytes(&tensor.dtype)?;
        let first = tensor.first_piece as usize;
        for i in first..first + tensor.piece_count as usize {
            let piece = self.index.pieces[i];
            let raw = self.read_piece(&piece, name, width).await?;
            sink(&raw)?;
        }
        Ok(())
    }

    /// Reads a whole tensor into memory.
    pub async fn read_tensor(&mut self, name: &str) -> Result<Vec<u8>> {
        let len = self
            .tensor(name)
            .ok_or_else(|| Error::NoSuchTensor(name.to_owned()))?
            .byte_len;
        let mut out = Vec::with_capacity(len as usize);
        self.read_tensor_into(name, |piece| {
            out.extend_from_slice(piece);
            Ok(())
        })
        .await?;
        Ok(out)
    }

    /// Fetches one piece by range, then checks and decodes it.
    async fn read_piece(&mut self, piece: &Piece, name: &str, width: u64) -> Result<Vec<u8>> {
        check_piece_bounds(piece)?;
        self.input
            .seek(SeekFrom::Start(piece.offset))
            .await
            .map_err(map_io)?;
        let mut stored = vec![0u8; piece.stored_len as usize];
        self.input.read_exact(&mut stored).await.map_err(map_io)?;
        decode_piece(piece, stored, name, width)
    }
}

/// A sharded archive read asynchronously: one open [`AsyncArchive`] per shard,
/// and the manifest that says which tensors each shard holds.
pub struct ShardedArchive<R> {
    metadata: BTreeMap<String, String>,
    shards: Vec<AsyncArchive<R>>,
    /// Shard index of each tensor.
    location: BTreeMap<String, usize>,
}

impl<R: AsyncRead + AsyncSeek + Unpin> ShardedArchive<R> {
    /// Pairs a manifest with its shards, which must be given in manifest order.
    /// Each shard must hold exactly the tensors the manifest lists for it.
    pub fn new(manifest: &Manifest, shards: Vec<AsyncArchive<R>>) -> Result<Self> {
        if shards.len() != manifest.shards.len() {
            return Err(Error::InvalidModel(format!(
                "the manifest lists {} shards, but {} were given",
                manifest.shards.len(),
                shards.len()
            )));
        }
        let mut location = BTreeMap::new();
        for (index, (entry, shard)) in manifest.shards.iter().zip(&shards).enumerate() {
            let held: BTreeSet<&str> = shard.tensors().iter().map(|t| t.name.as_str()).collect();
            let listed: BTreeSet<&str> = entry.tensors.iter().map(String::as_str).collect();
            if held != listed {
                return Err(Error::InvalidModel(format!(
                    "shard `{}` does not hold the tensors the manifest lists",
                    entry.file
                )));
            }
            for name in &entry.tensors {
                location.insert(name.clone(), index);
            }
        }
        Ok(Self {
            metadata: manifest.metadata.clone(),
            shards,
            location,
        })
    }

    /// The metadata carried over from the source model.
    pub fn metadata(&self) -> &BTreeMap<String, String> {
        &self.metadata
    }

    /// Names of every tensor, sorted.
    pub fn tensor_names(&self) -> impl Iterator<Item = &str> {
        self.location.keys().map(String::as_str)
    }

    /// Reads a whole tensor, from whichever shard holds it.
    pub async fn read_tensor(&mut self, name: &str) -> Result<Vec<u8>> {
        let shard = self.shard_of(name)?;
        self.shards[shard].read_tensor(name).await
    }

    /// Passes a tensor's pieces to `sink` in order, from whichever shard holds it.
    pub async fn read_tensor_into<F>(&mut self, name: &str, sink: F) -> Result<()>
    where
        F: FnMut(&[u8]) -> Result<()>,
    {
        let shard = self.shard_of(name)?;
        self.shards[shard].read_tensor_into(name, sink).await
    }

    fn shard_of(&self, name: &str) -> Result<usize> {
        self.location
            .get(name)
            .copied()
            .ok_or_else(|| Error::NoSuchTensor(name.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::{pack, PackOptions};

    /// A safetensors file with two tensors, and a metadata entry.
    fn safetensors_file(big_len: usize) -> Vec<u8> {
        let small: Vec<u8> = (0..8u8).collect();
        let big: Vec<u8> = (0..big_len).map(|i| (i * 7 % 251) as u8).collect();
        let header = format!(
            "{{\"__metadata__\":{{\"source\":\"test\"}},\
             \"big\":{{\"dtype\":\"U8\",\"shape\":[{big_len}],\"data_offsets\":[0,{big_len}]}},\
             \"small\":{{\"dtype\":\"U8\",\"shape\":[8],\"data_offsets\":[{big_len},{}]}}}}",
            big_len + 8
        );
        let mut out = (header.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(&big);
        out.extend_from_slice(&small);
        out
    }

    fn packed(big_len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        pack(
            Cursor::new(safetensors_file(big_len)),
            &mut out,
            &PackOptions::default(),
        )
        .unwrap();
        out
    }

    #[tokio::test]
    async fn reads_each_tensor_like_the_blocking_reader() {
        let archive_bytes = packed(100_000);
        let mut blocking = crate::Archive::open(Cursor::new(archive_bytes.clone())).unwrap();
        let mut asynchronous = AsyncArchive::open(Cursor::new(archive_bytes))
            .await
            .unwrap();

        assert_eq!(asynchronous.metadata()["source"], "test");
        assert_eq!(asynchronous.tensors().len(), 2);
        for name in ["big", "small"] {
            let expected = blocking.read_tensor(name).unwrap();
            assert_eq!(asynchronous.read_tensor(name).await.unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn streams_a_tensor_one_piece_at_a_time() {
        // Larger than one piece, so the sink sees several calls.
        let big_len = crate::MAX_PIECE_BYTES as usize + 1000;
        let mut archive = AsyncArchive::open(Cursor::new(packed(big_len)))
            .await
            .unwrap();

        let mut calls = 0;
        let mut total = 0usize;
        archive
            .read_tensor_into("big", |piece| {
                calls += 1;
                total += piece.len();
                Ok(())
            })
            .await
            .unwrap();
        assert!(calls >= 2, "expected several pieces, got {calls}");
        assert_eq!(total, big_len);
    }

    #[tokio::test]
    async fn missing_tensors_and_damage_are_reported() {
        let mut archive = AsyncArchive::open(Cursor::new(packed(64))).await.unwrap();
        assert!(matches!(
            archive.read_tensor("nope").await,
            Err(Error::NoSuchTensor(_))
        ));

        let mut damaged = packed(64);
        damaged[HEADER_LEN as usize + 3] ^= 0xff;
        let mut archive = AsyncArchive::open(Cursor::new(damaged)).await.unwrap();
        assert!(archive.read_tensor("big").await.is_err());
    }

    #[tokio::test]
    async fn rejects_files_that_are_not_archives() {
        assert!(
            AsyncArchive::open(Cursor::new(b"not an archive at all".to_vec()))
                .await
                .is_err()
        );
        assert!(AsyncArchive::open(Cursor::new(Vec::new())).await.is_err());
    }

    /// A writer whose bytes can be read back after it is dropped.
    #[derive(Clone, Default)]
    struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

    impl std::io::Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn sharded_archive_reads_each_tensor_from_its_shard() {
        use crate::sharded::pack_sharded;

        let big_len = 2000;
        let source = safetensors_file(big_len);
        let written: std::cell::RefCell<Vec<Shared>> = std::cell::RefCell::new(Vec::new());
        let (manifest, _) =
            pack_sharded(Cursor::new(&source), 1000, &PackOptions::default(), |_| {
                let shared = Shared::default();
                written.borrow_mut().push(shared.clone());
                Ok(shared)
            })
            .unwrap();
        assert_eq!(
            manifest.shards.len(),
            2,
            "big and small do not share a shard"
        );

        let mut shards = Vec::new();
        // Copy the bytes out first, so no borrow is held across an await.
        let shard_bytes: Vec<Vec<u8>> = written
            .borrow()
            .iter()
            .map(|shared| shared.0.borrow().clone())
            .collect();
        for bytes in shard_bytes {
            shards.push(AsyncArchive::open(Cursor::new(bytes)).await.unwrap());
        }
        let mut sharded = ShardedArchive::new(&manifest, shards).unwrap();
        assert_eq!(sharded.metadata()["source"], "test");
        assert_eq!(sharded.tensor_names().collect::<Vec<_>>(), ["big", "small"]);

        let expected = crate::Archive::open(Cursor::new(packed(big_len)))
            .unwrap()
            .read_tensor("big")
            .unwrap();
        assert_eq!(sharded.read_tensor("big").await.unwrap(), expected);
        assert_eq!(sharded.read_tensor("small").await.unwrap().len(), 8);
        assert!(matches!(
            sharded.read_tensor("nope").await,
            Err(Error::NoSuchTensor(_))
        ));
    }

    #[tokio::test]
    async fn a_shard_that_does_not_match_the_manifest_is_refused() {
        use crate::sharded::{Manifest, Shard};

        let archive = AsyncArchive::open(Cursor::new(packed(64))).await.unwrap();
        let manifest = Manifest {
            metadata: BTreeMap::new(),
            shards: vec![Shard {
                file: "a.tptn".to_owned(),
                tensors: vec!["wrong".to_owned()],
            }],
        };
        assert!(ShardedArchive::new(&manifest, vec![archive]).is_err());
    }
}
