//! Packs a multi-record FASTA file into a `.tpgn` archive, unpacks it, and
//! checks the bytes match. It also shows what the archive costs per base, and
//! that input the format cannot return exactly is refused, with the line named.
//!
//! Run with `cargo run -p tpt-press-genomics --example fasta_round_trip --release`.

use std::io::Cursor;

use tpt_press_genomics::{inspect, pack, unpack, Error, PackOptions};

/// Builds a FASTA file with three records, wrapped at 60 columns. A seeded
/// generator makes the same sequence on every run. Sequences are made of
/// random bases with some repeats, which is roughly what real DNA looks like
/// to a compressor.
fn sample_fasta() -> Vec<u8> {
    let mut state: u64 = 0x2026_0513;
    let mut base = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        b"ACGT"[(state % 4) as usize]
    };
    let mut out = Vec::new();
    for (name, length) in [
        ("chrA", 120_000usize),
        ("chrB", 45_000),
        ("plasmid_1", 3_100),
    ] {
        out.extend_from_slice(format!(">{name} synthetic example\n").as_bytes());
        let mut seq: Vec<u8> = (0..length).map(|_| base()).collect();
        // Copy a stretch forward, so there is something to find.
        let repeat = seq[1_000..1_400].to_vec();
        seq[length / 2..length / 2 + 400].copy_from_slice(&repeat);
        for line in seq.chunks(60) {
            out.extend_from_slice(line);
            out.push(b'\n');
        }
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fasta = sample_fasta();

    let mut archive = Vec::new();
    let stats = pack(Cursor::new(&fasta), &mut archive, &PackOptions::default())?;
    println!(
        "packed {} records, {} bases, into {} blocks",
        stats.records, stats.bases, stats.blocks
    );
    println!(
        "{} bytes -> {} bytes, {:.3} bits per base",
        stats.input_bytes,
        stats.output_bytes,
        stats.output_bytes as f64 * 8.0 / stats.bases as f64
    );

    let summary = inspect(Cursor::new(&archive))?;
    println!("inspect: {:?}, {} records", summary.kind, summary.records);

    let mut restored = Vec::new();
    unpack(Cursor::new(&archive), &mut restored)?;
    assert_eq!(restored, fasta);
    println!("unpacked file matches the original byte for byte");

    // The parser refuses input it could not give back exactly. CRLF line endings
    // are one example, and the error names the problem.
    let crlf = b">seq\r\nACGT\r\n".to_vec();
    match pack(Cursor::new(&crlf), Vec::new(), &PackOptions::default()) {
        Err(e @ Error::Unsupported { .. }) => println!("CRLF input refused: {e}"),
        Err(other) => return Err(other.into()),
        Ok(_) => return Err("CRLF input was accepted".into()),
    }
    Ok(())
}
