//! Integration tests for `.tpa` assets archives.

use std::fs;

use tempfile::TempDir;
use tpt_press_assets::{pack, Archive, Compression, Error, PackOptions, PAGE_SIZE};

/// Packs `items` into a file in `dir` and returns its path.
fn write_archive(
    dir: &TempDir,
    items: Vec<(String, Vec<u8>)>,
    opts: &PackOptions,
) -> tpt_press_assets::Result<std::path::PathBuf> {
    let path = dir.path().join("assets.tpa");
    let file = fs::File::create(&path).unwrap();
    pack(
        items.into_iter().map(Ok),
        std::io::BufWriter::new(file),
        opts,
    )?;
    Ok(path)
}

fn text(n: usize) -> Vec<u8> {
    format!("line {n}: the quick brown fox jumps over the lazy dog\n")
        .repeat(200)
        .into_bytes()
}

/// Pseudo-random bytes that zstd cannot shrink.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

#[test]
fn every_asset_round_trips() {
    let dir = TempDir::new().unwrap();
    let items = vec![
        ("textures/rock.dds".to_owned(), noise(50_000, 1)),
        ("config/levels.json".to_owned(), text(3)),
        ("empty.bin".to_owned(), Vec::new()),
        ("meshes/a/b/tree.mesh".to_owned(), noise(4097, 2)),
    ];
    let path = write_archive(&dir, items.clone(), &PackOptions::default()).unwrap();
    let archive = Archive::open(&path).unwrap();

    for (name, contents) in &items {
        let asset = archive.get(name).unwrap().expect("asset present");
        assert_eq!(&*asset, contents.as_slice(), "{name}");
    }
    archive.verify().unwrap();
}

#[test]
fn compressible_assets_shrink_and_incompressible_ones_are_stored_raw() {
    let dir = TempDir::new().unwrap();
    let items = vec![
        ("big.txt".to_owned(), text(500)),
        ("noise.bin".to_owned(), noise(20_000, 9)),
    ];
    let path = write_archive(&dir, items, &PackOptions::default()).unwrap();
    let archive = Archive::open(&path).unwrap();

    let text_entry = archive
        .entries()
        .iter()
        .find(|e| e.name == "big.txt")
        .unwrap();
    assert_eq!(text_entry.compression, Compression::Zstd);
    assert!(text_entry.stored_len < text_entry.raw_len / 10);

    let noise_entry = archive
        .entries()
        .iter()
        .find(|e| e.name == "noise.bin")
        .unwrap();
    assert_eq!(noise_entry.compression, Compression::Stored);
}

#[test]
fn stored_assets_are_zero_copy_and_page_aligned() {
    let dir = TempDir::new().unwrap();
    let items = vec![
        ("a.raw".to_owned(), noise(10_000, 3)),
        ("b.raw".to_owned(), noise(7_000, 4)),
    ];
    let opts = PackOptions {
        store_all: true,
        ..PackOptions::default()
    };
    let path = write_archive(&dir, items.clone(), &opts).unwrap();
    let archive = Archive::open(&path).unwrap();

    for entry in archive.entries() {
        assert_eq!(entry.offset % PAGE_SIZE, 0, "{}", entry.name);
        assert_eq!(entry.compression, Compression::Stored);
    }
    let asset = archive.get("a.raw").unwrap().unwrap();
    assert!(asset.is_zero_copy());
    assert_eq!(&*asset, items[0].1.as_slice());
}

#[test]
fn compressed_assets_are_decompressed_into_memory() {
    let dir = TempDir::new().unwrap();
    let path = write_archive(
        &dir,
        vec![("t.txt".to_owned(), text(100))],
        &PackOptions::default(),
    )
    .unwrap();
    let archive = Archive::open(&path).unwrap();
    assert!(!archive.get("t.txt").unwrap().unwrap().is_zero_copy());
}

#[test]
fn missing_name_returns_none() {
    let dir = TempDir::new().unwrap();
    let path = write_archive(
        &dir,
        vec![("a".to_owned(), text(1)), ("c".to_owned(), text(2))],
        &PackOptions::default(),
    )
    .unwrap();
    let archive = Archive::open(&path).unwrap();
    assert!(archive.get("b").unwrap().is_none());
    assert!(archive.get("").unwrap().is_none());
}

#[test]
fn lookup_finds_every_name_in_a_large_archive() {
    let dir = TempDir::new().unwrap();
    let items: Vec<(String, Vec<u8>)> = (0..600)
        .map(|i| (format!("dir{}/asset_{i:04}.bin", i % 7), text(i)))
        .collect();
    let path = write_archive(&dir, items.clone(), &PackOptions::default()).unwrap();
    let archive = Archive::open(&path).unwrap();
    assert_eq!(archive.entries().len(), 600);
    for (name, contents) in &items {
        assert_eq!(&*archive.get(name).unwrap().unwrap(), contents.as_slice());
    }
}

#[test]
fn duplicate_names_are_rejected() {
    let dir = TempDir::new().unwrap();
    let err = write_archive(
        &dir,
        vec![("same".to_owned(), text(1)), ("same".to_owned(), text(2))],
        &PackOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::DuplicateName(name) if name == "same"));
}

#[test]
fn unsafe_names_are_rejected_when_packing() {
    let dir = TempDir::new().unwrap();
    let err = write_archive(
        &dir,
        vec![("../escape.txt".to_owned(), text(1))],
        &PackOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::UnsafeName(_)));
}

#[test]
fn empty_archive_opens_and_has_no_assets() {
    let dir = TempDir::new().unwrap();
    let path = write_archive(&dir, Vec::new(), &PackOptions::default()).unwrap();
    let archive = Archive::open(&path).unwrap();
    assert!(archive.entries().is_empty());
    assert!(archive.get("anything").unwrap().is_none());
}

#[test]
fn corrupted_stored_asset_is_found_by_verify() {
    let dir = TempDir::new().unwrap();
    let opts = PackOptions {
        store_all: true,
        ..PackOptions::default()
    };
    let path = write_archive(&dir, vec![("x.raw".to_owned(), noise(5000, 5))], &opts).unwrap();

    let mut bytes = fs::read(&path).unwrap();
    let offset = Archive::open(&path).unwrap().entries()[0].offset as usize;
    bytes[offset + 10] ^= 0x40;
    fs::write(&path, bytes).unwrap();

    let archive = Archive::open(&path).unwrap();
    assert!(matches!(archive.verify(), Err(Error::ChecksumMismatch(name)) if name == "x.raw"));
}

#[test]
fn damaged_index_is_rejected_on_open() {
    let dir = TempDir::new().unwrap();
    let path = write_archive(
        &dir,
        vec![("a".to_owned(), text(1))],
        &PackOptions::default(),
    )
    .unwrap();
    let mut bytes = fs::read(&path).unwrap();
    let index_at = bytes.len() - 25;
    bytes[index_at] ^= 0xff;
    fs::write(&path, bytes).unwrap();
    assert!(matches!(Archive::open(&path), Err(Error::Corrupt(_))));
}

#[test]
fn truncated_archive_is_rejected_on_open() {
    let dir = TempDir::new().unwrap();
    let path = write_archive(
        &dir,
        vec![("a".to_owned(), text(1))],
        &PackOptions::default(),
    )
    .unwrap();
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() - 3]).unwrap();
    assert!(Archive::open(&path).is_err());
}

#[test]
fn non_archive_is_rejected() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("plain.txt");
    fs::write(&path, b"plain text, not an assets archive, long enough").unwrap();
    assert!(matches!(Archive::open(&path), Err(Error::BadMagic)));
}
