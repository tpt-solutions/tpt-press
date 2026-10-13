//! Hostile-input tests for the tensor readers: safetensors, GGUF and `.tptn` archives.
//!
//! Random and damaged inputs must never panic. They must be rejected, or read
//! as valid tensors whose checksums match.

use std::io::Cursor;

use proptest::prelude::*;
use tpt_press_tensors::{gguf, onnx, pack, safetensors, Archive, PackOptions};

fn valid_safetensors() -> Vec<u8> {
    let header = r#"{"w":{"dtype":"F32","shape":[16],"data_offsets":[0,64]},"b":{"dtype":"U8","shape":[4],"data_offsets":[64,68]}}"#;
    let mut out = (header.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(header.as_bytes());
    out.extend((0..68u8).collect::<Vec<u8>>());
    out
}

fn valid_archive() -> Vec<u8> {
    let mut out = Vec::new();
    pack(
        Cursor::new(valid_safetensors()),
        &mut out,
        &PackOptions::default(),
    )
    .unwrap();
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn random_bytes_never_panic_in_any_parser(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = safetensors::parse(&mut Cursor::new(&bytes));
        let _ = gguf::parse(&mut Cursor::new(&bytes));
        let _ = onnx::parse(&mut Cursor::new(&bytes));
        if let Ok(mut archive) = Archive::open(Cursor::new(&bytes)) {
            let _ = archive.verify();
        }
    }

    #[test]
    fn damaged_safetensors_never_panic(
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..8),
        cut in any::<usize>(),
    ) {
        let mut bytes = valid_safetensors();
        for (position, value) in edits {
            let at = position % bytes.len();
            bytes[at] = value;
        }
        bytes.truncate(cut % (bytes.len() + 1));
        let _ = safetensors::parse(&mut Cursor::new(&bytes));
        let _ = gguf::parse(&mut Cursor::new(&bytes));
    }

    #[test]
    fn damaged_archives_are_rejected_or_verified(
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..8),
        cut in any::<usize>(),
    ) {
        let mut bytes = valid_archive();
        for (position, value) in edits {
            let at = position % bytes.len();
            bytes[at] = value;
        }
        bytes.truncate(cut % (bytes.len() + 1));

        if let Ok(mut archive) = Archive::open(Cursor::new(&bytes)) {
            let names: Vec<String> = archive.tensors().iter().map(|t| t.name.clone()).collect();
            for name in names {
                let _ = archive.read_tensor(&name);
            }
            let _ = archive.verify();
        }
    }
}

/// Recomputes the index checksum in the footer after the index bytes were changed,
/// so a damaged index gets past the checksum and reaches the index parser.
fn rewrap_index(bytes: &mut [u8]) {
    let len = bytes.len();
    if len < 20 {
        return;
    }
    let footer = len - 20;
    let index_offset = u64::from_le_bytes(bytes[footer..footer + 8].try_into().unwrap()) as usize;
    let index_len = u32::from_le_bytes(bytes[footer + 8..footer + 12].try_into().unwrap()) as usize;
    if index_offset.checked_add(index_len) != Some(footer) {
        return;
    }
    let crc = crc32fast::hash(&bytes[index_offset..footer]);
    bytes[footer + 12..footer + 16].copy_from_slice(&crc.to_le_bytes());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn damaged_index_with_valid_checksum_never_panics(
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..6),
    ) {
        let mut bytes = valid_archive();
        let len = bytes.len();
        let footer = len - 20;
        let index_offset = u64::from_le_bytes(bytes[footer..footer + 8].try_into().unwrap()) as usize;
        for (position, value) in edits {
            let at = index_offset + position % (footer - index_offset);
            bytes[at] = value;
        }
        rewrap_index(&mut bytes);

        if let Ok(mut archive) = Archive::open(Cursor::new(&bytes)) {
            let names: Vec<String> = archive.tensors().iter().map(|t| t.name.clone()).collect();
            for name in names {
                let _ = archive.read_tensor(&name);
            }
            let _ = archive.verify();
        }
    }
}
