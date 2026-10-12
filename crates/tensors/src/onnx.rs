//! Reads the weights of an ONNX model.
//!
//! An ONNX file is a protocol buffer, `ModelProto`. Its weights are the graph's
//! initializers, `TensorProto` messages. This reader walks the protobuf wire
//! format directly, so it needs no code generation. It never reads weight bytes
//! while parsing: it records where each `raw_data` payload sits in the file, and
//! the archive packer reads them from there, the same way it reads safetensors.
//!
//! Only initializers stored inline (`raw_data`) are supported. Initializers
//! stored in external files, or as typed `*_data` fields, are rejected with an error.

use std::io::{Read, Seek, SeekFrom};

use crate::dtype;
use crate::error::{map_io, Error, Result};
use crate::model::{Model, TensorInfo};

/// `ModelProto` fields.
const MODEL_PRODUCER_NAME: u32 = 2;
const MODEL_PRODUCER_VERSION: u32 = 3;
const MODEL_GRAPH: u32 = 7;
const MODEL_VERSION: u32 = 5;

/// `GraphProto` fields.
const GRAPH_NAME: u32 = 2;
const GRAPH_INITIALIZER: u32 = 5;

/// `TensorProto` fields.
const TENSOR_DIMS: u32 = 1;
const TENSOR_DATA_TYPE: u32 = 2;
const TENSOR_NAME: u32 = 8;
const TENSOR_RAW_DATA: u32 = 9;
const TENSOR_DATA_LOCATION: u32 = 14;

/// `TensorProto.DataLocation.EXTERNAL`.
const LOCATION_EXTERNAL: u64 = 1;

/// Longest string field read into memory, in bytes.
const MAX_STRING: u64 = 64 * 1024;

/// One field of a protobuf message. Length-delimited fields are not read here,
/// because weights can be large. The caller reads them with `read_bytes` if needed.
#[derive(Debug, Clone, Copy)]
enum Field {
    Varint(u64),
    Fixed32,
    Fixed64,
    /// A length-delimited payload at `offset`, `len` bytes long.
    Bytes {
        offset: u64,
        len: u64,
    },
}

/// Parses an ONNX file and returns its initializers as tensors.
pub fn parse<R: Read + Seek>(r: &mut R) -> Result<Model> {
    let file_len = r.seek(SeekFrom::End(0)).map_err(map_io)?;

    let mut metadata = std::collections::BTreeMap::new();
    let mut tensors = Vec::new();
    walk(
        r,
        0,
        file_len,
        &mut |r, field, value| match (field, value) {
            (MODEL_PRODUCER_NAME, Field::Bytes { offset, len }) => {
                metadata.insert("onnx.producer_name".to_owned(), read_text(r, offset, len)?);
                Ok(())
            }
            (MODEL_PRODUCER_VERSION, Field::Bytes { offset, len }) => {
                metadata.insert(
                    "onnx.producer_version".to_owned(),
                    read_text(r, offset, len)?,
                );
                Ok(())
            }
            (MODEL_VERSION, Field::Varint(v)) => {
                metadata.insert("onnx.model_version".to_owned(), v.to_string());
                Ok(())
            }
            (MODEL_GRAPH, Field::Bytes { offset, len }) => walk(
                r,
                offset,
                offset + len,
                &mut |r, field, value| match (field, value) {
                    (GRAPH_NAME, Field::Bytes { offset, len }) => {
                        metadata.insert("onnx.graph_name".to_owned(), read_text(r, offset, len)?);
                        Ok(())
                    }
                    (GRAPH_INITIALIZER, Field::Bytes { offset, len }) => {
                        tensors.push(parse_tensor(r, offset, len, file_len)?);
                        Ok(())
                    }
                    _ => Ok(()),
                },
            ),
            _ => Ok(()),
        },
    )?;

    tensors.sort_by(|a, b| a.name.cmp(&b.name));
    if tensors.windows(2).any(|w| w[0].name == w[1].name) {
        return Err(invalid("two initializers share a name"));
    }
    Ok(Model {
        metadata,
        tensors,
        data_start: 0,
        data_len: file_len,
    })
}

/// Reads one `TensorProto`. Its `raw_data` bytes are located, not read.
fn parse_tensor<R: Read + Seek>(
    r: &mut R,
    start: u64,
    len: u64,
    file_len: u64,
) -> Result<TensorInfo> {
    let mut name = None;
    let mut data_type = None;
    let mut dims = Vec::new();
    let mut raw = None;
    let mut external = false;

    walk(
        r,
        start,
        start + len,
        &mut |r, field, value| match (field, value) {
            (TENSOR_DIMS, Field::Varint(d)) => {
                dims.push(d);
                Ok(())
            }
            (TENSOR_DIMS, Field::Bytes { offset, len }) => {
                // Packed dims: a run of varints.
                let mut pos = offset;
                while pos < offset + len {
                    let (d, next) = read_varint_at(r, pos, offset + len)?;
                    dims.push(d);
                    pos = next;
                }
                Ok(())
            }
            (TENSOR_DATA_TYPE, Field::Varint(t)) => {
                data_type = Some(t);
                Ok(())
            }
            (TENSOR_NAME, Field::Bytes { offset, len }) => {
                name = Some(read_text(r, offset, len)?);
                Ok(())
            }
            (TENSOR_RAW_DATA, Field::Bytes { offset, len }) => {
                raw = Some((offset, len));
                Ok(())
            }
            (TENSOR_DATA_LOCATION, Field::Varint(loc)) => {
                external = loc == LOCATION_EXTERNAL;
                Ok(())
            }
            _ => Ok(()),
        },
    )?;

    if external {
        return Err(Error::Unsupported("ONNX external data is not supported"));
    }
    let name = name.ok_or_else(|| invalid("an initializer has no name"))?;
    let data_type =
        data_type.ok_or_else(|| invalid(&format!("initializer `{name}` has no type")))?;
    let dtype = onnx_dtype(data_type)?;
    let (offset, raw_len) = raw.ok_or(Error::Unsupported(
        "only initializers stored in raw_data are supported",
    ))?;

    let shape: Vec<u64> = dims;
    let expected = dtype::byte_len(dtype, &shape)?;
    if expected != raw_len {
        return Err(invalid(&format!(
            "initializer `{name}` has {raw_len} bytes but its shape and type need {expected}"
        )));
    }
    if offset.checked_add(raw_len).is_none_or(|end| end > file_len) {
        return Err(invalid(&format!(
            "initializer `{name}` lies outside the file"
        )));
    }

    Ok(TensorInfo {
        name,
        dtype: dtype.to_owned(),
        shape,
        begin: offset,
        end: offset + raw_len,
    })
}

/// Maps an ONNX `TensorProto.DataType` to the dtype name used in archives.
fn onnx_dtype(code: u64) -> Result<&'static str> {
    Ok(match code {
        1 => "F32",
        2 => "U8",
        3 => "I8",
        4 => "U16",
        5 => "I16",
        6 => "I32",
        7 => "I64",
        9 => "BOOL",
        10 => "F16",
        11 => "F64",
        12 => "U32",
        13 => "U64",
        16 => "BF16",
        other => {
            return Err(Error::UnsupportedDtype(format!("ONNX data type {other}")));
        }
    })
}

/// Walks the protobuf fields in `start..end`, calling `f` for each one.
///
/// For a length-delimited field, `f` gets its location, and the reader skips past
/// the payload afterwards. `f` may seek, so each field starts at a known position.
fn walk<R, F>(r: &mut R, start: u64, end: u64, f: &mut F) -> Result<()>
where
    R: Read + Seek,
    F: FnMut(&mut R, u32, Field) -> Result<()>,
{
    let mut pos = start;
    while pos < end {
        let (tag, after_tag) = read_varint_at(r, pos, end)?;
        let field = u32::try_from(tag >> 3).map_err(|_| invalid("field number is too large"))?;
        if field == 0 {
            return Err(invalid("field number 0"));
        }
        let (value, next) = match tag & 7 {
            0 => {
                let (v, next) = read_varint_at(r, after_tag, end)?;
                (Field::Varint(v), next)
            }
            1 => (Field::Fixed64, check_fits(after_tag + 8, end)?),
            2 => {
                let (len, offset) = read_varint_at(r, after_tag, end)?;
                let next = offset
                    .checked_add(len)
                    .filter(|&n| n <= end)
                    .ok_or_else(|| invalid("field runs past the end of its message"))?;
                (Field::Bytes { offset, len }, next)
            }
            5 => (Field::Fixed32, check_fits(after_tag + 4, end)?),
            other => return Err(invalid(&format!("unsupported wire type {other}"))),
        };
        f(r, field, value)?;
        pos = next;
    }
    Ok(())
}

fn check_fits(next: u64, end: u64) -> Result<u64> {
    if next > end {
        Err(invalid("field runs past the end of its message"))
    } else {
        Ok(next)
    }
}

/// Reads a varint at `pos`. Returns the value and the position after it.
fn read_varint_at<R: Read + Seek>(r: &mut R, pos: u64, end: u64) -> Result<(u64, u64)> {
    r.seek(SeekFrom::Start(pos)).map_err(map_io)?;
    let mut value = 0u64;
    let mut i = 0u32;
    loop {
        if pos + u64::from(i) >= end {
            return Err(invalid("varint runs past the end of its message"));
        }
        let mut byte = [0u8; 1];
        r.read_exact(&mut byte).map_err(map_io)?;
        if i >= 10 {
            return Err(invalid("varint is too long"));
        }
        value |= u64::from(byte[0] & 0x7f) << (7 * i);
        i += 1;
        if byte[0] & 0x80 == 0 {
            return Ok((value, pos + u64::from(i)));
        }
    }
}

/// Reads a short string field into memory.
fn read_text<R: Read + Seek>(r: &mut R, offset: u64, len: u64) -> Result<String> {
    if len > MAX_STRING {
        return Err(invalid("string field is too long"));
    }
    r.seek(SeekFrom::Start(offset)).map_err(map_io)?;
    let mut bytes = vec![0u8; len as usize];
    r.read_exact(&mut bytes).map_err(map_io)?;
    String::from_utf8(bytes).map_err(|_| invalid("string field is not UTF-8"))
}

fn invalid(detail: &str) -> Error {
    Error::InvalidModel(format!("ONNX: {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    #[test]
    fn varints_decode() {
        let mut bytes = Vec::new();
        varint(300, &mut bytes);
        let (value, next) =
            read_varint_at(&mut Cursor::new(bytes.clone()), 0, bytes.len() as u64).unwrap();
        assert_eq!(value, 300);
        assert_eq!(next, 2);
    }

    #[test]
    fn unterminated_varint_is_rejected() {
        let bytes = vec![0x80u8, 0x80];
        assert!(read_varint_at(&mut Cursor::new(bytes.clone()), 0, 2).is_err());
    }
}
