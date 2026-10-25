//! The byte-shuffle encoding for floating-point tensors.

use std::io::Cursor;

use tpt_press_tensors::{pack, Archive, PackOptions};

/// A safetensors file holding one tensor of `count` elements of `dtype`.
fn safetensors(dtype: &str, width: usize, data: &[u8]) -> Vec<u8> {
    let count = data.len() / width;
    let header = format!(
        "{{\"w\":{{\"dtype\":\"{dtype}\",\"shape\":[{count}],\"data_offsets\":[0,{}]}}}}",
        data.len()
    );
    let mut file = (header.len() as u64).to_le_bytes().to_vec();
    file.extend_from_slice(header.as_bytes());
    file.extend_from_slice(data);
    file
}

/// bf16 weights with a Gaussian-like spread, from a fixed seed. Truncated f32
/// bits, as bf16 is stored.
fn bf16_weights(count: usize) -> Vec<u8> {
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        ((x >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    };
    (0..count)
        .flat_map(|_| {
            let (u1, u2) = (next(), next());
            let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
            let bits = ((z * 0.02) as f32).to_bits();
            ((bits >> 16) as u16).to_le_bytes()
        })
        .collect()
}

fn pack_bytes(file: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    pack(Cursor::new(file), &mut out, &PackOptions::default()).unwrap();
    out
}

#[test]
fn bf16_weights_compress_past_plain_zstd_and_round_trip() {
    let weights = bf16_weights(1 << 20);
    let file = safetensors("BF16", 2, &weights);
    let archive = pack_bytes(&file);

    // Plain zstd saves about 22% on this kind of data. The shuffle does better.
    assert!(
        archive.len() * 100 < weights.len() * 75,
        "archive is {} bytes for {} bytes of weights",
        archive.len(),
        weights.len()
    );

    let mut reader = Archive::open(Cursor::new(archive)).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.read_tensor("w").unwrap(), weights);
}

#[test]
fn bf16_exponent_split_beats_the_shuffle_and_round_trips() {
    let weights = bf16_weights(1 << 20);
    let archive = pack_bytes(&safetensors("BF16", 2, &weights));
    eprintln!(
        "bf16 split: {} bytes for {} bytes of weights ({:.1}%)",
        archive.len(),
        weights.len(),
        archive.len() as f64 * 100.0 / weights.len() as f64
    );

    let mut reader = Archive::open(Cursor::new(archive.clone())).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.read_tensor("w").unwrap(), weights);
}

#[test]
fn f32_tensors_round_trip_through_the_shuffle() {
    let values: Vec<u8> = (0..50_000u32)
        .flat_map(|i| (i as f32 * 0.001).sin().to_le_bytes())
        .collect();
    let archive = pack_bytes(&safetensors("F32", 4, &values));
    let mut reader = Archive::open(Cursor::new(archive)).unwrap();
    assert_eq!(reader.read_tensor("w").unwrap(), values);
}

#[test]
fn byte_tensors_skip_the_shuffle_and_still_round_trip() {
    let bytes: Vec<u8> = (0..100_000u32).map(|i| (i * 7 % 256) as u8).collect();
    let archive = pack_bytes(&safetensors("U8", 1, &bytes));
    let mut reader = Archive::open(Cursor::new(archive)).unwrap();
    assert_eq!(reader.read_tensor("w").unwrap(), bytes);
}

#[test]
fn incompressible_floats_are_stored_raw() {
    // Random bits have no structure, so no encoding should beat raw storage by much.
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let noise: Vec<u8> = (0..200_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    let archive = pack_bytes(&safetensors("F32", 4, &noise));
    // Only the header, the footer and the index add to the raw size.
    assert!(
        archive.len() < noise.len() + 4096,
        "{} bytes",
        archive.len()
    );
    let mut reader = Archive::open(Cursor::new(archive)).unwrap();
    assert_eq!(reader.read_tensor("w").unwrap(), noise);
}
