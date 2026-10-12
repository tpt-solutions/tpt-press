//! End-to-end tests for `inspect` on logs archives.

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

fn pack_sample(dir: &Path) -> std::path::PathBuf {
    let input = dir.join("app.jsonl");
    let text: String = (0..400)
        .map(|i| {
            let s = i * 30;
            format!(
                "{{\"timestamp\":\"2026-10-13T{:02}:{:02}:{:02}Z\",\"level\":\"info\",\"msg\":\"event {i}\"}}\n",
                s / 3600,
                (s / 60) % 60,
                s % 60
            )
        })
        .collect();
    fs::write(&input, text).unwrap();

    let archive = dir.join("app.tplg");
    let packed = run(&["pack-logs", path_str(&input), path_str(&archive)]);
    assert!(packed.status.success(), "{packed:?}");
    archive
}

#[test]
fn inspect_summarizes_a_logs_archive() {
    let dir = TempDir::new().unwrap();
    let archive = pack_sample(dir.path());

    let out = run(&["inspect", path_str(&archive)]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("format:         jsonl"), "{stdout}");
    assert!(stdout.contains("records:        400"), "{stdout}");
    assert!(
        stdout.contains("columns:        level, msg, timestamp"),
        "{stdout}"
    );
    assert!(
        stdout.contains("time range:     2026-10-13T00:00:00Z to 2026-10-13T03:19:30Z"),
        "{stdout}"
    );
}

#[test]
fn inspect_rejects_chunks_for_logs() {
    let dir = TempDir::new().unwrap();
    let archive = pack_sample(dir.path());

    let out = run(&["inspect", path_str(&archive), "--chunks"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--chunks"));
}
