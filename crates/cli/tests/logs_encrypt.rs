//! End-to-end tests for encrypted logs archives: `pack-logs` with a passphrase
//! or recipients, then `inspect` and `query` with the matching credentials.

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

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn write_log(dir: &Path) -> std::path::PathBuf {
    let input = dir.join("app.jsonl");
    let lines: String = (0..200)
        .map(|i| {
            format!(
                "{{\"timestamp\":\"2026-04-01T00:{:02}:{:02}Z\",\"level\":\"{}\",\"n\":{i}}}\n",
                i / 60,
                i % 60,
                if i % 9 == 0 { "error" } else { "info" }
            )
        })
        .collect();
    fs::write(&input, lines).unwrap();
    input
}

#[test]
fn passphrase_encrypted_logs_inspect_and_query() {
    let dir = TempDir::new().unwrap();
    let input = write_log(dir.path());
    let passphrase = dir.path().join("pass.txt");
    fs::write(&passphrase, "hunter2 horse battery\n").unwrap();
    let archive = dir.path().join("app.tplg");

    let packed = run(&[
        "pack-logs",
        path_str(&input),
        path_str(&archive),
        "--passphrase-file",
        path_str(&passphrase),
    ]);
    assert!(packed.status.success(), "{packed:?}");
    assert!(stdout(&packed).contains("encrypted"));

    // The archive is not readable without the passphrase.
    let bytes = fs::read(&archive).unwrap();
    assert!(bytes.starts_with(b"age-encryption.org/v1"));

    let inspected = run(&[
        "inspect",
        path_str(&archive),
        "--passphrase-file",
        path_str(&passphrase),
    ]);
    assert!(inspected.status.success(), "{inspected:?}");
    assert!(
        stdout(&inspected).contains("format:         jsonl"),
        "{}",
        stdout(&inspected)
    );
    assert!(stdout(&inspected).contains("records:        200"));

    let queried = run(&[
        "query",
        path_str(&archive),
        "--from",
        "2026-04-01T00:00:00Z",
        "--to",
        "2026-04-01T00:02:59Z",
        "--columns",
        "level",
        "--passphrase-file",
        path_str(&passphrase),
    ]);
    assert!(queried.status.success(), "{queried:?}");
    // One record a second for the first three minutes: 180 of the 200 records.
    assert_eq!(stdout(&queried).lines().count(), 180);
}

#[test]
fn wrong_passphrase_is_refused() {
    let dir = TempDir::new().unwrap();
    let input = write_log(dir.path());
    let right = dir.path().join("right.txt");
    let wrong = dir.path().join("wrong.txt");
    fs::write(&right, "right passphrase").unwrap();
    fs::write(&wrong, "wrong passphrase").unwrap();
    let archive = dir.path().join("app.tplg");
    assert!(run(&[
        "pack-logs",
        path_str(&input),
        path_str(&archive),
        "--passphrase-file",
        path_str(&right)
    ])
    .status
    .success());

    let out = run(&[
        "query",
        path_str(&archive),
        "--passphrase-file",
        path_str(&wrong),
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("wrong passphrase"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn logs_encrypted_to_a_key_open_only_with_that_key() {
    let dir = TempDir::new().unwrap();
    let input = write_log(dir.path());
    let alice = dir.path().join("alice.key");
    let bob = dir.path().join("bob.key");
    let keygen = run(&["keygen", path_str(&alice)]);
    assert!(keygen.status.success());
    let public = stdout(&keygen)
        .lines()
        .find_map(|l| l.strip_prefix("public key: "))
        .expect("keygen prints the public key")
        .to_owned();
    assert!(run(&["keygen", path_str(&bob)]).status.success());

    let archive = dir.path().join("app.tplg");
    let packed = run(&[
        "pack-logs",
        path_str(&input),
        path_str(&archive),
        "--recipient",
        &public,
    ]);
    assert!(packed.status.success(), "{packed:?}");
    assert!(stdout(&packed).contains("encrypted to public keys"));

    let queried = run(&["query", path_str(&archive), "--identity", path_str(&alice)]);
    assert!(queried.status.success(), "{queried:?}");
    assert_eq!(stdout(&queried).lines().count(), 200);

    let refused = run(&["query", path_str(&archive), "--identity", path_str(&bob)]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("not encrypted for this key"));
}

#[test]
fn a_failed_encrypted_pack_leaves_no_archive() {
    let dir = TempDir::new().unwrap();
    let input = write_log(dir.path());
    let archive = dir.path().join("app.tplg");

    let out = run(&[
        "pack-logs",
        path_str(&input),
        path_str(&archive),
        "--recipient",
        "age1nonsense",
    ]);
    assert!(!out.status.success());
    assert!(!archive.exists());
}

#[test]
fn tensor_flags_do_not_apply_to_encrypted_logs() {
    let dir = TempDir::new().unwrap();
    let input = write_log(dir.path());
    let passphrase = dir.path().join("pass.txt");
    fs::write(&passphrase, "pw").unwrap();
    let archive = dir.path().join("app.tplg");
    assert!(run(&[
        "pack-logs",
        path_str(&input),
        path_str(&archive),
        "--passphrase-file",
        path_str(&passphrase)
    ])
    .status
    .success());

    let out = run(&[
        "inspect",
        path_str(&archive),
        "--chunks",
        "--passphrase-file",
        path_str(&passphrase),
    ]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--chunks applies to generic archives"));
}
