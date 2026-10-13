//! Packs a whole directory tree into one `.tpa` archive, the way
//! `tpt-press pack-assets` does. The directory is built in a temporary folder
//! first, so the example runs without any input files.
//!
//! Run with `cargo run -p tpt-press-assets --example pack_directory`.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use tpt_press_assets::{pack, Archive, PackOptions};

/// Walks `dir` and returns every file as a `(relative name, contents)` pair.
/// Names use forward slashes on every platform.
fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out)?;
        } else {
            let relative = path.strip_prefix(root).expect("path is under root");
            let name = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push((name, path));
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::temp_dir().join("tpt-press-example-directory");
    let source = base.join("assets");
    fs::create_dir_all(source.join("ui/icons"))?;
    fs::write(source.join("ui/icons/save.png"), vec![1u8; 2_048])?;
    fs::write(source.join("ui/icons/load.png"), vec![2u8; 2_048])?;
    fs::write(source.join("readme.txt"), "game assets\n".repeat(50))?;

    let mut files = Vec::new();
    collect(&source, &source, &mut files)?;

    // Read each file only when the packer asks for it, so memory use stays at
    // one asset at a time.
    let archive_path = base.join("game.tpa");
    let assets = files
        .iter()
        .map(|(name, path)| Ok((name.clone(), fs::read(path)?)));
    let stats = pack(
        assets,
        File::create(&archive_path)?,
        &PackOptions::default(),
    )?;
    println!(
        "packed {} files into {} bytes",
        stats.assets, stats.archive_bytes
    );

    let archive = Archive::open(&archive_path)?;
    for entry in archive.entries() {
        println!("  {} ({} bytes)", entry.name, entry.raw_len);
    }

    fs::remove_dir_all(&base)?;
    Ok(())
}
