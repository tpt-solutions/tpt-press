//! Reads arrays archives: unpacks the whole array, summarises it, and reads
//! a hyperslab (a rectangular slice) by decoding only the chunks it touches.

use std::io::{Read, Seek, SeekFrom, Write};
use std::ops::Range;

use tpt_press_core::codec;

use crate::error::{map_io, Error, Result};
use crate::format::{self, Entry, Header};
use crate::layout::{overlap, runs, Geometry};

/// What an archive holds, from its header and index alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// The element type, as a NumPy `descr` string.
    pub dtype: String,
    /// The array shape.
    pub shape: Vec<u64>,
    /// The chunk shape.
    pub chunk_shape: Vec<u64>,
    /// Number of chunks.
    pub chunks: u64,
    /// Size of the uncompressed array, in bytes.
    pub raw_bytes: u64,
    /// Size of the archive file, in bytes.
    pub archive_bytes: u64,
    /// Whether the archive keeps an original `.npy` header.
    pub npy: bool,
}

/// A slice read from an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slice {
    /// The slice's elements, in C order, with no header.
    pub bytes: Vec<u8>,
    /// The slice's shape: the length of each range.
    pub shape: Vec<u64>,
    /// Chunks that were decoded. Chunks the slice does not touch are skipped.
    pub chunks_read: u64,
}

/// Reads the header and index, and returns the summary. No chunk is decoded.
pub fn inspect<R: Read + Seek>(mut input: R) -> Result<Summary> {
    let (header, geometry, entries) = open(&mut input)?;
    summarise(&header, &geometry, entries.len(), &mut input)
}

/// Writes the whole array. For a `.npy` archive, the original header comes first,
/// so the output is the original file.
pub fn unpack<R: Read + Seek, W: Write>(mut input: R, mut output: W) -> Result<Summary> {
    let (header, geometry, entries) = open(&mut input)?;
    let full: Vec<Range<u64>> = header.shape.iter().map(|&d| 0..d).collect();
    let slice = read_box(&mut input, &geometry, &entries, &full)?;
    output.write_all(&header.raw)?;
    output.write_all(&slice.bytes)?;
    output.flush()?;
    summarise(&header, &geometry, entries.len(), &mut input)
}

/// Reads one rectangular slice. `ranges` has one half-open range per axis.
pub fn read_slice<R: Read + Seek>(mut input: R, ranges: &[Range<u64>]) -> Result<Slice> {
    let (header, geometry, entries) = open(&mut input)?;
    if ranges.len() != header.shape.len() {
        return Err(Error::InvalidOption("give one range per axis"));
    }
    for (axis, (range, &length)) in ranges.iter().zip(&header.shape).enumerate() {
        if range.start > range.end || range.end > length {
            return Err(Error::RangeOutOfBounds {
                axis,
                start: range.start,
                end: range.end,
                length,
            });
        }
    }
    read_box(&mut input, &geometry, &entries, ranges)
}

/// Reads the header, the geometry and the index.
fn open<R: Read + Seek>(input: &mut R) -> Result<(Header, Geometry, Vec<Entry>)> {
    let header = format::read_header(input)?;
    let geometry = Geometry {
        shape: header.shape.clone(),
        chunk: header.chunk.clone(),
        elem: header.dtype.size(),
    };
    let data_start = format::header_len(&header);
    let entries = format::read_index(input, data_start, geometry.chunk_count())?;
    Ok((header, geometry, entries))
}

fn summarise<R: Seek>(
    header: &Header,
    geometry: &Geometry,
    chunks: usize,
    input: &mut R,
) -> Result<Summary> {
    let archive_bytes = input.seek(SeekFrom::End(0)).map_err(map_io)?;
    Ok(Summary {
        dtype: header.dtype.descr().to_owned(),
        shape: header.shape.clone(),
        chunk_shape: header.chunk.clone(),
        chunks: chunks as u64,
        raw_bytes: geometry.elements() * geometry.elem as u64,
        archive_bytes,
        npy: !header.raw.is_empty(),
    })
}

/// Gathers a box from the chunks it overlaps. Each chunk is decoded at most once.
fn read_box<R: Read + Seek>(
    input: &mut R,
    geometry: &Geometry,
    entries: &[Entry],
    ranges: &[Range<u64>],
) -> Result<Slice> {
    let box_start: Vec<u64> = ranges.iter().map(|r| r.start).collect();
    let box_extent: Vec<u64> = ranges.iter().map(|r| r.end - r.start).collect();
    let elem = geometry.elem;
    let total = box_extent.iter().product::<u64>() as usize * elem;
    let mut out = vec![0u8; total];
    let mut chunks_read = 0u64;

    for linear in 0..geometry.chunk_count() {
        let (chunk_start, chunk_extent) = geometry.chunk_box(linear);
        let Some((part_start, part_extent)) =
            overlap(&box_start, &box_extent, &chunk_start, &chunk_extent)
        else {
            continue;
        };
        let chunk = decode_chunk(input, &entries[linear as usize], &chunk_extent, elem)?;
        chunks_read += 1;

        let from_chunk = runs(
            &chunk_extent,
            &sub(&part_start, &chunk_start),
            &part_extent,
            elem,
        );
        let into_box = runs(
            &box_extent,
            &sub(&part_start, &box_start),
            &part_extent,
            elem,
        );
        for ((src, len), (dst, _)) in from_chunk.into_iter().zip(into_box) {
            out[dst..dst + len].copy_from_slice(&chunk[src..src + len]);
        }
    }

    Ok(Slice {
        bytes: out,
        shape: box_extent,
        chunks_read,
    })
}

/// Reads one chunk, checks its CRC, decompresses it, and undoes its filter.
fn decode_chunk<R: Read + Seek>(
    input: &mut R,
    entry: &Entry,
    extent: &[u64],
    elem: usize,
) -> Result<Vec<u8>> {
    let expected = extent.iter().product::<u64>() as usize * elem;
    let row = extent.last().copied().unwrap_or(1) as usize;
    input.seek(SeekFrom::Start(entry.offset)).map_err(map_io)?;
    let mut payload = vec![0u8; entry.len as usize];
    input.read_exact(&mut payload).map_err(map_io)?;
    if crc32fast::hash(&payload) != entry.crc {
        return Err(Error::Corrupt("chunk checksum does not match"));
    }
    let filtered = codec::decompress(&payload, expected)?;
    if filtered.len() != expected {
        return Err(Error::Corrupt("chunk has the wrong size"));
    }
    Ok(entry.filter.decode(&filtered, elem, row))
}

fn sub(a: &[u64], b: &[u64]) -> Vec<u64> {
    a.iter().zip(b).map(|(x, y)| x - y).collect()
}
