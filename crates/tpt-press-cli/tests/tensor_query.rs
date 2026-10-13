//! End-to-end tests for `inspect --tensors` and `query` on tensor archives,
//! including a PyTorch checkpoint packed with `pack-tensors`.

use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

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

/// A safetensors file with `w` (2x2 F32) and `b` (2 F32).
fn safetensors_model() -> Vec<u8> {
    let header = br#"{"b":{"dtype":"F32","shape":[2],"data_offsets":[0,8]},"w":{"dtype":"F32","shape":[2,2],"data_offsets":[8,24]}}"#;
    let mut out = (header.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(header);
    for v in [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// A `torch.save` checkpoint with one tensor, `w`: two f32 values, 1.5 and -2.25.
/// The pickle is what torch writes for a one-entry state dict, using the
/// opcodes the reader accepts.
fn pytorch_checkpoint() -> Vec<u8> {
    let pickle: &[u8] = b"\x80\x02ccollections\nOrderedDict\n)Rq\x00(\
        X\x01\x00\x00\x00w\
        ctorch._utils\n_rebuild_tensor_v2\n\
        ((X\x07\x00\x00\x00storagectorch\nFloatStorage\nX\x01\x00\x00\x000X\x03\x00\x00\x00cpuK\x02tQ\
        K\x00(K\x02t(K\x01t\x89ccollections\nOrderedDict\n)RtR\
        u.";
    let mut data = Vec::new();
    data.extend_from_slice(&1.5f32.to_le_bytes());
    data.extend_from_slice(&(-2.25f32).to_le_bytes());

    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    zip.start_file("ckpt/data.pkl", options).unwrap();
    zip.write_all(pickle).unwrap();
    zip.start_file("ckpt/data/0", options).unwrap();
    zip.write_all(&data).unwrap();
    zip.finish().unwrap().into_inner()
}

fn pack_tensor_archive(dir: &Path, model: &[u8]) -> std::path::PathBuf {
    let input = dir.join("model.bin");
    fs::write(&input, model).unwrap();
    let archive = dir.join("model.tptn");
    let packed = run(&["pack-tensors", path_str(&input), path_str(&archive)]);
    assert!(packed.status.success(), "{packed:?}");
    archive
}

#[test]
fn inspect_tensors_lists_every_tensor() {
    let dir = TempDir::new().unwrap();
    let archive = pack_tensor_archive(dir.path(), &safetensors_model());

    let out = run(&["inspect", path_str(&archive), "--tensors", "--verify"]);
    assert!(out.status.success(), "{out:?}");
    let text = stdout(&out);
    assert!(
        text.contains("b                                        F32      2"),
        "{text}"
    );
    assert!(
        text.contains("w                                        F32      2x2"),
        "{text}"
    );
    assert!(text.contains("all checksums match"));
}

#[test]
fn query_outputs_one_tensor_as_raw_bytes() {
    let dir = TempDir::new().unwrap();
    let archive = pack_tensor_archive(dir.path(), &safetensors_model());

    let out = run(&["query", path_str(&archive), "w"]);
    assert!(out.status.success(), "{out:?}");
    let expected: Vec<u8> = [3.0f32, 4.0, 5.0, 6.0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    assert_eq!(out.stdout, expected);
    assert!(stderr(&out).contains("tensor w: 16 bytes"));
}

#[test]
fn query_writes_to_a_file_with_output() {
    let dir = TempDir::new().unwrap();
    let archive = pack_tensor_archive(dir.path(), &safetensors_model());
    let target = dir.path().join("b.bin");

    let out = run(&[
        "query",
        path_str(&archive),
        "b",
        "--output",
        path_str(&target),
    ]);
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty());
    assert_eq!(fs::read(&target).unwrap().len(), 8);
}

#[test]
fn query_rejects_unknown_names_and_logs_only_flags() {
    let dir = TempDir::new().unwrap();
    let archive = pack_tensor_archive(dir.path(), &safetensors_model());

    let missing = run(&["query", path_str(&archive), "nope"]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("no tensor named `nope`"));

    let no_name = run(&["query", path_str(&archive)]);
    assert!(!no_name.status.success());
    assert!(stderr(&no_name).contains("needs a tensor name"));

    let wrong_flag = run(&[
        "query",
        path_str(&archive),
        "w",
        "--from",
        "2026-01-01T00:00:00Z",
    ]);
    assert!(!wrong_flag.status.success());
    assert!(stderr(&wrong_flag).contains("apply to logs archives"));
}

#[test]
fn a_pytorch_checkpoint_packs_and_reads_back() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("checkpoint.pt");
    fs::write(&input, pytorch_checkpoint()).unwrap();
    let archive = dir.path().join("checkpoint.tptn");

    let packed = run(&["pack-tensors", path_str(&input), path_str(&archive)]);
    assert!(packed.status.success(), "{packed:?}");

    let listed = run(&["inspect", path_str(&archive), "--tensors"]);
    assert!(stdout(&listed).contains("w"));
    assert!(stdout(&listed).contains("F32"));

    let out = run(&["query", path_str(&archive), "w"]);
    assert!(out.status.success(), "{out:?}");
    let mut expected = 1.5f32.to_le_bytes().to_vec();
    expected.extend_from_slice(&(-2.25f32).to_le_bytes());
    assert_eq!(out.stdout, expected);
}

#[test]
fn tensors_flag_is_refused_for_other_archives() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("x.bin");
    fs::write(&input, b"some data").unwrap();
    let archive = dir.path().join("x.tppr");
    assert!(run(&["pack", path_str(&input), path_str(&archive)])
        .status
        .success());

    let out = run(&["inspect", path_str(&archive), "--tensors"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--tensors applies to tensor archives"));
}
