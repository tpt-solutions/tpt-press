//! Compares the Gorilla codecs with raw storage and with zstd, on series that
//! look like metrics: regular and jittered timestamps, quantised readings, and
//! full-precision values.
//!
//! Run with `cargo run --release -p tpt-press-logs --example gorilla_sizes`.

use tpt_press_core::codec;
use tpt_press_logs::gorilla;

/// A small deterministic generator, so the numbers are the same every run.
struct Lcg(u64);

impl Lcg {
    fn next_unit(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn report(name: &str, raw: &[u8], gorilla_len: usize) {
    let zstd_len = codec::compress(raw, 3).expect("zstd").len();
    let shuffled_len = codec::compress(&byte_shuffle(raw, 8), 3)
        .expect("zstd")
        .len();
    println!(
        "{name:<38} raw {:>9}  zstd {:>9}  shuffle+zstd {:>9}  gorilla {:>9}",
        raw.len(),
        zstd_len,
        shuffled_len,
        gorilla_len,
    );
}

/// Groups the bytes of 8-byte values by position, as the tensor codec does.
fn byte_shuffle(data: &[u8], width: usize) -> Vec<u8> {
    let n = data.len() / width;
    let mut out = vec![0u8; data.len()];
    for (i, element) in data.chunks_exact(width).enumerate() {
        for (b, &byte) in element.iter().enumerate() {
            out[b * n + i] = byte;
        }
    }
    out
}

fn main() {
    let n = 1_000_000;
    let mut rng = Lcg(42);

    // Timestamps in milliseconds: one point per second, with up to 20 ms of jitter.
    let mut t = 1_767_225_600_000i64;
    let jittered: Vec<i64> = (0..n)
        .map(|_| {
            t += 1000 + (rng.next_unit() * 40.0) as i64 - 20;
            t
        })
        .collect();
    let raw: Vec<u8> = jittered.iter().flat_map(|v| v.to_le_bytes()).collect();
    report(
        "timestamps, 1 s with jitter",
        &raw,
        gorilla::encode_timestamps(&jittered).len(),
    );

    // Timestamps every 15 s, exactly.
    let exact: Vec<i64> = (0..n as i64)
        .map(|i| 1_767_225_600_000 + i * 15_000)
        .collect();
    let raw: Vec<u8> = exact.iter().flat_map(|v| v.to_le_bytes()).collect();
    report(
        "timestamps, every 15 s exactly",
        &raw,
        gorilla::encode_timestamps(&exact).len(),
    );

    // A temperature that wanders and is read to 0.1 degree.
    let mut temp = 21.5f64;
    let quantised: Vec<f64> = (0..n)
        .map(|_| {
            temp = (temp + (rng.next_unit() - 0.5) * 0.2).clamp(15.0, 30.0);
            (temp * 10.0).round() / 10.0
        })
        .collect();
    let raw: Vec<u8> = quantised.iter().flat_map(|v| v.to_le_bytes()).collect();
    report(
        "temperature, read to 0.1 degree",
        &raw,
        gorilla::encode_floats(&quantised).len(),
    );

    // The same wander with no quantisation, so almost every bit changes.
    let mut x = 21.5f64;
    let full: Vec<f64> = (0..n)
        .map(|_| {
            x = (x + (rng.next_unit() - 0.5) * 0.2).clamp(15.0, 30.0);
            x
        })
        .collect();
    let raw: Vec<u8> = full.iter().flat_map(|v| v.to_le_bytes()).collect();
    report(
        "temperature, full precision",
        &raw,
        gorilla::encode_floats(&full).len(),
    );
}
