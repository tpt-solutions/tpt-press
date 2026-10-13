//! Fetches records from an archive by name, without unpacking the rest. A
//! name lookup points at the one block that holds the record, so a fetch reads
//! a single block however large the file is. A range is 1-based and inclusive,
//! as in `samtools faidx`.
//!
//! Run with `cargo run -p tpt-press-genomics --example fetch_record`.

use std::io::Cursor;

use tpt_press_genomics::{fetch, pack, PackOptions};

/// The sequence bases in `fetched`, without the header line or the newlines.
fn bases(fetched: &[u8]) -> String {
    fetched
        .split(|&b| b == b'\n')
        .skip(1)
        .flatten()
        .map(|&b| char::from(b))
        .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Many small records, so the archive has several blocks to choose from.
    let mut fasta = Vec::new();
    for n in 0..500u32 {
        let mut state = u64::from(n) * 0x9E37_79B9 + 1;
        let seq: String = (0..1_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                ["A", "C", "G", "T"][(state % 4) as usize]
            })
            .collect();
        fasta.extend_from_slice(format!(">read_{n:04} sample\n").as_bytes());
        for line in seq.as_bytes().chunks(70) {
            fasta.extend_from_slice(line);
            fasta.push(b'\n');
        }
    }

    let mut archive = Vec::new();
    let stats = pack(
        Cursor::new(&fasta),
        &mut archive,
        &PackOptions {
            block_bases: 20_000,
            ..PackOptions::default()
        },
    )?;
    println!(
        "packed {} records into {} blocks",
        stats.records, stats.blocks
    );

    // The whole record.
    let mut out = Vec::new();
    let found = fetch(Cursor::new(&archive), "read_0042", None, &mut out)?;
    println!(
        "read_0042: {} bases long, read {} block(s)",
        found.record_length, found.blocks_read
    );
    let full = bases(&out);
    println!("first 20 bases: {}", &full[..20]);

    // Bases 101 to 130, counted from 1.
    let mut part = Vec::new();
    fetch(
        Cursor::new(&archive),
        "read_0042",
        Some((101, 130)),
        &mut part,
    )?;
    let range = bases(&part);
    assert_eq!(
        range,
        &full[100..130],
        "range must match the same bases in the whole record"
    );
    println!("bases 101 to 130: {range}");

    // A name that is not in the archive is an error, and nothing is written.
    let mut nothing = Vec::new();
    match fetch(Cursor::new(&archive), "read_9999", None, &mut nothing) {
        Ok(_) => println!("unexpected: read_9999 was found"),
        Err(e) => println!("read_9999: {e}"),
    }
    assert!(nothing.is_empty());
    Ok(())
}
