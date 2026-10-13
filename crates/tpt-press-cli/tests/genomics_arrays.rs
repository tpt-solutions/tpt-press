//! End-to-end tests for `pack-genomics`, `unpack-genomics`, `fetch-genomics`,
//! `pack-arrays`, `unpack-arrays` and `slice-arrays`.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tpt-press"))
        .args(args)
        .output()
        .expect("failed to run tpt-press")
}

fn path_str(p: &Path) -> &str {
    p.to_str().expect("test paths are valid UTF-8")
}

const FASTA: &str = ">chr1 first\nACGTACGTAC\nGTNNNN\n>chr2\nacgtACGT\n";

/// A `.npy` file with int16 values 0, 1, 2, ... in a 3x4 array.
fn npy_file() -> Vec<u8> {
    let text = "{'descr': '<i2', 'fortran_order': False, 'shape': (3, 4), }";
    let mut text = text.to_owned();
    while !(10 + text.len() + 1).is_multiple_of(64) {
        text.push(' ');
    }
    text.push('\n');
    let mut out = b"\x93NUMPY\x01\x00".to_vec();
    out.extend_from_slice(&(text.len() as u16).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
    for v in 0i16..12 {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

#[test]
fn genomics_round_trips_through_the_cli() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("in.fa");
    let archive = dir.path().join("in.tpgn");
    let back = dir.path().join("back.fa");
    fs::write(&input, FASTA).unwrap();

    let packed = run(&["pack-genomics", path_str(&input), path_str(&archive)]);
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );

    let unpacked = run(&["unpack-genomics", path_str(&archive), path_str(&back)]);
    assert!(unpacked.status.success());
    assert_eq!(fs::read(&back).unwrap(), FASTA.as_bytes());

    let inspected = run(&["inspect", path_str(&archive)]);
    let text = String::from_utf8_lossy(&inspected.stdout);
    assert!(text.contains("format:        FASTA"), "{text}");
    assert!(text.contains("records:       2"), "{text}");
}

#[test]
fn fetch_prints_a_record_or_a_range() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("in.fa");
    let archive = dir.path().join("in.tpgn");
    fs::write(&input, FASTA).unwrap();
    run(&["pack-genomics", path_str(&input), path_str(&archive)]);

    let whole = run(&["fetch-genomics", path_str(&archive), "chr2"]);
    assert_eq!(whole.stdout, b">chr2\nacgtACGT\n");

    let range = run(&[
        "fetch-genomics",
        path_str(&archive),
        "chr1",
        "--start",
        "9",
        "--end",
        "12",
    ]);
    assert_eq!(range.stdout, b">chr1 first\nACGT\n");

    let missing = run(&["fetch-genomics", path_str(&archive), "chrX"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no record named"));
}

#[test]
fn a_refused_input_leaves_no_archive() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("crlf.fa");
    let archive = dir.path().join("crlf.tpgn");
    fs::write(&input, ">a\r\nACGT\r\n").unwrap();

    let out = run(&["pack-genomics", path_str(&input), path_str(&archive)]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("CR characters"));
    assert!(!archive.exists());
}

#[test]
fn the_wrong_unpack_command_names_the_right_one() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("in.fa");
    let archive = dir.path().join("in.tpgn");
    fs::write(&input, FASTA).unwrap();
    run(&["pack-genomics", path_str(&input), path_str(&archive)]);

    let out = run(&[
        "unpack",
        path_str(&archive),
        path_str(&dir.path().join("x")),
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unpack-genomics"));
}

#[test]
fn npy_arrays_round_trip_and_slice() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("a.npy");
    let archive = dir.path().join("a.tpar");
    let back = dir.path().join("back.npy");
    fs::write(&input, npy_file()).unwrap();

    let packed = run(&["pack-arrays", path_str(&input), path_str(&archive)]);
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let unpacked = run(&["unpack-arrays", path_str(&archive), path_str(&back)]);
    assert!(unpacked.status.success());
    assert_eq!(fs::read(&back).unwrap(), fs::read(&input).unwrap());

    // Row 1, columns 1 and 2: values 5 and 6, as little-endian int16.
    let slice = run(&[
        "slice-arrays",
        path_str(&archive),
        "--range",
        "1:2",
        "--range",
        "1:3",
    ]);
    assert!(slice.status.success());
    assert_eq!(
        slice.stdout,
        [5i16.to_le_bytes(), 6i16.to_le_bytes()].concat()
    );
}

#[test]
fn raw_arrays_need_a_dtype_and_a_shape() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("raw.bin");
    let archive = dir.path().join("raw.tpar");
    fs::write(&input, [0u8; 8]).unwrap();

    let missing = run(&["pack-arrays", path_str(&input), path_str(&archive)]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--dtype"));
    assert!(!archive.exists());

    let packed = run(&[
        "pack-arrays",
        path_str(&input),
        path_str(&archive),
        "--dtype",
        "f4",
        "--shape",
        "2",
    ]);
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let info = run(&["inspect", path_str(&archive)]);
    assert!(String::from_utf8_lossy(&info.stdout).contains("npy header:    none"));
}

#[test]
fn slices_outside_the_array_are_refused() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("a.npy");
    let archive = dir.path().join("a.tpar");
    fs::write(&input, npy_file()).unwrap();
    run(&["pack-arrays", path_str(&input), path_str(&archive)]);

    let out = run(&[
        "slice-arrays",
        path_str(&archive),
        "--range",
        "0:4",
        "--range",
        "0:4",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not fit axis 0"));
}

#[cfg(feature = "strata")]
#[test]
fn sql_prints_a_result_table() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("in.jsonl");
    let archive = dir.path().join("in.tplg");
    fs::write(
        &input,
        "{\"timestamp\":\"2026-10-13T00:00:00Z\",\"level\":\"warn\",\"n\":1}\n\
         {\"timestamp\":\"2026-10-13T00:00:01Z\",\"level\":\"info\",\"n\":2}\n",
    )
    .unwrap();
    run(&["pack-logs", path_str(&input), path_str(&archive)]);

    let out = run(&[
        "query",
        path_str(&archive),
        "--sql",
        "SELECT SUM(n) FROM logs WHERE level = 'info'",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("SUM(n)"), "{text}");
    assert!(text.contains('2'), "{text}");
}

#[cfg(not(feature = "strata"))]
#[test]
fn sql_explains_the_feature_when_it_is_not_built_in() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("in.jsonl");
    let archive = dir.path().join("in.tplg");
    fs::write(&input, "{\"timestamp\":\"2026-10-13T00:00:00Z\",\"n\":1}\n").unwrap();
    run(&["pack-logs", path_str(&input), path_str(&archive)]);

    let out = run(&["query", path_str(&archive), "--sql", "SELECT n FROM logs"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("strata"));
}
