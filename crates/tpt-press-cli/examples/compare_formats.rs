//! Compresses one file with zip (deflate) and plain zstd, and prints size and time.
//!
//! Run with `cargo run -p tpt-press-cli --release --example compare_formats -- <input>`.
//! Each row is one run on this machine, so repeat it for variance.

use std::fs;
use std::io::{self, Write};
use std::time::Instant;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = std::env::args()
        .nth(1)
        .ok_or("usage: compare_formats <input>")?;
    let bytes = fs::read(&input)?;
    let name = std::path::Path::new(&input)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("input")
        .to_owned();

    println!("input: {} bytes", bytes.len());
    for level in [6, 9] {
        let started = Instant::now();
        let size = zip_size(&name, &bytes, level)?;
        report(&format!("zip, deflate level {level}"), size, started);
    }
    for level in [3, 19] {
        let started = Instant::now();
        let size = zstd_size(&bytes, level)?;
        report(&format!("plain zstd, level {level}"), size, started);
    }
    Ok(())
}

fn report(label: &str, size: u64, started: Instant) {
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    println!("{label:<26} {size:>12} bytes  {ms:>9.0} ms");
}

/// Writes a one-entry zip into memory and returns its size.
fn zip_size(name: &str, bytes: &[u8], level: i64) -> Result<u64, Box<dyn std::error::Error>> {
    let mut zip = ZipWriter::new(io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(level));
    zip.start_file(name, options)?;
    zip.write_all(bytes)?;
    let cursor = zip.finish()?;
    Ok(cursor.into_inner().len() as u64)
}

/// Compresses in one zstd frame, with no tpt-press framing, and returns its size.
fn zstd_size(bytes: &[u8], level: i32) -> Result<u64, Box<dyn std::error::Error>> {
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), level)?;
    encoder.write_all(bytes)?;
    let out = encoder.finish()?;
    Ok(out.len() as u64)
}
