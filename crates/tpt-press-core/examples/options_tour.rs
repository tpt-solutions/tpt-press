//! Packs the same log-like input under a range of options, and prints the size
//! each setting gives. Use it to choose a level, chunking mode and dictionary
//! for your own data.
//!
//! Run with `cargo run -p tpt-press-core --example options_tour --release`.

use std::io::Cursor;

use tpt_press_core::{pack, Chunking, PackOptions};

/// Deterministic log lines. A small linear congruential generator picks the
/// fields, so the output is the same on every run.
fn sample_input() -> Vec<u8> {
    let mut state: u32 = 2026;
    let mut next = move || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        state >> 8
    };
    let levels = ["debug", "info", "info", "info", "warn", "error"];
    let mut out = String::new();
    for i in 0..60_000u32 {
        let level = levels[next() as usize % levels.len()];
        let user = next() % 500;
        let ms = next() % 2_000;
        out.push_str(&format!(
            "{{\"seq\":{i},\"level\":\"{level}\",\"user\":{user},\"ms\":{ms},\"route\":\"/api/v1/items\"}}\n"
        ));
    }
    out.into_bytes()
}

fn main() -> tpt_press_core::Result<()> {
    let data = sample_input();
    println!("input: {} bytes\n", data.len());
    println!(
        "{:<34} {:>12} {:>8} {:>8}",
        "setting", "archive", "ratio", "chunks"
    );

    let run = |label: &str, opts: PackOptions| -> tpt_press_core::Result<()> {
        let mut archive = Vec::new();
        let stats = pack(Cursor::new(&data), &mut archive, &opts)?;
        println!(
            "{label:<34} {:>12} {:>7.2}x {:>8}",
            stats.archive_bytes,
            stats.uncompressed_bytes as f64 / stats.archive_bytes as f64,
            stats.chunks
        );
        Ok(())
    };

    // Compression level: higher is smaller and slower.
    for level in [1, 3, 9, 19] {
        run(
            &format!("level {level}, fixed 1 MiB"),
            PackOptions {
                level,
                ..PackOptions::default()
            },
        )?;
    }

    // Chunk size: smaller chunks let a reader decompress less at a time, and
    // compress a little worse.
    for chunk_size in [64 * 1024, 256 * 1024] {
        run(
            &format!("level 9, fixed {} KiB", chunk_size / 1024),
            PackOptions {
                chunk_size,
                level: 9,
                ..PackOptions::default()
            },
        )?;
    }

    // Content-defined chunks: boundaries follow the content, so an edit early in
    // the input does not move every later boundary.
    run(
        "level 9, content-defined 64 KiB",
        PackOptions {
            chunk_size: 64 * 1024,
            chunking: Chunking::ContentDefined,
            level: 9,
            ..PackOptions::default()
        },
    )?;

    // A zstd dictionary, trained on the start of the input. It pays off when
    // many chunks share structure, such as repeated JSON keys. This input has
    // few shared fragments, so here the dictionary costs bytes, which is the
    // point of measuring it on your own data.
    run(
        "level 9, fixed 64 KiB, dict 16 KiB",
        PackOptions {
            chunk_size: 64 * 1024,
            level: 9,
            dictionary: Some(16 * 1024),
            ..PackOptions::default()
        },
    )?;

    Ok(())
}
