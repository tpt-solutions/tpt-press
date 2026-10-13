//! End-to-end tests for `pack-logs` and `query`.

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

fn rfc3339(seconds: i64) -> String {
    format!(
        "2026-10-13T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

/// 400 JSON log lines, one every 30 seconds, starting at 00:00:00.
fn write_jsonl(dir: &Path) -> PathBuf {
    let path = dir.join("app.jsonl");
    let text: String = (0..400)
        .map(|i| {
            let level = if i % 9 == 0 { "error" } else { "info" };
            format!(
                "{{\"timestamp\":\"{}\",\"level\":\"{}\",\"msg\":\"event {}\"}}\n",
                rfc3339(i * 30),
                level,
                i
            )
        })
        .collect();
    fs::write(&path, text).unwrap();
    path
}

fn write_csv(dir: &Path) -> PathBuf {
    let path = dir.join("app.csv");
    let mut text = String::from("timestamp,level,msg\n");
    for i in 0..400 {
        text.push_str(&format!(
            "{},{},\"event {}, with comma\"\n",
            rfc3339(i * 30),
            if i % 9 == 0 { "error" } else { "info" },
            i
        ));
    }
    fs::write(&path, text).unwrap();
    path
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn pack_logs_then_query_by_time_range() {
    let dir = TempDir::new().unwrap();
    let input = write_jsonl(dir.path());
    let archive = dir.path().join("app.tplg");

    let packed = run(&["pack-logs", path_str(&input), path_str(&archive)]);
    assert!(packed.status.success(), "{packed:?}");
    assert!(stdout(&packed).contains("packed 400 records"));

    // Records 10..=20 fall in 00:05:00 to 00:10:00.
    let out = run(&[
        "query",
        path_str(&archive),
        "--from",
        "2026-10-13T00:05:00Z",
        "--to",
        "2026-10-13T00:10:00Z",
    ]);
    assert!(out.status.success(), "{out:?}");
    let lines: Vec<String> = stdout(&out).lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 11, "{}", stdout(&out));
    assert!(lines[0].contains("event 10"));
    assert!(stderr(&out).contains("matched 11 records"));
}

#[test]
fn query_projects_columns() {
    let dir = TempDir::new().unwrap();
    let input = write_jsonl(dir.path());
    let archive = dir.path().join("app.tplg");
    assert!(run(&["pack-logs", path_str(&input), path_str(&archive)])
        .status
        .success());

    let out = run(&["query", path_str(&archive), "--columns", "level,msg"]);
    assert!(out.status.success(), "{out:?}");
    let first = stdout(&out).lines().next().unwrap().to_owned();
    // Record 0 is an error, because every ninth record is one.
    assert_eq!(first, r#"{"level":"error","msg":"event 0"}"#);
}

#[test]
fn csv_is_detected_by_extension() {
    let dir = TempDir::new().unwrap();
    let input = write_csv(dir.path());
    let archive = dir.path().join("app.tplg");
    assert!(run(&["pack-logs", path_str(&input), path_str(&archive)])
        .status
        .success());

    let out = run(&[
        "query",
        path_str(&archive),
        "--columns",
        "msg,level",
        "--from",
        "2026-10-13T00:00:00Z",
        "--to",
        "2026-10-13T00:00:30Z",
    ]);
    assert!(out.status.success(), "{out:?}");
    let lines: Vec<String> = stdout(&out).lines().map(str::to_owned).collect();
    assert_eq!(
        lines,
        vec![
            "\"event 0, with comma\",error".to_owned(),
            "\"event 1, with comma\",info".to_owned(),
        ]
    );
}

#[test]
fn unknown_csv_column_fails() {
    let dir = TempDir::new().unwrap();
    let input = write_csv(dir.path());
    let archive = dir.path().join("app.tplg");
    assert!(run(&["pack-logs", path_str(&input), path_str(&archive)])
        .status
        .success());

    let out = run(&["query", path_str(&archive), "--columns", "nope"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("unknown column"));
}

#[test]
fn bad_timestamp_is_reported() {
    let dir = TempDir::new().unwrap();
    let input = write_jsonl(dir.path());
    let archive = dir.path().join("app.tplg");
    assert!(run(&["pack-logs", path_str(&input), path_str(&archive)])
        .status
        .success());

    let out = run(&["query", path_str(&archive), "--from", "yesterday"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("invalid timestamp"));
}

#[test]
fn query_rejects_a_plain_archive() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("plain.txt");
    fs::write(&input, b"just some bytes").unwrap();
    let archive = dir.path().join("plain.tppr");
    assert!(run(&["pack", path_str(&input), path_str(&archive)])
        .status
        .success());

    let out = run(&["query", path_str(&archive)]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("bad magic"));
}
