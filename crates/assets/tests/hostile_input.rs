//! Hostile-input tests for the assets reader.
//!
//! The reader memory-maps a file, so each case is written to disk. A damaged
//! archive must be rejected on open, or fail its checksums, and must never panic.

use std::fs;
use std::io::Cursor;

use proptest::prelude::*;
use tempfile::TempDir;
use tpt_press_assets::{pack, Archive, PackOptions};

fn valid_archive() -> Vec<u8> {
    let items: Vec<(String, Vec<u8>)> = vec![
        (
            "a.bin".to_owned(),
            (0..9000u32).map(|i| (i % 7) as u8).collect(),
        ),
        ("dir/b.txt".to_owned(), b"hello assets\n".repeat(50)),
        ("empty".to_owned(), Vec::new()),
    ];
    let mut out = Vec::new();
    pack(
        items.into_iter().map(Ok),
        Cursor::new(&mut out),
        &PackOptions::default(),
    )
    .unwrap();
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(120))]

    #[test]
    fn damaged_archives_never_panic(
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..8),
        cut in any::<usize>(),
    ) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("hostile.tpa");

        let mut bytes = valid_archive();
        for (position, value) in edits {
            let at = position % bytes.len();
            bytes[at] = value;
        }
        bytes.truncate(cut % (bytes.len() + 1));
        fs::write(&path, &bytes).unwrap();

        if let Ok(archive) = Archive::open(&path) {
            let names: Vec<String> = archive.entries().iter().map(|e| e.name.clone()).collect();
            for name in names {
                let _ = archive.get(&name);
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
    #![proptest_config(ProptestConfig::with_cases(120))]

    #[test]
    fn damaged_index_with_valid_checksum_never_panics(
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 1..6),
    ) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("hostile.tpa");

        let mut bytes = valid_archive();
        let footer = bytes.len() - 20;
        let index_offset = u64::from_le_bytes(bytes[footer..footer + 8].try_into().unwrap()) as usize;
        for (position, value) in edits {
            let at = index_offset + position % (footer - index_offset);
            bytes[at] = value;
        }
        rewrap_index(&mut bytes);
        fs::write(&path, &bytes).unwrap();

        if let Ok(archive) = Archive::open(&path) {
            let names: Vec<String> = archive.entries().iter().map(|e| e.name.clone()).collect();
            for name in names {
                let _ = archive.get(&name);
            }
            let _ = archive.verify();
        }
    }
}
