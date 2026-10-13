//! Inspects an archive without unpacking it, then unpacks it under an output
//! limit. A limit is how an application protects itself from an archive that
//! expands far more than expected.
//!
//! Run with `cargo run -p tpt-press-core --example inspect_and_limits`.

use std::io::Cursor;

use tpt_press_core::{archive, pack, PackOptions, UnpackOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = "GET /index.html 200 0.012s\n".repeat(40_000).into_bytes();
    let mut archive_bytes = Vec::new();
    pack(
        Cursor::new(&data),
        &mut archive_bytes,
        &PackOptions {
            chunk_size: 128 * 1024,
            ..PackOptions::default()
        },
    )?;

    // Inspect reads the header and the chunk table. It does not decompress.
    let report = archive::inspect(Cursor::new(&archive_bytes))?;
    println!(
        "{} chunks, {} bytes stored for {} bytes of input ({:.1}x)",
        report.chunks.len(),
        report.stats.archive_bytes,
        report.stats.uncompressed_bytes,
        report.ratio()
    );
    for chunk in report.chunks.iter().take(3) {
        println!(
            "  chunk at offset {}: {} bytes stored, {} bytes raw",
            chunk.offset, chunk.compressed_len, chunk.uncompressed_len
        );
    }

    // Unpacking with no limit restores everything.
    let mut restored = Vec::new();
    archive::unpack(
        Cursor::new(&archive_bytes),
        &mut restored,
        &UnpackOptions::default(),
    )?;
    assert_eq!(restored, data);
    println!("unpacked with no limit: {} bytes", restored.len());

    // A limit below the real size is refused. The check runs as the output
    // grows, so the work stops once the limit is passed.
    let limited = UnpackOptions {
        max_total_size: Some(1024 * 1024),
        ..UnpackOptions::default()
    };
    let mut sink = Vec::new();
    match archive::unpack(Cursor::new(&archive_bytes), &mut sink, &limited) {
        Ok(_) => println!("unexpectedly within the limit"),
        Err(e) => println!("unpacking with a 1 MiB limit refused: {e}"),
    }
    Ok(())
}
