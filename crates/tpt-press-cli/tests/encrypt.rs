//! End-to-end tests for encrypted archives, using `--passphrase-file`.
//!
//! Prompting is not tested here. It needs a real terminal, and on Windows
//! `rpassword` reads the console directly, which could hang a test run.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const MARKER: &[u8] = b"CLI-PLAINTEXT-MARKER";

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tpt-press"))
        .args(args)
        .output()
        .expect("failed to run tpt-press")
}

fn path_str(p: &Path) -> &str {
    p.to_str().expect("test paths are valid UTF-8")
}

/// Writes a passphrase file and returns its path.
fn passphrase_file(dir: &Path, name: &str, passphrase: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("{passphrase}\n")).unwrap();
    path
}

/// Writes a sample file containing a known marker, and returns its path.
fn sample_file(dir: &Path) -> PathBuf {
    let path = dir.join("input.bin");
    let mut data = MARKER.repeat(500);
    data.extend((0..300_000usize).map(|i| ((i * 17) % 251) as u8));
    fs::write(&path, data).unwrap();
    path
}

#[test]
fn encrypted_round_trip_via_passphrase_file() {
    let dir = TempDir::new().unwrap();
    let input = sample_file(dir.path());
    let archive = dir.path().join("input.tppr");
    let output = dir.path().join("restored.bin");
    let pass = passphrase_file(dir.path(), "pass.txt", "hunter2-test-phrase");

    let packed = run(&[
        "pack",
        path_str(&input),
        path_str(&archive),
        "--passphrase-file",
        path_str(&pass),
    ]);
    assert!(packed.status.success(), "{packed:?}");
    assert!(String::from_utf8_lossy(&packed.stdout).contains("encrypted"));

    let archive_bytes = fs::read(&archive).unwrap();
    assert!(
        !archive_bytes.windows(MARKER.len()).any(|w| w == MARKER),
        "plaintext visible in encrypted archive"
    );

    let unpacked = run(&[
        "unpack",
        path_str(&archive),
        path_str(&output),
        "--passphrase-file",
        path_str(&pass),
    ]);
    assert!(unpacked.status.success(), "{unpacked:?}");
    assert_eq!(fs::read(&input).unwrap(), fs::read(&output).unwrap());
}

#[test]
fn wrong_passphrase_fails_and_leaves_no_output() {
    let dir = TempDir::new().unwrap();
    let input = sample_file(dir.path());
    let archive = dir.path().join("input.tppr");
    let output = dir.path().join("restored.bin");
    let right = passphrase_file(dir.path(), "right.txt", "the-right-passphrase");
    let wrong = passphrase_file(dir.path(), "wrong.txt", "the-wrong-passphrase");

    assert!(run(&[
        "pack",
        path_str(&input),
        path_str(&archive),
        "--passphrase-file",
        path_str(&right),
    ])
    .status
    .success());

    let unpacked = run(&[
        "unpack",
        path_str(&archive),
        path_str(&output),
        "--passphrase-file",
        path_str(&wrong),
    ]);
    assert!(!unpacked.status.success());
    assert!(String::from_utf8_lossy(&unpacked.stderr).contains("decryption failed"));
    assert!(!output.exists());
}

#[test]
fn inspect_reads_encrypted_archive_with_passphrase_file() {
    let dir = TempDir::new().unwrap();
    let input = sample_file(dir.path());
    let archive = dir.path().join("input.tppr");
    let pass = passphrase_file(dir.path(), "pass.txt", "inspect-test-phrase");

    assert!(run(&[
        "pack",
        path_str(&input),
        path_str(&archive),
        "--passphrase-file",
        path_str(&pass),
    ])
    .status
    .success());

    let out = run(&[
        "inspect",
        path_str(&archive),
        "--passphrase-file",
        path_str(&pass),
    ]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("uncompressed:  310000 bytes"), "{stdout}");
}

#[test]
fn empty_passphrase_file_is_rejected() {
    let dir = TempDir::new().unwrap();
    let input = sample_file(dir.path());
    let archive = dir.path().join("input.tppr");
    let empty = dir.path().join("empty.txt");
    fs::write(&empty, "\n").unwrap();

    let packed = run(&[
        "pack",
        path_str(&input),
        path_str(&archive),
        "--passphrase-file",
        path_str(&empty),
    ]);
    assert!(!packed.status.success());
    assert!(String::from_utf8_lossy(&packed.stderr).contains("empty"));
    assert!(!archive.exists());
}
