//! Compares the two storage modes. Stored assets are kept raw and can be read
//! without copying from the memory-mapped file. Compressed assets are smaller,
//! but each read decompresses into a new buffer.
//!
//! Run with `cargo run -p tpt-press-assets --example zero_copy --release`.

use std::fs::File;
use std::path::PathBuf;

use tpt_press_assets::{pack, Archive, PackOptions};

fn build(path: &PathBuf, store_all: bool) -> Result<(), Box<dyn std::error::Error>> {
    // A flat, compressible texture, and random bytes that zstd cannot shrink.
    // The packer stores the second one raw, even without store_all.
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let noise: Vec<u8> = (0..1_000_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    let assets = vec![
        Ok((
            "textures/atlas.raw".to_owned(),
            vec![0x80u8; 8 * 1024 * 1024],
        )),
        Ok(("audio/noise.raw".to_owned(), noise)),
    ];
    let stats = pack(
        assets,
        File::create(path)?,
        &PackOptions {
            store_all,
            ..PackOptions::default()
        },
    )?;
    println!(
        "store_all = {store_all:<5}  archive {:>9} bytes  ({} stored, {} compressed)",
        stats.archive_bytes, stats.stored, stats.compressed
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir();
    let compressed = dir.join("tpt-press-example-compressed.tpa");
    let stored = dir.join("tpt-press-example-stored.tpa");

    build(&compressed, false)?;
    build(&stored, true)?;

    for (label, path) in [("compressed", &compressed), ("stored", &stored)] {
        let archive = Archive::open(path)?;
        let atlas = archive.get("textures/atlas.raw")?.ok_or("missing atlas")?;
        let noise = archive.get("audio/noise.raw")?.ok_or("missing noise")?;
        println!(
            "{label:<10} atlas zero-copy: {:<5}  noise zero-copy: {:<5}  atlas first byte: {:#04x}",
            atlas.is_zero_copy(),
            noise.is_zero_copy(),
            atlas[0]
        );
    }

    std::fs::remove_file(&compressed)?;
    std::fs::remove_file(&stored)?;
    Ok(())
}
