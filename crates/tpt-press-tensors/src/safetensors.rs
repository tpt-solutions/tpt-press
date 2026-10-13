//! Reads and writes the safetensors format.
//!
//! A safetensors file is an 8-byte little-endian header length, a JSON header of
//! that length, and then the raw tensor data. The header maps each tensor name to
//! its dtype, shape, and byte range inside the data, and it may carry string
//! metadata under `__metadata__`.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

use serde_json::{json, Map, Value};

use crate::dtype;
use crate::error::{map_io, Error, Result};

pub use crate::model::{Model, TensorInfo};

/// Largest JSON header accepted. This is the limit the safetensors format sets.
pub const MAX_HEADER_LEN: u64 = 100 * 1024 * 1024;

/// Key holding string metadata in the header.
const METADATA_KEY: &str = "__metadata__";

/// Size in bytes of one element of a plain (non-quantized) `dtype`.
pub fn element_size(dtype: &str) -> Result<u64> {
    match dtype::layout(dtype) {
        Some(layout) if layout.block_elements == 1 => Ok(layout.block_bytes),
        _ => Err(Error::UnsupportedDtype(dtype.to_owned())),
    }
}

/// Parses a safetensors file and checks every tensor's range against its shape and dtype.
pub fn parse<R: Read + Seek>(r: &mut R) -> Result<Model> {
    r.seek(SeekFrom::Start(0)).map_err(map_io)?;
    let mut len_bytes = [0u8; 8];
    r.read_exact(&mut len_bytes).map_err(map_io)?;
    let header_len = u64::from_le_bytes(len_bytes);
    if header_len == 0 || header_len > MAX_HEADER_LEN {
        return Err(invalid("header length is out of range"));
    }

    let mut header = vec![0u8; header_len as usize];
    r.read_exact(&mut header).map_err(map_io)?;
    let value: Value = serde_json::from_slice(&header).map_err(|e| invalid(&e.to_string()))?;
    let Value::Object(object) = value else {
        return Err(invalid("header is not a JSON object"));
    };

    let file_len = r.seek(SeekFrom::End(0)).map_err(map_io)?;
    let data_start = 8 + header_len;
    let data_len = file_len
        .checked_sub(data_start)
        .ok_or_else(|| invalid("file is shorter than its header"))?;

    let mut metadata = BTreeMap::new();
    let mut tensors = Vec::new();
    for (name, entry) in object {
        if name == METADATA_KEY {
            let Value::Object(pairs) = entry else {
                return Err(invalid("__metadata__ is not an object"));
            };
            for (key, value) in pairs {
                let Value::String(text) = value else {
                    return Err(invalid("__metadata__ values must be strings"));
                };
                metadata.insert(key, text);
            }
            continue;
        }
        tensors.push(parse_tensor(name, &entry, data_len)?);
    }
    tensors.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Model {
        metadata,
        tensors,
        data_start,
        data_len,
    })
}

fn parse_tensor(name: String, entry: &Value, data_len: u64) -> Result<TensorInfo> {
    let dtype = entry
        .get("dtype")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("tensor `{name}` has no dtype")))?
        .to_owned();
    let shape: Vec<u64> = entry
        .get("shape")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(&format!("tensor `{name}` has no shape")))?
        .iter()
        .map(|d| {
            d.as_u64()
                .ok_or_else(|| invalid(&format!("tensor `{name}` has a bad dimension")))
        })
        .collect::<Result<_>>()?;
    let offsets = entry
        .get("data_offsets")
        .and_then(Value::as_array)
        .filter(|o| o.len() == 2)
        .ok_or_else(|| invalid(&format!("tensor `{name}` has no data_offsets")))?;
    let begin = offsets[0]
        .as_u64()
        .ok_or_else(|| invalid(&format!("tensor `{name}` has a bad offset")))?;
    let end = offsets[1]
        .as_u64()
        .ok_or_else(|| invalid(&format!("tensor `{name}` has a bad offset")))?;

    let elements = shape
        .iter()
        .try_fold(1u64, |acc, &d| acc.checked_mul(d))
        .ok_or_else(|| invalid(&format!("tensor `{name}` is too large")))?;
    let expected = elements
        .checked_mul(element_size(&dtype)?)
        .ok_or_else(|| invalid(&format!("tensor `{name}` is too large")))?;
    if end < begin || end > data_len {
        return Err(invalid(&format!(
            "tensor `{name}` lies outside the data section"
        )));
    }
    if end - begin != expected {
        return Err(invalid(&format!(
            "tensor `{name}` has {} bytes but its shape and dtype need {expected}",
            end - begin
        )));
    }

    Ok(TensorInfo {
        name,
        dtype,
        shape,
        begin,
        end,
    })
}

/// Builds a safetensors header for tensors laid out back to back, in the given order.
///
/// The JSON is padded with spaces so that the data section starts on an
/// 8-byte boundary, which safetensors readers expect. Returns the length
/// prefix and the JSON together.
pub fn build_header(
    tensors: &[TensorInfo],
    metadata: &BTreeMap<String, String>,
) -> Result<Vec<u8>> {
    let mut object = Map::new();
    if !metadata.is_empty() {
        let pairs: Map<String, Value> = metadata
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        object.insert(METADATA_KEY.to_owned(), Value::Object(pairs));
    }
    for t in tensors {
        object.insert(
            t.name.clone(),
            json!({
                "dtype": t.dtype,
                "shape": t.shape,
                "data_offsets": [t.begin, t.end],
            }),
        );
    }

    let mut json =
        serde_json::to_vec(&Value::Object(object)).map_err(|e| invalid(&e.to_string()))?;
    while (json.len() + 8) % 8 != 0 {
        json.push(b' ');
    }

    let mut out = Vec::with_capacity(8 + json.len());
    out.extend_from_slice(&(json.len() as u64).to_le_bytes());
    out.extend_from_slice(&json);
    Ok(out)
}

fn invalid(detail: &str) -> Error {
    Error::Safetensors(detail.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn file(header: &str, data: &[u8]) -> Vec<u8> {
        let mut out = (header.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn parses_tensors_and_metadata() {
        let bytes = file(
            r#"{"__metadata__":{"format":"pt"},"w":{"dtype":"F32","shape":[2,2],"data_offsets":[0,16]},"b":{"dtype":"U8","shape":[3],"data_offsets":[16,19]}}"#,
            &[0u8; 19],
        );
        let model = parse(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(model.metadata.get("format").map(String::as_str), Some("pt"));
        assert_eq!(model.tensors.len(), 2);
        assert_eq!(model.tensors[0].name, "b");
        assert_eq!(model.tensors[1].element_count(), 4);
        assert_eq!(model.data_len, 19);
    }

    #[test]
    fn rejects_size_that_does_not_match_shape() {
        let bytes = file(
            r#"{"w":{"dtype":"F32","shape":[2],"data_offsets":[0,4]}}"#,
            &[0u8; 4],
        );
        assert!(matches!(
            parse(&mut Cursor::new(bytes)),
            Err(Error::Safetensors(_))
        ));
    }

    #[test]
    fn rejects_offsets_past_the_data() {
        let bytes = file(
            r#"{"w":{"dtype":"U8","shape":[8],"data_offsets":[0,8]}}"#,
            &[0u8; 4],
        );
        assert!(parse(&mut Cursor::new(bytes)).is_err());
    }

    #[test]
    fn rejects_unknown_dtype() {
        let bytes = file(
            r#"{"w":{"dtype":"Q7","shape":[1],"data_offsets":[0,1]}}"#,
            &[0u8; 1],
        );
        assert!(matches!(
            parse(&mut Cursor::new(bytes)),
            Err(Error::UnsupportedDtype(_))
        ));
    }

    #[test]
    fn built_header_parses_back() {
        let tensors = vec![TensorInfo {
            name: "x".to_owned(),
            dtype: "F16".to_owned(),
            shape: vec![3],
            begin: 0,
            end: 6,
        }];
        let mut metadata = BTreeMap::new();
        metadata.insert("k".to_owned(), "v".to_owned());
        let mut bytes = build_header(&tensors, &metadata).unwrap();
        assert_eq!(bytes.len() % 8, 0);
        bytes.extend_from_slice(&[0u8; 6]);
        let model = parse(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(model.tensors, tensors);
        assert_eq!(model.metadata, metadata);
    }
}
