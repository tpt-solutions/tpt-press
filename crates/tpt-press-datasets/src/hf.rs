//! Imports a Hugging Face-style folder into a dataset archive.
//!
//! The folder holds `metadata.jsonl`, with one JSON object per line. Each object
//! has a `file_name` field, a path relative to the folder. The other fields are
//! that row's metadata. Each row becomes one sample:
//!
//! - the key is the file path without its extension, so `img/0001.png` gives
//!   `img/0001`;
//! - the file's bytes are the member named by its extension, such as `png`;
//! - the row's other fields, as JSON, are the member `metadata`.
//!
//! Paths must stay inside the folder. Absolute paths and `..` are refused.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path};

use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::writer::{Member, Sample, Stats, Writer};

/// The metadata file that lists the folder's rows.
pub const METADATA_FILE: &str = "metadata.jsonl";

/// Reads `dir/metadata.jsonl` and its files, and writes one archive.
pub fn import_hf<W: Write>(dir: &Path, output: W, level: i32) -> Result<Stats> {
    let file = fs::File::open(dir.join(METADATA_FILE))
        .map_err(|e| Error::InvalidInput(format!("cannot read {METADATA_FILE}: {e}")))?;
    let mut writer = Writer::new(output, level)?;

    for (number, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let row = row_sample(dir, &line).map_err(|e| {
            Error::InvalidInput(format!("{METADATA_FILE} line {}: {e}", number + 1))
        })?;
        writer.push(row)?;
    }
    writer.finish()
}

/// Builds the sample for one metadata line.
fn row_sample(dir: &Path, line: &str) -> std::result::Result<Sample, String> {
    let mut fields: Map<String, Value> =
        serde_json::from_str(line).map_err(|e| format!("not a JSON object: {e}"))?;
    let file_name = match fields.remove("file_name") {
        Some(Value::String(name)) => name,
        _ => return Err("the row has no file_name string".to_owned()),
    };
    let relative = safe_relative(&file_name)?;

    let (stem, extension) = match file_name.rsplit_once('.') {
        Some((stem, ext)) if !ext.is_empty() && !ext.contains('/') => (stem, ext),
        _ => return Err(format!("{file_name:?} has no extension to name its member")),
    };
    let data =
        fs::read(dir.join(relative)).map_err(|e| format!("cannot read {file_name:?}: {e}"))?;
    let metadata = serde_json::to_vec(&Value::Object(fields)).map_err(|e| e.to_string())?;

    Ok(Sample {
        key: stem.to_owned(),
        members: vec![
            Member {
                name: extension.to_owned(),
                data,
            },
            Member {
                name: "metadata".to_owned(),
                data: metadata,
            },
        ],
    })
}

/// Returns the path if it is relative and stays inside the folder.
fn safe_relative(name: &str) -> std::result::Result<&Path, String> {
    let path = Path::new(name);
    let inside = path.components().all(|c| matches!(c, Component::Normal(_)));
    if !inside || name.is_empty() {
        return Err(format!(
            "{name:?} must be a relative path inside the folder"
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_paths_that_leave_the_folder() {
        assert!(safe_relative("img/0001.png").is_ok());
        assert!(safe_relative("../secret.png").is_err());
        assert!(safe_relative("/etc/passwd.png").is_err());
        assert!(safe_relative("").is_err());
    }
}
