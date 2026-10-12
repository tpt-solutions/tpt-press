//! Packs a safetensors file into a `.tptn` archive.

use std::io::{Read, Seek, SeekFrom, Write};

use tpt_press_core::codec;

use crate::dtype;
use crate::error::{map_io, Error, Result};
use crate::format::{self, Compression, Index, Piece, TensorEntry, HEADER_LEN, MAX_PIECE_BYTES};
use crate::{gguf, onnx, safetensors};

/// Options for [`pack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackOptions {
    /// zstd compression level, in `1..=22`.
    pub level: i32,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            level: codec::DEFAULT_LEVEL,
        }
    }
}

/// Counts from a completed [`pack`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Tensors written.
    pub tensors: u64,
    /// Pieces written. A tensor larger than 16 MiB is split into several.
    pub pieces: u64,
    /// Pieces stored compressed. The rest are stored raw.
    pub compressed_pieces: u64,
    /// Total tensor bytes before compression.
    pub input_bytes: u64,
    /// Size of the archive in bytes.
    pub archive_bytes: u64,
}

/// Packs the tensors of a model file into an archive. The format is found from
/// the file's first bytes: GGUF if it starts with `GGUF`, safetensors otherwise.
///
/// Tensors are read one piece at a time, so memory use is one piece. Each tensor
/// is split only at its own block boundaries, so no piece holds bytes from two
/// tensors.
pub fn pack<R, W>(mut input: R, mut output: W, opts: &PackOptions) -> Result<Stats>
where
    R: Read + Seek,
    W: Write,
{
    codec::validate_level(opts.level)?;
    let model = read_model(&mut input)?;

    output.write_all(&format::header_bytes())?;
    let mut offset = HEADER_LEN;
    let mut stats = Stats::default();
    let mut index = Index {
        metadata: model.metadata.clone(),
        tensors: Vec::with_capacity(model.tensors.len()),
        pieces: Vec::new(),
    };

    for tensor in &model.tensors {
        let first_piece = index.pieces.len() as u32;
        // Whole blocks only: a piece must not end in the middle of a quantized block.
        let unit = dtype::block_bytes(&tensor.dtype)?;
        let piece_bytes = (MAX_PIECE_BYTES / unit) * unit;
        let mut position = 0u64;
        while position < tensor.byte_len() {
            let len = (tensor.byte_len() - position).min(piece_bytes);
            input
                .seek(SeekFrom::Start(model.data_start + tensor.begin + position))
                .map_err(map_io)?;
            let mut raw = vec![0u8; len as usize];
            input.read_exact(&mut raw).map_err(map_io)?;

            let packed = codec::compress(&raw, opts.level)?;
            let (payload, compression) = if packed.len() < raw.len() {
                (packed, Compression::Zstd)
            } else {
                (raw, Compression::Stored)
            };

            output.write_all(&payload)?;
            index.pieces.push(Piece {
                offset,
                stored_len: payload.len() as u32,
                raw_len: len as u32,
                crc32: crc32fast::hash(&payload),
                compression,
            });
            offset += payload.len() as u64;
            position += len;

            stats.pieces += 1;
            if compression == Compression::Zstd {
                stats.compressed_pieces += 1;
            }
        }

        let piece_count = index.pieces.len() as u32 - first_piece;
        index.tensors.push(TensorEntry {
            name: tensor.name.clone(),
            dtype: tensor.dtype.clone(),
            shape: tensor.shape.clone(),
            byte_len: tensor.byte_len(),
            first_piece,
            piece_count,
        });
        stats.tensors += 1;
        stats.input_bytes += tensor.byte_len();
    }

    let footer = format::encode_index_and_footer(&index, offset)?;
    output.write_all(&footer)?;
    output.flush()?;
    stats.archive_bytes = offset + footer.len() as u64;
    Ok(stats)
}

/// Reads the model description. GGUF is recognized by its magic. Safetensors and
/// ONNX have none, so each is tried in turn, and the errors say why neither fit.
fn read_model<R: Read + Seek>(input: &mut R) -> Result<crate::model::Model> {
    input.seek(SeekFrom::Start(0)).map_err(map_io)?;
    let mut magic = [0u8; 4];
    let read = input.read(&mut magic).map_err(map_io)?;
    input.seek(SeekFrom::Start(0)).map_err(map_io)?;
    if read == 4 && &magic == b"GGUF" {
        return gguf::parse(input);
    }
    match safetensors::parse(input) {
        Ok(model) => Ok(model),
        Err(safetensors_error) => {
            input.seek(SeekFrom::Start(0)).map_err(map_io)?;
            onnx::parse(input).map_err(|onnx_error| {
                Error::InvalidModel(format!(
                    "not a supported model file (safetensors: {safetensors_error}; ONNX: {onnx_error})"
                ))
            })
        }
    }
}
