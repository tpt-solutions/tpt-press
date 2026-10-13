//! Reads a rectangle out of a large archived array. Only the chunks the
//! rectangle touches are decoded, which is what makes large arrays practical
//! to read piece by piece.
//!
//! Run with `cargo run -p tpt-press-arrays --example read_slice --release`.

use std::io::Cursor;

use tpt_press_arrays::{pack_npy, read_slice, PackOptions};

/// A `.npy` file of `i64` values, 1000 x 1000. Element (r, c) holds r * 1000 + c,
/// so each value shows where it came from.
fn npy_index_grid() -> Vec<u8> {
    let header = "{'descr': '<i8', 'fortran_order': False, 'shape': (1000, 1000), }";
    let mut padded = header.to_owned();
    while !(10 + padded.len() + 1).is_multiple_of(64) {
        padded.push(' ');
    }
    padded.push('\n');

    let mut file = b"\x93NUMPY".to_vec();
    file.extend_from_slice(&[1, 0]);
    file.extend_from_slice(&(padded.len() as u16).to_le_bytes());
    file.extend_from_slice(padded.as_bytes());
    for r in 0..1000i64 {
        for c in 0..1000i64 {
            file.extend_from_slice(&(r * 1000 + c).to_le_bytes());
        }
    }
    file
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let npy = npy_index_grid();
    let mut archive = Vec::new();
    let stats = pack_npy(
        Cursor::new(&npy),
        &mut archive,
        &PackOptions {
            chunk_shape: Some(vec![100, 100]),
            ..PackOptions::default()
        },
    )?;
    println!(
        "{} chunks of {:?}, {} bytes archived",
        stats.chunks, stats.chunk_shape, stats.output_bytes
    );

    // Rows 190 to 209 and columns 480 to 519. The rectangle crosses chunk
    // edges, so it touches 4 of the 100 chunks. The other 96 are never read.
    let slice = read_slice(Cursor::new(&archive), &[190..210, 480..520])?;
    println!(
        "slice shape {:?}, decoded {} of {} chunks",
        slice.shape, slice.chunks_read, stats.chunks
    );

    // Check against the values the file was built from.
    let values: Vec<i64> = slice
        .bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| i64::from_le_bytes(*b))
        .collect();
    let width = slice.shape[1] as usize;
    for row in [0, 9, 10, 19] {
        let first = &values[row * width..row * width + 4];
        println!("row {}: starts {:?}", 190 + row, first);
    }
    assert_eq!(values[0], 190 * 1000 + 480);
    assert_eq!(values[values.len() - 1], 209 * 1000 + 519);
    println!("all {} values match the source", values.len());
    Ok(())
}
