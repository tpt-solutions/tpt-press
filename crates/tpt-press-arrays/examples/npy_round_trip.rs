//! Writes a NumPy `.npy` file, packs it into a `.tpar` archive, and unpacks it.
//! The original `.npy` header is kept, so the unpacked bytes are the original
//! file, exactly.
//!
//! Run with `cargo run -p tpt-press-arrays --example npy_round_trip --release`.

use std::io::Cursor;

use tpt_press_arrays::{inspect, pack_npy, unpack, PackOptions};

/// A `.npy` (version 1.0) file of `f32` values on a 256 x 512 grid. The values
/// are a smooth field, the kind of data a simulation writes out.
fn npy_field() -> Vec<u8> {
    let (rows, cols) = (256usize, 512usize);
    let mut header =
        format!("{{'descr': '<f4', 'fortran_order': False, 'shape': ({rows}, {cols}), }}");
    // The header, with its length, is padded with spaces so the data starts on
    // a 64-byte boundary, and ends in a newline, as NumPy writes it.
    while !(10 + header.len() + 1).is_multiple_of(64) {
        header.push(' ');
    }
    header.push('\n');

    let mut file = b"\x93NUMPY".to_vec();
    file.extend_from_slice(&[1, 0]);
    file.extend_from_slice(&(header.len() as u16).to_le_bytes());
    file.extend_from_slice(header.as_bytes());
    for r in 0..rows {
        for c in 0..cols {
            let (x, y) = (c as f32 / 64.0, r as f32 / 64.0);
            file.extend_from_slice(&(x.sin() + y.cos()).to_le_bytes());
        }
    }
    file
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let npy = npy_field();

    let mut archive = Vec::new();
    // Small chunks, so the array is split into several pieces and each is
    // filtered and compressed on its own.
    let options = PackOptions {
        chunk_shape: Some(vec![64, 128]),
        ..PackOptions::default()
    };
    let stats = pack_npy(Cursor::new(&npy), &mut archive, &options)?;
    println!(
        "{} {:?}: {} chunks of shape {:?}",
        stats.dtype, stats.shape, stats.chunks, stats.chunk_shape
    );
    println!(
        "{} bytes of .npy -> {} bytes archived ({:.1}x)",
        stats.input_bytes,
        stats.output_bytes,
        stats.input_bytes as f64 / stats.output_bytes as f64
    );

    let summary = inspect(Cursor::new(&archive))?;
    println!(
        "inspect: npy input = {}, raw bytes = {}",
        summary.npy, summary.raw_bytes
    );

    let mut restored = Vec::new();
    unpack(Cursor::new(&archive), &mut restored)?;
    assert_eq!(restored, npy);
    println!("unpacked file is the original .npy, byte for byte");
    Ok(())
}
