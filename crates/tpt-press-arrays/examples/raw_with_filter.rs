//! Packs a raw array, with no `.npy` header, and compares the filters. The
//! writer can try every filter per chunk and keep the smallest (`Auto`), or be
//! told to use one (`Fixed`). The best fixed filter depends on the data. Here
//! the difference from the previous element wins over the Lorenzo prediction,
//! which is why `Auto` is the default.
//!
//! Run with `cargo run -p tpt-press-arrays --example raw_with_filter --release`.

use std::io::Cursor;

use tpt_press_arrays::{pack_raw, Filter, FilterChoice, PackOptions};

/// Raw little-endian `u16` samples of a 2D ramp, with a little noise on each
/// sample. There is no header, so the dtype and shape go in as arguments.
fn raw_ramp(rows: usize, cols: usize) -> Vec<u8> {
    let mut state: u64 = 7;
    let mut out = Vec::with_capacity(rows * cols * 2);
    for r in 0..rows {
        for c in 0..cols {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let noise = (state % 9) as usize;
            let value = (r * 9 + c * 5 + (c * r) % 7 + noise) as u16;
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (rows, cols) = (1_024u64, 1_024u64);
    let data = raw_ramp(rows as usize, cols as usize);
    let shape = [rows, cols];

    let choices = [
        ("auto (try all)", FilterChoice::Auto),
        ("none", FilterChoice::Fixed(Filter::None)),
        ("shuffle", FilterChoice::Fixed(Filter::Shuffle)),
        (
            "delta (previous element)",
            FilterChoice::Fixed(Filter::Delta),
        ),
        ("lorenzo prediction", FilterChoice::Fixed(Filter::Lorenzo)),
    ];

    println!("{:<24} {:>12} {:>10}", "filter", "archive", "ratio");
    for (label, filter) in choices {
        let mut archive = Vec::new();
        let stats = pack_raw(
            Cursor::new(&data),
            &mut archive,
            "<u2",
            &shape,
            &PackOptions {
                filter,
                ..PackOptions::default()
            },
        )?;
        println!(
            "{label:<24} {:>12} {:>9.2}x",
            stats.output_bytes,
            stats.input_bytes as f64 / stats.output_bytes as f64
        );
    }
    Ok(())
}
