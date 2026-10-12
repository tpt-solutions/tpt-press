//! End-to-end tests for the `inspect` subcommand.

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

/// Packs a 2.5 MiB file in 1 MiB chunks and returns the archive path.
fn make_archive(dir: &Path) -> std::path::PathBuf {
    let input = dir.join("input.bin");
    let data: Vec<u8> = (0..2_621_440usize)
        .map(|i| ((i * 13) % 251) as u8)
        .collect();
    fs::write(&input, data).unwrap();

    let archive = dir.join("input.tppr");
    let packed = run(&[
        "pack",
        path_str(&input),
        path_str(&archive),
        "--chunk-size",
        "1048576",
    ]);
    assert!(packed.status.success(), "{packed:?}");
    archive
}

#[test]
fn inspect_reports_summary() {
    let dir = TempDir::new().unwrap();
    let archive = make_archive(dir.path());

    let out = run(&["inspect", path_str(&archive)]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("chunks:        3"), "{stdout}");
    assert!(stdout.contains("uncompressed:  2621440 bytes"), "{stdout}");
    assert!(stdout.contains("ratio:"), "{stdout}");
    assert!(!stdout.contains("offset"), "table printed without --chunks");
}

#[test]
fn inspect_chunks_prints_one_row_per_chunk() {
    let dir = TempDir::new().unwrap();
    let archive = make_archive(dir.path());

    let out = run(&["inspect", path_str(&archive), "--chunks"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("offset"), "{stdout}");
    // Header row plus three chunk rows, after the summary and a blank line.
    let rows = stdout.lines().filter(|l| l.ends_with('x')).count();
    assert_eq!(
        rows,
        3 + 1,
        "expected summary ratio plus 3 chunk rows:\n{stdout}"
    );
}

#[test]
fn inspect_rejects_corrupt_archive() {
    let dir = TempDir::new().unwrap();
    let archive = make_archive(dir.path());

    let mut bytes = fs::read(&archive).unwrap();
    let at = 8 + 12 + 100;
    bytes[at] ^= 0xff;
    fs::write(&archive, bytes).unwrap();

    let out = run(&["inspect", path_str(&archive)]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("corrupt"));
}

#[test]
fn inspect_rejects_non_archive() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("plain.txt");
    fs::write(&file, b"hello there, not an archive").unwrap();

    let out = run(&["inspect", path_str(&file)]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("bad magic"));
}
