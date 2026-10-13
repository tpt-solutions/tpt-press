//! Reads a Zarr v2 array from its directory store.
//!
//! The array is assembled in memory, cropping each edge chunk to the array's
//! real extent, and then packed as any other array. Supported: C order, no
//! filters, and the `null` and `zstd` compressors. Missing chunks take the
//! array's `fill_value`. Other compressors, such as Blosc, are refused with a
//! message naming them.

use std::io::Write;
use std::path::Path;

use serde_json::Value;
use tpt_press_core::codec;

use crate::dtype::Dtype;
use crate::error::{Error, Result};
use crate::layout::runs;
use crate::writer::{pack_array, PackOptions, Stats};

/// Packs the Zarr v2 array in `dir`, which holds a `.zarray` file.
pub fn pack_zarr<W: Write>(dir: &Path, output: W, options: &PackOptions) -> Result<Stats> {
    let meta_path = dir.join(".zarray");
    let text = std::fs::read_to_string(&meta_path).map_err(|e| Error::Format {
        message: format!("cannot read {}: {e}", meta_path.display()),
    })?;
    let meta: Value = serde_json::from_str(&text).map_err(|e| Error::Format {
        message: format!("{} is not valid JSON: {e}", meta_path.display()),
    })?;

    if meta["zarr_format"].as_u64() != Some(2) {
        return Err(Error::Unsupported {
            message: "only Zarr format 2 arrays are supported",
        });
    }
    if let Some(order) = meta.get("order").and_then(Value::as_str) {
        if order != "C" {
            return Err(Error::Unsupported {
                message: "only C-ordered Zarr arrays are supported",
            });
        }
    }
    if !meta["filters"].is_null() {
        return Err(Error::Unsupported {
            message: "Zarr filters are not supported",
        });
    }

    let dtype = Dtype::parse(
        meta["dtype"]
            .as_str()
            .ok_or_else(|| format_error("the .zarray dtype is missing"))?,
    )?;
    let shape = u64_list(&meta["shape"], "shape")?;
    let chunk = u64_list(&meta["chunks"], "chunks")?;
    if shape.len() != chunk.len() || shape.is_empty() {
        return Err(format_error("the .zarray shape and chunks disagree"));
    }
    if chunk.contains(&0) {
        return Err(format_error("a Zarr chunk length is zero"));
    }
    let separator = meta["dimension_separator"].as_str().unwrap_or(".");
    let compressor = compressor_of(&meta["compressor"])?;

    let elem = dtype.size();
    let total = shape.iter().product::<u64>() as usize * elem;
    let mut data = vec![0u8; total];
    let fill = fill_element(&dtype, &meta["fill_value"])?;
    if fill.iter().any(|&b| b != 0) {
        for element in data.chunks_exact_mut(elem) {
            element.copy_from_slice(&fill);
        }
    }

    let grid: Vec<u64> = shape
        .iter()
        .zip(&chunk)
        .map(|(&s, &c)| s.div_ceil(c))
        .collect();
    let chunk_elements: u64 = chunk.iter().product();
    let chunk_bytes = chunk_elements as usize * elem;
    let chunk_origin = vec![0u64; chunk.len()];
    for linear in 0..grid.iter().product::<u64>() {
        let index = unravel(linear, &grid);
        let origin: Vec<u64> = index.iter().zip(&chunk).map(|(&i, &c)| i * c).collect();
        let extent: Vec<u64> = origin
            .iter()
            .zip(&shape)
            .zip(&chunk)
            .map(|((&o, &s), &c)| c.min(s - o))
            .collect();
        let name = index
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(separator);
        let path = dir.join(&name);
        if !path.exists() {
            continue;
        }
        let stored = std::fs::read(&path).map_err(|e| Error::Format {
            message: format!("cannot read chunk {name}: {e}"),
        })?;
        let decoded = decompress(&stored, compressor.as_deref(), chunk_bytes, &name)?;
        let from = runs(&chunk, &chunk_origin, &extent, elem);
        let into = runs(&shape, &origin, &extent, elem);
        for ((src, len), (dst, _)) in from.into_iter().zip(into) {
            data[dst..dst + len].copy_from_slice(&decoded[src..src + len]);
        }
    }

    pack_array(Vec::new(), dtype, shape, &data, options, output)
}

fn format_error(message: &str) -> Error {
    Error::Format {
        message: message.to_owned(),
    }
}

/// The compressor's id, or `None` for no compression.
fn compressor_of(value: &Value) -> Result<Option<String>> {
    match value {
        Value::Null => Ok(None),
        Value::Object(map) => {
            let id = map
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| format_error("the .zarray compressor has no id"))?;
            match id {
                "zstd" => Ok(Some(id.to_owned())),
                other => Err(Error::Format {
                    message: format!(
                        "Zarr compressor `{other}` is not supported; null and zstd are"
                    ),
                }),
            }
        }
        _ => Err(format_error("the .zarray compressor is malformed")),
    }
}

fn decompress(
    stored: &[u8],
    compressor: Option<&str>,
    expected: usize,
    name: &str,
) -> Result<Vec<u8>> {
    let raw = match compressor {
        None => stored.to_vec(),
        Some(_) => codec::decompress(stored, expected)?,
    };
    if raw.len() != expected {
        return Err(Error::Format {
            message: format!("chunk {name} has {} bytes; expected {expected}", raw.len()),
        });
    }
    Ok(raw)
}

/// The bytes of the fill value, which stand in for missing chunks.
fn fill_element(dtype: &Dtype, fill: &Value) -> Result<Vec<u8>> {
    let size = dtype.size();
    let kind = dtype.descr().as_bytes().get(1).copied().unwrap_or(b'?');
    let bytes = match (kind, fill) {
        (_, Value::Null) => vec![0u8; size],
        (b'b', Value::Bool(b)) => vec![u8::from(*b)],
        (b'f', Value::Number(n)) => float_bytes(n.as_f64().unwrap_or(0.0), size)?,
        (b'f', Value::String(s)) => {
            let v = match s.as_str() {
                "NaN" => f64::NAN,
                "Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                _ => return Err(format_error("the Zarr fill value is not a number")),
            };
            float_bytes(v, size)?
        }
        (b'i' | b'u', Value::Number(n)) => {
            let v = n
                .as_i64()
                .map(|i| i as u64)
                .or_else(|| n.as_u64())
                .ok_or_else(|| format_error("the Zarr fill value is not an integer"))?;
            v.to_le_bytes()[..size].to_vec()
        }
        _ => return Err(format_error("the Zarr fill value does not match the dtype")),
    };
    Ok(bytes)
}

fn float_bytes(v: f64, size: usize) -> Result<Vec<u8>> {
    match size {
        8 => Ok(v.to_le_bytes().to_vec()),
        4 => Ok((v as f32).to_le_bytes().to_vec()),
        _ => Err(Error::Unsupported {
            message: "Zarr fill values are supported for 4- and 8-byte floats",
        }),
    }
}

fn u64_list(value: &Value, what: &str) -> Result<Vec<u64>> {
    value
        .as_array()
        .ok_or_else(|| format_error(&format!("the .zarray {what} is not a list")))?
        .iter()
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| format_error(&format!("the .zarray {what} has a bad entry")))
        })
        .collect()
}

/// Splits a linear index into a multi-index over `grid`, in C order.
fn unravel(mut linear: u64, grid: &[u64]) -> Vec<u64> {
    let mut index = vec![0u64; grid.len()];
    for d in (0..grid.len()).rev() {
        index[d] = linear % grid[d];
        linear /= grid[d];
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::unpack;
    use std::io::Cursor;

    fn write_array(dir: &Path, meta: &str, chunks: &[(&str, Vec<u8>)]) {
        std::fs::write(dir.join(".zarray"), meta).unwrap();
        for (name, bytes) in chunks {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
    }

    fn i16s(values: &[i16]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn edge_chunks_are_cropped_and_missing_chunks_take_the_fill() {
        // A 3 x 5 array in 2 x 3 chunks: the chunk grid is 2 x 2, with edge chunks.
        let dir = tempfile::tempdir().unwrap();
        let meta = r#"{"zarr_format": 2, "shape": [3, 5], "chunks": [2, 3],
            "dtype": "<i2", "compressor": null, "fill_value": 9,
            "order": "C", "filters": null}"#;
        let chunk00 = i16s(&[1, 2, 3, 4, 5, 6]);
        // Chunk "1.1" covers rows 2..4 and columns 3..6, so only one row and two columns exist.
        // Its stored bytes are the full 2 x 3 chunk, with padding.
        let chunk11 = i16s(&[7, 8, 0, 0, 0, 0]);
        write_array(dir.path(), meta, &[("0.0", chunk00), ("1.1", chunk11)]);

        let mut archive = Vec::new();
        pack_zarr(dir.path(), &mut archive, &PackOptions::default()).unwrap();
        let mut out = Vec::new();
        unpack(Cursor::new(archive), &mut out).unwrap();
        // Row 0: 1 2 3 9 9. Row 1: 4 5 6 9 9. Row 2: 9 9 9 7 8.
        assert_eq!(out, i16s(&[1, 2, 3, 9, 9, 4, 5, 6, 9, 9, 9, 9, 9, 7, 8]));
    }

    #[test]
    fn zstd_chunks_decode() {
        let dir = tempfile::tempdir().unwrap();
        let meta = r#"{"zarr_format": 2, "shape": [4], "chunks": [4],
            "dtype": "<u1", "compressor": {"id": "zstd", "level": 3},
            "fill_value": 0, "order": "C", "filters": null}"#;
        let stored = codec::compress(&[10u8, 20, 30, 40], 3).unwrap();
        write_array(dir.path(), meta, &[("0", stored)]);
        let mut archive = Vec::new();
        pack_zarr(dir.path(), &mut archive, &PackOptions::default()).unwrap();
        let mut out = Vec::new();
        unpack(Cursor::new(archive), &mut out).unwrap();
        assert_eq!(out, vec![10, 20, 30, 40]);
    }

    #[test]
    fn blosc_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let meta = r#"{"zarr_format": 2, "shape": [4], "chunks": [4],
            "dtype": "<u1", "compressor": {"id": "blosc", "cname": "lz4"},
            "fill_value": 0, "order": "C", "filters": null}"#;
        write_array(dir.path(), meta, &[]);
        let err = pack_zarr(dir.path(), Vec::new(), &PackOptions::default()).unwrap_err();
        assert!(err.to_string().contains("blosc"), "got {err}");
    }
}
