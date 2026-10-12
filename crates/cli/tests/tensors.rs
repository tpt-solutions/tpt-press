//! End-to-end tests for `pack-tensors`, `unpack-tensors` and `inspect` on tensor archives.

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

/// A two-tensor safetensors file. Tensors are laid out in name order, so the
/// data section of the unpacked file matches this one byte for byte.
fn write_model(dir: &Path) -> (PathBuf, Vec<u8>) {
    let weight: Vec<u8> = (0..4096u32)
        .flat_map(|i| (i as f32 * 0.25).to_le_bytes())
        .collect();
    let bias: Vec<u8> = (0..32u16).flat_map(|i| (i * 3).to_le_bytes()).collect();
    let mut data = bias.clone();
    data.extend_from_slice(&weight);

    let header = format!(
        r#"{{"__metadata__":{{"format":"pt"}},"bias":{{"dtype":"U16","shape":[32],"data_offsets":[0,{}]}},"weight":{{"dtype":"F32","shape":[32,128],"data_offsets":[{},{}]}}}}"#,
        bias.len(),
        bias.len(),
        bias.len() + weight.len()
    );
    let mut file = (header.len() as u64).to_le_bytes().to_vec();
    file.extend_from_slice(header.as_bytes());
    file.extend_from_slice(&data);

    let path = dir.join("model.safetensors");
    fs::write(&path, &file).unwrap();
    (path, data)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn pack_inspect_and_unpack_round_trip() {
    let dir = TempDir::new().unwrap();
    let (model, data) = write_model(dir.path());
    let archive = dir.path().join("model.tptn");
    let restored = dir.path().join("restored.safetensors");

    let packed = run(&["pack-tensors", path_str(&model), path_str(&archive)]);
    assert!(packed.status.success(), "{packed:?}");
    assert!(stdout(&packed).contains("packed 2 tensors"));

    let inspected = run(&["inspect", path_str(&archive), "--verify"]);
    assert!(inspected.status.success(), "{inspected:?}");
    let text = stdout(&inspected);
    assert!(text.contains("tensors:        2"), "{text}");
    assert!(text.contains("parameters:     4128"), "{text}");
    assert!(text.contains("format = pt"), "{text}");
    assert!(text.contains("all checksums match"), "{text}");

    let unpacked = run(&["unpack-tensors", path_str(&archive), path_str(&restored)]);
    assert!(unpacked.status.success(), "{unpacked:?}");

    let restored_bytes = fs::read(&restored).unwrap();
    let data_start = restored_bytes.len() - data.len();
    assert_eq!(&restored_bytes[data_start..], data.as_slice());
}

#[test]
fn inspect_verify_catches_damage() {
    let dir = TempDir::new().unwrap();
    let (model, _) = write_model(dir.path());
    let archive = dir.path().join("model.tptn");
    assert!(run(&["pack-tensors", path_str(&model), path_str(&archive)])
        .status
        .success());

    let mut bytes = fs::read(&archive).unwrap();
    bytes[8 + 40] ^= 0x01;
    fs::write(&archive, bytes).unwrap();

    let inspected = run(&["inspect", path_str(&archive), "--verify"]);
    assert!(!inspected.status.success());
}

#[test]
fn pack_rejects_a_file_that_is_not_safetensors() {
    let dir = TempDir::new().unwrap();
    let plain = dir.path().join("plain.bin");
    fs::write(&plain, b"plain bytes, definitely not a model file").unwrap();
    let archive = dir.path().join("out.tptn");

    let packed = run(&["pack-tensors", path_str(&plain), path_str(&archive)]);
    assert!(!packed.status.success());
    assert!(String::from_utf8_lossy(&packed.stderr).contains("safetensors"));
    assert!(!archive.exists());
}
