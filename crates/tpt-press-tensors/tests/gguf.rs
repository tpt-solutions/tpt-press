//! Integration tests for GGUF input.

use std::io::Cursor;

use tpt_press_tensors::{gguf, pack, unpack, Archive, Error, PackOptions};

/// One tensor to write into a GGUF file.
struct GgufTensor {
    name: &'static str,
    ggml_type: u32,
    dims: Vec<u64>,
    data: Vec<u8>,
}

/// Builds a GGUF v3 file with the given metadata strings and tensors.
fn gguf_file(kv: &[(&str, &str)], tensors: &[GgufTensor]) -> Vec<u8> {
    let mut out = b"GGUF".to_vec();
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&(tensors.len() as u64).to_le_bytes());
    out.extend_from_slice(&(kv.len() as u64).to_le_bytes());
    for (key, value) in kv {
        put_string(&mut out, key);
        out.extend_from_slice(&8u32.to_le_bytes()); // string value type
        put_string(&mut out, value);
    }

    // Each tensor starts on a 32-byte boundary inside the data section.
    let mut offsets = Vec::new();
    let mut next = 0u64;
    for t in tensors {
        offsets.push(next);
        next += (t.data.len() as u64).div_ceil(32) * 32;
    }
    for (t, offset) in tensors.iter().zip(&offsets) {
        put_string(&mut out, t.name);
        out.extend_from_slice(&(t.dims.len() as u32).to_le_bytes());
        for d in &t.dims {
            out.extend_from_slice(&d.to_le_bytes());
        }
        out.extend_from_slice(&t.ggml_type.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
    }

    while !out.len().is_multiple_of(32) {
        out.push(0);
    }
    let data_start = out.len();
    for (t, offset) in tensors.iter().zip(&offsets) {
        out.resize(data_start + *offset as usize, 0);
        out.extend_from_slice(&t.data);
    }
    out
}

fn put_string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u64).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn floats(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn sample_file() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let norm = floats(&[1.0, 2.0, 3.0, 4.0]);
    let quant: Vec<u8> = (0..34u8).collect(); // one Q8_0 block
    let file = gguf_file(
        &[("general.architecture", "llama")],
        &[
            GgufTensor {
                name: "norm",
                ggml_type: 0,
                dims: vec![4],
                data: norm.clone(),
            },
            GgufTensor {
                name: "blk.0.attn_q.weight",
                ggml_type: 8,
                dims: vec![32],
                data: quant.clone(),
            },
        ],
    );
    (file, norm, quant)
}

#[test]
fn quantized_and_plain_tensors_round_trip_through_an_archive() {
    let (file, norm, quant) = sample_file();
    let mut archive_bytes = Vec::new();
    let stats = pack(
        Cursor::new(file),
        &mut archive_bytes,
        &PackOptions::default(),
    )
    .unwrap();
    assert_eq!(stats.tensors, 2);

    let mut archive = Archive::open(Cursor::new(archive_bytes)).unwrap();
    assert_eq!(
        archive
            .metadata()
            .get("general.architecture")
            .map(String::as_str),
        Some("llama")
    );
    let q = archive.tensor("blk.0.attn_q.weight").unwrap();
    assert_eq!(q.dtype, "Q8_0");
    assert_eq!(q.byte_len, 34);
    assert_eq!(archive.read_tensor("norm").unwrap(), norm);
    assert_eq!(archive.read_tensor("blk.0.attn_q.weight").unwrap(), quant);
    archive.verify().unwrap();
}

#[test]
fn unpacking_quantized_tensors_to_safetensors_is_refused() {
    let (file, _, _) = sample_file();
    let mut archive_bytes = Vec::new();
    pack(
        Cursor::new(file),
        &mut archive_bytes,
        &PackOptions::default(),
    )
    .unwrap();

    let mut archive = Archive::open(Cursor::new(archive_bytes)).unwrap();
    let mut out = Vec::new();
    assert!(matches!(
        unpack(&mut archive, &mut out),
        Err(Error::UnsupportedDtype(dtype)) if dtype == "Q8_0"
    ));
    assert!(
        out.is_empty(),
        "nothing should be written before the refusal"
    );
}

#[test]
fn plain_gguf_unpacks_to_safetensors() {
    let norm = floats(&[5.0, 6.0]);
    let file = gguf_file(
        &[],
        &[GgufTensor {
            name: "a",
            ggml_type: 0,
            dims: vec![2],
            data: norm.clone(),
        }],
    );
    let mut archive_bytes = Vec::new();
    pack(
        Cursor::new(file),
        &mut archive_bytes,
        &PackOptions::default(),
    )
    .unwrap();

    let mut archive = Archive::open(Cursor::new(archive_bytes)).unwrap();
    let mut out = Vec::new();
    unpack(&mut archive, &mut out).unwrap();
    assert!(out.ends_with(&norm));
}

#[test]
fn quantized_tensor_must_be_whole_blocks() {
    let file = gguf_file(
        &[],
        &[GgufTensor {
            name: "bad",
            ggml_type: 8,
            dims: vec![33],
            data: vec![0u8; 34],
        }],
    );
    let mut out = Vec::new();
    assert!(pack(Cursor::new(file), &mut out, &PackOptions::default()).is_err());
}

#[test]
fn unknown_ggml_type_is_refused() {
    let file = gguf_file(
        &[],
        &[GgufTensor {
            name: "mystery",
            ggml_type: 99,
            dims: vec![4],
            data: vec![0u8; 16],
        }],
    );
    let mut out = Vec::new();
    assert!(matches!(
        pack(Cursor::new(file), &mut out, &PackOptions::default()),
        Err(Error::UnsupportedDtype(_))
    ));
}

#[test]
fn unsupported_gguf_version_is_refused() {
    let (mut file, _, _) = sample_file();
    file[4..8].copy_from_slice(&1u32.to_le_bytes());
    assert!(matches!(
        gguf::parse(&mut Cursor::new(file)),
        Err(Error::InvalidModel(_))
    ));
}

#[test]
fn tensor_beyond_the_data_section_is_refused() {
    let (file, _, _) = sample_file();
    // Cut the file short, so the quantized tensor runs past the end.
    let cut = file[..file.len() - 40].to_vec();
    assert!(gguf::parse(&mut Cursor::new(cut)).is_err());
}
