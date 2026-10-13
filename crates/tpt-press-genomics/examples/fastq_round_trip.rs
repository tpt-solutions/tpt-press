//! Packs FASTQ reads, which carry a quality string for each base, and unpacks
//! them. Qualities are coded separately from the bases, so the two can be
//! compared. The round trip is exact, including the `+` line.
//!
//! Run with `cargo run -p tpt-press-genomics --example fastq_round_trip`.

use std::io::Cursor;

use tpt_press_genomics::{pack, unpack, PackOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Reads of 100 bases. Qualities fall off towards the end of each read, as
    // they do on many sequencers. The Phred scale is ASCII 33 onwards.
    let mut fastq = Vec::new();
    for n in 0..2_000u32 {
        let bases: String = (0..100u32)
            .map(|i| ["A", "C", "G", "T"][((i * 11 + n * 5) % 4) as usize])
            .collect();
        let quality: String = (0..100u32)
            .map(|i| {
                let phred = 40i32 - (i as i32 / 6) + ((n + i) % 3) as i32;
                char::from(33 + phred.clamp(2, 41) as u8)
            })
            .collect();
        fastq.extend_from_slice(format!("@read/{n} lane=1\n{bases}\n+\n{quality}\n").as_bytes());
    }

    let mut archive = Vec::new();
    let stats = pack(Cursor::new(&fastq), &mut archive, &PackOptions::default())?;
    println!(
        "{} reads, {} bases: {} bytes of FASTQ -> {} bytes ({:.2}x)",
        stats.records,
        stats.bases,
        stats.input_bytes,
        stats.output_bytes,
        stats.input_bytes as f64 / stats.output_bytes as f64
    );

    let mut restored = Vec::new();
    unpack(Cursor::new(&archive), &mut restored)?;
    assert_eq!(restored, fastq);
    println!("unpacked file matches the original byte for byte");
    Ok(())
}
