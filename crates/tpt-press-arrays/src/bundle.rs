//! Bundles of named arrays, for `.npz` files that hold several arrays.
//!
//! Each member is a complete `TPAR` archive, so anything that reads a `TPAR`
//! archive can read a member once it has been copied out with [`read_member`].
//!
//! ```text
//! header : magic "TPBN" | version u16 | reserved u16 | count u32
//! index  : count * (name_len u16 | name bytes | offset u64 | length u64)
//! data   : the member archives, back to back
//! ```
//!
//! Offsets are from the start of the file. Integers are little-endian.

use std::io::{Read, Seek, SeekFrom, Write};

use crate::error::{map_io, Error, Result};

/// Magic bytes at the start of every bundle.
pub const BUNDLE_MAGIC: [u8; 4] = *b"TPBN";
const VERSION: u16 = 1;
const HEADER_LEN: u64 = 12;

/// One member of a bundle, as listed in its index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleEntry {
    /// The array's name, such as `temps` for `temps.npy` in an `.npz` file.
    pub name: String,
    /// Where the member's archive starts in the file.
    pub offset: u64,
    /// The member's archive length in bytes.
    pub length: u64,
}

/// Whether `bytes` starts with the bundle magic.
pub fn is_bundle(bytes: &[u8]) -> bool {
    bytes.starts_with(&BUNDLE_MAGIC)
}

/// Writes a bundle of `(name, archive)` members.
pub fn write_bundle<W: Write>(mut output: W, members: &[(String, Vec<u8>)]) -> Result<u64> {
    let count =
        u32::try_from(members.len()).map_err(|_| Error::InvalidOption("too many members"))?;
    let mut index = Vec::new();
    for (name, _) in members {
        let name_len = u16::try_from(name.len())
            .map_err(|_| Error::InvalidOption("member name is too long"))?;
        index.extend_from_slice(&name_len.to_le_bytes());
        index.extend_from_slice(name.as_bytes());
        // The offsets are filled in below, once the index length is known.
        index.extend_from_slice(&[0u8; 16]);
    }

    let mut data_at = HEADER_LEN + index.len() as u64;
    let mut position = 0usize;
    for (name, archive) in members {
        let offset_at = position + 2 + name.len();
        index[offset_at..offset_at + 8].copy_from_slice(&data_at.to_le_bytes());
        index[offset_at + 8..offset_at + 16].copy_from_slice(&(archive.len() as u64).to_le_bytes());
        position = offset_at + 16;
        data_at += archive.len() as u64;
    }

    output.write_all(&BUNDLE_MAGIC)?;
    output.write_all(&VERSION.to_le_bytes())?;
    output.write_all(&[0, 0])?;
    output.write_all(&count.to_le_bytes())?;
    output.write_all(&index)?;
    for (_, archive) in members {
        output.write_all(archive)?;
    }
    output.flush()?;
    Ok(data_at)
}

/// Reads the bundle's header and index. Every member must lie inside the file.
pub fn bundle_members<R: Read + Seek>(mut input: R) -> Result<Vec<BundleEntry>> {
    input.seek(SeekFrom::Start(0)).map_err(map_io)?;
    let mut header = [0u8; HEADER_LEN as usize];
    input.read_exact(&mut header).map_err(map_io)?;
    if header[..4] != BUNDLE_MAGIC {
        return Err(Error::BadMagic);
    }
    let version = u16::from_le_bytes([header[4], header[5]]);
    if version != VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    let count = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
    let file_len = input.seek(SeekFrom::End(0)).map_err(map_io)?;

    input.seek(SeekFrom::Start(HEADER_LEN)).map_err(map_io)?;
    let mut entries = Vec::new();
    for _ in 0..count {
        let mut len = [0u8; 2];
        input.read_exact(&mut len).map_err(map_io)?;
        let mut name = vec![0u8; u16::from_le_bytes(len) as usize];
        input.read_exact(&mut name).map_err(map_io)?;
        let name =
            String::from_utf8(name).map_err(|_| Error::Corrupt("member name is not text"))?;
        let mut span = [0u8; 16];
        input.read_exact(&mut span).map_err(map_io)?;
        let offset = u64::from_le_bytes(span[..8].try_into().expect("8 bytes"));
        let length = u64::from_le_bytes(span[8..].try_into().expect("8 bytes"));
        if offset.checked_add(length).is_none_or(|end| end > file_len) {
            return Err(Error::Corrupt(
                "a bundle member runs past the end of the file",
            ));
        }
        entries.push(BundleEntry {
            name,
            offset,
            length,
        });
    }
    Ok(entries)
}

/// Copies one member's archive out of the bundle.
pub fn read_member<R: Read + Seek>(mut input: R, name: &str) -> Result<Vec<u8>> {
    let entries = bundle_members(&mut input)?;
    let entry = entries
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| Error::Format {
            message: format!(
                "no array named `{name}` in the bundle; it holds: {}",
                entries
                    .iter()
                    .map(|e| e.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })?;
    input.seek(SeekFrom::Start(entry.offset)).map_err(map_io)?;
    let mut bytes = vec![0u8; entry.length as usize];
    input.read_exact(&mut bytes).map_err(map_io)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn members_round_trip_through_the_index() {
        let members = vec![
            ("a".to_owned(), b"first archive".to_vec()),
            ("bee".to_owned(), b"second".to_vec()),
        ];
        let mut file = Vec::new();
        write_bundle(&mut file, &members).unwrap();
        assert!(is_bundle(&file));
        let listed = bundle_members(Cursor::new(&file)).unwrap();
        assert_eq!(
            listed.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["a", "bee"]
        );
        assert_eq!(read_member(Cursor::new(&file), "bee").unwrap(), b"second");
        assert!(read_member(Cursor::new(&file), "missing").is_err());
    }

    #[test]
    fn a_member_past_the_end_is_refused() {
        let members = vec![("a".to_owned(), vec![1u8; 8])];
        let mut file = Vec::new();
        write_bundle(&mut file, &members).unwrap();
        file.truncate(file.len() - 4);
        assert!(bundle_members(Cursor::new(&file)).is_err());
    }
}
