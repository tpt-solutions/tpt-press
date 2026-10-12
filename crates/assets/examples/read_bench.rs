//! Compares reading assets three ways. Each run starts from a closed file, so
//! the numbers include opening, mapping and page faults:
//!
//! - raw assets from a `.tpa`, used in place through the memory map (zero-copy)
//! - compressible assets from a `.tpa`, decompressed into memory
//! - the same raw assets as loose files, read with `fs::read`
//!
//! Run with: `cargo run --release -p tpt-press-assets --example read_bench`

use std::fs;
use std::io::BufWriter;
use std::path::Path;
use std::time::Instant;

use tempfile::TempDir;
use tpt_press_assets::{pack, Archive, PackOptions};

const ASSETS: usize = 400;
const ASSET_LEN: usize = 256 * 1024;
const RUNS: usize = 5;

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

/// Bytes that zstd shrinks well.
fn text(len: usize, seed: usize) -> Vec<u8> {
    format!("asset {seed} line of repeated text\n")
        .into_bytes()
        .into_iter()
        .cycle()
        .take(len)
        .collect()
}

fn pack_to(path: &Path, items: &[(String, Vec<u8>)], store_all: bool) {
    let file = fs::File::create(path).unwrap();
    let opts = PackOptions {
        store_all,
        ..PackOptions::default()
    };
    let stats = pack(items.iter().cloned().map(Ok), BufWriter::new(file), &opts).unwrap();
    println!(
        "  {} assets, {} MB in -> {} MB on disk ({} stored raw, {} compressed)",
        stats.assets,
        stats.input_bytes / 1_000_000,
        stats.archive_bytes / 1_000_000,
        stats.stored,
        stats.compressed
    );
}

/// Touches one byte per page, so every page of the asset is read.
fn touch(bytes: &[u8]) -> u64 {
    bytes.iter().step_by(4096).map(|&b| u64::from(b)).sum()
}

/// Runs `f` `RUNS` times and returns the best wall time in milliseconds.
fn best_of<F: FnMut() -> u64>(mut f: F) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..RUNS {
        let start = Instant::now();
        let sum = f();
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        std::hint::black_box(sum);
        best = best.min(elapsed);
    }
    best
}

fn main() {
    let dir = TempDir::new().unwrap();
    let raw_items: Vec<(String, Vec<u8>)> = (0..ASSETS)
        .map(|i| (format!("raw/{i:04}.bin"), noise(ASSET_LEN, i as u64 + 1)))
        .collect();
    let text_items: Vec<(String, Vec<u8>)> = (0..ASSETS)
        .map(|i| (format!("text/{i:04}.txt"), text(ASSET_LEN, i)))
        .collect();

    let loose_dir = dir.path().join("loose");
    fs::create_dir_all(&loose_dir).unwrap();
    let mut loose_paths = Vec::new();
    for (name, contents) in &raw_items {
        let path = loose_dir.join(name.replace('/', "_"));
        fs::write(&path, contents).unwrap();
        loose_paths.push(path);
    }

    let raw_path = dir.path().join("raw.tpa");
    let text_path = dir.path().join("text.tpa");
    println!("packing:");
    println!("raw assets, stored:");
    pack_to(&raw_path, &raw_items, true);
    println!("text assets, compressed:");
    pack_to(&text_path, &text_items, false);

    let mb = (ASSETS * ASSET_LEN) as f64 / 1_000_000.0;
    println!("\nreading all {ASSETS} assets ({mb:.0} MB), best of {RUNS} runs:");

    let t = best_of(|| {
        let archive = Archive::open(&raw_path).unwrap();
        let mut sum = 0u64;
        for (name, _) in &raw_items {
            let asset = archive.get(name).unwrap().unwrap();
            assert!(asset.is_zero_copy());
            sum = sum.wrapping_add(touch(&asset));
        }
        sum
    });
    println!("  raw, memory-mapped, zero-copy:  {t:>8.1} ms");

    let t = best_of(|| {
        let archive = Archive::open(&text_path).unwrap();
        let mut sum = 0u64;
        for (name, _) in &text_items {
            let asset = archive.get(name).unwrap().unwrap();
            assert!(!asset.is_zero_copy());
            sum = sum.wrapping_add(touch(&asset));
        }
        sum
    });
    println!("  compressed, decompressed:       {t:>8.1} ms");

    let t = best_of(|| {
        let mut sum = 0u64;
        for path in &loose_paths {
            sum = sum.wrapping_add(touch(&fs::read(path).unwrap()));
        }
        sum
    });
    println!("  loose files, fs::read:          {t:>8.1} ms");

    println!("\nreading only the first 4 KB of each asset, best of {RUNS} runs:");
    let t = best_of(|| {
        let archive = Archive::open(&raw_path).unwrap();
        let mut sum = 0u64;
        for (name, _) in &raw_items {
            let asset = archive.get(name).unwrap().unwrap();
            sum = sum.wrapping_add(u64::from(asset[0]));
        }
        sum
    });
    println!("  raw, memory-mapped, zero-copy:  {t:>8.1} ms");

    let t = best_of(|| {
        let mut sum = 0u64;
        for path in &loose_paths {
            let bytes = fs::read(path).unwrap();
            sum = sum.wrapping_add(u64::from(bytes[0]));
        }
        sum
    });
    println!("  loose files, fs::read (whole):  {t:>8.1} ms");

    println!(
        "\nratio on disk for the text assets: {:.1}% of their raw size",
        100.0 * fs::metadata(&text_path).unwrap().len() as f64 / (ASSETS * ASSET_LEN) as f64
    );
}
