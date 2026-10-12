//! Reads the GGUF model format (versions 2 and 3).
//!
//! A GGUF file is a header with the tensor and metadata counts, then the
//! metadata key-value pairs, then one directory entry per tensor. The tensor
//! data starts at the first multiple of the file's alignment (32 by default)
//! after the directory. Each tensor's offset counts from that point.
//!
//! Dimensions are kept in GGUF order, where the first dimension varies
//! fastest. Quantized tensors keep their GGML type name, such as `Q8_0`.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

use crate::dtype;
use crate::error::{map_io, Error, Result};
use crate::model::{Model, TensorInfo};

/// Magic bytes at the start of every GGUF file.
pub const MAGIC: [u8; 4] = *b"GGUF";

/// Alignment of the data section when the file does not say otherwise.
const DEFAULT_ALIGNMENT: u64 = 32;

/// Largest string accepted, in bytes.
const MAX_STRING_LEN: u64 = 1024 * 1024;

/// Most tensors accepted.
const MAX_TENSORS: u64 = 1 << 20;

/// Most metadata entries accepted.
const MAX_METADATA: u64 = 1 << 20;

/// Most dimensions accepted for one tensor.
const MAX_DIMS: u32 = 8;

/// Parses a GGUF file's header, metadata and tensor directory.
pub fn parse<R: Read + Seek>(r: &mut R) -> Result<Model> {
    r.seek(SeekFrom::Start(0)).map_err(map_io)?;
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).map_err(map_io)?;
    if magic != MAGIC {
        return Err(invalid("not a GGUF file"));
    }
    let version = read_u32(r)?;
    if version != 2 && version != 3 {
        return Err(invalid(&format!("unsupported GGUF version {version}")));
    }
    let tensor_count = read_u64(r)?;
    let kv_count = read_u64(r)?;
    if tensor_count > MAX_TENSORS || kv_count > MAX_METADATA {
        return Err(invalid(
            "file declares too many tensors or metadata entries",
        ));
    }

    let mut metadata = BTreeMap::new();
    let mut alignment = DEFAULT_ALIGNMENT;
    for _ in 0..kv_count {
        let key = read_string(r)?;
        let value_type = read_u32(r)?;
        let value = read_value(r, value_type)?;
        if key == "general.alignment" {
            alignment = value
                .as_u64()
                .filter(|a| a.is_power_of_two())
                .ok_or_else(|| invalid("general.alignment is not a power of two"))?;
        }
        metadata.insert(key, value.to_text());
    }

    let mut infos = Vec::with_capacity(tensor_count as usize);
    for _ in 0..tensor_count {
        let name = read_string(r)?;
        let dims = read_u32(r)?;
        if dims > MAX_DIMS {
            return Err(invalid(&format!("tensor `{name}` has too many dimensions")));
        }
        let mut shape = Vec::with_capacity(dims as usize);
        for _ in 0..dims {
            shape.push(read_u64(r)?);
        }
        let ggml_type = read_u32(r)?;
        let offset = read_u64(r)?;
        infos.push((name, shape, ggml_type, offset));
    }

    let directory_end = r.stream_position().map_err(map_io)?;
    let file_len = r.seek(SeekFrom::End(0)).map_err(map_io)?;
    let data_start = directory_end.div_ceil(alignment) * alignment;
    let data_len = file_len
        .checked_sub(data_start)
        .ok_or_else(|| invalid("file ends inside the tensor directory"))?;

    let mut tensors = Vec::with_capacity(infos.len());
    for (name, shape, ggml_type, offset) in infos {
        let dtype_name = ggml_dtype(ggml_type)?;
        let byte_len = dtype::byte_len(dtype_name, &shape)?;
        let end = offset
            .checked_add(byte_len)
            .filter(|&end| end <= data_len)
            .ok_or_else(|| invalid(&format!("tensor `{name}` lies outside the data section")))?;
        tensors.push(TensorInfo {
            name,
            dtype: dtype_name.to_owned(),
            shape,
            begin: offset,
            end,
        });
    }
    tensors.sort_by(|a, b| a.name.cmp(&b.name));
    if tensors.windows(2).any(|w| w[0].name == w[1].name) {
        return Err(invalid("two tensors share a name"));
    }

    Ok(Model {
        metadata,
        tensors,
        data_start,
        data_len,
    })
}

/// Name of a GGML type id, for the types this crate can size.
fn ggml_dtype(id: u32) -> Result<&'static str> {
    Ok(match id {
        0 => "F32",
        1 => "F16",
        2 => "Q4_0",
        3 => "Q4_1",
        6 => "Q5_0",
        7 => "Q5_1",
        8 => "Q8_0",
        9 => "Q8_1",
        10 => "Q2_K",
        11 => "Q3_K",
        12 => "Q4_K",
        13 => "Q5_K",
        14 => "Q6_K",
        15 => "Q8_K",
        24 => "I8",
        25 => "I16",
        26 => "I32",
        27 => "I64",
        28 => "F64",
        30 => "BF16",
        other => return Err(Error::UnsupportedDtype(format!("GGML type {other}"))),
    })
}

/// A metadata value, decoded just enough to store as text.
enum Value {
    Text(String),
    Number(String),
    Array(u64),
}

impl Value {
    fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    fn to_text(&self) -> String {
        match self {
            Value::Text(text) | Value::Number(text) => text.clone(),
            Value::Array(count) => format!("[{count} items]"),
        }
    }
}

fn read_value<R: Read + Seek>(r: &mut R, value_type: u32) -> Result<Value> {
    Ok(match value_type {
        0 => Value::Number(read_array::<1, R>(r)?[0].to_string()),
        1 => Value::Number((read_array::<1, R>(r)?[0] as i8).to_string()),
        2 => Value::Number(u16::from_le_bytes(read_array(r)?).to_string()),
        3 => Value::Number(i16::from_le_bytes(read_array(r)?).to_string()),
        4 => Value::Number(u32::from_le_bytes(read_array(r)?).to_string()),
        5 => Value::Number(i32::from_le_bytes(read_array(r)?).to_string()),
        6 => Value::Number(f32::from_le_bytes(read_array(r)?).to_string()),
        7 => Value::Number(
            match read_array::<1, R>(r)?[0] {
                0 => "false",
                _ => "true",
            }
            .to_owned(),
        ),
        8 => Value::Text(read_string(r)?),
        9 => {
            let element_type = read_u32(r)?;
            let count = read_u64(r)?;
            if count > MAX_METADATA * 1024 {
                return Err(invalid("metadata array is too long"));
            }
            for _ in 0..count {
                read_value(r, element_type)?;
            }
            Value::Array(count)
        }
        10 => Value::Number(u64::from_le_bytes(read_array(r)?).to_string()),
        11 => Value::Number(i64::from_le_bytes(read_array(r)?).to_string()),
        12 => Value::Number(f64::from_le_bytes(read_array(r)?).to_string()),
        other => return Err(invalid(&format!("unknown metadata value type {other}"))),
    })
}

fn read_array<const N: usize, R: Read>(r: &mut R) -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    r.read_exact(&mut buf).map_err(map_io)?;
    Ok(buf)
}

fn read_u32<R: Read>(r: &mut R) -> Result<u32> {
    Ok(u32::from_le_bytes(read_array(r)?))
}

fn read_u64<R: Read>(r: &mut R) -> Result<u64> {
    Ok(u64::from_le_bytes(read_array(r)?))
}

fn read_string<R: Read>(r: &mut R) -> Result<String> {
    let len = read_u64(r)?;
    if len > MAX_STRING_LEN {
        return Err(invalid("string is too long"));
    }
    let mut bytes = vec![0u8; len as usize];
    r.read_exact(&mut bytes).map_err(map_io)?;
    String::from_utf8(bytes).map_err(|_| invalid("string is not UTF-8"))
}

fn invalid(detail: &str) -> Error {
    Error::InvalidModel(format!("GGUF: {detail}"))
}
