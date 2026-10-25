//! Imports a WebDataset-style tar file into a dataset archive.
//!
//! WebDataset names each file `<key>.<member>`, where the key is the path up to
//! the first dot of the file name, and the member is the rest of the name. Files
//! that share a key are one sample. Samples are written in the order their keys
//! first change, so a sample's files are expected to sit together in the tar, as
//! WebDataset writes them. Files without a dot are skipped and counted.

use std::io::{Read, Write};

use crate::error::{Error, Result};
use crate::format::MAX_MEMBER_BYTES;
use crate::writer::{Member, Sample, Stats, Writer};

/// Reads a tar file and writes its samples as one archive.
pub fn import_tar<R, W>(input: R, output: W, level: i32) -> Result<Stats>
where
    R: Read,
    W: Write,
{
    let mut writer = Writer::new(output, level)?;
    let mut archive = tar::Archive::new(input);
    let mut current: Option<Sample> = None;

    for entry in archive.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.to_string_lossy().into_owned();
        let Some((key, name)) = split_path(&path) else {
            writer.note_skipped();
            continue;
        };
        if entry.size() > MAX_MEMBER_BYTES {
            return Err(Error::InvalidInput(format!(
                "tar entry {path:?} is larger than 1 GiB"
            )));
        }
        let mut data = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut data)?;

        if current.as_ref().is_some_and(|s| s.key != key) {
            if let Some(done) = current.take() {
                writer.push(done)?;
            }
        }
        let sample = current.get_or_insert_with(|| Sample {
            key: key.clone(),
            members: Vec::new(),
        });
        sample.members.push(Member {
            name: name.to_owned(),
            data,
        });
    }
    if let Some(done) = current.take() {
        writer.push(done)?;
    }
    writer.finish()
}

/// Splits a tar path into its WebDataset key and member name. Returns `None` for
/// files that are not members: no dot in the name, or nothing before the dot.
fn split_path(path: &str) -> Option<(String, &str)> {
    // `tar -cf x.tar .` names files `./dir/file`. The `./` is not part of the key.
    let path = path.strip_prefix("./").unwrap_or(path);
    let (dir, base) = match path.rsplit_once('/') {
        Some((dir, base)) => (Some(dir), base),
        None => (None, path),
    };
    let dot = base.find('.')?;
    if dot == 0 || dot + 1 == base.len() {
        return None;
    }
    let stem = &base[..dot];
    let name = &base[dot + 1..];
    let key = match dir {
        Some(dir) => format!("{dir}/{stem}"),
        None => stem.to_owned(),
    };
    Some((key, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_key_and_member_at_the_first_dot() {
        assert_eq!(
            split_path("shard/000001.jpg"),
            Some(("shard/000001".to_owned(), "jpg"))
        );
        assert_eq!(
            split_path("0001.seg.png"),
            Some(("0001".to_owned(), "seg.png"))
        );
        assert_eq!(
            split_path("./shard/0001.jpg"),
            Some(("shard/0001".to_owned(), "jpg"))
        );
        assert_eq!(split_path("README"), None);
        assert_eq!(split_path(".hidden"), None);
        assert_eq!(split_path("trailing."), None);
    }
}
