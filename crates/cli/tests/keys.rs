//! End-to-end tests for public-key encryption: `keygen`, `pack --recipient`,
//! and `unpack` or `inspect` with `--identity`.

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

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Creates an identity file and returns its public key.
fn keygen(path: &Path) -> String {
    let out = run(&["keygen", path_str(path)]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines()
        .find_map(|l| l.strip_prefix("public key: "))
        .expect("keygen prints the public key")
        .to_owned()
}

#[test]
fn keygen_writes_an_identity_and_refuses_to_overwrite_it() {
    let dir = TempDir::new().unwrap();
    let identity = dir.path().join("me.key");
    let public = keygen(&identity);
    assert!(public.starts_with("age1"), "{public}");

    let text = fs::read_to_string(&identity).unwrap();
    assert!(text.contains("AGE-SECRET-KEY-1"));

    let again = run(&["keygen", path_str(&identity)]);
    assert!(!again.status.success());
    assert!(stderr(&again).contains("cannot create"));
    // The original key is untouched.
    assert_eq!(fs::read_to_string(&identity).unwrap(), text);
}

#[test]
fn encrypted_to_a_key_opens_only_with_that_key() {
    let dir = TempDir::new().unwrap();
    let data = b"secret data ".repeat(5000);
    let input = dir.path().join("input.bin");
    fs::write(&input, &data).unwrap();
    let alice = dir.path().join("alice.key");
    let bob = dir.path().join("bob.key");
    let alice_public = keygen(&alice);
    keygen(&bob);

    let archive = dir.path().join("out.tppr");
    let packed = run(&[
        "pack",
        path_str(&input),
        path_str(&archive),
        "--recipient",
        &alice_public,
    ]);
    assert!(packed.status.success(), "{packed:?}");
    assert!(String::from_utf8_lossy(&packed.stdout).contains("encrypted to public keys"));

    let restored = dir.path().join("restored.bin");
    let out = run(&[
        "unpack",
        path_str(&archive),
        path_str(&restored),
        "--identity",
        path_str(&alice),
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(fs::read(&restored).unwrap(), data);

    let refused = run(&[
        "unpack",
        path_str(&archive),
        path_str(&dir.path().join("nope.bin")),
        "--identity",
        path_str(&bob),
    ]);
    assert!(!refused.status.success());
    assert!(stderr(&refused).contains("not encrypted for this key"));

    let inspected = run(&[
        "inspect",
        path_str(&archive),
        "--identity",
        path_str(&alice),
    ]);
    assert!(inspected.status.success(), "{inspected:?}");
}

#[test]
fn recipient_file_lists_keys_and_ignores_comments() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.bin");
    fs::write(&input, b"hello").unwrap();
    let identity = dir.path().join("id.key");
    let public = keygen(&identity);

    let recipients = dir.path().join("recipients.txt");
    fs::write(&recipients, format!("# team\n\n{public}\n")).unwrap();
    let archive = dir.path().join("out.tppr");
    let packed = run(&[
        "pack",
        path_str(&input),
        path_str(&archive),
        "--recipient-file",
        path_str(&recipients),
    ]);
    assert!(packed.status.success(), "{packed:?}");

    let restored = dir.path().join("restored.bin");
    let out = run(&[
        "unpack",
        path_str(&archive),
        path_str(&restored),
        "--identity",
        path_str(&identity),
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(fs::read(&restored).unwrap(), b"hello");
}

#[test]
fn bad_keys_and_conflicting_flags_are_rejected() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.bin");
    fs::write(&input, b"hello").unwrap();
    let archive = dir.path().join("out.tppr");

    let bad = run(&[
        "pack",
        path_str(&input),
        path_str(&archive),
        "--recipient",
        "age1nonsense",
    ]);
    assert!(!bad.status.success());
    assert!(stderr(&bad).contains("bad recipient key"));
    assert!(!archive.exists(), "a failed pack must leave no output");

    let conflict = run(&[
        "pack",
        path_str(&input),
        path_str(&archive),
        "--encrypt",
        "--recipient",
        "age1nonsense",
    ]);
    assert!(!conflict.status.success());
    assert!(stderr(&conflict).contains("cannot be used with"));

    // A real archive, so the identity file is what fails.
    assert!(run(&["pack", path_str(&input), path_str(&archive)])
        .status
        .success());
    let empty = dir.path().join("empty.key");
    fs::write(&empty, "# nothing here\n").unwrap();
    let no_keys = run(&[
        "unpack",
        path_str(&archive),
        path_str(&dir.path().join("x.bin")),
        "--identity",
        path_str(&empty),
    ]);
    assert!(!no_keys.status.success());
    assert!(stderr(&no_keys).contains("has no keys"));
}
