//! End-to-end tests for `pack --content-defined` and `pack --dictionary`.

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

fn repetitive_log(lines: usize) -> Vec<u8> {
    (0..lines)
        .map(|i| {
            format!(
                "{{\"level\":\"info\",\"service\":\"api\",\"route\":\"/v1/items\",\"latency_ms\":{}}}\n",
                (i * 31) % 500
            )
        })
        .collect::<String>()
        .into_bytes()
}

#[test]
fn content_defined_and_dictionary_packs_round_trip() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("app.log");
    let data = repetitive_log(40_000);
    fs::write(&input, &data).unwrap();

    let archive = dir.path().join("app.tppr");
    let packed = run(&[
        "pack",
        path_str(&input),
        path_str(&archive),
        "--content-defined",
        "--chunk-size",
        "8192",
        "--dictionary",
        "65536",
    ]);
    assert!(packed.status.success(), "{packed:?}");

    let inspected = run(&["inspect", path_str(&archive)]);
    assert!(
        stdout(&inspected).contains("dictionary:"),
        "{}",
        stdout(&inspected)
    );

    let restored = dir.path().join("restored.log");
    let out = run(&["unpack", path_str(&archive), path_str(&restored)]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(fs::read(&restored).unwrap(), data);
}

#[test]
fn a_content_defined_average_must_be_a_power_of_two() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("in.bin");
    fs::write(&input, b"data").unwrap();
    let archive = dir.path().join("out.tppr");

    let out = run(&[
        "pack",
        path_str(&input),
        path_str(&archive),
        "--content-defined",
        "--chunk-size",
        "3000",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("power of two"));
    assert!(!archive.exists());
}

#[test]
fn unpack_max_output_refuses_and_leaves_no_file() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("big.bin");
    fs::write(&input, vec![7u8; 300_000]).unwrap();
    let archive = dir.path().join("big.tppr");
    assert!(run(&["pack", path_str(&input), path_str(&archive)])
        .status
        .success());

    let target = dir.path().join("out.bin");
    let out = run(&[
        "unpack",
        path_str(&archive),
        path_str(&target),
        "--max-output",
        "1000",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("more than 1000 bytes"));
    assert!(!target.exists());

    let ok = run(&[
        "unpack",
        path_str(&archive),
        path_str(&target),
        "--max-output",
        "300000",
    ]);
    assert!(ok.status.success(), "{ok:?}");
}
