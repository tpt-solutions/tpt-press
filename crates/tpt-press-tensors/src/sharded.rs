//! Sharded tensor archives: a model split into several `.tptn` shards, and a
//! manifest that says which tensors each shard holds.
//!
//! Each shard is a complete tensor archive, so it can be opened on its own. A
//! loader reads the small manifest first, then fetches only the shards it needs.
//! That suits models too big for one file, and sources such as object stores,
//! where each shard can be a separate range-readable object.
//!
//! The manifest is JSON:
//!
//! ```json
//! {"format": "tpt-press-shards", "version": 1,
//!  "metadata": {"key": "value"},
//!  "shards": [{"file": "shard-000.tptn", "tensors": ["a", "b"]}]}
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, Write};

use serde_json::{json, Map, Value};

use tpt_press_core::codec;

use crate::error::{Error, Result};
use crate::model::Model;
use crate::writer::{read_model, write_archive, PackOptions, Stats};

/// The `format` field of a manifest.
pub const MANIFEST_FORMAT: &str = "tpt-press-shards";

/// The `version` field of a manifest. Only this version is read.
pub const MANIFEST_VERSION: u64 = 1;

/// Name of the manifest file inside a sharded archive's directory.
pub const MANIFEST_FILE: &str = "manifest.json";

/// One shard: its file name, and the tensors it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shard {
    /// File name, relative to the manifest's directory.
    pub file: String,
    /// Names of the tensors in this shard, in archive order.
    pub tensors: Vec<String>,
}

/// The description of a sharded archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Metadata carried over from the source model.
    pub metadata: BTreeMap<String, String>,
    /// The shards, in the order they were written.
    pub shards: Vec<Shard>,
}

impl Manifest {
    /// Parses and checks a manifest. The manifest may come from an untrusted
    /// source, so shard file names must be plain names, and a tensor may appear
    /// in only one shard.
    pub fn parse(text: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(text).map_err(manifest_error)?;
        let object = value
            .as_object()
            .ok_or_else(|| manifest_error("the manifest is not a JSON object"))?;
        if object.get("format").and_then(Value::as_str) != Some(MANIFEST_FORMAT) {
            return Err(manifest_error("not a tpt-press shard manifest"));
        }
        if object.get("version").and_then(Value::as_u64) != Some(MANIFEST_VERSION) {
            return Err(manifest_error("unsupported manifest version"));
        }

        let mut metadata = BTreeMap::new();
        if let Some(entries) = object.get("metadata").and_then(Value::as_object) {
            for (key, value) in entries {
                let value = value
                    .as_str()
                    .ok_or_else(|| manifest_error("metadata values must be strings"))?;
                metadata.insert(key.clone(), value.to_owned());
            }
        }

        let entries = object
            .get("shards")
            .and_then(Value::as_array)
            .ok_or_else(|| manifest_error("no shard list"))?;
        let mut shards = Vec::with_capacity(entries.len());
        let mut seen = BTreeSet::new();
        for entry in entries {
            let file = entry
                .get("file")
                .and_then(Value::as_str)
                .ok_or_else(|| manifest_error("a shard has no file name"))?;
            validate_file_name(file)?;
            let tensors = entry
                .get("tensors")
                .and_then(Value::as_array)
                .ok_or_else(|| manifest_error("a shard has no tensor list"))?
                .iter()
                .map(|t| {
                    let name = t
                        .as_str()
                        .ok_or_else(|| manifest_error("tensor names must be strings"))?;
                    if !seen.insert(name.to_owned()) {
                        return Err(manifest_error(format!("tensor `{name}` is listed twice")));
                    }
                    Ok(name.to_owned())
                })
                .collect::<Result<Vec<_>>>()?;
            shards.push(Shard {
                file: file.to_owned(),
                tensors,
            });
        }
        Ok(Self { metadata, shards })
    }

    /// The manifest as pretty-printed JSON.
    pub fn to_json(&self) -> String {
        let shards: Vec<Value> = self
            .shards
            .iter()
            .map(|s| json!({"file": s.file, "tensors": s.tensors}))
            .collect();
        let metadata: Map<String, Value> = self
            .metadata
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        let value = json!({
            "format": MANIFEST_FORMAT,
            "version": MANIFEST_VERSION,
            "metadata": metadata,
            "shards": shards,
        });
        serde_json::to_string_pretty(&value).expect("a JSON value always serialises")
    }

    /// Number of tensors across all shards.
    pub fn tensor_count(&self) -> usize {
        self.shards.iter().map(|s| s.tensors.len()).sum()
    }
}

/// Shard file names must be plain: no directories, no drive letters, no `..`.
fn validate_file_name(file: &str) -> Result<()> {
    let plain = !file.is_empty()
        && file != "."
        && file != ".."
        && !file.contains(['/', '\\', ':'])
        && !file.chars().any(char::is_control);
    if plain {
        Ok(())
    } else {
        Err(manifest_error(format!("unsafe shard file name `{file}`")))
    }
}

fn manifest_error(message: impl std::fmt::Display) -> Error {
    Error::InvalidModel(format!("shard manifest: {message}"))
}

/// Packs a model into shards of at most `shard_bytes` of tensor data each. A
/// tensor larger than that gets a shard to itself. Tensors keep their name order.
///
/// `open_shard` is called with each shard's file name, and returns where to
/// write it, so the caller decides whether shards go to files, memory or a
/// network. The manifest is returned for the caller to store.
pub fn pack_sharded<R, W, F>(
    mut input: R,
    shard_bytes: u64,
    opts: &PackOptions,
    mut open_shard: F,
) -> Result<(Manifest, Vec<Stats>)>
where
    R: Read + Seek,
    W: Write,
    F: FnMut(&str) -> std::io::Result<W>,
{
    if shard_bytes == 0 {
        return Err(Error::InvalidOption("shard size must be at least one byte"));
    }
    codec::validate_level(opts.level)?;
    let model = read_model(&mut input)?;

    let mut stats = Vec::new();
    let mut shards = Vec::new();
    for (index, group) in group_tensors(&model, shard_bytes).into_iter().enumerate() {
        let file = format!("shard-{index:03}.tptn");
        let part = Model {
            metadata: model.metadata.clone(),
            tensors: group.clone(),
            data_start: model.data_start,
            data_len: model.data_len,
        };
        let output = open_shard(&file)?;
        stats.push(write_archive(&part, &mut input, output, opts, None)?);
        shards.push(Shard {
            file,
            tensors: group.into_iter().map(|t| t.name).collect(),
        });
    }

    Ok((
        Manifest {
            metadata: model.metadata,
            shards,
        },
        stats,
    ))
}

/// Splits the tensors into groups whose data is at most `shard_bytes`, except
/// that a single tensor always fits in its own group.
fn group_tensors(model: &Model, shard_bytes: u64) -> Vec<Vec<crate::model::TensorInfo>> {
    let mut groups: Vec<Vec<crate::model::TensorInfo>> = Vec::new();
    let mut current: Vec<crate::model::TensorInfo> = Vec::new();
    let mut current_bytes = 0u64;
    for tensor in &model.tensors {
        let len = tensor.byte_len();
        if !current.is_empty() && current_bytes + len > shard_bytes {
            groups.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current.push(tensor.clone());
        current_bytes += len;
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io::{Cursor, Write};
    use std::rc::Rc;

    use super::*;

    /// A safetensors file with `count` U8 tensors of `len` bytes each.
    fn model_file(count: usize, len: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
        let mut header = String::from("{");
        let mut data = Vec::new();
        let mut payloads = Vec::new();
        for i in 0..count {
            let bytes: Vec<u8> = (0..len).map(|j| ((i * 31 + j) % 251) as u8).collect();
            if i > 0 {
                header.push(',');
            }
            header.push_str(&format!(
                "\"t{i:02}\":{{\"dtype\":\"U8\",\"shape\":[{len}],\"data_offsets\":[{},{}]}}",
                data.len(),
                data.len() + len
            ));
            data.extend_from_slice(&bytes);
            payloads.push(bytes);
        }
        header.push('}');
        let mut out = (header.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(&data);
        (out, payloads)
    }

    /// A writer whose bytes can be read back after it is dropped.
    #[derive(Clone, Default)]
    struct Shared(Rc<RefCell<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn shards_hold_their_tensors_and_read_back() {
        let (file, payloads) = model_file(5, 3000);
        let outputs: RefCell<Vec<(String, Shared)>> = RefCell::new(Vec::new());
        let (manifest, stats) =
            pack_sharded(Cursor::new(&file), 7000, &PackOptions::default(), |name| {
                let shared = Shared::default();
                outputs.borrow_mut().push((name.to_owned(), shared.clone()));
                Ok(shared)
            })
            .unwrap();

        // Two tensors fit in 7000 bytes, so five tensors make three shards.
        let sizes: Vec<usize> = manifest.shards.iter().map(|s| s.tensors.len()).collect();
        assert_eq!(sizes, [2, 2, 1]);
        assert_eq!(stats.len(), 3);
        assert_eq!(manifest.tensor_count(), 5);

        // Every shard is a complete archive holding exactly the listed tensors.
        for (shard, (name, bytes)) in manifest.shards.iter().zip(outputs.borrow().iter()) {
            assert_eq!(&shard.file, name);
            let bytes = bytes.0.borrow().clone();
            let mut archive = crate::Archive::open(Cursor::new(bytes)).unwrap();
            archive.verify().unwrap();
            for tensor in &shard.tensors {
                let i: usize = tensor[1..].parse().unwrap();
                assert_eq!(archive.read_tensor(tensor).unwrap(), payloads[i]);
            }
        }
    }

    #[test]
    fn manifest_round_trips_through_json() {
        let manifest = Manifest {
            metadata: BTreeMap::from([("source".to_owned(), "test".to_owned())]),
            shards: vec![Shard {
                file: "shard-000.tptn".to_owned(),
                tensors: vec!["a".to_owned(), "b".to_owned()],
            }],
        };
        assert_eq!(Manifest::parse(&manifest.to_json()).unwrap(), manifest);
    }

    #[test]
    fn unsafe_manifests_are_rejected() {
        let traversal = r#"{"format":"tpt-press-shards","version":1,
            "shards":[{"file":"../../etc/passwd","tensors":["a"]}]}"#;
        assert!(Manifest::parse(traversal).is_err());

        let absolute = r#"{"format":"tpt-press-shards","version":1,
            "shards":[{"file":"/tmp/x.tptn","tensors":["a"]}]}"#;
        assert!(Manifest::parse(absolute).is_err());

        let duplicate = r#"{"format":"tpt-press-shards","version":1,
            "shards":[{"file":"a.tptn","tensors":["t"]},{"file":"b.tptn","tensors":["t"]}]}"#;
        assert!(Manifest::parse(duplicate).is_err());

        let wrong_format = r#"{"format":"something-else","version":1,"shards":[]}"#;
        assert!(Manifest::parse(wrong_format).is_err());
    }

    #[test]
    fn zero_shard_size_is_rejected() {
        let (file, _) = model_file(1, 8);
        let result = pack_sharded(Cursor::new(&file), 0, &PackOptions::default(), |_| {
            Ok(Vec::<u8>::new())
        });
        assert!(matches!(result, Err(Error::InvalidOption(_))));
    }
}
