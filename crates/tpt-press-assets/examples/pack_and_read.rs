//! Packs a small set of named assets into a `.tpa` file, then opens it, lists
//! the entries, and reads each one back, checking the contents match.
//!
//! Run with `cargo run -p tpt-press-assets --example pack_and_read`.

use std::fs::File;
use std::path::PathBuf;

use tpt_press_assets::{pack, Archive, PackOptions};

/// Stand-in contents. Real assets would come from disk or a build step.
fn sample_assets() -> Vec<(String, Vec<u8>)> {
    vec![
        (
            "levels/forest.json".to_owned(),
            br#"{"name":"forest","spawns":[1,4,9]}"#.repeat(200),
        ),
        (
            "sounds/step.wav".to_owned(),
            (0..4_096u32).map(|i| (i * 7 % 251) as u8).collect(),
        ),
        ("textures/grass.raw".to_owned(), vec![0x3c; 64 * 1024]),
        ("config/empty.cfg".to_owned(), Vec::new()),
    ]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path: PathBuf = std::env::temp_dir().join("tpt-press-example-pack-and-read.tpa");
    let assets = sample_assets();

    // Pack. Names must be unique, relative, and use forward slashes.
    let stats = pack(
        assets.iter().cloned().map(Ok),
        File::create(&path)?,
        &PackOptions::default(),
    )?;
    println!(
        "packed {} assets: {} stored, {} compressed, {} bytes -> {} bytes",
        stats.assets, stats.stored, stats.compressed, stats.input_bytes, stats.archive_bytes
    );

    // Open. Only the header and the index are read here.
    let archive = Archive::open(&path)?;
    println!("\n{:<22} {:>10} {:>10}  storage", "name", "raw", "stored");
    for entry in archive.entries() {
        println!(
            "{:<22} {:>10} {:>10}  {:?}",
            entry.name, entry.raw_len, entry.stored_len, entry.compression
        );
    }

    // Read each asset and compare it with the original.
    for (name, original) in &assets {
        let asset = archive.get(name)?.ok_or("asset missing from the index")?;
        assert_eq!(
            &asset[..],
            &original[..],
            "{name} differs after a round trip"
        );
    }
    archive.verify()?;
    println!(
        "\nall {} assets read back, and verify() passed",
        assets.len()
    );

    // A name that is not in the archive is None, not an error.
    println!(
        "missing name: {:?}",
        archive.get("nope/missing.bin")?.is_none()
    );

    std::fs::remove_file(&path)?;
    Ok(())
}
