//! Packs some repetitive text into a generic archive, then unpacks it and
//! checks that the bytes match.
//!
//! Run with `cargo run -p tpt-press-core --example round_trip`.

use std::io::Cursor;

use tpt_press_core::{pack, unpack, PackOptions, UnpackOptions};

fn main() -> tpt_press_core::Result<()> {
    let data = "the quick brown fox jumps over the lazy dog\n".repeat(20_000);

    let mut archive = Vec::new();
    let stats = pack(
        Cursor::new(data.as_bytes()),
        &mut archive,
        &PackOptions::default(),
    )?;
    println!(
        "packed {} bytes into {} ({:.1}x, {} chunks)",
        stats.uncompressed_bytes,
        stats.archive_bytes,
        stats.uncompressed_bytes as f64 / stats.archive_bytes as f64,
        stats.chunks
    );

    let mut restored = Vec::new();
    unpack(
        Cursor::new(&archive),
        &mut restored,
        &UnpackOptions::default(),
    )?;
    assert_eq!(restored, data.as_bytes());
    println!("round trip ok");
    Ok(())
}
