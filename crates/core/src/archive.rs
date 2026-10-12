//! Streaming pack and unpack of the generic archive format.
//!
//! Input is read in fixed-size chunks. Each batch of chunks is compressed or
//! decompressed in parallel with rayon, then written in order. Memory use is
//! bounded by the batch size, not the input size.

use std::io::{self, Read, Write};

use rayon::prelude::*;

use crate::codec::{self, DEFAULT_LEVEL};
use crate::error::{Error, Result};
use crate::format::{self, FrameHeader, FRAME_HEADER_LEN, HEADER_LEN};

/// Default uncompressed bytes per chunk.
pub const DEFAULT_CHUNK_SIZE: usize = 1024 * 1024;

/// Largest chunk the format can describe. Frame sizes are stored as `u32`.
pub const MAX_CHUNK_SIZE: usize = 64 * 1024 * 1024;

/// Options for [`pack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackOptions {
    /// Uncompressed bytes per chunk. Must be in `1..=MAX_CHUNK_SIZE`.
    pub chunk_size: usize,
    /// zstd compression level, in `1..=22`.
    pub level: i32,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
            level: DEFAULT_LEVEL,
        }
    }
}

/// Options for [`unpack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnpackOptions {
    /// Largest uncompressed chunk accepted. Chunks above this are rejected
    /// before any allocation, which protects against hostile headers.
    pub max_chunk_size: usize,
}

impl Default for UnpackOptions {
    fn default() -> Self {
        Self {
            max_chunk_size: MAX_CHUNK_SIZE,
        }
    }
}

/// Byte and chunk counts for a completed pack or unpack.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Uncompressed payload bytes, the input for `pack` and the output for `unpack`.
    pub uncompressed_bytes: u64,
    /// Total archive bytes, the output for `pack` and the input for `unpack`.
    pub archive_bytes: u64,
    /// Number of data chunks.
    pub chunks: u64,
}

/// Compresses everything from `input` into an archive written to `output`.
pub fn pack<R: Read, W: Write>(mut input: R, mut output: W, opts: &PackOptions) -> Result<Stats> {
    if opts.chunk_size == 0 || opts.chunk_size > MAX_CHUNK_SIZE {
        return Err(Error::InvalidArgument("chunk_size must be 1 to 64 MiB"));
    }
    codec::validate_level(opts.level)?;

    let batch_len = batch_len();
    let mut stats = Stats::default();

    format::write_header(&mut output)?;
    stats.archive_bytes += HEADER_LEN as u64;

    loop {
        let batch = read_batch(&mut input, opts.chunk_size, batch_len)?;
        if batch.is_empty() {
            break;
        }

        let packed: Vec<Vec<u8>> = batch
            .par_iter()
            .map(|chunk| codec::compress(chunk, opts.level))
            .collect::<Result<_>>()?;

        for (raw, comp) in batch.iter().zip(&packed) {
            FrameHeader {
                compressed_len: comp.len() as u32,
                uncompressed_len: raw.len() as u32,
                crc32: crc32fast::hash(comp),
            }
            .write(&mut output)?;
            output.write_all(comp)?;

            stats.uncompressed_bytes += raw.len() as u64;
            stats.archive_bytes += (FRAME_HEADER_LEN + comp.len()) as u64;
            stats.chunks += 1;
        }
    }

    FrameHeader::END.write(&mut output)?;
    stats.archive_bytes += FRAME_HEADER_LEN as u64;
    output.flush()?;
    Ok(stats)
}

/// Restores the original bytes from an archive read from `input` into `output`.
///
/// Every chunk is checked against its CRC32 and size limits before it is
/// decompressed. An archive that ends early returns [`Error::Truncated`].
pub fn unpack<R: Read, W: Write>(
    mut input: R,
    mut output: W,
    opts: &UnpackOptions,
) -> Result<Stats> {
    if opts.max_chunk_size == 0 || opts.max_chunk_size > MAX_CHUNK_SIZE {
        return Err(Error::InvalidArgument("max_chunk_size must be 1 to 64 MiB"));
    }

    format::read_header(&mut input).map_err(truncated)?;
    let mut stats = Stats {
        archive_bytes: HEADER_LEN as u64,
        ..Stats::default()
    };

    let batch_len = batch_len();
    let mut index: u64 = 0;
    let mut finished = false;

    while !finished {
        let mut frames: Vec<(FrameHeader, Vec<u8>)> = Vec::with_capacity(batch_len);

        while frames.len() < batch_len {
            let header = FrameHeader::read(&mut input).map_err(truncated)?;
            stats.archive_bytes += FRAME_HEADER_LEN as u64;

            if header.compressed_len == 0 {
                if header != FrameHeader::END {
                    return Err(Error::ChunkCorrupt { index });
                }
                finished = true;
                break;
            }

            check_limits(&header, opts.max_chunk_size)?;

            let mut payload = vec![0u8; header.compressed_len as usize];
            input
                .read_exact(&mut payload)
                .map_err(|e| truncated(e.into()))?;
            stats.archive_bytes += payload.len() as u64;

            if crc32fast::hash(&payload) != header.crc32 {
                return Err(Error::ChunkCorrupt { index });
            }

            frames.push((header, payload));
            index += 1;
        }

        let decoded: Vec<Vec<u8>> = frames
            .par_iter()
            .map(|(header, payload)| codec::decompress(payload, header.uncompressed_len as usize))
            .collect::<Result<_>>()?;

        let first_index = index - frames.len() as u64;
        for (offset, ((header, _), data)) in frames.iter().zip(&decoded).enumerate() {
            if data.len() != header.uncompressed_len as usize {
                return Err(Error::ChunkCorrupt {
                    index: first_index + offset as u64,
                });
            }
            output.write_all(data)?;
            stats.uncompressed_bytes += data.len() as u64;
            stats.chunks += 1;
        }
    }

    output.flush()?;
    Ok(stats)
}

/// Location and sizes of one chunk, as recorded in the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkInfo {
    /// Byte offset of the chunk's frame header within the archive.
    pub offset: u64,
    /// Size of the compressed payload in bytes.
    pub compressed_len: u32,
    /// Size of the chunk once decompressed, as declared by its frame header.
    pub uncompressed_len: u32,
}

/// The structure of an archive, read without decompressing its payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    /// Every data chunk, in archive order.
    pub chunks: Vec<ChunkInfo>,
    /// Totals. `uncompressed_bytes` comes from frame headers and is not
    /// confirmed by decompression. Use [`unpack`] to verify the data.
    pub stats: Stats,
}

impl Inspection {
    /// Uncompressed bytes divided by archive bytes. Above 1.0 means the archive is smaller.
    pub fn ratio(&self) -> f64 {
        self.stats.uncompressed_bytes as f64 / self.stats.archive_bytes.max(1) as f64
    }
}

/// Reads an archive's structure without decompressing it.
///
/// Each payload is still read and checked against its CRC32, so corruption is
/// reported. Size limits are the same as [`UnpackOptions::default`].
pub fn inspect<R: Read>(mut input: R) -> Result<Inspection> {
    format::read_header(&mut input).map_err(truncated)?;
    let mut stats = Stats {
        archive_bytes: HEADER_LEN as u64,
        ..Stats::default()
    };
    let mut chunks = Vec::new();
    let mut payload = Vec::new();

    loop {
        let offset = stats.archive_bytes;
        let header = FrameHeader::read(&mut input).map_err(truncated)?;
        stats.archive_bytes += FRAME_HEADER_LEN as u64;

        if header.compressed_len == 0 {
            if header != FrameHeader::END {
                return Err(Error::ChunkCorrupt {
                    index: chunks.len() as u64,
                });
            }
            break;
        }

        check_limits(&header, MAX_CHUNK_SIZE)?;

        payload.resize(header.compressed_len as usize, 0);
        input
            .read_exact(&mut payload)
            .map_err(|e| truncated(e.into()))?;
        stats.archive_bytes += payload.len() as u64;

        if crc32fast::hash(&payload) != header.crc32 {
            return Err(Error::ChunkCorrupt {
                index: chunks.len() as u64,
            });
        }

        stats.uncompressed_bytes += u64::from(header.uncompressed_len);
        stats.chunks += 1;
        chunks.push(ChunkInfo {
            offset,
            compressed_len: header.compressed_len,
            uncompressed_len: header.uncompressed_len,
        });
    }

    Ok(Inspection { chunks, stats })
}

/// Rejects a frame whose declared sizes exceed the limits, before any allocation.
fn check_limits(header: &FrameHeader, max_chunk_size: usize) -> Result<()> {
    if header.uncompressed_len as usize > max_chunk_size {
        return Err(Error::ChunkTooLarge {
            size: u64::from(header.uncompressed_len),
            limit: max_chunk_size,
        });
    }
    let max_compressed = max_compressed_len(max_chunk_size);
    if header.compressed_len as usize > max_compressed {
        return Err(Error::ChunkTooLarge {
            size: u64::from(header.compressed_len),
            limit: max_compressed,
        });
    }
    Ok(())
}

/// Maps an unexpected end of input to [`Error::Truncated`]. Other errors pass through.
fn truncated(e: Error) -> Error {
    match e {
        Error::Io(ref io) if io.kind() == io::ErrorKind::UnexpectedEof => Error::Truncated,
        other => other,
    }
}

/// Number of chunks held in memory at once. Two per worker keeps threads busy.
fn batch_len() -> usize {
    rayon::current_num_threads().saturating_mul(2).max(1)
}

/// Upper bound on the compressed size of a chunk with `uncompressed` bytes,
/// allowing for zstd's worst-case expansion of incompressible data.
fn max_compressed_len(uncompressed: usize) -> usize {
    uncompressed + uncompressed / 128 + 1024
}

/// Reads up to `max_chunks` chunks of `chunk_size` bytes. A short final chunk
/// is returned as is. An empty result means end of input.
fn read_batch<R: Read>(
    input: &mut R,
    chunk_size: usize,
    max_chunks: usize,
) -> io::Result<Vec<Vec<u8>>> {
    let mut batch = Vec::with_capacity(max_chunks);
    while batch.len() < max_chunks {
        let mut buf = vec![0u8; chunk_size];
        let filled = read_fill(input, &mut buf)?;
        if filled == 0 {
            break;
        }
        buf.truncate(filled);
        batch.push(buf);
        if filled < chunk_size {
            break;
        }
    }
    Ok(batch)
}

/// Fills `buf` from `r` unless end of input comes first. Returns bytes read.
fn read_fill<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}
