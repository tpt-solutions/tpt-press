//! End-to-end tests that run the built `tpt-press` binary.

use std::fs;
use std::path::{Path, PathBuf};
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

fn sample_file(dir: &Path, name: &str, len: usize) -> PathBuf {
    let path = dir.join(name);
    let data: Vec<u8> = (0..len).map(|i| ((i * 13) % 251) as u8).collect();
    fs::write(&path, data).unwrap();
    path
}

#[test]
fn pack_then_unpack_restores_the_file() {
    let dir = TempDir::new().unwrap();
    let input = sample_file(dir.path(), "input.bin", 3 * 1024 * 1024 + 7);
    let archive = dir.path().join("input.tppr");
    let output = dir.path().join("restored.bin");

    let packed = run(&["pack", path_str(&input), path_str(&archive)]);
    assert!(packed.status.success(), "{packed:?}");
    assert!(String::from_utf8_lossy(&packed.stdout).contains("packed"));

    let unpacked = run(&["unpack", path_str(&archive), path_str(&output)]);
    assert!(unpacked.status.success(), "{unpacked:?}");

    assert_eq!(fs::read(&input).unwrap(), fs::read(&output).unwrap());
}

#[test]
fn corrupt_archive_fails_and_leaves_no_output() {
    let dir = TempDir::new().unwrap();
    let input = sample_file(dir.path(), "input.bin", 100_000);
    let archive = dir.path().join("input.tppr");
    let output = dir.path().join("restored.bin");
    assert!(run(&["pack", path_str(&input), path_str(&archive)])
        .status
        .success());

    let mut bytes = fs::read(&archive).unwrap();
    let last = bytes.len() - 20;
    bytes[last] ^= 0xff;
    fs::write(&archive, bytes).unwrap();

    let unpacked = run(&["unpack", path_str(&archive), path_str(&output)]);
    assert!(!unpacked.status.success());
    assert!(!output.exists());
}

#[test]
fn truncated_archive_reports_truncation() {
    let dir = TempDir::new().unwrap();
    let input = sample_file(dir.path(), "input.bin", 100_000);
    let archive = dir.path().join("input.tppr");
    let output = dir.path().join("restored.bin");
    assert!(run(&["pack", path_str(&input), path_str(&archive)])
        .status
        .success());

    let bytes = fs::read(&archive).unwrap();
    fs::write(&archive, &bytes[..bytes.len() / 2]).unwrap();

    let unpacked = run(&["unpack", path_str(&archive), path_str(&output)]);
    assert!(!unpacked.status.success());
    assert!(String::from_utf8_lossy(&unpacked.stderr).contains("truncated"));
    assert!(!output.exists());
}

#[test]
fn missing_input_is_an_error() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("does-not-exist.bin");
    let archive = dir.path().join("out.tppr");

    let packed = run(&["pack", path_str(&missing), path_str(&archive)]);
    assert!(!packed.status.success());
    assert!(String::from_utf8_lossy(&packed.stderr).contains("cannot read"));
    assert!(!archive.exists());
}

#[test]
fn out_of_range_level_is_rejected_by_the_parser() {
    let dir = TempDir::new().unwrap();
    let input = sample_file(dir.path(), "input.bin", 10);
    let archive = dir.path().join("out.tppr");

    let packed = run(&[
        "pack",
        path_str(&input),
        path_str(&archive),
        "--level",
        "30",
    ]);
    assert_eq!(packed.status.code(), Some(2));
    assert!(!archive.exists());
}
