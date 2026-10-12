//! End-to-end test for packing a GGUF model with `pack-tensors`.

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

/// A GGUF v3 file with one F32 tensor and one Q8_0 tensor.
fn gguf_model() -> Vec<u8> {
    let mut out = b"GGUF".to_vec();
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&2u64.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());

    let put = |out: &mut Vec<u8>, name: &str, dims: &[u64], ggml: u32, offset: u64| {
        out.extend_from_slice(&(name.len() as u64).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(dims.len() as u32).to_le_bytes());
        for d in dims {
            out.extend_from_slice(&d.to_le_bytes());
        }
        out.extend_from_slice(&ggml.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
    };
    put(&mut out, "norm", &[4], 0, 0);
    put(&mut out, "wq", &[32], 8, 32);

    while !out.len().is_multiple_of(32) {
        out.push(0);
    }
    let data_start = out.len();
    out.resize(data_start + 16, 0); // norm: four f32 values
    out.extend((0..16u8).collect::<Vec<u8>>()); // norm data
    out.resize(data_start + 32, 0);
    out.extend((0..34u8).collect::<Vec<u8>>()); // wq: one Q8_0 block
    out
}

#[test]
fn gguf_model_packs_and_inspects_with_its_quantized_type() {
    let dir = TempDir::new().unwrap();
    let model = dir.path().join("model.gguf");
    let archive = dir.path().join("model.tptn");
    fs::write(&model, gguf_model()).unwrap();

    let packed = run(&["pack-tensors", path_str(&model), path_str(&archive)]);
    assert!(packed.status.success(), "{packed:?}");

    let inspected = run(&["inspect", path_str(&archive), "--verify"]);
    assert!(inspected.status.success(), "{inspected:?}");
    let text = String::from_utf8_lossy(&inspected.stdout).into_owned();
    assert!(text.contains("Q8_0"), "{text}");
    assert!(text.contains("all checksums match"), "{text}");

    let unpacked = run(&[
        "unpack-tensors",
        path_str(&archive),
        path_str(&dir.path().join("out.safetensors")),
    ]);
    assert!(!unpacked.status.success());
    assert!(String::from_utf8_lossy(&unpacked.stderr).contains("Q8_0"));
}
