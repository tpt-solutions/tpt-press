//! Delta archives: a fine-tune packed against its base, read back with the base.

use std::io::Cursor;

use tpt_press_tensors::{pack, pack_with_base, Archive, Error, PackOptions};

/// A safetensors file holding `tensors` as (name, dtype, width, data).
fn safetensors(tensors: &[(&str, &str, usize, &[u8])]) -> Vec<u8> {
    let mut header = String::from("{");
    let mut body = Vec::new();
    for (i, (name, dtype, width, data)) in tensors.iter().enumerate() {
        if i > 0 {
            header.push(',');
        }
        header.push_str(&format!(
            "\"{name}\":{{\"dtype\":\"{dtype}\",\"shape\":[{}],\"data_offsets\":[{},{}]}}",
            data.len() / width,
            body.len(),
            body.len() + data.len()
        ));
        body.extend_from_slice(data);
    }
    header.push('}');
    let mut file = (header.len() as u64).to_le_bytes().to_vec();
    file.extend_from_slice(header.as_bytes());
    file.extend_from_slice(&body);
    file
}

/// bf16 weights with a Gaussian-like spread, from a fixed seed.
fn bf16_weights(count: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
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

/// The base's weights with pseudo-random changes to the two lowest mantissa
/// bits: about one value in four changes. That is how a short fine-tune
/// nudges weights, with no regular pattern for the coder to exploit.
fn fine_tune(base: &[u8]) -> Vec<u8> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    base.as_chunks::<2>()
        .0
        .iter()
        .flat_map(|pair| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let mut v = u16::from_le_bytes([pair[0], pair[1]]);
            if x.is_multiple_of(4) {
                v ^= ((x >> 8) & 0b11) as u16;
            }
            v.to_le_bytes()
        })
        .collect()
}

fn base_archive(weights: &[u8]) -> Vec<u8> {
    let file = safetensors(&[("w", "BF16", 2, weights)]);
    let mut out = Vec::new();
    pack(Cursor::new(file), &mut out, &PackOptions::default()).unwrap();
    out
}

#[test]
fn a_fine_tune_packed_against_its_base_reads_back_exactly() {
    let base_weights = bf16_weights(1 << 19, 0x2545_F491_4F6C_DD1D);
    let tuned = fine_tune(&base_weights);
    let extra = vec![7u8; 4096];
    let tuned_file = safetensors(&[("w", "BF16", 2, &tuned), ("extra", "U8", 1, &extra)]);

    let base_bytes = base_archive(&base_weights);
    let mut base = Archive::open(Cursor::new(base_bytes.clone())).unwrap();

    let mut delta_out = Vec::new();
    let stats = pack_with_base(
        Cursor::new(tuned_file.clone()),
        &mut delta_out,
        &PackOptions::default(),
        &mut base,
    )
    .unwrap();
    assert!(stats.delta_pieces > 0, "no piece used the delta");

    // Packed alone, the fine-tune is larger: the delta is almost all zeros.
    let mut standalone = Vec::new();
    pack(
        Cursor::new(tuned_file),
        &mut standalone,
        &PackOptions::default(),
    )
    .unwrap();
    assert!(
        delta_out.len() * 100 < standalone.len() * 60,
        "delta {} vs standalone {}",
        delta_out.len(),
        standalone.len()
    );

    eprintln!(
        "delta {} bytes, standalone {} bytes, raw {} bytes, delta pieces {}",
        delta_out.len(),
        standalone.len(),
        tuned.len() + extra.len(),
        stats.delta_pieces
    );
    let base_again = Archive::open(Cursor::new(base_bytes)).unwrap();
    let mut reader = Archive::open_with_base(Cursor::new(delta_out.clone()), base_again).unwrap();
    reader.verify().unwrap();
    assert_eq!(reader.read_tensor("w").unwrap(), tuned);
    assert_eq!(reader.read_tensor("extra").unwrap(), extra);
}

#[test]
fn a_delta_refuses_to_read_without_its_base() {
    let base_weights = bf16_weights(1 << 16, 99);
    let tuned = fine_tune(&base_weights);
    let base_bytes = base_archive(&base_weights);
    let mut base = Archive::open(Cursor::new(base_bytes)).unwrap();
    let file = safetensors(&[("w", "BF16", 2, &tuned)]);
    let mut out = Vec::new();
    pack_with_base(
        Cursor::new(file),
        &mut out,
        &PackOptions::default(),
        &mut base,
    )
    .unwrap();

    // Plain open succeeds, but a delta piece cannot be read.
    let mut plain = Archive::open(Cursor::new(out.clone())).unwrap();
    assert!(matches!(plain.read_tensor("w"), Err(Error::BaseNeeded(_))));
    // Opening with the wrong base is refused before any piece is read.
    let other = base_archive(&bf16_weights(1 << 16, 7));
    let wrong = Archive::open(Cursor::new(other)).unwrap();
    assert!(matches!(
        Archive::open_with_base(Cursor::new(out), wrong),
        Err(Error::BaseMismatch { .. })
    ));
}

#[test]
fn a_plain_archive_is_not_opened_as_a_delta() {
    let weights = bf16_weights(1 << 12, 3);
    let plain = base_archive(&weights);
    let base = Archive::open(Cursor::new(plain.clone())).unwrap();
    assert!(matches!(
        Archive::open_with_base(Cursor::new(plain), base),
        Err(Error::InvalidOption(_))
    ));
}
