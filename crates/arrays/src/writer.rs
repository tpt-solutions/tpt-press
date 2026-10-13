//! Packs an array into a chunked arrays archive.
//!
//! The input is read whole, so memory use is about the size of the array.
//! Each chunk is tried with each allowed filter, and the smallest result is kept.

use std::io::{Read, Write};

use tpt_press_core::codec;

use crate::dtype::{self, Dtype};
use crate::error::{Error, Result};
use crate::filter::Filter;
use crate::format::{self, Entry, Header, MAX_DIMS};
use crate::layout::{runs, Geometry};

/// Target size of a default chunk, in bytes, before compression.
pub const TARGET_CHUNK_BYTES: usize = 1 << 20;

/// Which filters the writer may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterChoice {
    /// Try every filter on each chunk and keep the smallest.
    Auto,
    /// Use this filter for every chunk.
    Fixed(Filter),
}

/// Options for [`pack_npy`] and [`pack_raw`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackOptions {
    /// Chunk shape, one length per axis. `None` picks about [`TARGET_CHUNK_BYTES`]
    /// per chunk, by splitting the first axis.
    pub chunk_shape: Option<Vec<u64>>,
    /// zstd compression level, in `1..=22`.
    pub level: i32,
    /// Which filters the writer may use.
    pub filter: FilterChoice,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            chunk_shape: None,
            level: codec::DEFAULT_LEVEL,
            filter: FilterChoice::Auto,
        }
    }
}

/// Counts from a completed pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    /// The element type, as a NumPy `descr` string.
    pub dtype: String,
    /// The array shape.
    pub shape: Vec<u64>,
    /// The chunk shape used.
    pub chunk_shape: Vec<u64>,
    /// Number of chunks written.
    pub chunks: u64,
    /// Bytes of array data read.
    pub input_bytes: u64,
    /// Bytes written to the archive.
    pub output_bytes: u64,
    /// Chunks stored with each filter, indexed by [`Filter::code`].
    pub filter_counts: [u64; 6],
}

/// Packs a `.npy` file. Its exact header is kept, so unpacking reproduces the file.
pub fn pack_npy<R: Read, W: Write>(
    mut input: R,
    output: W,
    options: &PackOptions,
) -> Result<Stats> {
    let mut bytes = Vec::new();
    input.read_to_end(&mut bytes)?;
    let (header, offset) = dtype::parse_npy(&bytes)?;
    let data = &bytes[offset..];
    check_data_len(data, &header.shape, &header.dtype)?;
    pack_array(
        header.raw,
        header.dtype,
        header.shape,
        data,
        options,
        output,
    )
}

/// Packs raw bytes with the element type and shape given by the caller.
/// The bytes are stored in C order, and no header is kept.
pub fn pack_raw<R: Read, W: Write>(
    mut input: R,
    output: W,
    descr: &str,
    shape: &[u64],
    options: &PackOptions,
) -> Result<Stats> {
    let dtype = Dtype::parse(descr)?;
    let mut data = Vec::new();
    input.read_to_end(&mut data)?;
    check_data_len(&data, shape, &dtype)?;
    pack_array(Vec::new(), dtype, shape.to_vec(), &data, options, output)
}

/// A chunk shape that gives about [`TARGET_CHUNK_BYTES`] per chunk, by splitting the first axis.
/// Trailing axes are kept whole, so a single row is never split.
pub fn default_chunk_shape(shape: &[u64], elem: usize) -> Vec<u64> {
    if shape.is_empty() {
        return Vec::new();
    }
    let row_bytes = shape[1..].iter().product::<u64>() * elem as u64;
    let rows = match (TARGET_CHUNK_BYTES as u64).checked_div(row_bytes) {
        Some(rows) => rows.max(1).min(shape[0]),
        None => shape[0],
    };
    let mut chunk = vec![rows.max(1)];
    chunk.extend_from_slice(&shape[1..]);
    chunk
}

fn check_data_len(data: &[u8], shape: &[u64], dtype: &Dtype) -> Result<()> {
    let elements = element_count(shape)?;
    let expected = elements
        .checked_mul(dtype.size() as u64)
        .ok_or(Error::Format {
            message: "the array is too large".to_owned(),
        })?;
    if data.len() as u64 != expected {
        return Err(Error::Format {
            message: format!(
                "the data is {} bytes, but shape {:?} of {} needs {}",
                data.len(),
                shape,
                dtype.descr(),
                expected
            ),
        });
    }
    Ok(())
}

fn element_count(shape: &[u64]) -> Result<u64> {
    if shape.is_empty() || shape.len() > MAX_DIMS {
        return Err(Error::Unsupported {
            message: "arrays need between 1 and 32 dimensions; scalars are not supported",
        });
    }
    shape
        .iter()
        .try_fold(1u64, |acc, &d| acc.checked_mul(d))
        .ok_or(Error::Format {
            message: "the array is too large".to_owned(),
        })
}

fn pack_array<W: Write>(
    raw: Vec<u8>,
    dtype: Dtype,
    shape: Vec<u64>,
    data: &[u8],
    options: &PackOptions,
    mut output: W,
) -> Result<Stats> {
    codec::validate_level(options.level)?;
    element_count(&shape)?;
    let elem = dtype.size();
    let chunk = match &options.chunk_shape {
        Some(chunk) => {
            if chunk.len() != shape.len() {
                return Err(Error::InvalidOption(
                    "chunk shape must have one length per axis",
                ));
            }
            if chunk.contains(&0) {
                return Err(Error::InvalidOption("chunk lengths must be at least 1"));
            }
            chunk.clone()
        }
        None => default_chunk_shape(&shape, elem),
    };
    let geometry = Geometry {
        shape: shape.clone(),
        chunk: chunk.clone(),
        elem,
    };

    let header = Header {
        dtype: dtype.clone(),
        shape: shape.clone(),
        chunk: chunk.clone(),
        raw,
    };
    let mut offset = format::write_header(&mut output, &header)?;
    let candidates: Vec<Filter> = match options.filter {
        FilterChoice::Auto => Filter::ALL.to_vec(),
        FilterChoice::Fixed(filter) => vec![filter],
    };
    let mut entries: Vec<Entry> = Vec::new();
    let mut filter_counts = [0u64; 6];

    for linear in 0..geometry.chunk_count() {
        let (start, extent) = geometry.chunk_box(linear);
        let bytes = gather(data, &shape, elem, &start, &extent);

        let mut best: Option<(Filter, Vec<u8>)> = None;
        for filter in candidates.iter().copied() {
            let row = extent.last().copied().unwrap_or(1) as usize;
            let compressed = codec::compress(&filter.encode(&bytes, elem, row), options.level)?;
            if best
                .as_ref()
                .is_none_or(|(_, b)| compressed.len() < b.len())
            {
                best = Some((filter, compressed));
            }
        }
        let (filter, payload) = best.ok_or(Error::InvalidOption("no filter was allowed"))?;

        output.write_all(&payload)?;
        filter_counts[filter.code() as usize] += 1;
        entries.push(Entry {
            offset,
            len: payload.len() as u64,
            crc: crc32fast::hash(&payload),
            filter,
        });
        offset += payload.len() as u64;
    }

    format::write_index(&mut output, &entries, offset)?;
    output.flush()?;
    Ok(Stats {
        dtype: dtype.descr().to_owned(),
        shape,
        chunk_shape: chunk,
        chunks: entries.len() as u64,
        input_bytes: data.len() as u64,
        output_bytes: format::file_len(entries.len(), offset),
        filter_counts,
    })
}

/// Copies one box out of a C-order array, as one buffer.
fn gather(data: &[u8], shape: &[u64], elem: usize, start: &[u64], extent: &[u64]) -> Vec<u8> {
    let total: u64 = extent.iter().product::<u64>() * elem as u64;
    let mut out = Vec::with_capacity(total as usize);
    for (offset, len) in runs(shape, start, extent, elem) {
        out.extend_from_slice(&data[offset..offset + len]);
    }
    out
}
