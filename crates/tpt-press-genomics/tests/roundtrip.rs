//! Round trips, refusals, random access and corruption for genomics archives.

use std::io::Cursor;

use proptest::prelude::*;
use tpt_press_genomics::{fetch, inspect, pack, unpack, Error, Kind, PackOptions};

fn options(block_bases: usize) -> PackOptions {
    PackOptions {
        block_bases,
        ..PackOptions::default()
    }
}

fn pack_bytes(input: &[u8], block_bases: usize) -> Result<Vec<u8>, Error> {
    let mut archive = Vec::new();
    pack(
        Cursor::new(input.to_vec()),
        &mut archive,
        &options(block_bases),
    )?;
    Ok(archive)
}

fn unpack_bytes(archive: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    unpack(Cursor::new(archive.to_vec()), &mut out)?;
    Ok(out)
}

fn fetch_bytes(archive: &[u8], id: &str, range: Option<(u64, u64)>) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    fetch(Cursor::new(archive.to_vec()), id, range, &mut out)?;
    Ok(out)
}

const FASTA: &str = ">chr1 test chromosome\n\
ACGTACGTAC\n\
GTNNNNNNNN\n\
nnnnACGTRY\n\
acgtacgtac\n\
GG*-\n\
>empty\n\
>exact\n\
ACGTACGTAC\n\
ACGTACGTAC\n\
>short\n\
ACG\n";

const FASTQ: &str = "@read1 lane 3\n\
ACGTNNacgt\n\
+\n\
IIIIIIIIII\n\
@read2\n\
GATTACA\n\
+read2\n\
ABCDEFG\n\
@empty\n\
\n\
+\n\
\n";

#[test]
fn fasta_round_trips_byte_for_byte() {
    for block_bases in [1, 7, 1 << 20] {
        let archive = pack_bytes(FASTA.as_bytes(), block_bases).unwrap();
        assert_eq!(
            unpack_bytes(&archive).unwrap(),
            FASTA.as_bytes(),
            "block size {block_bases}"
        );
    }
}

#[test]
fn fastq_round_trips_byte_for_byte() {
    for block_bases in [1, 1 << 20] {
        let archive = pack_bytes(FASTQ.as_bytes(), block_bases).unwrap();
        assert_eq!(
            unpack_bytes(&archive).unwrap(),
            FASTQ.as_bytes(),
            "block size {block_bases}"
        );
    }
}

#[test]
fn inspect_reports_counts_without_decoding() {
    let archive = pack_bytes(FASTA.as_bytes(), 8).unwrap();
    let summary = inspect(Cursor::new(archive.clone())).unwrap();
    assert_eq!(summary.kind, Kind::Fasta);
    assert_eq!(summary.records, 4);
    // chr1 has 44 bases, empty has none, exact has 20, short has 3.
    assert_eq!(summary.bases, 44 + 20 + 3);
    assert!(summary.blocks > 1);
    assert_eq!(summary.archive_bytes, archive.len() as u64);
}

#[test]
fn fetch_returns_a_whole_record() {
    let archive = pack_bytes(FASTA.as_bytes(), 8).unwrap();
    let out = fetch_bytes(&archive, "exact", None).unwrap();
    assert_eq!(out, b">exact\nACGTACGTAC\nACGTACGTAC\n");
}

#[test]
fn fetch_matches_the_name_before_the_first_space() {
    let archive = pack_bytes(FASTA.as_bytes(), 8).unwrap();
    let out = fetch_bytes(&archive, "chr1", Some((1, 4))).unwrap();
    assert_eq!(out, b">chr1 test chromosome\nACGT\n");
}

#[test]
fn fetch_range_crosses_blocks_and_lines() {
    let archive = pack_bytes(FASTA.as_bytes(), 3).unwrap();
    // Bases 9..=24 of chr1, which span two lines of 10 and cross blocks.
    let out = fetch_bytes(&archive, "chr1", Some((9, 24))).unwrap();
    let whole = b"ACGTACGTACGTNNNNNNNNnnnnACGTRYacgtacgtacGG*-";
    let expected: Vec<u8> = {
        let mut v = b">chr1 test chromosome\n".to_vec();
        for line in whole[8..24].chunks(10) {
            v.extend_from_slice(line);
            v.push(b'\n');
        }
        v
    };
    assert_eq!(out, expected);
}

#[test]
fn fetch_range_keeps_quality_in_step() {
    let archive = pack_bytes(FASTQ.as_bytes(), 1 << 20).unwrap();
    let out = fetch_bytes(&archive, "read2", Some((2, 4))).unwrap();
    assert_eq!(out, b"@read2\nATT\n+read2\nBCD\n");
}

#[test]
fn fetch_reports_missing_names_and_bad_ranges() {
    let archive = pack_bytes(FASTA.as_bytes(), 8).unwrap();
    assert!(matches!(
        fetch_bytes(&archive, "nope", None),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        fetch_bytes(&archive, "short", Some((1, 4))),
        Err(Error::RangeOutOfBounds { length: 3, .. })
    ));
    assert!(matches!(
        fetch_bytes(&archive, "short", Some((0, 2))),
        Err(Error::RangeOutOfBounds { .. })
    ));
    assert!(matches!(
        fetch_bytes(&archive, "short", Some((3, 2))),
        Err(Error::RangeOutOfBounds { .. })
    ));
}

#[test]
fn fetch_reads_only_blocks_up_to_the_match() {
    let archive = pack_bytes(FASTA.as_bytes(), 1).unwrap();
    let mut out = Vec::new();
    let stats = fetch(Cursor::new(archive), "chr1", None, &mut out).unwrap();
    assert_eq!(stats.blocks_read, 1);
    assert_eq!(stats.record_length, 44);
}

#[test]
fn refuses_input_it_cannot_return_exactly() {
    let cases: [(&str, &str); 8] = [
        (">a\r\nACGT\r\n", "CR"),
        (">a\nACGT", "newline"),
        (">a\nACGTA\nAC\nACGTA\n", "widths change"),
        (">a\nACGT\n\nACGT\n", "blank"),
        ("@r\nACGT\n-\nIIII\n", "'+'"),
        ("@r\nACGT\n+\nIII\n", "quality"),
        ("not fasta\n", "must start"),
        ("", "empty"),
    ];
    for (input, expect) in cases {
        let err = pack_bytes(input.as_bytes(), 1 << 20)
            .unwrap_err()
            .to_string();
        assert!(err.contains(expect), "input {input:?} gave {err}");
    }
}

#[test]
fn a_shorter_last_line_is_fine_but_a_longer_one_is_not() {
    assert!(pack_bytes(b">a\nACGT\nACGT\nAC\n", 1 << 20).is_ok());
    assert!(pack_bytes(b">a\nACGT\nAC\nACGT\n", 1 << 20).is_err());
}

#[test]
fn a_flipped_byte_in_a_block_is_detected() {
    let mut archive = pack_bytes(FASTA.as_bytes(), 1 << 20).unwrap();
    // The first block starts after the 8-byte header.
    archive[12] ^= 0x40;
    let err = unpack_bytes(&archive).unwrap_err();
    assert!(matches!(err, Error::Corrupt(_)), "got {err:?}");
}

#[test]
fn a_truncated_archive_is_detected() {
    let archive = pack_bytes(FASTA.as_bytes(), 1 << 20).unwrap();
    for cut in [archive.len() - 1, archive.len() / 2, 10] {
        assert!(unpack_bytes(&archive[..cut]).is_err(), "cut at {cut}");
    }
}

#[test]
fn a_foreign_file_is_not_an_archive() {
    assert!(matches!(
        unpack_bytes(b"not a genomics archive at all, just some bytes"),
        Err(Error::BadMagic) | Err(Error::Truncated) | Err(Error::Corrupt(_))
    ));
}

fn dna_fasta(records: &[(String, Vec<u8>, usize)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (id, seq, width) in records {
        out.push(b'>');
        out.extend_from_slice(id.as_bytes());
        out.push(b'\n');
        for line in seq.chunks((*width).max(1)) {
            out.extend_from_slice(line);
            out.push(b'\n');
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn random_fasta_round_trips(
        records in prop::collection::vec(
            (
                "[a-z0-9_]{1,8}",
                prop::collection::vec(
                    prop::sample::select(b"ACGTacgtNnRYKM-*".to_vec()),
                    1..200,
                ),
                1usize..80,
            ),
            1..6,
        ),
        block_bases in 1usize..300,
    ) {
        let input = dna_fasta(&records);
        let archive = pack_bytes(&input, block_bases).unwrap();
        prop_assert_eq!(unpack_bytes(&archive).unwrap(), input);
    }

    #[test]
    fn random_fasta_ranges_match_the_source(
        seq in prop::collection::vec(prop::sample::select(b"ACGTacgtN".to_vec()), 1..300),
        width in 1usize..50,
        block_bases in 1usize..100,
        a in 1u64..300,
        b in 1u64..300,
    ) {
        let input = dna_fasta(&[("r".to_owned(), seq.clone(), width)]);
        let archive = pack_bytes(&input, block_bases).unwrap();
        let (start, end) = (a.min(b), a.max(b));
        let len = seq.len() as u64;
        let result = fetch_bytes(&archive, "r", Some((start, end)));
        if end <= len {
            let part = &seq[(start - 1) as usize..end as usize];
            let expected = dna_fasta(&[("r".to_owned(), part.to_vec(), width)]);
            prop_assert_eq!(result.unwrap(), expected);
        } else {
            prop_assert!(result.is_err());
        }
    }
}

#[test]
fn duplicate_names_fetch_the_first_record_from_one_block() {
    let input = b">dup first\nACGT\n>other\nGGGG\n>dup second\nTTTT\n";
    let archive = pack_bytes(input, 1).unwrap();
    let mut out = Vec::new();
    let stats = fetch(Cursor::new(archive), "dup", None, &mut out).unwrap();
    assert_eq!(out, b">dup first\nACGT\n");
    assert_eq!(stats.blocks_read, 1);
}

#[test]
fn modelled_bases_and_qualities_round_trip_at_scale() {
    // Enough bases and qualities for the context models to switch on, and
    // long enough that several blocks learn separately.
    let mut state = 7u64;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        (state >> 33) as usize
    };
    let mut text = Vec::new();
    for r in 0..3000 {
        let len = 50 + next() % 100;
        text.extend_from_slice(format!("@r{r}\n").as_bytes());
        let seq: Vec<u8> = (0..len).map(|_| b"ACGT"[next() % 4]).collect();
        text.extend_from_slice(&seq);
        text.extend_from_slice(b"\n+\n");
        let qual: Vec<u8> = (0..len)
            .map(|i| b'#' + ((i * 3 + next() % 5) % 38) as u8)
            .collect();
        text.extend_from_slice(&qual);
        text.push(b'\n');
    }
    let archive = pack_bytes(&text, 40_000).unwrap();
    assert_eq!(unpack_bytes(&archive).unwrap(), text);
}

#[test]
fn vcf_round_trips_across_blocks_and_line_shapes() {
    // Header lines, then 10,000 calls: a chromosome change, three line shapes
    // (8, 9 and 10 columns), and a POS that goes back at the change. Blocks hold
    // 4,096 records, so this spans three.
    let mut text = String::from(
        "##fileformat=VCFv4.2\n##source=test\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\tFORMAT\tS1\tS2\n",
    );
    let mut pos = 100u64;
    for i in 0..10_000u64 {
        let chrom = if i < 6000 { "chr1" } else { "chr2" };
        if i == 6000 {
            pos = 50;
        }
        pos += 1 + (i * 7) % 13;
        let line = match i % 3 {
            0 => format!(
                "{chrom}\t{pos}\trs{i}\tA\tG\t{}\tPASS\tDP={}\tGT:DP\t0/1:{}\t1|1:{}\n",
                i % 90,
                i % 40,
                i % 40,
                i % 7
            ),
            1 => format!("{chrom}\t{pos}\t.\tC\tT,G\t.\t.\t.\n"),
            _ => format!("{chrom}\t{pos}\t.\tG\tA\t9\tq10\tAF=0.5\tGT\t.\n"),
        };
        text.push_str(&line);
    }
    let archive = pack_bytes(text.as_bytes(), 40_000).unwrap();
    assert_eq!(unpack_bytes(&archive).unwrap(), text.as_bytes());
}

#[test]
fn vcf_refuses_a_header_line_after_the_data() {
    let text = b"##fileformat=VCFv4.2\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n1\t10\t.\tA\tG\t.\t.\t.\n#late\n";
    assert!(pack_bytes(text, 40_000).is_err());
}
