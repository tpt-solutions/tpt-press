//! End-to-end tests for `pack-tensors --shard-size`, and reading the result
//! with `inspect`, `query` and `unpack`.

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

/// A safetensors model with three F32 tensors of 1000 values each.
fn model(dir: &Path) -> (PathBuf, Vec<(String, Vec<u8>)>) {
    let mut header = String::from("{");
    let mut data = Vec::new();
    let mut expected = Vec::new();
    for (i, name) in ["embed", "attn", "mlp"].iter().enumerate() {
        let values: Vec<f32> = (0..1000).map(|j| (i * 1000 + j) as f32 * 0.5).collect();
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        if i > 0 {
            header.push(',');
        }
        header.push_str(&format!(
            "\"{name}\":{{\"dtype\":\"F32\",\"shape\":[1000],\"data_offsets\":[{},{}]}}",
            data.len(),
            data.len() + bytes.len()
        ));
        data.extend_from_slice(&bytes);
        expected.push((name.to_string(), bytes));
    }
    header.push('}');
    let mut file = (header.len() as u64).to_le_bytes().to_vec();
    file.extend_from_slice(header.as_bytes());
    file.extend_from_slice(&data);
    let path = dir.join("model.safetensors");
    fs::write(&path, file).unwrap();
    (path, expected)
}

#[test]
fn sharded_pack_inspect_query_round_trip() {
    let dir = TempDir::new().unwrap();
    let (input, expected) = model(dir.path());
    let out_dir = dir.path().join("sharded");

    // Each tensor is 4000 bytes, so a 5000-byte shard holds one tensor.
    let packed = run(&[
        "pack-tensors",
        path_str(&input),
        path_str(&out_dir),
        "--shard-size",
        "5000",
    ]);
    assert!(packed.status.success(), "{packed:?}");
    assert!(stdout(&packed).contains("into 3 shards"));
    assert!(out_dir.join("manifest.json").exists());
    assert!(out_dir.join("shard-000.tptn").exists());

    let inspected = run(&["inspect", path_str(&out_dir), "--tensors", "--verify"]);
    assert!(inspected.status.success(), "{inspected:?}");
    let text = stdout(&inspected);
    assert!(text.contains("shards:         3"), "{text}");
    assert!(text.contains("tensors:        3"), "{text}");
    assert!(text.contains("all checksums match"), "{text}");

    for (name, bytes) in &expected {
        let out = run(&["query", path_str(&out_dir), name]);
        assert!(out.status.success(), "{out:?}");
        assert_eq!(&out.stdout, bytes, "tensor {name}");
    }

    let missing = run(&["query", path_str(&out_dir), "nope"]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("no tensor named `nope`"));
}

#[test]
fn unpacking_a_sharded_directory_points_at_query() {
    let dir = TempDir::new().unwrap();
    let (input, _) = model(dir.path());
    let out_dir = dir.path().join("sharded");
    assert!(run(&[
        "pack-tensors",
        path_str(&input),
        path_str(&out_dir),
        "--shard-size",
        "1000000"
    ])
    .status
    .success());

    let out = run(&[
        "unpack",
        path_str(&out_dir),
        path_str(&dir.path().join("x.bin")),
    ]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("query <directory> <name>"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_damaged_shard_fails_verification() {
    let dir = TempDir::new().unwrap();
    let (input, _) = model(dir.path());
    let out_dir = dir.path().join("sharded");
    assert!(run(&[
        "pack-tensors",
        path_str(&input),
        path_str(&out_dir),
        "--shard-size",
        "5000"
    ])
    .status
    .success());

    let shard = out_dir.join("shard-001.tptn");
    let mut bytes = fs::read(&shard).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    fs::write(&shard, bytes).unwrap();

    let out = run(&["inspect", path_str(&out_dir), "--verify"]);
    assert!(!out.status.success());
}

#[test]
fn a_failed_shard_pack_leaves_no_directory() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("not-a-model.bin");
    fs::write(&input, b"this is not a model at all").unwrap();
    let out_dir = dir.path().join("sharded");

    let out = run(&[
        "pack-tensors",
        path_str(&input),
        path_str(&out_dir),
        "--shard-size",
        "100",
    ]);
    assert!(!out.status.success());
    assert!(!out_dir.exists());
}
