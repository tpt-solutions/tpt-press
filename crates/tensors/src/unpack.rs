//! Writes a tensor archive back out as a safetensors file.

use std::collections::BTreeMap;
use std::io::{Read, Seek, Write};

use crate::error::Result;
use crate::reader::Archive;
use crate::safetensors::{self, TensorInfo};

/// Counts from a completed [`unpack`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Tensors written.
    pub tensors: u64,
    /// Tensor bytes written.
    pub data_bytes: u64,
}

/// Writes every tensor in `archive` as a safetensors file to `output`.
///
/// Tensors are laid out in name order, each starting where the last one ends,
/// and the header records their offsets. The result is a valid safetensors file
/// with the same tensors, shapes, dtypes, bytes and metadata. The byte layout
/// can differ from the original file, because the original may have ordered
/// its tensors differently.
pub fn unpack<R, W>(archive: &mut Archive<R>, mut output: W) -> Result<Stats>
where
    R: Read + Seek,
    W: Write,
{
    let mut begin = 0u64;
    let mut layout = Vec::with_capacity(archive.tensors().len());
    for t in archive.tensors() {
        // safetensors cannot hold quantized types, so refuse them before writing anything.
        safetensors::element_size(&t.dtype)?;
        layout.push(TensorInfo {
            name: t.name.clone(),
            dtype: t.dtype.clone(),
            shape: t.shape.clone(),
            begin,
            end: begin + t.byte_len,
        });
        begin += t.byte_len;
    }

    let metadata: BTreeMap<String, String> = archive.metadata().clone();
    output.write_all(&safetensors::build_header(&layout, &metadata)?)?;

    let mut stats = Stats::default();
    for info in &layout {
        archive.read_tensor_into(&info.name, |piece| {
            output.write_all(piece)?;
            Ok(())
        })?;
        stats.tensors += 1;
        stats.data_bytes += info.byte_len();
    }
    output.flush()?;
    Ok(stats)
}
