//! Archive type detection: `inspect` reads every archive type from its magic,
//! and `unpack` points at the right subcommand for the specialised types.

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

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// One archive of each type, packed from small inputs. Returns their paths.
struct Archives {
    generic: PathBuf,
    logs: PathBuf,
    tensors: PathBuf,
    assets: PathBuf,
}

fn make_archives(dir: &Path) -> Archives {
    let input = dir.join("input.bin");
    fs::write(&input, b"hello hello hello hello".repeat(100)).unwrap();
    let generic = dir.join("generic.tppr");
    assert!(run(&["pack", path_str(&input), path_str(&generic)])
        .status
        .success());

    let log = dir.join("log.jsonl");
    let lines: String = (0..50)
        .map(|i| {
            format!(
                "{{\"timestamp\":\"2026-01-01T00:{:02}:00Z\",\"level\":\"info\",\"n\":{i}}}\n",
                i % 60
            )
        })
        .collect();
    fs::write(&log, lines).unwrap();
    let logs = dir.join("log.tplg");
    assert!(run(&["pack-logs", path_str(&log), path_str(&logs)])
        .status
        .success());

    // A safetensors file with one F32 tensor of two elements.
    let header = br#"{"w":{"dtype":"F32","shape":[2],"data_offsets":[0,8]}}"#;
    let mut model = (header.len() as u64).to_le_bytes().to_vec();
    model.extend_from_slice(header);
    model.extend_from_slice(&[0u8; 8]);
    let model_path = dir.join("model.safetensors");
    fs::write(&model_path, model).unwrap();
    let tensors = dir.join("model.tptn");
    assert!(
        run(&["pack-tensors", path_str(&model_path), path_str(&tensors)])
            .status
            .success()
    );

    let tree = dir.join("tree");
    fs::create_dir_all(&tree).unwrap();
    fs::write(tree.join("a.txt"), b"alpha".repeat(40)).unwrap();
    fs::write(tree.join("b.txt"), b"beta").unwrap();
    let assets = dir.join("tree.tpa");
    assert!(run(&["pack-assets", path_str(&tree), path_str(&assets)])
        .status
        .success());

    Archives {
        generic,
        logs,
        tensors,
        assets,
    }
}

#[test]
fn inspect_identifies_every_archive_type() {
    let dir = TempDir::new().unwrap();
    let a = make_archives(dir.path());

    let generic = run(&["inspect", path_str(&a.generic)]);
    assert!(generic.status.success(), "{generic:?}");
    assert!(stdout(&generic).contains("chunks:"));

    let logs = run(&["inspect", path_str(&a.logs)]);
    assert!(logs.status.success(), "{logs:?}");
    assert!(stdout(&logs).contains("format:"));

    let tensors = run(&["inspect", path_str(&a.tensors), "--verify"]);
    assert!(tensors.status.success(), "{tensors:?}");
    assert!(stdout(&tensors).contains("tensors:        1"));
    assert!(stdout(&tensors).contains("all checksums match"));

    let assets = run(&["inspect", path_str(&a.assets), "--verify"]);
    assert!(assets.status.success(), "{assets:?}");
    assert!(stdout(&assets).contains("assets:         2"));
    assert!(stdout(&assets).contains("all 2 checksums match"));
}

#[test]
fn inspect_rejects_chunks_for_non_generic_archives() {
    let dir = TempDir::new().unwrap();
    let a = make_archives(dir.path());

    for archive in [&a.logs, &a.tensors, &a.assets] {
        let out = run(&["inspect", path_str(archive), "--chunks"]);
        assert!(!out.status.success(), "{archive:?} accepted --chunks");
        assert!(stderr(&out).contains("--chunks applies to generic archives"));
    }
}

#[test]
fn unpack_points_specialised_archives_at_the_right_command() {
    let dir = TempDir::new().unwrap();
    let a = make_archives(dir.path());
    let target = dir.path().join("out.bin");

    for (archive, command) in [
        (&a.logs, "query"),
        (&a.tensors, "unpack-tensors"),
        (&a.assets, "unpack-assets"),
    ] {
        let out = run(&["unpack", path_str(archive), path_str(&target)]);
        assert!(!out.status.success(), "{archive:?} unpacked as generic");
        let message = stderr(&out);
        assert!(message.contains(&format!("use `{command}`")), "{message}");
        assert!(!target.exists(), "no output may be left behind");
    }
}

#[test]
fn unpack_still_restores_generic_archives() {
    let dir = TempDir::new().unwrap();
    let a = make_archives(dir.path());
    let restored = dir.path().join("restored.bin");

    let out = run(&["unpack", path_str(&a.generic), path_str(&restored)]);
    assert!(out.status.success(), "{out:?}");
    let original = fs::read(dir.path().join("input.bin")).unwrap();
    assert_eq!(fs::read(&restored).unwrap(), original);
}
