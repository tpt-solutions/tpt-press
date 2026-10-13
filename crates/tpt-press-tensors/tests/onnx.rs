//! Integration tests for ONNX input.

use std::io::Cursor;

use tpt_press_tensors::{onnx, pack, unpack, Archive, Error, PackOptions};

fn varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn key(field: u32, wire: u8, out: &mut Vec<u8>) {
    varint(u64::from(field) << 3 | u64::from(wire), out);
}

fn bytes_field(field: u32, payload: &[u8], out: &mut Vec<u8>) {
    key(field, 2, out);
    varint(payload.len() as u64, out);
    out.extend_from_slice(payload);
}

fn varint_field(field: u32, value: u64, out: &mut Vec<u8>) {
    key(field, 0, out);
    varint(value, out);
}

/// A TensorProto with the given name, type, dims and raw bytes.
fn tensor(name: &str, data_type: u64, dims: &[u64], raw: &[u8]) -> Vec<u8> {
    let mut packed = Vec::new();
    for d in dims {
        varint(*d, &mut packed);
    }
    let mut t = Vec::new();
    bytes_field(1, &packed, &mut t);
    varint_field(2, data_type, &mut t);
    bytes_field(8, name.as_bytes(), &mut t);
    bytes_field(9, raw, &mut t);
    t
}

/// A ModelProto holding one graph with the given initializers.
fn model(producer: &str, initializers: &[Vec<u8>]) -> Vec<u8> {
    let mut graph = Vec::new();
    bytes_field(2, b"main", &mut graph);
    for init in initializers {
        bytes_field(5, init, &mut graph);
    }
    let mut out = Vec::new();
    varint_field(1, 8, &mut out); // ir_version
    bytes_field(2, producer.as_bytes(), &mut out);
    varint_field(5, 3, &mut out); // model_version
    bytes_field(7, &graph, &mut out);
    out
}

fn sample() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let weight: Vec<u8> = (0..24u8).collect(); // F32 [2,3]
    let steps: Vec<u8> = (0..16u8).rev().collect(); // I64 [2]
    let file = model(
        "unit-test",
        &[
            tensor("weight", 1, &[2, 3], &weight),
            tensor("steps", 7, &[2], &steps),
        ],
    );
    (file, weight, steps)
}

#[test]
fn initializers_round_trip_through_an_archive() {
    let (file, weight, steps) = sample();
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
            .get("onnx.producer_name")
            .map(String::as_str),
        Some("unit-test")
    );
    assert_eq!(
        archive
            .metadata()
            .get("onnx.graph_name")
            .map(String::as_str),
        Some("main")
    );
    let w = archive.tensor("weight").unwrap();
    assert_eq!(w.dtype, "F32");
    assert_eq!(w.shape, vec![2, 3]);
    assert_eq!(archive.read_tensor("weight").unwrap(), weight);
    assert_eq!(archive.read_tensor("steps").unwrap(), steps);
    archive.verify().unwrap();
}

#[test]
fn onnx_weights_unpack_to_safetensors() {
    let (file, weight, _) = sample();
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
    assert!(out.windows(weight.len()).any(|w| w == weight.as_slice()));
}

#[test]
fn parse_reports_locations_without_reading_weights() {
    let (file, _, _) = sample();
    let model = onnx::parse(&mut Cursor::new(file.clone())).unwrap();
    let w = model.tensors.iter().find(|t| t.name == "weight").unwrap();
    assert_eq!(
        &file[w.begin as usize..w.end as usize],
        (0..24u8).collect::<Vec<u8>>().as_slice()
    );
}

#[test]
fn external_data_is_refused() {
    let mut t = tensor("w", 1, &[1], &[0, 0, 0, 0]);
    varint_field(14, 1, &mut t); // data_location = EXTERNAL
    let file = model("x", &[t]);
    assert!(matches!(
        onnx::parse(&mut Cursor::new(file)),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn typed_data_without_raw_data_is_refused() {
    let mut t = Vec::new();
    bytes_field(1, &[1], &mut t);
    varint_field(2, 1, &mut t);
    bytes_field(8, b"w", &mut t);
    let file = model("x", &[t]);
    assert!(matches!(
        onnx::parse(&mut Cursor::new(file)),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn raw_data_must_match_the_shape() {
    let file = model("x", &[tensor("w", 1, &[2, 3], &[0u8; 20])]);
    assert!(matches!(
        onnx::parse(&mut Cursor::new(file)),
        Err(Error::InvalidModel(_))
    ));
}

#[test]
fn unsupported_data_type_is_refused() {
    let file = model("x", &[tensor("s", 8, &[1], b"abcd")]); // STRING
    assert!(matches!(
        onnx::parse(&mut Cursor::new(file)),
        Err(Error::UnsupportedDtype(_))
    ));
}

#[test]
fn packing_a_non_model_file_explains_both_failures() {
    let mut out = Vec::new();
    let err = pack(
        Cursor::new(b"neither safetensors nor onnx, just some text".to_vec()),
        &mut out,
        &PackOptions::default(),
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("safetensors") && message.contains("ONNX"),
        "{message}"
    );
}
